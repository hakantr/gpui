//! The Metal half of the bounded external-surface bridge: the registry that owns the GPU resources
//! an externally produced surface is drawn from, and the single privileged producer accessor that
//! lets the external compositor draw into one.
//!
//! This is the sibling of `gpui_windows/src/external_registry.rs` and
//! `gpui_wgpu/src/external_registry.rs`, deliberately structured and named the same way so the
//! three backends read as one implementation.
//!
//! Two things live here, and they are kept apart on purpose:
//!
//! * [`ExternalSurfaceRegistry`] — crate-private storage. It maps the opaque
//!   [`ExternalSurfaceHandle`] onto the texture, size, byte order and byte cost of a surface, and
//!   it refuses an extent the device cannot create. The renderer resolves handles against it at
//!   draw time. It never hands out the device.
//! * [`ExternalSurfaceProducer`] — the **producer** face, reached through the platform-specific
//!   `gpui_macos::external_surface_producer` lookup. It is for the one privileged compositor and it
//!   carries exactly what producing a surface needs: the [`metal::Device`] and the
//!   [`metal::CommandQueue`] (unavoidable — a producer cannot create a pipeline, a render pass or a
//!   command buffer without them), plus registering, retiring, reading the device generation, and
//!   one exact-set collective generation rebuild. No consumer shader source, no GPUI render pass,
//!   no drawable target, and no raw registry or unbounded reset authority.
//!
//! Ordinary GPUI consumers never see any of this: they see `Window::paint_external_surface` and
//! `Window::external_surface_capabilities`, and an opaque handle. Reaching the producer requires
//! taking a direct dependency on the platform crate that re-exports the producer.
//!
//! Every surface lives in GPUI's own device, so the sync model of this step is
//! `SameQueueOrdered`, and on Metal that guarantee holds across command buffers: a producer pass
//! encoded in a **separate** `MTLCommandBuffer`, committed before GPUI's, still
//! yields the finished content to GPUI's own command buffer on the same `MTLCommandQueue`. The
//! producer therefore does not have to share GPUI's command buffer — which it could not reach
//! anyway — and submission order on the shared queue is the whole story. Neither a fence nor a
//! cross-device shared handle is part of this step, and neither is claimed in the capability
//! snapshot.
//!
//! The producer's exact-set rebuild kills every handle collectively. **macOS is the one profile
//! with no programmatic device-loss notification**: Metal has no `DXGI_ERROR_DEVICE_REMOVED`
//! equivalent and no `Device::on_uncaptured_error`/`lost` future.
//! Production therefore exposes no fabricated loss event; the public bounded rebuild consumes the
//! private transaction, while its confirmed-loss arm remains test-only.

use std::{cell::RefCell, rc::Rc};

use collections::{FxHashMap, FxHashSet};
use gpui::{
    BindingProof, CloseOutcome, DevicePixels, EXTERNAL_CONTRACT_VERSION,
    ExternalSurfaceCapabilities, ExternalSurfaceError, ExternalSurfaceFormat,
    ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError,
    ExternalSurfaceHandle, PublicationId, PublicationLedger, RegistryObservation,
    RegistryUnavailableReason, RetireSafety, Size,
};
use metal::{MTLGPUFamily, MTLPixelFormat, MTLStorageMode, MTLTextureUsage};

/// The bytes one pixel of an external surface costs. Contract v1 has a single logical format,
/// 8 bits per channel `unorm`.
const BYTES_PER_PIXEL: u64 = 4;

/// The device-free bookkeeping half of the registry: identity, generation and byte accounting.
///
/// It is split out from [`ExternalSurfaceRegistry`] so the whole admission path — id and
/// generation assignment, byte and surface-count accounting, and every [`ExternalSurfaceError`] it
/// can produce — is exercised by unit tests that need no GPU at all.
#[derive(Debug)]
struct ExternalSurfaceLedger {
    /// The device generation in force. Raising it invalidates every handle.
    generation: u64,
    /// The id the next registration receives. Ids are never reused, not even across generations.
    next_id: u64,
    /// The total bytes of every currently registered surface.
    registered_bytes: u64,
    /// How many surfaces are currently registered.
    in_flight: u32,
    /// How many draws were skipped because the handle no longer resolved. Telemetry only: a stale
    /// draw is skipped and counted, never a panic.
    skipped_draws: u64,
}

impl ExternalSurfaceLedger {
    fn new() -> Self {
        Self {
            // Generation 0 is legal: a handle minted before any collective transition is fresh
            // for it, and the first transition moves everything to 1.
            generation: 0,
            next_id: 0,
            registered_bytes: 0,
            in_flight: 0,
            skipped_draws: 0,
        }
    }

    /// Admits one surface and returns its identity and byte cost.
    ///
    /// A non-positive extent cannot be a texture at all, so it is
    /// [`ExternalSurfaceError::InvalidGroup`], and it is rejected before anything else. The byte
    /// order is checked next. There is no other admission limit: the device's own extent limit is
    /// applied by the registry, which knows the device.
    fn admit(
        &mut self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, u64), ExternalSurfaceError> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            return Err(ExternalSurfaceError::InvalidGroup);
        }

        // There is no silent per-platform format substitution: this backend reports `Bgra8Unorm`
        // and only `Bgra8Unorm`, which is the byte order every Metal render target in GPUI uses.
        if format != ExternalSurfaceFormat::Bgra8Unorm {
            return Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Bgra8Unorm,
                actual: format,
            });
        }

        let bytes = size.width.0 as u64 * size.height.0 as u64 * BYTES_PER_PIXEL;
        let handle = ExternalSurfaceHandle::new(self.next_id, self.generation);
        self.next_id += 1;
        self.registered_bytes = self.registered_bytes.saturating_add(bytes);
        self.in_flight += 1;
        Ok((handle, bytes))
    }

    /// Gives an admitted surface's accounting back, after a retire or a failed allocation.
    fn withdraw(&mut self, bytes: u64) {
        self.registered_bytes = self.registered_bytes.saturating_sub(bytes);
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Moves to an already checked successor generation and drops its resource accounting.
    fn advance_generation(&mut self, current_generation: u64) {
        self.generation = current_generation;
        self.registered_bytes = 0;
        self.in_flight = 0;
    }

    /// Clears accounting after a real loss at the terminal generation, without wrapping.
    #[cfg(test)]
    fn exhaust_generation(&mut self) {
        self.generation = u64::MAX;
        self.registered_bytes = 0;
        self.in_flight = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenerationAdvanceCause {
    DeliberateRebuild,
    // Metal exposes no confirmed device-loss event. This arm exists only so the renderer's
    // same-file and integration tests exercise the shared no-wrap transaction without creating a
    // fake production loss API.
    #[cfg(test)]
    DeviceLost,
}

/// One registered surface: the resource GPUI owns on behalf of the producer.
struct RegisteredExternalSurface {
    /// The texture itself. GPUI keeps its own reference for as long as the surface is registered,
    /// so the producer's clone can never leave the renderer sampling a freed texture.
    texture: metal::Texture,
    /// The physical size, in device pixels. Crops are normalized against this, not against the
    /// descriptor, so a descriptor that disagrees with the resource cannot push the sampling
    /// outside the texture.
    size: Size<DevicePixels>,
    /// The byte order. Contract v1 allows a single one here, but it is stored rather than assumed
    /// so that adding the `Rgba8Unorm` fallback stays an additive change.
    _format: ExternalSurfaceFormat,
    /// What this surface costs in bytes.
    bytes: u64,
}

/// The renderer-side storage of every registered external surface.
///
/// This is the resource owner named by the contract: the consumer carries only the opaque
/// `{ id, generation }` identity, and nothing about the underlying GPU resource reaches GPUI's
/// public API. The registry **never** exposes its device.
pub(crate) struct ExternalSurfaceRegistry {
    /// The device surfaces are created on. Private on purpose: the producer receives the device
    /// from [`ExternalSurfaceProducer`], which is the one accessor documented to carry it.
    device: metal::Device,
    /// The storage mode registered surfaces are created with. See [`surface_storage_mode`].
    storage_mode: MTLStorageMode,
    /// The device's own largest 2D texture edge. See [`device_max_texture_dimension`].
    max_texture_dimension: u64,
    ledger: ExternalSurfaceLedger,
    /// The publication ledger this window owns. Contract 1.1: the sole owner of the scene
    /// generation, liveness, the sticky exhaustion flag and the retire watermark.
    publications: PublicationLedger,
    surfaces: FxHashMap<u64, RegisteredExternalSurface>,
    /// How many observation queries this registry has answered (contract 1.2).
    ///
    /// **Test apparatus, per registry, never process-global.** It compiles only under
    /// `test`/`test-support`, it is not contract, ledger or publication state — so it does not
    /// touch the read-only invariant — and it exists so a guard can prove the query really
    /// reached the producer entry, or really did not.
    #[cfg(any(test, feature = "test-support"))]
    observation_calls: std::cell::Cell<u32>,
}

impl ExternalSurfaceRegistry {
    /// Builds an empty registry on GPUI's own device.
    pub(crate) fn new(device: metal::Device) -> Self {
        let storage_mode = surface_storage_mode(&device);
        let max_texture_dimension = device_max_texture_dimension(&device);
        Self {
            device,
            storage_mode,
            max_texture_dimension,
            ledger: ExternalSurfaceLedger::new(),
            publications: PublicationLedger::new(0),
            surfaces: FxHashMap::default(),
            #[cfg(any(test, feature = "test-support"))]
            observation_calls: std::cell::Cell::new(0),
        }
    }

    /// Registers a surface and returns its identity together with a clone of the texture.
    ///
    /// The texture is created on GPUI's own device with `RenderTarget | ShaderRead` usage: the
    /// first is what lets the producer draw into it, the second is what lets GPUI sample it. There
    /// is no shared handle and no cross-device path, because this step is deliberately the
    /// same-device one. The clone is what makes the producer able to build its own render pass
    /// over the surface; nothing else about the resource leaves this module.
    pub(crate) fn register(
        &mut self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, metal::Texture), ExternalSurfaceError> {
        if self.publications.device_generation_exhausted() {
            return Err(ExternalSurfaceError::DeviceLost);
        }

        let (handle, bytes) = self.ledger.admit(size, format)?;
        // `new_texture` aborts rather than returning on failure, so the device's own extent limit
        // is checked first, and an extent beyond it is refused the way D3D11 reports a refused
        // allocation: `TransientFailure`, with nothing consumed.
        if size.width.0 as u64 > self.max_texture_dimension
            || size.height.0 as u64 > self.max_texture_dimension
        {
            self.ledger.withdraw(bytes);
            return Err(ExternalSurfaceError::TransientFailure);
        }

        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(size.width.0 as u64);
        descriptor.set_height(size.height.0 as u64);
        descriptor.set_pixel_format(metal_format(format));
        descriptor.set_usage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.set_storage_mode(self.storage_mode);
        let texture = self.device.new_texture(&descriptor);

        self.surfaces.insert(
            handle.id,
            RegisteredExternalSurface {
                texture: texture.clone(),
                size,
                _format: format,
                bytes,
            },
        );
        Ok((handle, texture))
    }

    /// The texture a fresh handle samples, or `None` when the handle is stale or unknown.
    ///
    /// A stale handle is not an error here and never a panic: the draw path skips the surface and
    /// counts it, and the consumer learns of the death from the device generation.
    pub(crate) fn resolve(&self, handle: ExternalSurfaceHandle) -> Option<&metal::Texture> {
        self.entry(handle).map(|surface| &surface.texture)
    }

    /// The physical size of a fresh handle's surface, or `None` when it is stale or unknown.
    pub(crate) fn surface_size(&self, handle: ExternalSurfaceHandle) -> Option<Size<DevicePixels>> {
        self.entry(handle).map(|surface| surface.size)
    }

    fn entry(&self, handle: ExternalSurfaceHandle) -> Option<&RegisteredExternalSurface> {
        if self.publications.device_generation_exhausted()
            || !handle.is_fresh_for(self.ledger.generation)
        {
            return None;
        }
        self.surfaces.get(&handle.id)
    }

    /// Drops a surface and gives its accounting back. Retiring a stale handle is a no-op: the
    /// generation bump already released everything.
    pub(crate) fn retire(&mut self, handle: ExternalSurfaceHandle) {
        if self.publications.device_generation_exhausted()
            || !handle.is_fresh_for(self.ledger.generation)
        {
            return;
        }
        if let Some(surface) = self.surfaces.remove(&handle.id) {
            self.ledger.withdraw(surface.bytes);
        }
    }

    /// The device generation in force.
    pub(crate) fn device_generation(&self) -> u64 {
        self.ledger.generation
    }

    /// Validates that `expected_surfaces` is this registry's exact physical live set.
    fn validate_expected_surfaces(
        &self,
        expected_surfaces: &[ExternalSurfaceHandle],
    ) -> Result<(u64, u64), ExternalSurfaceGenerationRolloverError> {
        let registry_surface_count = u64::try_from(self.surfaces.len()).map_err(|_| {
            ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                reason: RegistryUnavailableReason::AccountingOverflow,
            }
        })?;
        let registry_bytes = self
            .surfaces
            .values()
            .try_fold(0u64, |total, surface| total.checked_add(surface.bytes))
            .ok_or(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingOverflow,
                },
            )?;

        if registry_surface_count != u64::from(self.ledger.in_flight)
            || registry_bytes != self.ledger.registered_bytes
        {
            return Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingMismatch,
                },
            );
        }

        let expected_surface_count = u64::try_from(expected_surfaces.len()).map_err(|_| {
            ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                reason: RegistryUnavailableReason::AccountingOverflow,
            }
        })?;
        if expected_surface_count != registry_surface_count {
            return Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch {
                expected_surface_count,
                registry_surface_count,
            });
        }

        let mut seen = FxHashSet::default();
        if expected_surfaces.iter().any(|handle| {
            !handle.is_fresh_for(self.ledger.generation)
                || !self.surfaces.contains_key(&handle.id)
                || !seen.insert(*handle)
        }) {
            return Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch {
                expected_surface_count,
                registry_surface_count,
            });
        }

        Ok((registry_surface_count, registry_bytes))
    }

    /// The one collective generation transaction used by deliberate rebuild and confirmed loss.
    fn advance_external_surface_generation(
        &mut self,
        cause: GenerationAdvanceCause,
        released_surface_count: u64,
        released_external_surface_bytes: u64,
    ) -> Result<ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError> {
        let previous_generation = self.ledger.generation;
        let Some(current_generation) = previous_generation.checked_add(1) else {
            return match cause {
                GenerationAdvanceCause::DeliberateRebuild => {
                    Err(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                        generation: previous_generation,
                    })
                }
                #[cfg(test)]
                GenerationAdvanceCause::DeviceLost => {
                    self.surfaces.clear();
                    self.ledger.exhaust_generation();
                    self.publications.exhaust_device_generation();
                    Ok(
                        ExternalSurfaceGenerationRollover::from_committed_registry_rollover(
                            previous_generation,
                            previous_generation,
                            released_surface_count,
                            released_external_surface_bytes,
                        ),
                    )
                }
            };
        };

        self.surfaces.clear();
        self.ledger.advance_generation(current_generation);
        self.publications
            .advance_device_generation(current_generation);

        Ok(
            ExternalSurfaceGenerationRollover::from_committed_registry_rollover(
                previous_generation,
                current_generation,
                released_surface_count,
                released_external_surface_bytes,
            ),
        )
    }

    /// Deliberately replaces this registry's complete external-surface domain.
    fn rebuild_external_surface_domain(
        &mut self,
        expected_surfaces: &[ExternalSurfaceHandle],
    ) -> Result<ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError> {
        let (released_surface_count, released_external_surface_bytes) =
            self.validate_expected_surfaces(expected_surfaces)?;
        self.advance_external_surface_generation(
            GenerationAdvanceCause::DeliberateRebuild,
            released_surface_count,
            released_external_surface_bytes,
        )
    }

    /// Test-only confirmed-loss entry used by this module and the renderer integration harness.
    /// Production Metal exposes no event that could call it honestly.
    #[cfg(test)]
    pub(crate) fn invalidate_all(&mut self) {
        let released_surface_count = u64::try_from(self.surfaces.len()).unwrap_or(u64::MAX);
        let released_external_surface_bytes = self
            .surfaces
            .values()
            .try_fold(0u64, |total, surface| total.checked_add(surface.bytes))
            .unwrap_or(u64::MAX);
        self.advance_external_surface_generation(
            GenerationAdvanceCause::DeviceLost,
            released_surface_count,
            released_external_surface_bytes,
        )
        .expect("confirmed loss always clears; terminal generation never wraps");
    }

    /// Records that a consumer draw command was successfully issued for `handle`.
    ///
    /// Crate-private and reached only from the renderer's draw path. It is deliberately absent
    /// from [`ExternalSurfaceProducer`], so neither a producer nor a consumer can declare a
    /// surface drawn.
    pub(crate) fn note_drawn(&mut self, handle: ExternalSurfaceHandle) {
        self.publications.note_drawn(handle);
    }

    pub(crate) fn publications_mut(&mut self) -> &mut PublicationLedger {
        &mut self.publications
    }

    pub(crate) fn publications(&self) -> &PublicationLedger {
        &self.publications
    }

    /// This registry's read-only observation, derived from its own snapshot (contract 1.2).
    ///
    /// One immutable borrow, one snapshot: the count and the byte total are read from the same
    /// collection, so no registration or retirement can slip between them, and the scope and
    /// generation travel with them. The measure is derived from the **real collection**; the
    /// ledger is only the cross-check. Nothing here mutates the registry, the ledger or the
    /// publication book.
    pub(crate) fn observation(&self, producer_generation: u64) -> RegistryObservation {
        if self.publications.device_generation_exhausted() {
            return RegistryObservation::from_device_generation_exhausted(
                self.publications.registry_scope(),
                producer_generation,
            );
        }
        RegistryObservation::from_registry_snapshot(
            self.publications.registry_scope(),
            producer_generation,
            self.surfaces.len(),
            self.surfaces.values().map(|surface| surface.bytes),
            self.ledger.in_flight,
            self.ledger.registered_bytes,
        )
    }

    /// Records one observation query. Checked: the apparatus never wraps silently.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn note_observation_call(&self) {
        let sonraki = self
            .observation_calls
            .get()
            .checked_add(1)
            .expect("gozlem cagri sayaci tukendi; sessiz sarma YASAK");
        self.observation_calls.set(sonraki);
    }

    /// How many observation queries this registry has answered.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn observation_calls(&self) -> u32 {
        self.observation_calls.get()
    }

    /// Records that a draw was skipped because its handle no longer resolved, and returns how many
    /// have been skipped over the life of this registry.
    ///
    /// Skipping is the contract's answer to a dead handle — never a panic, and never a draw from
    /// content the consumer no longer owns — so the count is the only trace it leaves.
    pub(crate) fn note_skipped_draw(&mut self) -> u64 {
        self.ledger.skipped_draws += 1;
        self.ledger.skipped_draws
    }

    /// The capability snapshot of this backend.
    ///
    /// The snapshot is read once over the window/device lifetime rather than per frame.
    pub(crate) fn capabilities(&self) -> ExternalSurfaceCapabilities {
        ExternalSurfaceCapabilities {
            supported: true,
            contract_version: EXTERNAL_CONTRACT_VERSION,
            device_generation: self.ledger.generation,
            // Every Metal render target in GPUI is `MTLPixelFormat::BGRA8Unorm` — the layer, the
            // path intermediate and the headless target alike — so BGRA is the native byte order
            // here and the RGBA fallback is not offered: a mismatch has to be an observable
            // `FormatMismatch`, not a silent conversion.
            format_bgra8_unorm: true,
            format_rgba8_unorm: false,
            // Both of the contract's sampling modes are real pipeline state here: the external
            // pipeline carries a nearest and a linear `MTLSamplerState`, and the descriptor picks
            // between them per draw.
            sampling_nearest: true,
            sampling_linear: true,
            // The producer draws on GPUI's own device and submits on GPUI's own command queue, so
            // submission order is the whole synchronization story of this step — and on Metal that
            // is not a hopeful reading: a producer pass in a *separate*
            // `MTLCommandBuffer`, committed first, is already visible to GPUI's own buffer on the
            // same queue.
            sync_same_queue_ordered: true,
            sync_context_ordered: false,
            // Neither is claimed, because neither is implemented. Metal does have shared events and
            // `MTLFence`, but this step does not create, signal or wait on one, and an unclaimed
            // capability is better than an unbacked one. The keyed mutex is a D3D11 concept with no
            // Metal counterpart at all.
            sync_fence: false,
            sync_keyed_mutex: false,
            // Honest, and derived rather than assumed: a registered surface is created with
            // `MTLStorageMode::Shared` on unified memory and `MTLStorageMode::Managed` otherwise,
            // and `MTLTexture::replaceRegion` — the CPU write path — is legal on both and illegal
            // only on `Private`. So a CPU producer really does have a path here, and both fields
            // follow the storage mode instead of being copied from another backend's answer.
            sync_cpu_ready: self.storage_mode != MTLStorageMode::Private,
            supports_affine: true,
            supports_crop: true,
            supports_clip: true,
            cpu_fallback: self.storage_mode != MTLStorageMode::Private,
            // The render thread never waits, and this step does not keep a previous generation
            // alive to fall back on: an unready group is skipped and reported rather than drawn
            // from stale content.
            allow_stale_reuse: false,
        }
    }
}

/// The storage mode registered surfaces are created with, and with it the honest answer to
/// `cpu_fallback`.
///
/// This is the same rule GPUI already applies to its own CPU-written resources — `Shared` where the
/// CPU and GPU share memory, `Managed` where they do not — rather than the `Private` a
/// GPU-only render target would get. The difference is deliberate: `replaceRegion` is rejected on a
/// `Private` texture, so choosing `Private` would mean reporting `cpu_fallback: false`, and the
/// contract's CPU fallback is worth the managed copy on the one configuration that pays for it
/// (an Intel Mac with a discrete GPU). The producer's own render pass works identically under all
/// three modes.
fn surface_storage_mode(device: &metal::DeviceRef) -> MTLStorageMode {
    if device.has_unified_memory() {
        MTLStorageMode::Shared
    } else {
        MTLStorageMode::Managed
    }
}

/// The device's own largest 2D texture edge, in pixels.
///
/// Metal has no query for it; Apple's Metal feature set tables give it per GPU family: 16384 for
/// the Apple3 family and later and for every Mac family, 8192 for Apple1 and Apple2.
fn device_max_texture_dimension(device: &metal::DeviceRef) -> u64 {
    if device.supports_family(MTLGPUFamily::Apple3)
        || device.supports_family(MTLGPUFamily::Mac1)
        || device.supports_family(MTLGPUFamily::Mac2)
    {
        16_384
    } else {
        8_192
    }
}

/// The Metal byte order of a contract format.
///
/// Only `Bgra8Unorm` is reported by this backend and `ExternalSurfaceLedger::admit` refuses
/// everything else before a texture is ever created; the second arm is the truthful mapping of the
/// fallback byte order rather than a silent substitution, so enabling it later stays additive.
fn metal_format(format: ExternalSurfaceFormat) -> MTLPixelFormat {
    match format {
        ExternalSurfaceFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        ExternalSurfaceFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
    }
}

/// The **producer** face of the bridge: what the single privileged external compositor needs to
/// draw into a GPUI-owned surface, and nothing more.
///
/// This is not a general renderer handle and it is not for ordinary GPUI consumers. It exists
/// because a producer physically cannot create a pipeline, a render pass or a command buffer
/// without a device and a queue. `gpui_macos` re-exports it so that reaching the
/// producer takes a direct platform dependency rather than exposing the device through `gpui`.
///
/// What it carries:
///
/// * [`device`](Self::device) — GPUI's own [`metal::Device`]. The producer creates its pipelines and
///   its own render pass descriptors over registered surfaces on it.
/// * [`command_queue`](Self::command_queue) — GPUI's own [`metal::CommandQueue`]. Committing the
///   producer's command buffer on it ahead of GPUI's frame is what makes `SameQueueOrdered` true,
///   and a *separate* command buffer on that queue is enough: the producer
///   never needs, and never gets, GPUI's own command buffer or encoder.
/// * [`register`](Self::register) / [`retire`](Self::retire) — the surface lifecycle.
/// * [`device_generation`](Self::device_generation) — the liveness of every handle at once.
/// * [`rebuild_external_surface_domain`](Self::rebuild_external_surface_domain) — one bounded,
///   exact-set collective generation transition.
///
/// What it deliberately does not carry: consumer shader source, GPUI's command buffer or render
/// command encoder, the layer's drawable or any other GPUI render target, and any raw registry or
/// unbounded reset. The sole deliberate generation operation is the exact-set rebuild below.
///
/// **N23 — the mutation sentinel.** As elsewhere in this contract the two doctests are a pair, and
/// only the pair tells "this method is deliberately absent" apart from "the producer is unusable".
/// The read-only publication surface is callable from outside:
///
/// ```no_run
/// # use gpui_apple::ExternalSurfaceProducer;
/// fn okur(producer: &ExternalSurfaceProducer) -> gpui::RetireSafety {
///     producer.retire_safety()
/// }
/// ```
///
/// but the registry mutation that declares a surface drawn is not:
///
/// ```compile_fail
/// # use gpui_apple::ExternalSurfaceProducer;
/// fn yazar(producer: &ExternalSurfaceProducer, handle: gpui::ExternalSurfaceHandle) {
///     producer.note_drawn(handle);
/// }
/// ```
///
/// `note_drawn` lives on the crate-private registry and is reached only from the renderer's draw
/// path, so no producer can declare a surface drawn.
///
/// A producer is bound to one window's renderer and to the device generation it was acquired in.
/// After an invalidation, `register` reports [`ExternalSurfaceError::DeviceLost`]: the recovery is a
/// full rebuild, which starts by acquiring a new producer from the platform accessor. The
/// type is neither `Send` nor `Sync`, because the registry it shares with the renderer, and the
/// AppKit window that owns both, live on the window's own thread.
pub struct ExternalSurfaceProducer {
    device: metal::Device,
    command_queue: metal::CommandQueue,
    generation: u64,
    registry: Rc<RefCell<ExternalSurfaceRegistry>>,
}

impl ExternalSurfaceProducer {
    pub(crate) fn new(
        device: metal::Device,
        command_queue: metal::CommandQueue,
        registry: Rc<RefCell<ExternalSurfaceRegistry>>,
    ) -> Self {
        let generation = registry.borrow().device_generation();
        Self {
            device,
            command_queue,
            generation,
            registry,
        }
    }

    /// GPUI's device, on which the producer creates its own pipelines and render pass descriptors.
    pub fn device(&self) -> &metal::DeviceRef {
        &self.device
    }

    /// GPUI's command queue. A producer pass committed on it before GPUI's frame is ordered ahead
    /// of GPUI's own work even when it is encoded in a separate command buffer, which
    /// is exactly what `ExternalSyncToken::SameQueueOrdered` claims.
    pub fn command_queue(&self) -> &metal::CommandQueueRef {
        &self.command_queue
    }

    /// The device generation this producer was acquired in.
    ///
    /// It differing from the window's current generation means every handle this producer minted
    /// is dead and the producer itself has to be replaced.
    pub fn device_generation(&self) -> u64 {
        self.generation
    }

    /// Closes `handle` to future publication (contract 1.1).
    ///
    /// Atomic, idempotent and not reversible. It stops *fresh* paint only: live scenes and replay
    /// continuations keep running, because replay is not a paint call.
    pub fn close(&self, handle: ExternalSurfaceHandle) -> CloseOutcome {
        let mut registry = self.registry.borrow_mut();
        if registry.device_generation() != self.generation {
            return CloseOutcome::Unknown;
        }
        registry.publications_mut().close(handle)
    }

    /// Whether `id` was ever bound to a consumer draw command (contract 1.1).
    ///
    /// Evidence of a recorded draw command, never of GPU completion or present.
    pub fn binding_proof(&self, id: PublicationId) -> BindingProof {
        let registry = self.registry.borrow();
        if registry.device_generation() != self.generation {
            return BindingProof::StaleGeneration;
        }
        registry.publications().proof(id)
    }

    /// How far this producer can safely retire (contract 1.1).
    ///
    /// The threshold never skips a live publication, and it is not a `bool`: ask
    /// [`gpui::RetireWatermark::coverage`], which also validates scope.
    pub fn retire_safety(&self) -> RetireSafety {
        let registry = self.registry.borrow();
        if registry.device_generation() != self.generation {
            return RetireSafety::StaleProducer;
        }
        registry.publications().retire_safety()
    }

    /// This registry's read-only observation (contract 1.2).
    ///
    /// The single public observation surface. There is no `PlatformWindow` seam and no raw
    /// registry, handle list, iterator or map: the caller gets typed measures and an opaque
    /// scope. Asking is mutation-free and produces no progress — it is not a polling instrument
    /// for a loop waiting on a consumer.
    pub fn registry_observation(&self) -> RegistryObservation {
        let registry = self.registry.borrow();
        #[cfg(any(test, feature = "test-support"))]
        registry.note_observation_call();
        registry.observation(self.generation)
    }

    /// How many observation queries this producer's registry has answered. Test apparatus: it
    /// reads through the producer, never through a raw registry.
    #[cfg(any(test, feature = "test-support"))]
    pub fn observation_calls_for_test(&self) -> u32 {
        self.registry.borrow().observation_calls()
    }

    /// Registers a surface of `size` and returns its opaque identity together with the texture to
    /// draw into.
    ///
    /// The texture carries `RenderTarget | ShaderRead`, so the producer can attach it to its own
    /// render pass; GPUI keeps its own reference and samples it. The content is expected
    /// **premultiplied** and without group opacity applied — GPUI's composite is the sole owner of
    /// group opacity.
    pub fn register(
        &self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, metal::Texture), ExternalSurfaceError> {
        let mut registry = self.registry.borrow_mut();
        if registry.device_generation() != self.generation {
            return Err(ExternalSurfaceError::DeviceLost);
        }
        registry.register(size, format)
    }

    /// Collectively replaces the exact external-surface domain this producer owns (contract 1.3).
    ///
    /// The borrowed handle slice is an exact-set assertion, not a registry accessor. On success
    /// the physical resources, accounting and publication domain move together; every error is
    /// mutation-free.
    pub fn rebuild_external_surface_domain(
        &self,
        expected_surfaces: &[ExternalSurfaceHandle],
    ) -> Result<ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError> {
        let mut registry = self
            .registry
            .try_borrow_mut()
            .map_err(|_| ExternalSurfaceGenerationRolloverError::RegistryBusy)?;
        if registry.device_generation() != self.generation {
            return Err(ExternalSurfaceGenerationRolloverError::StaleProducer {
                producer_generation: self.generation,
                current_generation: registry.device_generation(),
            });
        }
        registry.rebuild_external_surface_domain(expected_surfaces)
    }

    /// Retires a surface and gives its accounting back. Retiring a handle from a dead generation is a
    /// no-op, not an error.
    pub fn retire(&self, handle: ExternalSurfaceHandle) {
        let mut registry = self.registry.borrow_mut();
        if registry.device_generation() != self.generation {
            return;
        }
        registry.retire(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> ExternalSurfaceLedger {
        ExternalSurfaceLedger::new()
    }

    fn size_of(width: i32, height: i32) -> Size<DevicePixels> {
        Size {
            width: DevicePixels(width),
            height: DevicePixels(height),
        }
    }

    fn admit(
        ledger: &mut ExternalSurfaceLedger,
        width: i32,
        height: i32,
    ) -> Result<(ExternalSurfaceHandle, u64), ExternalSurfaceError> {
        ledger.admit(size_of(width, height), ExternalSurfaceFormat::Bgra8Unorm)
    }

    /// A Metal device, or `None` where the host has none at all. Device-backed tests return early
    /// in that case, following the same convention as the atlas tests.
    fn device() -> Option<metal::Device> {
        metal::Device::system_default()
    }

    /// [`ExternalSurfaceRegistry::register`] with the texture dropped, so that a result can be
    /// compared: `metal::Texture` has no `PartialEq`, and the identity under test is the handle.
    fn register(
        registry: &mut ExternalSurfaceRegistry,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<ExternalSurfaceHandle, ExternalSurfaceError> {
        registry.register(size, format).map(|(handle, _)| handle)
    }

    // --- Ledger: identity, generation and accounting, with no GPU involved -----------------

    #[test]
    fn admission_mints_increasing_ids_in_the_current_generation() {
        let mut ledger = ledger();
        let (first, bytes) = admit(&mut ledger, 16, 16).unwrap();
        let (second, _) = admit(&mut ledger, 16, 16).unwrap();

        assert_eq!(first, ExternalSurfaceHandle::new(0, 0));
        assert_eq!(second, ExternalSurfaceHandle::new(1, 0));
        assert_eq!(bytes, 16 * 16 * BYTES_PER_PIXEL);
        assert_eq!(ledger.in_flight, 2);
        assert_eq!(ledger.registered_bytes, 2 * bytes);
    }

    #[test]
    fn withdrawing_gives_the_accounting_back() {
        let mut ledger = ledger();
        let (_, bytes) = admit(&mut ledger, 16, 16).unwrap();
        ledger.withdraw(bytes);
        assert_eq!(ledger.in_flight, 0);
        assert_eq!(ledger.registered_bytes, 0);

        // Over-withdrawing saturates instead of wrapping into a huge count.
        ledger.withdraw(bytes);
        assert_eq!(ledger.in_flight, 0);
        assert_eq!(ledger.registered_bytes, 0);
    }

    #[test]
    fn a_non_positive_extent_is_an_invalid_group() {
        let mut ledger = ledger();
        for (width, height) in [(0, 16), (16, 0), (-1, 16), (16, -1)] {
            assert_eq!(
                admit(&mut ledger, width, height),
                Err(ExternalSurfaceError::InvalidGroup),
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn an_unreported_byte_order_is_a_format_mismatch() {
        let mut ledger = ledger();
        assert_eq!(
            ledger.admit(size_of(16, 16), ExternalSurfaceFormat::Rgba8Unorm),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Bgra8Unorm,
                actual: ExternalSurfaceFormat::Rgba8Unorm,
            })
        );
        assert_eq!(ledger.in_flight, 0);
    }

    #[test]
    fn invalidating_raises_the_generation_and_clears_the_accounting() {
        let mut ledger = ledger();
        admit(&mut ledger, 16, 16).unwrap();
        assert_eq!(ledger.generation, 0);

        ledger.advance_generation(1);

        assert_eq!(ledger.generation, 1);
        assert_eq!(ledger.in_flight, 0);
        assert_eq!(ledger.registered_bytes, 0);

        // Ids keep climbing across generations, so a stale id can never alias a live one.
        let (handle, _) = admit(&mut ledger, 16, 16).unwrap();
        assert_eq!(handle, ExternalSurfaceHandle::new(1, 1));
    }

    // --- Capability snapshot ----------------------------------------------------------------

    #[test]
    fn the_capability_snapshot_reports_what_this_backend_carries() {
        let Some(device) = device() else {
            return;
        };
        let registry = ExternalSurfaceRegistry::new(device);
        let caps = registry.capabilities();

        assert!(caps.supported);
        assert_eq!(caps.contract_version, EXTERNAL_CONTRACT_VERSION);
        assert_eq!(caps.device_generation, 0);
        assert!(caps.format_bgra8_unorm);
        assert!(!caps.format_rgba8_unorm);
        assert!(caps.sampling_nearest && caps.sampling_linear);
        assert!(caps.sync_same_queue_ordered);
        assert!(!caps.sync_context_ordered);
        assert!(!caps.sync_fence);
        assert!(!caps.sync_keyed_mutex);
        assert!(caps.supports_affine && caps.supports_crop && caps.supports_clip);
        assert!(!caps.allow_stale_reuse);
    }

    /// The CPU-fallback answer is derived from the storage mode the registry actually uses rather
    /// than asserted, so it stays honest on both an Apple-silicon and an Intel Mac.
    #[test]
    fn the_cpu_fallback_answer_follows_the_storage_mode() {
        let Some(device) = device() else {
            return;
        };
        let registry = ExternalSurfaceRegistry::new(device.clone());
        let caps = registry.capabilities();

        let expected = if device.has_unified_memory() {
            MTLStorageMode::Shared
        } else {
            MTLStorageMode::Managed
        };
        assert_eq!(registry.storage_mode, expected);
        // Neither mode is `Private`, so a CPU producer can reach a registered surface with
        // `replaceRegion` and both fields are `true` on every Mac GPUI runs on.
        assert!(caps.cpu_fallback);
        assert!(caps.sync_cpu_ready);
    }

    // --- Registry: the parts that need a real device ------------------------------------------

    #[test]
    fn a_registered_surface_resolves_until_it_is_retired() {
        let Some(device) = device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(device);

        let (handle, texture) = registry
            .register(size_of(64, 32), ExternalSurfaceFormat::Bgra8Unorm)
            .expect("registration should succeed on a Metal device");

        assert_eq!(texture.width(), 64);
        assert_eq!(texture.height(), 32);
        assert_eq!(texture.pixel_format(), MTLPixelFormat::BGRA8Unorm);
        assert!(
            texture
                .usage()
                .contains(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead),
            "the producer draws into it and GPUI samples it"
        );
        assert!(registry.resolve(handle).is_some());
        assert_eq!(registry.surface_size(handle), Some(size_of(64, 32)));

        registry.retire(handle);

        assert!(registry.resolve(handle).is_none());
        assert!(registry.surface_size(handle).is_none());
        // The accounting came back, so the next registration is admitted.
        assert!(
            registry
                .register(size_of(64, 32), ExternalSurfaceFormat::Bgra8Unorm)
                .is_ok()
        );
    }

    #[test]
    fn an_unknown_id_resolves_to_nothing() {
        let Some(device) = device() else {
            return;
        };
        let registry = ExternalSurfaceRegistry::new(device);
        assert!(registry.resolve(ExternalSurfaceHandle::new(7, 0)).is_none());
    }

    #[test]
    fn invalidate_all_makes_every_handle_stale_at_once() {
        let Some(device) = device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(device);

        let (first, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert!(registry.resolve(first).is_some());
        assert!(registry.resolve(second).is_some());

        registry.invalidate_all();

        assert_eq!(registry.device_generation(), 1);
        assert!(registry.resolve(first).is_none());
        assert!(registry.resolve(second).is_none());
        // Retiring a dead handle is a no-op rather than an error or a double free.
        registry.retire(first);
        assert_eq!(registry.device_generation(), 1);

        // A handle minted after the bump is fresh again.
        let (third, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert_eq!(third.generation, 1);
        assert!(registry.resolve(third).is_some());
    }

    /// The device's own extent limit is the only size limit. An extent beyond it is refused as an
    /// allocation failure before any texture exists — `new_texture` would abort instead of
    /// failing — and the refusal consumes nothing.
    #[test]
    fn an_extent_beyond_the_device_limit_is_refused_as_an_allocation_failure() {
        let Some(device) = device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(device);
        let limit = registry.max_texture_dimension as i32;

        assert_eq!(
            register(
                &mut registry,
                size_of(limit + 1, 16),
                ExternalSurfaceFormat::Bgra8Unorm
            ),
            Err(ExternalSurfaceError::TransientFailure)
        );
        assert_eq!(
            register(
                &mut registry,
                size_of(16, 16),
                ExternalSurfaceFormat::Rgba8Unorm
            ),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Bgra8Unorm,
                actual: ExternalSurfaceFormat::Rgba8Unorm,
            })
        );
        assert_eq!(registry.ledger.in_flight, 0);
        assert_eq!(registry.ledger.registered_bytes, 0);

        // Nothing caps the surface count: a fourth concurrent registration succeeds.
        for _ in 0..4 {
            assert!(
                registry
                    .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
                    .is_ok()
            );
        }
        assert_eq!(registry.ledger.in_flight, 4);
    }

    #[test]
    fn skipped_draws_are_counted_rather_than_fatal() {
        let Some(device) = device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(device);
        assert_eq!(registry.note_skipped_draw(), 1);
        assert_eq!(registry.note_skipped_draw(), 2);
    }

    #[test]
    fn a_producer_from_a_dead_generation_reports_device_lost() {
        let Some(device) = device() else {
            return;
        };
        let queue = device.new_command_queue();
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        let producer = ExternalSurfaceProducer::new(device, queue, registry.clone());

        assert_eq!(producer.device_generation(), 0);
        let (handle, _texture) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert!(registry.borrow().resolve(handle).is_some());

        registry.borrow_mut().invalidate_all();

        assert_eq!(
            producer
                .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
                .map(|(handle, _)| handle),
            Err(ExternalSurfaceError::DeviceLost)
        );
        // Retiring through a stale producer is still harmless.
        producer.retire(handle);
        assert_eq!(registry.borrow().device_generation(), 1);
    }

    #[test]
    fn deliberate_rollover_requires_the_exact_set_and_stales_every_old_authority() {
        let Some(device) = device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        let producer = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (first, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = producer
            .register(size_of(8, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let old_publication = registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(first)
            .unwrap();
        assert_eq!(producer.close(first), CloseOutcome::Closed);
        let old_watermark = match producer.retire_safety() {
            RetireSafety::Through(watermark) => watermark,
            other => panic!("closed, unused publication must be terminal: {other:?}"),
        };

        let receipt = producer
            .rebuild_external_surface_domain(&[second, first])
            .unwrap();
        assert_eq!(receipt.previous_generation, 0);
        assert_eq!(receipt.current_generation, 1);
        assert_eq!(receipt.released_surface_count, 2);
        assert_eq!(
            receipt.released_external_surface_bytes,
            (16 * 16 + 8 * 16) * BYTES_PER_PIXEL
        );
        {
            let registry = registry.borrow();
            assert!(registry.surfaces.is_empty());
            assert_eq!(registry.ledger.in_flight, 0);
            assert_eq!(registry.ledger.registered_bytes, 0);
        }

        {
            let _borrow = registry.borrow();
            assert_eq!(
                producer.rebuild_external_surface_domain(&[]),
                Err(ExternalSurfaceGenerationRolloverError::RegistryBusy)
            );
        }
        registry.borrow_mut().ledger.registered_bytes = 1;
        assert_eq!(
            producer.rebuild_external_surface_domain(&[]),
            Err(ExternalSurfaceGenerationRolloverError::StaleProducer {
                producer_generation: 0,
                current_generation: 1,
            })
        );
        registry.borrow_mut().ledger.registered_bytes = 0;
        assert_eq!(
            producer.binding_proof(old_publication),
            BindingProof::StaleGeneration
        );
        assert_eq!(producer.retire_safety(), RetireSafety::StaleProducer);

        let fresh = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (fresh_handle, _) = fresh
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert_eq!(fresh_handle, ExternalSurfaceHandle::new(2, 1));
        let fresh_publication = registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(fresh_handle)
            .unwrap();
        assert_eq!(
            old_watermark.coverage(fresh_publication),
            gpui::WatermarkCoverage::StaleGeneration
        );

        let stale_observation = producer.registry_observation();
        assert_eq!(
            stale_observation.live_count,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::StaleDeviceGeneration,
            }
        );
        assert_eq!(
            stale_observation.nominal_bytes,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::StaleDeviceGeneration,
            }
        );
        assert_eq!(producer.close(fresh_handle), CloseOutcome::Unknown);
        producer.retire(fresh_handle);
        assert!(registry.borrow().resolve(fresh_handle).is_some());
        assert_eq!(
            fresh.binding_proof(fresh_publication),
            BindingProof::Pending
        );
        registry.borrow_mut().note_drawn(fresh_handle);
        assert_eq!(fresh.binding_proof(fresh_publication), BindingProof::Bound);
    }

    #[test]
    fn deliberate_rollover_mismatch_busy_and_accounting_errors_are_mutation_free() {
        let Some(device) = device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        let producer = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (first, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let snapshot = || {
            let registry = registry.borrow();
            (
                registry.ledger.generation,
                registry.ledger.next_id,
                registry.ledger.in_flight,
                registry.ledger.registered_bytes,
                registry.surfaces.len(),
                registry.storage_mode,
            )
        };
        let before = snapshot();

        for expected in [
            vec![first],
            vec![first, first],
            vec![first, ExternalSurfaceHandle::new(second.id, 1)],
            vec![first, ExternalSurfaceHandle::new(999, 0)],
        ] {
            assert!(matches!(
                producer.rebuild_external_surface_domain(&expected),
                Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch { .. })
            ));
            assert_eq!(snapshot(), before);
            assert!(registry.borrow().resolve(first).is_some());
            assert!(registry.borrow().resolve(second).is_some());
        }

        {
            let _borrow = registry.borrow();
            assert_eq!(
                producer.rebuild_external_surface_domain(&[first, second]),
                Err(ExternalSurfaceGenerationRolloverError::RegistryBusy)
            );
        }
        assert_eq!(snapshot(), before);

        registry.borrow_mut().ledger.registered_bytes += 1;
        let corrupted = snapshot();
        assert_eq!(
            producer.rebuild_external_surface_domain(&[first, second]),
            Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingMismatch,
                }
            )
        );
        assert_eq!(snapshot(), corrupted);
    }

    #[test]
    fn physical_byte_overflow_is_typed_and_mutation_free() {
        let Some(device) = device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        let producer = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (first, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        {
            let mut registry = registry.borrow_mut();
            registry.surfaces.get_mut(&first.id).unwrap().bytes = u64::MAX;
            registry.surfaces.get_mut(&second.id).unwrap().bytes = 1;
            registry.ledger.registered_bytes = u64::MAX;
        }
        let before = {
            let registry = registry.borrow();
            (
                registry.ledger.generation,
                registry.ledger.next_id,
                registry.ledger.in_flight,
                registry.ledger.registered_bytes,
                registry.surfaces.get(&first.id).unwrap().bytes,
                registry.surfaces.get(&second.id).unwrap().bytes,
            )
        };

        assert_eq!(
            producer.rebuild_external_surface_domain(&[first, second]),
            Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingOverflow,
                }
            )
        );
        let registry = registry.borrow();
        assert_eq!(
            (
                registry.ledger.generation,
                registry.ledger.next_id,
                registry.ledger.in_flight,
                registry.ledger.registered_bytes,
                registry.surfaces.get(&first.id).unwrap().bytes,
                registry.surfaces.get(&second.id).unwrap().bytes,
            ),
            before
        );
    }

    #[test]
    fn deliberate_max_preserves_every_resource_and_helper() {
        let Some(device) = device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        {
            let mut registry = registry.borrow_mut();
            registry.ledger.generation = u64::MAX;
            registry.publications.advance_device_generation(u64::MAX);
        }
        let producer = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (handle, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let before = {
            let registry = registry.borrow();
            (
                registry.ledger.generation,
                registry.ledger.next_id,
                registry.ledger.in_flight,
                registry.ledger.registered_bytes,
                registry.surfaces.len(),
                registry.storage_mode,
            )
        };

        registry.borrow_mut().ledger.registered_bytes += 1;
        assert_eq!(
            producer.rebuild_external_surface_domain(&[handle]),
            Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingMismatch,
                }
            )
        );
        registry.borrow_mut().ledger.registered_bytes -= 1;
        assert!(matches!(
            producer.rebuild_external_surface_domain(&[]),
            Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch { .. })
        ));
        assert_eq!(
            producer.rebuild_external_surface_domain(&[handle]),
            Err(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                generation: u64::MAX,
            })
        );
        let registry = registry.borrow();
        assert_eq!(
            (
                registry.ledger.generation,
                registry.ledger.next_id,
                registry.ledger.in_flight,
                registry.ledger.registered_bytes,
                registry.surfaces.len(),
                registry.storage_mode,
            ),
            before
        );
        assert!(registry.resolve(handle).is_some());
    }

    #[test]
    fn confirmed_loss_at_max_is_clear_no_wrap_terminal_and_idempotent() {
        let Some(device) = device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        {
            let mut registry = registry.borrow_mut();
            registry.ledger.generation = u64::MAX - 1;
            registry
                .publications
                .advance_device_generation(u64::MAX - 1);
        }
        let stale = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        registry.borrow_mut().invalidate_all();
        let current = ExternalSurfaceProducer::new(
            device.clone(),
            device.new_command_queue(),
            registry.clone(),
        );
        let (handle, _) = current
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let publication = registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(handle)
            .unwrap();

        registry.borrow_mut().invalidate_all();
        {
            let registry = registry.borrow();
            assert_eq!(registry.device_generation(), u64::MAX);
            assert!(registry.surfaces.is_empty());
            assert_eq!(registry.ledger.in_flight, 0);
            assert_eq!(registry.ledger.registered_bytes, 0);
            assert!(registry.publications.device_generation_exhausted());
        }

        assert_eq!(
            stale.rebuild_external_surface_domain(&[]),
            Err(ExternalSurfaceGenerationRolloverError::StaleProducer {
                producer_generation: u64::MAX - 1,
                current_generation: u64::MAX,
            })
        );
        assert_eq!(
            current.rebuild_external_surface_domain(&[]),
            Err(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                generation: u64::MAX,
            })
        );
        assert!(matches!(
            current.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::DeviceLost)
        ));
        current.retire(handle);
        assert_eq!(current.close(handle), CloseOutcome::Unknown);
        assert_eq!(
            current.binding_proof(publication),
            BindingProof::StaleGeneration
        );
        assert_eq!(
            current.retire_safety(),
            RetireSafety::CounterExhausted {
                counter: gpui::PublicationCounter::DeviceGeneration,
            }
        );
        let observation = current.registry_observation();
        assert_eq!(
            observation.live_count,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::DeviceGenerationExhausted,
            }
        );
        assert_eq!(
            observation.nominal_bytes,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::DeviceGenerationExhausted,
            }
        );
        assert!(matches!(
            observation.scope,
            gpui::RegistryScopeState::Known(scope)
                if scope.device_generation() == u64::MAX
        ));

        registry.borrow_mut().invalidate_all();
        let registry = registry.borrow();
        assert_eq!(registry.device_generation(), u64::MAX);
        assert!(registry.surfaces.is_empty());
        assert!(registry.publications.device_generation_exhausted());
    }

    // --- Contract 1.2: registry observation ---------------------------------------------------
    //
    // This was D1's **red** guard: it compiled and reached the real producer entry, but the core
    // derivation was still a skeleton. D2 made it **green** on macOS/Metal without changing the
    // assertion. The counter assertion remains the apparatus's positive control — an empty-registry
    // claim would be worthless if we could not first prove the query arrived.

    #[test]
    fn a2_bos_registry_measured_sifir_dondurur() {
        let device = device().expect(
            "gozlem nobeti GERCEK cihaz ister; cihaz yoksa hucre BLOCKED yazilir, sessizce GECILMEZ",
        );
        let command_queue = device.new_command_queue();
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));
        let producer = ExternalSurfaceProducer::new(device, command_queue, registry);

        let gozlem = producer.registry_observation();

        assert_eq!(
            producer.observation_calls_for_test(),
            1,
            "POZITIF KONTROL: sorgu gercekten producer girisine ulasti"
        );
        assert_eq!(
            gozlem.live_count,
            gpui::RegistryMeasure::Measured(0),
            "GERCEKTEN bos registry Measured(0) dondurur; Unsupported DEGIL"
        );
    }
}
