//! The Metal half of the bounded external-surface bridge: the registry that owns the GPU resources
//! an externally produced surface is drawn from, and the single privileged producer accessor that
//! lets the external compositor draw into one.
//!
//! This is the sibling of `gpui_windows/src/external_registry.rs` and
//! `gpui_wgpu/src/external_registry.rs`, deliberately structured and named the same way so the
//! three backends read as one implementation. It is the sixth and last of the six runtime profiles
//! the bridge was proven on.
//!
//! Two things live here, and the contract keeps them apart on purpose (`KOPRU_SOZLESMESI.md`
//! §6c, decision D-K16):
//!
//! * [`ExternalSurfaceRegistry`] — crate-private storage. It maps the opaque
//!   [`ExternalSurfaceHandle`] onto the texture, size, byte order and byte cost of a surface, and
//!   it enforces the capability budgets. The renderer resolves handles against it at draw time. It
//!   never hands out the device.
//! * [`ExternalSurfaceProducer`] — the **producer** face, reached through the platform-specific
//!   `gpui_macos::external_surface_producer` lookup. It is for the one privileged compositor and it
//!   carries exactly what producing a surface needs: the [`metal::Device`] and the
//!   [`metal::CommandQueue`] (unavoidable — a producer cannot create a pipeline, a render pass or a
//!   command buffer without them), plus registering, retiring, and reading the device generation.
//!   No consumer shader source, no GPUI render pass, no drawable target, and no way to invalidate
//!   the registry out from under GPUI.
//!
//! Ordinary GPUI consumers never see any of this: they see `Window::paint_external_surface` and
//! `Window::external_surface_capabilities`, and an opaque handle. Reaching the producer requires
//! taking a direct dependency on the platform crate that re-exports the producer.
//!
//! Every surface lives in GPUI's own device (D-K12), so the sync model of this step is
//! `SameQueueOrdered`, and on Metal that guarantee is unusually well evidenced: the S3 spike showed
//! a producer pass encoded in a **separate** `MTLCommandBuffer`, committed before GPUI's, still
//! yields the finished content to GPUI's own command buffer on the same `MTLCommandQueue`. The
//! producer therefore does not have to share GPUI's command buffer — which it could not reach
//! anyway — and submission order on the shared queue is the whole story. Neither a fence nor a
//! cross-device shared handle is part of this step, and neither is claimed in the capability
//! snapshot.
//!
//! Losing the device kills every handle at once, not one by one: the S1 spike showed a TDR is
//! adapter-wide. That is what [`ExternalSurfaceRegistry::invalidate_all`] expresses, and why
//! `generation` is not optional in the handle. **macOS is the one profile with no programmatic
//! device-loss notification**: Metal has no `DXGI_ERROR_DEVICE_REMOVED` equivalent and no
//! `Device::on_uncaptured_error`/`lost` future, which the S3 evidence records. So nothing in this
//! backend calls `invalidate_all` on its own; it exists because a resize or a deliberate teardown
//! still has to be able to kill every handle at once, and because the registry's tests need to
//! prove that a dead handle is skipped rather than drawn.

use std::{cell::RefCell, rc::Rc};

use collections::FxHashMap;
use gpui::{
    BindingProof, CloseOutcome, DevicePixels, EXTERNAL_CONTRACT_VERSION, ExternalBudgetResource,
    ExternalSurfaceCapabilities, ExternalSurfaceError, ExternalSurfaceFormat,
    ExternalSurfaceHandle, PublicationId, PublicationLedger, RetireSafety, Size,
};
use metal::{MTLPixelFormat, MTLStorageMode, MTLTextureUsage};

/// The bytes one pixel of an external surface costs. Contract v1 has a single logical format,
/// 8 bits per channel `unorm`.
const BYTES_PER_PIXEL: u64 = 4;

/// The capability budgets this backend reports and enforces.
///
/// **These numbers are provisional.** B1 froze only the *mechanism* — that the fields exist, that
/// an overrun produces [`ExternalSurfaceError::BudgetExceeded`], and that the values are
/// negotiated at runtime. The numbers themselves belong to open decision A-K05 and close with
/// G1b's device matrix; until then they are deliberately conservative so that no consumer builds
/// on a limit measured on a single machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternalSurfaceBudget {
    /// The largest width/height of a single surface.
    pub(crate) max_size: Size<DevicePixels>,
    /// The largest total pixel count of a single surface.
    pub(crate) max_pixels: u64,
    /// The largest allocation of a single surface, in bytes.
    pub(crate) max_bytes: u64,
    /// The largest number of surfaces that may be registered at once.
    pub(crate) max_in_flight_surfaces: u32,
    /// The aggregate ceiling over every registered surface, in bytes.
    ///
    /// The capability snapshot has no field for this, so it is deliberately derived from the two
    /// limits that *are* published — `max_bytes * max_in_flight_surfaces`. A consumer that stays
    /// inside both can therefore never be refused by a limit it cannot compute, while the registry
    /// still refuses to grow without bound if either of those limits is later widened
    /// independently.
    pub(crate) max_total_bytes: u64,
}

impl ExternalSurfaceBudget {
    /// The provisional budgets of the Metal backend (A-K05, closes in G1b).
    ///
    /// `max_size` is 4096x4096, the same provisional ceiling the D3D11 and wgpu steps report, so
    /// the backends' budgets stay comparable. Unlike wgpu it is not lowered to a device limit,
    /// because there is nothing to lower it to: every Metal GPU family GPUI runs on allows at least
    /// an 8192x8192 2D texture, so the provisional ceiling is always the binding one.
    /// `max_pixels` and `max_bytes` are derived from `max_size` rather than chosen separately, so
    /// the three cannot drift apart. `max_in_flight_surfaces` matches the layer's maximum drawable
    /// count: one in-flight surface per frame the presentation path can have outstanding.
    pub(crate) const PROVISIONAL: Self = Self {
        max_size: Size {
            width: DevicePixels(4096),
            height: DevicePixels(4096),
        },
        max_pixels: 4096 * 4096,
        max_bytes: 4096 * 4096 * BYTES_PER_PIXEL,
        max_in_flight_surfaces: crate::metal_renderer::MAX_DRAWABLE_COUNT as u32,
        max_total_bytes: 4096 * 4096 * BYTES_PER_PIXEL * crate::metal_renderer::MAX_DRAWABLE_COUNT,
    };
}

/// The device-free bookkeeping half of the registry: identity, generation and budget accounting.
///
/// It is split out from [`ExternalSurfaceRegistry`] so the whole admission policy — id and
/// generation assignment, byte and in-flight accounting, and every [`ExternalSurfaceError`] it can
/// produce — is exercised by unit tests that need no GPU at all.
#[derive(Debug)]
struct ExternalSurfaceLedger {
    /// The device generation in force. Raising it invalidates every handle.
    generation: u64,
    /// The id the next registration receives. Ids are never reused, not even across generations.
    next_id: u64,
    /// The budgets in force.
    budget: ExternalSurfaceBudget,
    /// The total bytes of every currently registered surface.
    registered_bytes: u64,
    /// How many surfaces are currently registered.
    in_flight: u32,
    /// How many draws were skipped because the handle no longer resolved. Telemetry only: a stale
    /// draw is skipped and counted, never a panic.
    skipped_draws: u64,
}

impl ExternalSurfaceLedger {
    fn new(budget: ExternalSurfaceBudget) -> Self {
        Self {
            // Generation 0 is a legal generation: a handle minted before any invalidation is fresh
            // for it, and the first `invalidate_all` moves everything to 1.
            generation: 0,
            next_id: 0,
            budget,
            registered_bytes: 0,
            in_flight: 0,
            skipped_draws: 0,
        }
    }

    /// Admits one surface against the budgets and returns its identity and byte cost.
    ///
    /// The per-surface checks run in the frozen validation order of the contract — byte order,
    /// then size, then pixels, then bytes — so that a consumer sees the same classification here
    /// as it does from `ExternalSurfaceDescriptor::validate`. The two registry-level limits, which
    /// cannot be observed from a single descriptor, are checked after them: the in-flight count
    /// first, then the aggregate byte ceiling.
    ///
    /// A non-positive extent is not a budget failure but a semantics one: it cannot be a texture
    /// at all, so it is [`ExternalSurfaceError::InvalidGroup`], and it is rejected before anything
    /// else because every later check would be arithmetic on a meaningless size.
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

        let width = size.width.0 as u64;
        let height = size.height.0 as u64;

        if size.width > self.budget.max_size.width {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: width,
                limit: self.budget.max_size.width.0 as u64,
            });
        }
        if size.height > self.budget.max_size.height {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: height,
                limit: self.budget.max_size.height.0 as u64,
            });
        }

        let pixels = width * height;
        if pixels > self.budget.max_pixels {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Pixels,
                requested: pixels,
                limit: self.budget.max_pixels,
            });
        }

        let bytes = pixels * BYTES_PER_PIXEL;
        if bytes > self.budget.max_bytes {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: bytes,
                limit: self.budget.max_bytes,
            });
        }

        if self.in_flight >= self.budget.max_in_flight_surfaces {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::InFlightSurfaces,
                requested: self.in_flight as u64 + 1,
                limit: self.budget.max_in_flight_surfaces as u64,
            });
        }

        let total_bytes = self.registered_bytes + bytes;
        if total_bytes > self.budget.max_total_bytes {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: total_bytes,
                limit: self.budget.max_total_bytes,
            });
        }

        let handle = ExternalSurfaceHandle::new(self.next_id, self.generation);
        self.next_id += 1;
        self.registered_bytes = total_bytes;
        self.in_flight += 1;
        Ok((handle, bytes))
    }

    /// Gives an admitted surface's budget back, after a retire or a failed allocation.
    fn withdraw(&mut self, bytes: u64) {
        self.registered_bytes = self.registered_bytes.saturating_sub(bytes);
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Raises the generation, which kills every outstanding handle at once, and drops the
    /// accounting with it.
    fn invalidate_all(&mut self) {
        self.generation += 1;
        self.registered_bytes = 0;
        self.in_flight = 0;
    }
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
    /// What this surface costs against the byte budget.
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
    ledger: ExternalSurfaceLedger,
    /// The publication ledger this window owns. Contract 1.1: the sole owner of the scene
    /// generation, liveness, the sticky exhaustion flag and the retire watermark.
    publications: PublicationLedger,
    surfaces: FxHashMap<u64, RegisteredExternalSurface>,
}

impl ExternalSurfaceRegistry {
    /// Builds an empty registry on GPUI's own device, with the provisional budgets.
    pub(crate) fn new(device: metal::Device) -> Self {
        Self::with_budget(device, ExternalSurfaceBudget::PROVISIONAL)
    }

    fn with_budget(device: metal::Device, budget: ExternalSurfaceBudget) -> Self {
        let storage_mode = surface_storage_mode(&device);
        Self {
            device,
            storage_mode,
            ledger: ExternalSurfaceLedger::new(budget),
            publications: PublicationLedger::new(0),
            surfaces: FxHashMap::default(),
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
        let (handle, bytes) = self.ledger.admit(size, format)?;

        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(size.width.0 as u64);
        descriptor.set_height(size.height.0 as u64);
        descriptor.set_pixel_format(metal_format(format));
        descriptor.set_usage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.set_storage_mode(self.storage_mode);
        // `new_texture` aborts rather than returning on failure, so unlike the D3D11 backend there
        // is no allocation failure to translate into `TransientFailure` here. The budgets above are
        // what keep the request inside what the device can serve.
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
        if !handle.is_fresh_for(self.ledger.generation) {
            return None;
        }
        self.surfaces.get(&handle.id)
    }

    /// Drops a surface and gives its budget back. Retiring a stale handle is a no-op: the
    /// generation bump already released everything.
    pub(crate) fn retire(&mut self, handle: ExternalSurfaceHandle) {
        if !handle.is_fresh_for(self.ledger.generation) {
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

    /// Kills every handle at once by raising the generation.
    ///
    /// This is the device-loss action, and it is collective on purpose: the S1 spike showed the
    /// loss is adapter-wide, so no surviving subset of handles exists to keep. The only way out of
    /// the invalidated state is a full rebuild on the consumer's side.
    ///
    /// **Nothing in this backend calls it automatically, and that is a recorded fact rather than an
    /// omission.** macOS has no programmatic device-loss notification: Metal offers no
    /// `DXGI_ERROR_DEVICE_REMOVED`-style result and no lost-device callback, so there is no event
    /// to hang an automatic invalidation on, and inventing one — a heuristic on a command buffer
    /// error, say — would report a loss the platform never confirmed. What remains is the explicit
    /// caller: a teardown or a deliberate rebuild.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn invalidate_all(&mut self) {
        self.surfaces.clear();
        self.ledger.invalidate_all();
        // The handle and its publication identity go stale together; neither outlives the device.
        self.publications.note_device_lost();
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
    /// The snapshot is read once over the window/device lifetime rather than per frame, and the
    /// budget numbers come from the same [`ExternalSurfaceBudget`] the registry enforces, so a
    /// consumer that respects the snapshot can never be refused by a limit it could not see.
    pub(crate) fn capabilities(&self) -> ExternalSurfaceCapabilities {
        let budget = self.ledger.budget;
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
            // is not a hopeful reading: the S3 spike proved a producer pass in a *separate*
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
            max_size: budget.max_size,
            max_pixels: budget.max_pixels,
            max_bytes: budget.max_bytes,
            max_in_flight_surfaces: budget.max_in_flight_surfaces,
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
/// without a device and a queue (decision D-K16). `gpui_macos` re-exports it so that reaching the
/// producer takes a direct platform dependency rather than exposing the device through `gpui`.
///
/// What it carries:
///
/// * [`device`](Self::device) — GPUI's own [`metal::Device`]. The producer creates its pipelines and
///   its own render pass descriptors over registered surfaces on it.
/// * [`command_queue`](Self::command_queue) — GPUI's own [`metal::CommandQueue`]. Committing the
///   producer's command buffer on it ahead of GPUI's frame is what makes `SameQueueOrdered` true,
///   and the S3 spike showed a *separate* command buffer on that queue is enough: the producer
///   never needs, and never gets, GPUI's own command buffer or encoder.
/// * [`register`](Self::register) / [`retire`](Self::retire) — the surface lifecycle.
/// * [`device_generation`](Self::device_generation) — the liveness of every handle at once.
///
/// What it deliberately does not carry: consumer shader source, GPUI's command buffer or render
/// command encoder, the layer's drawable or any other GPUI render target, and any way to invalidate
/// the registry — raising the generation is GPUI's action, not the producer's.
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
/// path, so neither a producer nor `gpui-ec` can declare a surface drawn.
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
    /// of GPUI's own work even when it is encoded in a separate command buffer (S3 evidence), which
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
        self.registry.borrow_mut().publications_mut().close(handle)
    }

    /// Whether `id` was ever bound to a consumer draw command (contract 1.1).
    ///
    /// Evidence of a recorded draw command, never of GPU completion or present.
    pub fn binding_proof(&self, id: PublicationId) -> BindingProof {
        self.registry.borrow().publications().proof(id)
    }

    /// How far this producer can safely retire (contract 1.1).
    ///
    /// The threshold never skips a live publication, and it is not a `bool`: ask
    /// [`gpui::RetireWatermark::coverage`], which also validates scope.
    pub fn retire_safety(&self) -> RetireSafety {
        self.registry.borrow().publications().retire_safety()
    }

    /// Registers a surface of `size` and returns its opaque identity together with the texture to
    /// draw into.
    ///
    /// The texture carries `RenderTarget | ShaderRead`, so the producer can attach it to its own
    /// render pass; GPUI keeps its own reference and samples it. The content is expected
    /// **premultiplied** and without group opacity applied — GPUI's composite is the sole owner of
    /// group opacity (D-K14).
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

    /// Retires a surface and gives its budget back. Retiring a handle from a dead generation is a
    /// no-op, not an error.
    pub fn retire(&self, handle: ExternalSurfaceHandle) {
        self.registry.borrow_mut().retire(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_budget() -> ExternalSurfaceBudget {
        ExternalSurfaceBudget {
            max_size: Size {
                width: DevicePixels(256),
                height: DevicePixels(128),
            },
            max_pixels: 256 * 128,
            max_bytes: 256 * 128 * BYTES_PER_PIXEL,
            max_in_flight_surfaces: 2,
            max_total_bytes: 2 * 256 * 128 * BYTES_PER_PIXEL,
        }
    }

    fn ledger() -> ExternalSurfaceLedger {
        ExternalSurfaceLedger::new(small_budget())
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

    // --- Ledger: identity, generation and budgets, with no GPU involved --------------------

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
    fn withdrawing_gives_the_budget_back() {
        let mut ledger = ledger();
        let (_, bytes) = admit(&mut ledger, 16, 16).unwrap();
        ledger.withdraw(bytes);
        assert_eq!(ledger.in_flight, 0);
        assert_eq!(ledger.registered_bytes, 0);

        // Over-withdrawing saturates instead of wrapping into a huge budget.
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
    fn an_oversized_extent_names_the_size_budget() {
        let mut ledger = ledger();
        assert_eq!(
            admit(&mut ledger, 257, 16),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 257,
                limit: 256,
            })
        );
        assert_eq!(
            admit(&mut ledger, 16, 129),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 129,
                limit: 128,
            })
        );
    }

    #[test]
    fn too_many_pixels_name_the_pixel_budget() {
        let mut ledger = ExternalSurfaceLedger::new(ExternalSurfaceBudget {
            max_pixels: 1_024,
            ..small_budget()
        });
        assert_eq!(
            admit(&mut ledger, 64, 64),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Pixels,
                requested: 64 * 64,
                limit: 1_024,
            })
        );
    }

    #[test]
    fn too_many_bytes_name_the_byte_budget() {
        let mut ledger = ExternalSurfaceLedger::new(ExternalSurfaceBudget {
            max_bytes: 1_024,
            ..small_budget()
        });
        assert_eq!(
            admit(&mut ledger, 64, 64),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: 64 * 64 * BYTES_PER_PIXEL,
                limit: 1_024,
            })
        );
    }

    #[test]
    fn one_surface_too_many_names_the_in_flight_budget() {
        let mut ledger = ledger();
        let (_, bytes) = admit(&mut ledger, 16, 16).unwrap();
        admit(&mut ledger, 16, 16).unwrap();

        assert_eq!(
            admit(&mut ledger, 16, 16),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::InFlightSurfaces,
                requested: 3,
                limit: 2,
            })
        );

        // Retiring one makes room again, and the refused registration consumed nothing.
        ledger.withdraw(bytes);
        assert!(admit(&mut ledger, 16, 16).is_ok());
    }

    #[test]
    fn the_aggregate_byte_ceiling_names_the_byte_budget() {
        // Every surface fits the per-surface budget and the in-flight count, but together they do
        // not fit the registry. With the provisional budget this cannot happen, because the
        // aggregate is derived from the two published limits; a narrowed aggregate still refuses.
        let mut ledger = ExternalSurfaceLedger::new(ExternalSurfaceBudget {
            max_size: size_of(256, 256),
            max_pixels: 256 * 256,
            max_bytes: 128 * 128 * BYTES_PER_PIXEL,
            max_in_flight_surfaces: 4,
            max_total_bytes: 3 * 128 * 128 * BYTES_PER_PIXEL / 2,
        });
        assert!(admit(&mut ledger, 128, 128).is_ok());
        assert_eq!(
            admit(&mut ledger, 128, 128),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: 2 * 128 * 128 * BYTES_PER_PIXEL,
                limit: 3 * 128 * 128 * BYTES_PER_PIXEL / 2,
            })
        );
    }

    #[test]
    fn the_provisional_aggregate_never_binds_before_the_published_limits() {
        let budget = ExternalSurfaceBudget::PROVISIONAL;
        assert_eq!(
            budget.max_total_bytes,
            budget.max_bytes * budget.max_in_flight_surfaces as u64
        );
        assert_eq!(budget.max_bytes, budget.max_pixels * BYTES_PER_PIXEL);
        assert_eq!(
            budget.max_pixels,
            budget.max_size.width.0 as u64 * budget.max_size.height.0 as u64
        );
    }

    #[test]
    fn invalidating_raises_the_generation_and_frees_the_budget() {
        let mut ledger = ledger();
        admit(&mut ledger, 16, 16).unwrap();
        assert_eq!(ledger.generation, 0);

        ledger.invalidate_all();

        assert_eq!(ledger.generation, 1);
        assert_eq!(ledger.in_flight, 0);
        assert_eq!(ledger.registered_bytes, 0);

        // Ids keep climbing across generations, so a stale id can never alias a live one.
        let (handle, _) = admit(&mut ledger, 16, 16).unwrap();
        assert_eq!(handle, ExternalSurfaceHandle::new(1, 1));
    }

    // --- Capability snapshot ----------------------------------------------------------------

    #[test]
    fn the_capability_snapshot_reports_the_budgets_it_enforces() {
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
        assert_eq!(caps.max_size, ExternalSurfaceBudget::PROVISIONAL.max_size);
        assert_eq!(
            caps.max_pixels,
            ExternalSurfaceBudget::PROVISIONAL.max_pixels
        );
        assert_eq!(caps.max_bytes, ExternalSurfaceBudget::PROVISIONAL.max_bytes);
        assert_eq!(
            caps.max_in_flight_surfaces,
            ExternalSurfaceBudget::PROVISIONAL.max_in_flight_surfaces
        );
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
        // The budget came back, so the next registration is admitted.
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

    #[test]
    fn a_budget_overrun_reaches_the_registry_unchanged() {
        let Some(device) = device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::with_budget(device, small_budget());

        assert_eq!(
            register(
                &mut registry,
                size_of(512, 16),
                ExternalSurfaceFormat::Bgra8Unorm
            ),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 512,
                limit: 256,
            })
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

        // A refused registration consumed nothing: both in-flight slots are still available.
        assert!(
            registry
                .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
                .is_ok()
        );
        assert!(
            registry
                .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
                .is_ok()
        );
        assert!(matches!(
            registry.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::InFlightSurfaces,
                ..
            })
        ));
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
}
