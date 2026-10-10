//! The wgpu half of the bounded external-surface bridge: the registry that owns the GPU resources
//! an externally produced surface is drawn from, and the single privileged producer accessor that
//! lets the external compositor draw into one.
//!
//! This is the sibling of `gpui_windows/src/external_registry.rs`, deliberately structured and
//! named the same way so the two backends read as one implementation. Four runtime profiles reach
//! the external surface through this crate: Browser WebGL2, Browser WebGPU, Linux wgpu-Vulkan and
//! Linux wgpu-GL.
//!
//! Two things live here, and they are kept apart on purpose:
//!
//! * [`ExternalSurfaceRegistry`] — crate-private storage. It maps the opaque
//!   [`ExternalSurfaceHandle`] onto the texture, its view, the two sampling bind groups, the size,
//!   the byte order and the byte cost of a surface, and it refuses an extent the device cannot
//!   create. The renderer resolves handles against it at draw time. It never hands out the device.
//! * [`ExternalSurfaceProducer`] — the **producer** face, reached through
//!   [`crate::WgpuRenderer::external_surface_producer`]. It is for the one privileged external
//!   compositor, and it carries exactly what producing a surface needs: the [`wgpu::Device`] and
//!   the [`wgpu::Queue`] (unavoidable — a producer cannot create its own pipelines, render passes
//!   or submissions without them), plus registering, retiring, reading the device generation, and
//!   the bounded exact-set rebuild operation. No consumer shader source, no GPUI render pass, no
//!   swap-chain target, and no arbitrary reset or per-handle generation mutation is exposed.
//!
//! Ordinary GPUI consumers never see any of this: they see `Window::paint_external_surface` and
//! `Window::external_surface_capabilities`, and an opaque handle. Reaching the producer requires
//! taking a direct dependency on this platform crate.
//!
//! Every surface lives in GPUI's own device, so the sync model of this step is submission
//! order: `SameQueueOrdered` on the native backends and on WebGPU, and `ContextOrdered` on WebGL2,
//! where the guarantee is the command order of the one shared WebGL2 context. Neither
//! a fence nor a cross-device shared handle is part of this step, and neither is claimed in the
//! capability snapshot.
//!
//! Losing the device kills every handle at once, not one by one: a GPU timeout reset is
//! adapter-wide. That is what the browser-only `ExternalSurfaceRegistry::invalidate_all` entry
//! expresses, and why
//! `generation` is not optional in the handle.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use collections::{FxHashMap, FxHashSet};
use gpui::{
    BindingProof, CloseOutcome, DevicePixels, EXTERNAL_CONTRACT_VERSION, ExternalSampling,
    ExternalSurfaceCapabilities, ExternalSurfaceError, ExternalSurfaceFormat,
    ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError,
    ExternalSurfaceHandle, PublicationId, PublicationLedger, RegistryObservation,
    RegistryUnavailableReason, RetireSafety, Size,
};

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
    /// The device/context generation in force. Raising it invalidates every handle.
    generation: u64,
    /// The id the next registration receives. Ids are never reused, not even across generations.
    next_id: u64,
    /// The one byte order this backend reports, which is the one the context selected.
    format: ExternalSurfaceFormat,
    /// The total bytes of every currently registered surface.
    registered_bytes: u64,
    /// How many surfaces are currently registered.
    in_flight: u32,
    /// How many draws were skipped because the handle no longer resolved. Telemetry only: a stale
    /// draw is skipped and counted, never a panic.
    skipped_draws: u64,
}

impl ExternalSurfaceLedger {
    fn new(format: ExternalSurfaceFormat) -> Self {
        Self {
            // Generation 0 is a legal generation: a handle minted before any device loss is fresh
            // for it, and the first `invalidate_all` moves everything to 1.
            generation: 0,
            next_id: 0,
            format,
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

        // There is no silent per-platform format substitution: this backend reports exactly the
        // byte order the context selected, and refuses the other one.
        if format != self.format {
            return Err(ExternalSurfaceError::FormatMismatch {
                expected: self.format,
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

    /// Commits an already-checked successor generation and drops its accounting.
    fn advance_generation(&mut self, generation: u64) {
        debug_assert_eq!(self.generation.checked_add(1), Some(generation));
        self.generation = generation;
        self.registered_bytes = 0;
        self.in_flight = 0;
    }

    /// Clears accounting at the sticky terminal generation without wrapping it.
    fn exhaust_generation(&mut self) {
        self.generation = u64::MAX;
        self.registered_bytes = 0;
        self.in_flight = 0;
    }
}

/// One registered surface: the resource GPUI owns on behalf of the producer.
struct RegisteredExternalSurface {
    /// The texture itself. GPUI keeps its own reference for as long as the surface is registered,
    /// so the producer's clone can never leave the renderer holding a dangling view.
    _texture: wgpu::Texture,
    /// The view the consumer samples through. It is kept alive by the bind groups below, and held
    /// here as well so the resource stays observable from the registry itself.
    _view: wgpu::TextureView,
    /// The bind group the consumer samples through in [`ExternalSampling::Nearest`].
    ///
    /// This is where the two backends genuinely differ: D3D11 binds a view and a sampler
    /// separately, so its registry stores one shader resource view and the pipeline picks the
    /// sampler; wgpu binds a texture and its sampler together, so the sampling mode is baked into
    /// the bind group and the registry stores one per mode. Both are built once, at registration,
    /// rather than per frame.
    bind_group_nearest: wgpu::BindGroup,
    /// The bind group the consumer samples through in [`ExternalSampling::Linear`].
    bind_group_linear: wgpu::BindGroup,
    /// The physical size, in device pixels. Crops are normalized against this, not against the
    /// descriptor, so a descriptor that disagrees with the resource cannot push the sampling
    /// outside the texture.
    size: Size<DevicePixels>,
    /// The byte order. Contract v1 allows one per backend here, but it is stored rather than
    /// assumed so that reporting both stays an additive change.
    _format: ExternalSurfaceFormat,
    /// What this surface costs in bytes.
    bytes: u64,
}

/// The registry-owned GPU helpers are either wholly usable on one device or wholly absent.
///
/// Independent `Option` fields would admit half-valid states (for example a new device paired
/// with an old bind-group layout). Keeping them under one discriminant also makes real loss at
/// `u64::MAX` physically release every old-device reference in one assignment.
enum ExternalSurfaceGpuState {
    Active {
        device: Arc<wgpu::Device>,
        texture_layout: wgpu::BindGroupLayout,
        sampler_nearest: wgpu::Sampler,
        sampler_linear: wgpu::Sampler,
    },
    GenerationExhausted,
}

impl ExternalSurfaceGpuState {
    fn active(device: Arc<wgpu::Device>) -> Self {
        let texture_layout = create_external_texture_layout(&device);
        let sampler_nearest =
            create_external_sampler(&device, "external_surface_sampler_nearest", false);
        let sampler_linear =
            create_external_sampler(&device, "external_surface_sampler_linear", true);
        Self::Active {
            device,
            texture_layout,
            sampler_nearest,
            sampler_linear,
        }
    }
}

/// Both deliberate rollover and real device loss consume this one generation transaction.
enum GenerationAdvanceCause {
    Deliberate,
    /// `Some` replaces the dead native device. `None` is browser context loss, where rendering
    /// stops until reload and the active helpers need only be dropped if the counter is terminal.
    DeviceLost(Option<Arc<wgpu::Device>>),
}

/// The renderer-side storage of every registered external surface.
///
/// This is the resource owner named by the contract: the consumer carries only the opaque
/// `{ id, generation }` identity, and nothing about the underlying GPU resource reaches GPUI's
/// public API. The registry **never** exposes its device.
pub(crate) struct ExternalSurfaceRegistry {
    /// Device, layout and samplers share one validity discriminant; terminal loss drops all four.
    gpu: ExternalSurfaceGpuState,
    /// Whether this backend is the WebGL2 one, which is the only one whose sync guarantee is
    /// `ContextOrdered` rather than `SameQueueOrdered`.
    webgl2: bool,
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
    ///
    /// `format` is the byte order the context selected — `Bgra8Unorm` everywhere except wasm+GL,
    /// where it is `Rgba8Unorm` — and it is the only one this registry admits. `webgl2` is what
    /// `WgpuContext::uses_webgl_instance_data` reports, which is exactly the WebGL2 backend.
    pub(crate) fn new(
        device: Arc<wgpu::Device>,
        format: ExternalSurfaceFormat,
        webgl2: bool,
    ) -> Self {
        Self {
            gpu: ExternalSurfaceGpuState::active(device),
            webgl2,
            ledger: ExternalSurfaceLedger::new(format),
            publications: PublicationLedger::new(0),
            surfaces: FxHashMap::default(),
            #[cfg(any(test, feature = "test-support"))]
            observation_calls: std::cell::Cell::new(0),
        }
    }

    /// The layout the external-surface pipeline's sampling bind group must be built with.
    pub(crate) fn texture_bind_group_layout(&self) -> Option<&wgpu::BindGroupLayout> {
        match &self.gpu {
            ExternalSurfaceGpuState::Active { texture_layout, .. } => Some(texture_layout),
            ExternalSurfaceGpuState::GenerationExhausted => None,
        }
    }

    /// Registers a surface and returns its identity together with a clone of the texture.
    ///
    /// The texture is created with `RENDER_ATTACHMENT | TEXTURE_BINDING` and nothing else: no
    /// `COPY_DST`, because this step's producer draws rather than uploads, and no shared handle,
    /// because the surface lives in GPUI's own device. The clone is what makes the producer able to
    /// create its own view and render pass over the surface; the sampling bind groups stay here,
    /// because sampling is the consumer's side of the contract.
    ///
    /// Unlike D3D11's `CreateTexture2D`, `wgpu::Device::create_texture` has no fallible return: an
    /// allocation that the device refuses surfaces through the device's error scope, not here. So
    /// the device's own limit, `max_texture_dimension_2d`, is checked before the texture is created,
    /// and an extent beyond it is refused the way D3D11 reports a refused allocation:
    /// [`ExternalSurfaceError::TransientFailure`], with nothing consumed.
    pub(crate) fn register(
        &mut self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, wgpu::Texture), ExternalSurfaceError> {
        let ExternalSurfaceGpuState::Active {
            device,
            texture_layout,
            sampler_nearest,
            sampler_linear,
        } = &self.gpu
        else {
            return Err(ExternalSurfaceError::DeviceLost);
        };
        let (handle, bytes) = self.ledger.admit(size, format)?;
        let device_limit = device.limits().max_texture_dimension_2d;
        if size.width.0 as u32 > device_limit || size.height.0 as u32 > device_limit {
            self.ledger.withdraw(bytes);
            return Err(ExternalSurfaceError::TransientFailure);
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("external_surface"),
            size: wgpu::Extent3d {
                width: size.width.0 as u32,
                height: size.height.0 as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format(format),
            // `COPY_DST` is what makes the contract's CPU fallback reachable: without it a
            // producer that has lost its GPU path cannot upload pixels with `write_texture`, and
            // the capability below would have to report `cpu_fallback: false` while the D3D11
            // backend reports `true` for the same logical capability. The flag costs nothing on a
            // render target, so the six profiles keep one answer instead of two.
            // `COPY_SRC` is what makes the producer's own compositing reachable. The external
            // renderer computes Porter-Duff and blend functions in a shader rather than through
            // fixed-function blending - blend functions read the destination's value and cannot
            // be expressed as a blend equation at all - so every command reads the target back
            // through a copy. Without this flag the producer's submit fails validation and GPUI
            // silently skips the surface, which looks exactly like a blank area.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group_nearest =
            Self::create_bind_group(device, texture_layout, &view, sampler_nearest);
        let bind_group_linear =
            Self::create_bind_group(device, texture_layout, &view, sampler_linear);

        self.surfaces.insert(
            handle.id,
            RegisteredExternalSurface {
                _texture: texture.clone(),
                _view: view,
                bind_group_nearest,
                bind_group_linear,
                size,
                _format: format,
                bytes,
            },
        );
        Ok((handle, texture))
    }

    fn create_bind_group(
        device: &wgpu::Device,
        texture_layout: &wgpu::BindGroupLayout,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("external_surface_bind_group"),
            layout: texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// The bind group a fresh handle is sampled through, or `None` when the handle is stale or
    /// unknown.
    ///
    /// A stale handle is not an error here and never a panic: the draw path skips the surface and
    /// counts it, and the consumer learns of the death from the device generation.
    pub(crate) fn resolve(
        &self,
        handle: ExternalSurfaceHandle,
        sampling: ExternalSampling,
    ) -> Option<&wgpu::BindGroup> {
        self.entry(handle).map(|surface| match sampling {
            ExternalSampling::Nearest => &surface.bind_group_nearest,
            ExternalSampling::Linear => &surface.bind_group_linear,
        })
    }

    /// The physical size of a fresh handle's surface, or `None` when it is stale or unknown.
    pub(crate) fn surface_size(&self, handle: ExternalSurfaceHandle) -> Option<Size<DevicePixels>> {
        self.entry(handle).map(|surface| surface.size)
    }

    fn entry(&self, handle: ExternalSurfaceHandle) -> Option<&RegisteredExternalSurface> {
        if matches!(self.gpu, ExternalSurfaceGpuState::GenerationExhausted) {
            return None;
        }
        if !handle.is_fresh_for(self.ledger.generation) {
            return None;
        }
        self.surfaces.get(&handle.id)
    }

    /// Drops a surface and gives its accounting back. Retiring a stale handle is a no-op: the
    /// generation bump already released everything.
    pub(crate) fn retire(&mut self, handle: ExternalSurfaceHandle) {
        if matches!(self.gpu, ExternalSurfaceGpuState::GenerationExhausted) {
            return;
        }
        if !handle.is_fresh_for(self.ledger.generation) {
            return;
        }
        if let Some(surface) = self.surfaces.remove(&handle.id) {
            self.ledger.withdraw(surface.bytes);
        }
    }

    /// The device/context generation in force.
    pub(crate) fn device_generation(&self) -> u64 {
        self.ledger.generation
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
        if matches!(self.gpu, ExternalSurfaceGpuState::GenerationExhausted) {
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

    /// Kills every handle at once by raising the generation.
    ///
    /// This is the browser device-loss action, and it is collective on purpose: the loss is
    /// adapter-wide, so no surviving subset of handles exists to keep. At
    /// `u64::MAX` the same transition clears the domain and drops its GPU helpers without wrapping.
    #[cfg(any(target_family = "wasm", test))]
    pub(crate) fn invalidate_all(&mut self) {
        let _ = self.advance_external_surface_generation(GenerationAdvanceCause::DeviceLost(None));
    }

    /// Adopts the recreated device after a device-lost recovery and invalidates everything the old
    /// one owned.
    ///
    /// The layout and the samplers belong to the dead device, so they are rebuilt here rather than
    /// carried over; the registry object itself survives, which is what lets a producer acquired
    /// before the loss observe the raised generation instead of registering onto a dead device.
    ///
    /// Native only, because recovery itself is: a browser that loses its graphics context does not
    /// get a new device, it gets a page reload, and that path raises the generation through
    /// the browser-only `invalidate_all` entry alone.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn handle_device_lost(&mut self, device: Arc<wgpu::Device>) {
        let _ = self
            .advance_external_surface_generation(GenerationAdvanceCause::DeviceLost(Some(device)));
    }

    /// Validates and commits one deliberate collective domain rollover.
    fn rebuild_external_surface_domain(
        &mut self,
        expected_surfaces: &[ExternalSurfaceHandle],
    ) -> Result<ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError> {
        let registry_surface_count = u64::try_from(self.surfaces.len()).map_err(|_| {
            ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                reason: RegistryUnavailableReason::AccountingOverflow,
            }
        })?;
        let registry_surface_bytes = self
            .surfaces
            .values()
            .try_fold(0u64, |total, surface| total.checked_add(surface.bytes))
            .ok_or(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingOverflow,
                },
            )?;
        if registry_surface_count != u64::from(self.ledger.in_flight)
            || registry_surface_bytes != self.ledger.registered_bytes
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
        let exact = expected_surfaces.iter().all(|handle| {
            handle.is_fresh_for(self.ledger.generation)
                && seen.insert(handle.id)
                && self.surfaces.contains_key(&handle.id)
        });
        if !exact {
            return Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch {
                expected_surface_count,
                registry_surface_count,
            });
        }

        let Some(_) = self.ledger.generation.checked_add(1) else {
            return Err(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                generation: self.ledger.generation,
            });
        };

        self.advance_external_surface_generation(GenerationAdvanceCause::Deliberate)
            .ok_or(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                generation: self.ledger.generation,
            })
    }

    /// The one generation state machine shared by deliberate rollover and real device loss.
    ///
    /// A deliberate terminal attempt is mutation-free. Real terminal loss instead clears every
    /// external resource, leaves the numeric generation at `u64::MAX`, and drops the registry's
    /// old-device helpers without constructing replacements.
    fn advance_external_surface_generation(
        &mut self,
        cause: GenerationAdvanceCause,
    ) -> Option<ExternalSurfaceGenerationRollover> {
        let previous_generation = self.ledger.generation;
        let released_surface_count = u64::try_from(self.surfaces.len()).unwrap_or(u64::MAX);
        let released_external_surface_bytes = self.ledger.registered_bytes;

        let Some(current_generation) = previous_generation.checked_add(1) else {
            if matches!(cause, GenerationAdvanceCause::Deliberate) {
                return None;
            }
            self.surfaces.clear();
            self.ledger.exhaust_generation();
            self.publications.exhaust_device_generation();
            self.gpu = ExternalSurfaceGpuState::GenerationExhausted;
            return None;
        };

        self.surfaces.clear();
        self.ledger.advance_generation(current_generation);
        self.publications
            .advance_device_generation(current_generation);

        if let GenerationAdvanceCause::DeviceLost(Some(device)) = cause {
            // This factory is deliberately after the checked generation preflight. At MAX the
            // branch above has already made the state terminal and no new helper is allocated.
            self.gpu = ExternalSurfaceGpuState::active(device);
        }

        Some(
            ExternalSurfaceGenerationRollover::from_committed_registry_rollover(
                previous_generation,
                current_generation,
                released_surface_count,
                released_external_surface_bytes,
            ),
        )
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
            // Exactly the byte order `WgpuContext::select_color_texture_format` settled on:
            // `Bgra8Unorm` everywhere except wasm+GL, where WebGL2 cannot render to BGRA at all
            // and `Rgba8Unorm` is the fallback the contract names. The other one is not offered,
            // because a mismatch has to be an observable `FormatMismatch` rather than a silent
            // conversion.
            format_bgra8_unorm: self.ledger.format == ExternalSurfaceFormat::Bgra8Unorm,
            format_rgba8_unorm: self.ledger.format == ExternalSurfaceFormat::Rgba8Unorm,
            sampling_nearest: true,
            sampling_linear: true,
            // The producer draws on GPUI's own device and submits on GPUI's own queue, so
            // submission order is the whole synchronization story of this step — except on WebGL2,
            // where there is no queue to speak of and the guarantee is the command order of the one
            // shared context. Exactly one of the two is claimed, because they are the
            // same guarantee under two different names and claiming both would overstate it.
            sync_same_queue_ordered: !self.webgl2,
            sync_context_ordered: self.webgl2,
            sync_fence: false,
            sync_keyed_mutex: false,
            // A registered surface carries `COPY_DST`, so `Queue::write_texture` reaches it and a
            // CPU producer really does have a path here — the capability is claimed because it
            // works, not because it is convenient.
            sync_cpu_ready: true,
            supports_affine: true,
            supports_crop: true,
            supports_clip: true,
            cpu_fallback: true,
            // The render thread never waits, and this step does not keep a previous generation
            // alive to fall back on: an unready group is skipped and reported rather than drawn
            // from stale content.
            allow_stale_reuse: false,
        }
    }
}

/// The **producer** face of the bridge: what the single privileged external compositor needs to
/// draw into a GPUI-owned surface, and nothing more.
///
/// This is not a general renderer handle and it is not for ordinary GPUI consumers. It exists
/// because a producer physically cannot create a pipeline, a render pass or a submission without a
/// device and a queue, and it is confined to this platform crate so that reaching
/// it takes a direct dependency on `gpui_wgpu` rather than on `gpui`.
///
/// What it carries:
///
/// * [`device`](Self::device) — GPUI's own [`wgpu::Device`]. The producer creates its pipelines and
///   its own views over registered surfaces on it.
/// * [`queue`](Self::queue) — GPUI's own [`wgpu::Queue`]. Submitting the producer pass on it ahead
///   of GPUI's frame is what makes `SameQueueOrdered` (or, on WebGL2, `ContextOrdered`) true.
/// * [`register`](Self::register) / [`retire`](Self::retire) — the surface lifecycle.
/// * [`device_generation`](Self::device_generation) — the liveness of every handle at once.
/// * [`rebuild_external_surface_domain`](Self::rebuild_external_surface_domain) — one bounded,
///   exact-current-set collective generation rollover.
///
/// What it deliberately does not carry: consumer shader source, GPUI's render pass or command
/// encoder, the swap-chain texture or any other GPUI render target, the bind groups GPUI samples
/// through, or an arbitrary registry reset. The only producer-requested generation transition is
/// the bounded deliberate rebuild below: it accepts the exact current handle set and shares the
/// registry's production generation primitive with real device loss.
///
/// A producer is bound to one window's renderer and to the device generation it was acquired in.
/// After a device loss, `register` reports [`ExternalSurfaceError::DeviceLost`]: the recovery is a
/// full rebuild, which starts by acquiring a new producer from
/// [`crate::WgpuRenderer::external_surface_producer`]. The type is neither `Send` nor `Sync`,
/// because the registry it shares with the renderer lives on the window's own thread.
/// **N23 — the mutation sentinel.** The pair tells "this method is deliberately absent" apart from
/// "the producer is unusable". The read-only publication surface is callable from outside:
///
/// ```no_run
/// # use gpui_wgpu::ExternalSurfaceProducer;
/// fn okur(producer: &ExternalSurfaceProducer) -> gpui::RetireSafety {
///     producer.retire_safety()
/// }
/// ```
///
/// but the registry mutation that declares a surface drawn is not:
///
/// ```compile_fail
/// # use gpui_wgpu::ExternalSurfaceProducer;
/// fn yazar(producer: &ExternalSurfaceProducer, handle: gpui::ExternalSurfaceHandle) {
///     producer.note_drawn(handle);
/// }
/// ```
pub struct ExternalSurfaceProducer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    generation: u64,
    registry: Rc<RefCell<ExternalSurfaceRegistry>>,
}

impl ExternalSurfaceProducer {
    pub(crate) fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        registry: Rc<RefCell<ExternalSurfaceRegistry>>,
    ) -> Self {
        let generation = registry.borrow().device_generation();
        Self {
            device,
            queue,
            generation,
            registry,
        }
    }

    /// GPUI's device, on which the producer creates its own pipelines and texture views.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// GPUI's queue, on which the producer submits its own pass before GPUI's frame.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
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

    /// Collectively replaces this registry's exact external-surface generation (contract 1.3).
    ///
    /// `expected_surfaces` is borrowed only for this call. It grants no registry access and must
    /// name the registry's complete current handle set exactly once each. Every refusal is
    /// mutation-free; only a fully validated call advances the shared generation transaction.
    pub fn rebuild_external_surface_domain(
        &self,
        expected_surfaces: &[ExternalSurfaceHandle],
    ) -> Result<ExternalSurfaceGenerationRollover, ExternalSurfaceGenerationRolloverError> {
        let Ok(mut registry) = self.registry.try_borrow_mut() else {
            return Err(ExternalSurfaceGenerationRolloverError::RegistryBusy);
        };
        let current_generation = registry.device_generation();
        if current_generation != self.generation {
            return Err(ExternalSurfaceGenerationRolloverError::StaleProducer {
                producer_generation: self.generation,
                current_generation,
            });
        }
        registry.rebuild_external_surface_domain(expected_surfaces)
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
    /// The texture carries `RENDER_ATTACHMENT | TEXTURE_BINDING`, so the producer can create its
    /// own view and render pass over it; GPUI keeps the sampling bind groups. The content is
    /// expected **premultiplied** and without group opacity applied — GPUI's composite is the sole
    /// owner of group opacity.
    pub fn register(
        &self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, wgpu::Texture), ExternalSurfaceError> {
        let mut registry = self.registry.borrow_mut();
        if registry.device_generation() != self.generation {
            return Err(ExternalSurfaceError::DeviceLost);
        }
        registry.register(size, format)
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

/// The wgpu byte order of a contract format.
///
/// Only the byte order the context selected is admitted by `ExternalSurfaceLedger::admit`; the
/// other arm is the truthful mapping of the second byte order rather than a silent substitution,
/// so that a backend which reports it later needs no change here.
fn wgpu_format(format: ExternalSurfaceFormat) -> wgpu::TextureFormat {
    match format {
        ExternalSurfaceFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
        ExternalSurfaceFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
    }
}

/// The contract byte order of a wgpu format, or `None` for anything the bridge cannot carry.
pub(crate) fn external_format(format: wgpu::TextureFormat) -> Option<ExternalSurfaceFormat> {
    match format {
        wgpu::TextureFormat::Bgra8Unorm => Some(ExternalSurfaceFormat::Bgra8Unorm),
        wgpu::TextureFormat::Rgba8Unorm => Some(ExternalSurfaceFormat::Rgba8Unorm),
        _ => None,
    }
}

/// The bind group layout every external surface is sampled through: the texture and its sampler,
/// which wgpu binds together.
fn create_external_texture_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("external_surface_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// One of the external-surface pipeline's two samplers, in one of the contract's two sampling
/// modes.
///
/// The addressing is **clamp-to-edge** in both, and that is a correctness requirement rather than a
/// default: a crop names a sub-rectangle of the surface, and with repeat addressing a linear filter
/// at the crop's edge would fetch the opposite edge of the surface and bleed it into the result.
/// The D3D11 step's samplers are clamped for the same reason.
fn create_external_sampler(device: &wgpu::Device, label: &str, linear: bool) -> wgpu::Sampler {
    let filter = if linear {
        wgpu::FilterMode::Linear
    } else {
        wgpu::FilterMode::Nearest
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filter,
        min_filter: filter,
        // A registered surface has exactly one mip level, so there is nothing to filter between.
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

#[cfg(all(test, not(target_family = "wasm")))]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Creating or driving many headless Vulkan devices concurrently is not a contract property
    /// and destabilizes some software/CI drivers. A test that asks for a real device holds this
    /// lease for its whole test thread; repeated requests inside the same test remain re-entrant.
    static TEST_GPU: Mutex<()> = Mutex::new(());
    thread_local! {
        static TEST_GPU_LEASE: RefCell<Option<MutexGuard<'static, ()>>> = const {
            RefCell::new(None)
        };
    }

    fn hold_test_gpu_lease() {
        TEST_GPU_LEASE.with(|lease| {
            if lease.borrow().is_none() {
                let guard = TEST_GPU
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *lease.borrow_mut() = Some(guard);
            }
        });
    }

    /// A device on whatever backend this host offers, or `None` when there is no adapter at all.
    /// Device-backed tests return early in that case.
    pub(crate) fn test_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
        test_adapter_and_device().map(|(_adapter, device, queue)| (device, queue))
    }

    /// Like [`test_device`], but keeps the adapter, which is the only thing that can be asked what
    /// usages a texture format actually supports.
    pub(crate) fn test_adapter_and_device()
    -> Option<(wgpu::Adapter, Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
        hold_test_gpu_lease();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            // Headless: there is no window and no display handle, which is what makes these tests
            // runnable on a CI worker.
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            // Report real adapter limits; bucketing is for untrusted callers.
            apply_limit_buckets: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("gpui_external_surface_test_device"),
                default_queue: wgpu::QueueDescriptor {
                    label: Some("gpui_external_surface_test_queue"),
                },
                required_features: wgpu::Features::empty(),
                // The same limits GPUI's own device creation asks for, so a surface this registry
                // admits is one the real renderer's device would admit too.
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits())
                    .using_alignment(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }),
        )
        .ok()?;
        Some((adapter, Arc::new(device), Arc::new(queue)))
    }

    fn ledger() -> ExternalSurfaceLedger {
        ExternalSurfaceLedger::new(ExternalSurfaceFormat::Bgra8Unorm)
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

    fn registry() -> Option<ExternalSurfaceRegistry> {
        let (device, _queue) = test_device()?;
        Some(ExternalSurfaceRegistry::new(
            device,
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        ))
    }

    /// A browser-shaped registry immediately before its final representable generation.
    ///
    /// Kept inside the test module so renderer tests can exercise the production browser-loss
    /// transition without adding a reset or raw-registry surface to the production API.
    pub(crate) fn browser_registry_before_terminal() -> Option<(
        wgpu::Adapter,
        Arc<wgpu::Device>,
        Arc<wgpu::Queue>,
        ExternalSurfaceRegistry,
    )> {
        let (adapter, device, queue) = test_adapter_and_device()?;
        let mut registry = ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Rgba8Unorm,
            true,
        );
        registry.ledger.generation = u64::MAX - 1;
        registry
            .publications
            .advance_device_generation(u64::MAX - 1);
        Some((adapter, device, queue, registry))
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

        // The wasm+GL registry reports the other byte order, and refuses BGRA in the same way.
        let mut ledger = ExternalSurfaceLedger::new(ExternalSurfaceFormat::Rgba8Unorm);
        assert_eq!(
            ledger.admit(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Rgba8Unorm,
                actual: ExternalSurfaceFormat::Bgra8Unorm,
            })
        );
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
        let Some(registry) = registry() else {
            return;
        };
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
        // Claimed because the registered texture carries `COPY_DST`, and claimed identically by
        // the D3D11 backend: the same logical capability must not answer differently per profile.
        assert!(caps.sync_cpu_ready);
        assert!(caps.cpu_fallback);
        assert!(caps.supports_affine && caps.supports_crop && caps.supports_clip);
        assert!(!caps.allow_stale_reuse);
    }

    /// WebGL2 has no queue of its own: the guarantee there is the command order of the one shared
    /// context, which is `ContextOrdered` and not `SameQueueOrdered`.
    #[test]
    fn the_webgl2_snapshot_claims_context_order_instead_of_queue_order() {
        let Some((device, _queue)) = test_device() else {
            return;
        };
        let registry =
            ExternalSurfaceRegistry::new(device, ExternalSurfaceFormat::Rgba8Unorm, true);
        let caps = registry.capabilities();

        assert!(caps.sync_context_ordered);
        assert!(!caps.sync_same_queue_ordered);
        // wasm+GL is also the one profile whose byte order is the RGBA fallback.
        assert!(caps.format_rgba8_unorm);
        assert!(!caps.format_bgra8_unorm);
    }

    // --- Registry: the parts that need a real device ------------------------------------------

    #[test]
    fn a_registered_surface_resolves_until_it_is_retired() {
        let Some(mut registry) = registry() else {
            return;
        };

        let (handle, _texture) = registry
            .register(size_of(64, 32), ExternalSurfaceFormat::Bgra8Unorm)
            .expect("registration should succeed on a real device");

        assert!(
            registry
                .resolve(handle, ExternalSampling::Nearest)
                .is_some()
        );
        assert!(registry.resolve(handle, ExternalSampling::Linear).is_some());
        assert_eq!(registry.surface_size(handle), Some(size_of(64, 32)));

        registry.retire(handle);

        assert!(
            registry
                .resolve(handle, ExternalSampling::Nearest)
                .is_none()
        );
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
        let Some(registry) = registry() else {
            return;
        };
        assert!(
            registry
                .resolve(ExternalSurfaceHandle::new(7, 0), ExternalSampling::Nearest)
                .is_none()
        );
    }

    #[test]
    fn invalidate_all_makes_every_handle_stale_at_once() {
        let Some(mut registry) = registry() else {
            return;
        };

        let (first, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert!(registry.resolve(first, ExternalSampling::Nearest).is_some());
        assert!(
            registry
                .resolve(second, ExternalSampling::Nearest)
                .is_some()
        );

        registry.invalidate_all();

        assert_eq!(registry.device_generation(), 1);
        assert!(registry.resolve(first, ExternalSampling::Nearest).is_none());
        assert!(
            registry
                .resolve(second, ExternalSampling::Nearest)
                .is_none()
        );
        // Retiring a dead handle is a no-op rather than an error or a double free.
        registry.retire(first);
        assert_eq!(registry.device_generation(), 1);

        // A handle minted after the bump is fresh again.
        let (third, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert_eq!(third.generation, 1);
        assert!(registry.resolve(third, ExternalSampling::Nearest).is_some());
    }

    /// The device's own extent limit is the only size limit. An extent beyond it is refused as an
    /// allocation failure before any texture exists, and the refusal consumes nothing.
    #[test]
    fn an_extent_beyond_the_device_limit_is_refused_as_an_allocation_failure() {
        let Some((device, _queue)) = test_device() else {
            return;
        };
        let limit = device.limits().max_texture_dimension_2d as i32;
        let mut registry =
            ExternalSurfaceRegistry::new(device, ExternalSurfaceFormat::Bgra8Unorm, false);

        assert_eq!(
            registry.register(size_of(limit + 1, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::TransientFailure)
        );
        assert_eq!(
            registry.register(size_of(16, limit + 1), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::TransientFailure)
        );
        assert_eq!(
            registry.register(size_of(16, 16), ExternalSurfaceFormat::Rgba8Unorm),
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
        let Some(mut registry) = registry() else {
            return;
        };
        assert_eq!(registry.note_skipped_draw(), 1);
        assert_eq!(registry.note_skipped_draw(), 2);
    }

    #[test]
    fn a_producer_from_a_dead_generation_reports_device_lost() {
        let (old_device, old_queue) =
            test_device().expect("normal recovery guard requires a real first wgpu device");
        let (new_device, new_queue) =
            test_device().expect("normal recovery guard requires a real replacement wgpu device");
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&old_device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(old_device, old_queue, Rc::clone(&registry));

        assert_eq!(producer.device_generation(), 0);
        let (handle, _texture) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert!(
            registry
                .borrow()
                .resolve(handle, ExternalSampling::Nearest)
                .is_some()
        );

        registry
            .borrow_mut()
            .handle_device_lost(Arc::clone(&new_device));

        assert_eq!(
            producer.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::DeviceLost)
        );
        // Retiring through a stale producer is still harmless.
        producer.retire(handle);
        assert_eq!(registry.borrow().device_generation(), 1);
        assert!(registry.borrow().texture_bind_group_layout().is_some());

        let fresh = ExternalSurfaceProducer::new(new_device, new_queue, Rc::clone(&registry));
        let (fresh_handle, _) = fresh
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .expect("normal recovery must rebuild new-device external helpers");
        assert_eq!(fresh_handle.generation, 1);
        assert!(
            registry
                .borrow()
                .resolve(fresh_handle, ExternalSampling::Nearest)
                .is_some()
        );
    }

    #[test]
    fn deliberate_rollover_releases_exact_set_and_keeps_active_helpers() {
        let (device, queue) =
            test_device().expect("contract 1.3 rollover guard requires a real wgpu device");
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(
            Arc::clone(&device),
            Arc::clone(&queue),
            Rc::clone(&registry),
        );
        let (first, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = producer
            .register(size_of(32, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();

        let receipt = producer
            .rebuild_external_surface_domain(&[second, first])
            .expect("the complete set commits atomically");

        assert_eq!(receipt.previous_generation, 0);
        assert_eq!(receipt.current_generation, 1);
        assert_eq!(receipt.released_surface_count, 2);
        assert_eq!(
            receipt.released_external_surface_bytes,
            (16 * 16 + 32 * 16) * BYTES_PER_PIXEL
        );
        let registry = registry.borrow();
        assert!(registry.surfaces.is_empty());
        assert_eq!(registry.ledger.in_flight, 0);
        assert_eq!(registry.ledger.registered_bytes, 0);
        assert!(registry.texture_bind_group_layout().is_some());
    }

    #[test]
    fn deliberate_rollover_refuses_substitution_without_mutation() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(device, queue, Rc::clone(&registry));
        let (handle, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let substituted = ExternalSurfaceHandle::new(handle.id + 1, handle.generation);

        assert_eq!(
            producer.rebuild_external_surface_domain(&[substituted]),
            Err(ExternalSurfaceGenerationRolloverError::SurfaceSetMismatch {
                expected_surface_count: 1,
                registry_surface_count: 1,
            })
        );
        let registry = registry.borrow();
        assert_eq!(registry.device_generation(), 0);
        assert!(registry.surfaces.contains_key(&handle.id));
        assert_eq!(registry.ledger.in_flight, 1);
        assert_eq!(registry.ledger.registered_bytes, 16 * 16 * BYTES_PER_PIXEL);
    }

    #[test]
    fn stale_producer_cannot_mutate_a_fresh_generation() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let stale = ExternalSurfaceProducer::new(
            Arc::clone(&device),
            Arc::clone(&queue),
            Rc::clone(&registry),
        );
        stale.rebuild_external_surface_domain(&[]).unwrap();
        let fresh = ExternalSurfaceProducer::new(device, queue, Rc::clone(&registry));
        let (fresh_handle, _) = fresh
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let fresh_id = registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(fresh_handle)
            .unwrap();

        stale.retire(fresh_handle);
        assert_eq!(stale.close(fresh_handle), CloseOutcome::Unknown);
        assert_eq!(stale.binding_proof(fresh_id), BindingProof::StaleGeneration);
        assert_eq!(stale.retire_safety(), RetireSafety::StaleProducer);
        assert_eq!(
            stale.registry_observation().live_count,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::StaleDeviceGeneration,
            }
        );
        assert!(registry.borrow().surfaces.contains_key(&fresh_handle.id));
        assert!(matches!(
            stale.rebuild_external_surface_domain(&[fresh_handle]),
            Err(ExternalSurfaceGenerationRolloverError::StaleProducer {
                producer_generation: 0,
                current_generation: 1,
            })
        ));
    }

    #[test]
    fn rollover_busy_and_accounting_failures_precede_set_validation() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(device, queue, Rc::clone(&registry));
        let (handle, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();

        {
            let _held = registry.borrow_mut();
            assert_eq!(
                producer.rebuild_external_surface_domain(&[]),
                Err(ExternalSurfaceGenerationRolloverError::RegistryBusy)
            );
        }

        registry.borrow_mut().ledger.registered_bytes += 1;
        assert_eq!(
            producer.rebuild_external_surface_domain(&[]),
            Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingMismatch,
                }
            )
        );
        let registry = registry.borrow();
        assert_eq!(registry.device_generation(), 0);
        assert!(registry.surfaces.contains_key(&handle.id));
    }

    #[test]
    fn real_collection_byte_overflow_is_not_misclassified_as_mismatch() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(device, queue, Rc::clone(&registry));
        let (first, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let (second, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        registry
            .borrow_mut()
            .surfaces
            .get_mut(&first.id)
            .unwrap()
            .bytes = u64::MAX;

        assert_eq!(
            producer.rebuild_external_surface_domain(&[first, second]),
            Err(
                ExternalSurfaceGenerationRolloverError::RegistryUnavailable {
                    reason: RegistryUnavailableReason::AccountingOverflow,
                }
            )
        );
        assert_eq!(registry.borrow().device_generation(), 0);
    }

    #[test]
    fn real_loss_at_max_is_clear_sticky_and_helper_free() {
        let (device, queue) =
            test_device().expect("terminal device-loss guard requires a real wgpu device");
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        {
            let mut registry = registry.borrow_mut();
            registry.ledger.generation = u64::MAX;
            registry.publications.advance_device_generation(u64::MAX);
        }
        let producer =
            ExternalSurfaceProducer::new(Arc::clone(&device), queue, Rc::clone(&registry));
        let (handle, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        let publication = registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(handle)
            .unwrap();

        registry
            .borrow_mut()
            .handle_device_lost(Arc::clone(&device));

        {
            let registry = registry.borrow();
            assert_eq!(registry.device_generation(), u64::MAX);
            assert!(registry.surfaces.is_empty());
            assert_eq!(registry.ledger.in_flight, 0);
            assert_eq!(registry.ledger.registered_bytes, 0);
            assert!(registry.texture_bind_group_layout().is_none());
            assert!(matches!(
                registry.gpu,
                ExternalSurfaceGpuState::GenerationExhausted
            ));
        }
        assert_eq!(
            producer.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::DeviceLost)
        );
        assert_eq!(producer.close(handle), CloseOutcome::Unknown);
        assert_eq!(
            producer.binding_proof(publication),
            BindingProof::StaleGeneration
        );
        assert_eq!(
            producer.retire_safety(),
            RetireSafety::CounterExhausted {
                counter: gpui::PublicationCounter::DeviceGeneration,
            }
        );
        let observation = producer.registry_observation();
        assert_eq!(
            observation.live_count,
            gpui::RegistryMeasure::Unavailable {
                reason: RegistryUnavailableReason::DeviceGenerationExhausted,
            }
        );

        // A repeated real loss is idempotent: no wrap and no helper resurrection.
        registry.borrow_mut().handle_device_lost(device);
        let registry = registry.borrow();
        assert_eq!(registry.device_generation(), u64::MAX);
        assert!(registry.texture_bind_group_layout().is_none());
    }

    #[test]
    fn deliberate_max_is_mutation_free_including_gpu_helpers() {
        let (device, queue) =
            test_device().expect("deliberate-MAX guard requires a real wgpu device");
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            Arc::clone(&device),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        {
            let mut registry = registry.borrow_mut();
            registry.ledger.generation = u64::MAX;
            registry.publications.advance_device_generation(u64::MAX);
        }
        let producer = ExternalSurfaceProducer::new(device, queue, Rc::clone(&registry));
        let (handle, _) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();

        assert_eq!(
            producer.rebuild_external_surface_domain(&[handle]),
            Err(ExternalSurfaceGenerationRolloverError::CounterExhausted {
                generation: u64::MAX,
            })
        );
        let registry = registry.borrow();
        assert_eq!(registry.device_generation(), u64::MAX);
        assert!(registry.surfaces.contains_key(&handle.id));
        assert_eq!(registry.ledger.in_flight, 1);
        assert!(registry.texture_bind_group_layout().is_some());
        assert!(matches!(
            registry.gpu,
            ExternalSurfaceGpuState::Active { .. }
        ));
    }

    #[test]
    fn browser_context_loss_at_max_drops_helpers_without_wrapping() {
        let (device, _queue) =
            test_device().expect("browser terminal-loss guard requires a real wgpu device");
        let mut registry =
            ExternalSurfaceRegistry::new(device, ExternalSurfaceFormat::Rgba8Unorm, true);
        registry.ledger.generation = u64::MAX;
        registry.publications.advance_device_generation(u64::MAX);
        let (handle, _) = registry
            .register(size_of(16, 16), ExternalSurfaceFormat::Rgba8Unorm)
            .unwrap();

        registry.invalidate_all();
        registry.invalidate_all();

        assert_eq!(registry.device_generation(), u64::MAX);
        assert!(registry.surfaces.is_empty());
        assert!(
            registry
                .resolve(handle, ExternalSampling::Nearest)
                .is_none()
        );
        assert!(registry.texture_bind_group_layout().is_none());
        assert!(registry.publications.device_generation_exhausted());
    }

    #[test]
    fn an_old_device_layout_is_rejected_by_a_new_device() {
        let (old_device, _) =
            test_device().expect("DeviceMismatch guard requires a real first wgpu device");
        let (new_device, _) =
            test_device().expect("DeviceMismatch guard requires a distinct replacement device");
        let old_layout = create_external_texture_layout(&old_device);

        let scope = new_device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _pipeline_layout = new_device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("old_external_layout_on_new_device_negative_control"),
            bind_group_layouts: &[Some(&old_layout)],
            immediate_size: 0,
        });
        let error = pollster::block_on(scope.pop()).expect("old-device layout must be rejected");
        let text = error.to_string();
        let debug = format!("{error:?}");
        assert!(
            debug.contains("DeviceMismatch") && text.contains("doesn't match"),
            "the negative counterexample must carry the wgpu-core DeviceMismatch source: \
             display={text}; debug={debug}"
        );
    }

    #[test]
    fn the_contract_byte_orders_round_trip() {
        for format in [
            ExternalSurfaceFormat::Bgra8Unorm,
            ExternalSurfaceFormat::Rgba8Unorm,
        ] {
            assert_eq!(external_format(wgpu_format(format)), Some(format));
        }
        assert_eq!(external_format(wgpu::TextureFormat::Rgba16Float), None);
    }

    // --- Contract 1.2: registry observation ---------------------------------------------------
    //
    // This was D1's **red** guard: it compiled and reached the real producer entry, but the core
    // derivation was still a skeleton. D2 made it **green** on wgpu→Metal without changing the
    // assertion. The counter assertion remains the apparatus's positive control — an empty-registry
    // claim would be worthless if we could not first prove the query arrived.

    #[test]
    fn a2_bos_registry_measured_sifir_dondurur() {
        let (device, queue) =
            test_device().expect(
                "gozlem nobeti GERCEK cihaz ister; cihaz yoksa hucre BLOCKED yazilir, sessizce GECILMEZ",
            );
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
            device.clone(),
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
        )));
        let producer = ExternalSurfaceProducer::new(device, queue, registry);

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
