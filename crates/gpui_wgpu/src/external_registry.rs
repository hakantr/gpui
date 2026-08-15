//! The wgpu half of the bounded external-surface bridge: the registry that owns the GPU resources
//! an externally produced surface is drawn from, and the single privileged producer accessor that
//! lets the external compositor draw into one.
//!
//! This is the sibling of `gpui_windows/src/external_registry.rs`, deliberately structured and
//! named the same way so the two backends read as one implementation. What it opens is four of the
//! six runtime profiles at once: Browser WebGL2, Browser WebGPU, Linux wgpu-Vulkan and Linux
//! wgpu-GL all reach the bridge through this crate.
//!
//! Two things live here, and the contract keeps them apart on purpose (`KOPRU_SOZLESMESI.md`
//! §6c, decision D-K16):
//!
//! * [`ExternalSurfaceRegistry`] — crate-private storage. It maps the opaque
//!   [`ExternalSurfaceHandle`] onto the texture, its view, the two sampling bind groups, the size,
//!   the byte order and the byte cost of a surface, and it enforces the capability budgets. The
//!   renderer resolves handles against it at draw time. It never hands out the device.
//! * [`ExternalSurfaceProducer`] — the **producer** face, reached through
//!   [`crate::WgpuRenderer::external_surface_producer`]. It is for the one privileged external
//!   compositor, and it carries exactly what producing a surface needs: the [`wgpu::Device`] and
//!   the [`wgpu::Queue`] (unavoidable — a producer cannot create its own pipelines, render passes
//!   or submissions without them), plus registering, retiring, and reading the device generation.
//!   No consumer shader source, no GPUI render pass, no swap-chain target, and no way to invalidate
//!   the registry out from under GPUI.
//!
//! Ordinary GPUI consumers never see any of this: they see `Window::paint_external_surface` and
//! `Window::external_surface_capabilities`, and an opaque handle. Reaching the producer requires
//! taking a direct dependency on this platform crate.
//!
//! Every surface lives in GPUI's own device (D-K12), so the sync model of this step is submission
//! order: `SameQueueOrdered` on the native backends and on WebGPU, and `ContextOrdered` on WebGL2,
//! where the guarantee is the command order of the one shared WebGL2 context (S2 evidence). Neither
//! a fence nor a cross-device shared handle is part of this step, and neither is claimed in the
//! capability snapshot.
//!
//! Losing the device kills every handle at once, not one by one: the S1 spike showed a TDR is
//! adapter-wide. That is what [`ExternalSurfaceRegistry::invalidate_all`] expresses, and why
//! `generation` is not optional in the handle.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use collections::FxHashMap;
use gpui::{
    DevicePixels, EXTERNAL_CONTRACT_VERSION, ExternalBudgetResource, ExternalSampling,
    ExternalSurfaceCapabilities, ExternalSurfaceError, ExternalSurfaceFormat,
    ExternalSurfaceHandle, Size,
};

/// The bytes one pixel of an external surface costs. Contract v1 has a single logical format,
/// 8 bits per channel `unorm`.
const BYTES_PER_PIXEL: u64 = 4;

/// The provisional per-surface extent ceiling, before it is lowered to what the device can
/// actually allocate. See [`ExternalSurfaceBudget::PROVISIONAL`].
const PROVISIONAL_EXTENT: u64 = 4096;

/// The provisional in-flight surface ceiling.
///
/// It is the same 3 the D3D11 step reports, and for the same reason: one in-flight surface per
/// frame the presentation path can have outstanding — `desired_maximum_frame_latency` is 2 in
/// [`crate::WgpuRenderer`], plus the frame being recorded.
const MAX_IN_FLIGHT_SURFACES: u32 = 3;

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
    /// The provisional budgets of the wgpu backend (A-K05, closes in G1b).
    ///
    /// `max_size` is 4096x4096, the same provisional ceiling the D3D11 step reports, so the two
    /// backends' budgets stay comparable. `max_pixels` and `max_bytes` are derived from it rather
    /// than chosen separately, so the three cannot drift apart.
    ///
    /// The registry itself never uses this directly — it uses [`Self::provisional_for`], which
    /// lowers the ceiling to what the device can allocate — so outside tests this is the named
    /// anchor the documentation and the D3D11 backend's own `PROVISIONAL` are compared against.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const PROVISIONAL: Self = Self::provisional_for(u32::MAX);

    /// The provisional budgets, lowered to what this device can actually allocate.
    ///
    /// wgpu's device limits are the real ceiling: `max_texture_dimension_2d` is 2048 in
    /// `Limits::downlevel_webgl2_defaults()` before `using_resolution` raises it to whatever the
    /// adapter reports, and asking for a texture above it is a validation error rather than a
    /// budget answer. Reporting the lower of the two is what §9 means by negotiating the values at
    /// runtime, and on every adapter GPUI supports today it is the provisional 4096.
    pub(crate) const fn provisional_for(max_texture_dimension_2d: u32) -> Self {
        let extent = if (max_texture_dimension_2d as u64) < PROVISIONAL_EXTENT {
            max_texture_dimension_2d as u64
        } else {
            PROVISIONAL_EXTENT
        };
        let pixels = extent * extent;
        let bytes = pixels * BYTES_PER_PIXEL;
        Self {
            max_size: Size {
                width: DevicePixels(extent as i32),
                height: DevicePixels(extent as i32),
            },
            max_pixels: pixels,
            max_bytes: bytes,
            max_in_flight_surfaces: MAX_IN_FLIGHT_SURFACES,
            max_total_bytes: bytes * MAX_IN_FLIGHT_SURFACES as u64,
        }
    }
}

/// The device-free bookkeeping half of the registry: identity, generation and budget accounting.
///
/// It is split out from [`ExternalSurfaceRegistry`] so the whole admission policy — id and
/// generation assignment, byte and in-flight accounting, and every [`ExternalSurfaceError`] it can
/// produce — is exercised by unit tests that need no GPU at all.
#[derive(Debug)]
struct ExternalSurfaceLedger {
    /// The device/context generation in force. Raising it invalidates every handle.
    generation: u64,
    /// The id the next registration receives. Ids are never reused, not even across generations.
    next_id: u64,
    /// The budgets in force.
    budget: ExternalSurfaceBudget,
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
    fn new(budget: ExternalSurfaceBudget, format: ExternalSurfaceFormat) -> Self {
        Self {
            // Generation 0 is a legal generation: a handle minted before any device loss is fresh
            // for it, and the first `invalidate_all` moves everything to 1.
            generation: 0,
            next_id: 0,
            budget,
            format,
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

        // There is no silent per-platform format substitution: this backend reports exactly the
        // byte order the context selected, and refuses the other one.
        if format != self.format {
            return Err(ExternalSurfaceError::FormatMismatch {
                expected: self.format,
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
    device: Arc<wgpu::Device>,
    /// The layout every sampling bind group is built with. The external pipeline is built with
    /// this same object, so the two can never drift apart.
    texture_layout: wgpu::BindGroupLayout,
    /// The two samplers of the contract's two sampling modes. Both are **clamp-to-edge**: a crop
    /// that stops short of the surface edge must not be able to bleed the opposite edge into the
    /// result under linear filtering.
    sampler_nearest: wgpu::Sampler,
    sampler_linear: wgpu::Sampler,
    /// Whether this backend is the WebGL2 one, which is the only one whose sync guarantee is
    /// `ContextOrdered` rather than `SameQueueOrdered`.
    webgl2: bool,
    ledger: ExternalSurfaceLedger,
    surfaces: FxHashMap<u64, RegisteredExternalSurface>,
}

impl ExternalSurfaceRegistry {
    /// Builds an empty registry on GPUI's own device, with the provisional budgets lowered to what
    /// the device can allocate.
    ///
    /// `format` is the byte order the context selected — `Bgra8Unorm` everywhere except wasm+GL,
    /// where it is `Rgba8Unorm` — and it is the only one this registry admits. `webgl2` is what
    /// `WgpuContext::uses_webgl_instance_data` reports, which is exactly the WebGL2 backend.
    pub(crate) fn new(
        device: Arc<wgpu::Device>,
        format: ExternalSurfaceFormat,
        webgl2: bool,
    ) -> Self {
        let budget =
            ExternalSurfaceBudget::provisional_for(device.limits().max_texture_dimension_2d);
        Self::with_budget(device, format, webgl2, budget)
    }

    fn with_budget(
        device: Arc<wgpu::Device>,
        format: ExternalSurfaceFormat,
        webgl2: bool,
        budget: ExternalSurfaceBudget,
    ) -> Self {
        let texture_layout = create_external_texture_layout(&device);
        let sampler_nearest =
            create_external_sampler(&device, "external_surface_sampler_nearest", false);
        let sampler_linear =
            create_external_sampler(&device, "external_surface_sampler_linear", true);
        Self {
            device,
            texture_layout,
            sampler_nearest,
            sampler_linear,
            webgl2,
            ledger: ExternalSurfaceLedger::new(budget, format),
            surfaces: FxHashMap::default(),
        }
    }

    /// The layout the external-surface pipeline's sampling bind group must be built with.
    pub(crate) fn texture_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.texture_layout
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
    /// allocation that the device refuses surfaces through the device's error scope, not here.
    /// That is why the budget check above is the real gate — in particular `max_size`, which is
    /// lowered to the device's own `max_texture_dimension_2d`.
    pub(crate) fn register(
        &mut self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, wgpu::Texture), ExternalSurfaceError> {
        let (handle, bytes) = self.ledger.admit(size, format)?;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
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
        let bind_group_nearest = self.create_bind_group(&view, &self.sampler_nearest);
        let bind_group_linear = self.create_bind_group(&view, &self.sampler_linear);

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
        &self,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("external_surface_bind_group"),
            layout: &self.texture_layout,
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

    /// The device/context generation in force.
    pub(crate) fn device_generation(&self) -> u64 {
        self.ledger.generation
    }

    /// Kills every handle at once by raising the generation.
    ///
    /// This is the device-loss action, and it is collective on purpose: the S1 spike showed the
    /// loss is adapter-wide, so no surviving subset of handles exists to keep. The only way out of
    /// the invalidated state is a full rebuild on the consumer's side.
    pub(crate) fn invalidate_all(&mut self) {
        self.surfaces.clear();
        self.ledger.invalidate_all();
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
    /// [`Self::invalidate_all`] alone.
    #[cfg(any(not(target_family = "wasm"), test))]
    pub(crate) fn handle_device_lost(&mut self, device: Arc<wgpu::Device>) {
        self.texture_layout = create_external_texture_layout(&device);
        self.sampler_nearest =
            create_external_sampler(&device, "external_surface_sampler_nearest", false);
        self.sampler_linear =
            create_external_sampler(&device, "external_surface_sampler_linear", true);
        self.device = device;
        self.invalidate_all();
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
            // shared context (S2 evidence). Exactly one of the two is claimed, because they are the
            // same guarantee under two different names and claiming both would overstate it.
            sync_same_queue_ordered: !self.webgl2,
            sync_context_ordered: self.webgl2,
            sync_fence: false,
            sync_keyed_mutex: false,
            // A registered surface carries `COPY_DST`, so `Queue::write_texture` reaches it and a
            // CPU producer really does have a path here — the capability is claimed because it
            // works, not because it is convenient.
            sync_cpu_ready: true,
            max_size: budget.max_size,
            max_pixels: budget.max_pixels,
            max_bytes: budget.max_bytes,
            max_in_flight_surfaces: budget.max_in_flight_surfaces,
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
/// device and a queue (decision D-K16), and it is confined to this platform crate so that reaching
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
///
/// What it deliberately does not carry: consumer shader source, GPUI's render pass or command
/// encoder, the swap-chain texture or any other GPUI render target, the bind groups GPUI samples
/// through, and any way to invalidate the registry — raising the generation is GPUI's device-loss
/// action, not the producer's.
///
/// A producer is bound to one window's renderer and to the device generation it was acquired in.
/// After a device loss, `register` reports [`ExternalSurfaceError::DeviceLost`]: the recovery is a
/// full rebuild, which starts by acquiring a new producer from
/// [`crate::WgpuRenderer::external_surface_producer`]. The type is neither `Send` nor `Sync`,
/// because the registry it shares with the renderer lives on the window's own thread.
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

    /// Registers a surface of `size` and returns its opaque identity together with the texture to
    /// draw into.
    ///
    /// The texture carries `RENDER_ATTACHMENT | TEXTURE_BINDING`, so the producer can create its
    /// own view and render pass over it; GPUI keeps the sampling bind groups. The content is
    /// expected **premultiplied** and without group opacity applied — GPUI's composite is the sole
    /// owner of group opacity (D-K14).
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

    /// Retires a surface and gives its budget back. Retiring a handle from a dead generation is a
    /// no-op, not an error.
    pub fn retire(&self, handle: ExternalSurfaceHandle) {
        self.registry.borrow_mut().retire(handle);
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A device on whatever backend this host offers, or `None` when there is no adapter at all.
    /// Device-backed tests return early in that case.
    pub(crate) fn test_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
        test_adapter_and_device().map(|(_adapter, device, queue)| (device, queue))
    }

    /// Like [`test_device`], but keeps the adapter, which is the only thing that can be asked what
    /// usages a texture format actually supports.
    pub(crate) fn test_adapter_and_device()
    -> Option<(wgpu::Adapter, Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
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
        ExternalSurfaceLedger::new(small_budget(), ExternalSurfaceFormat::Bgra8Unorm)
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

        // The wasm+GL registry reports the other byte order, and refuses BGRA in the same way.
        let mut ledger =
            ExternalSurfaceLedger::new(small_budget(), ExternalSurfaceFormat::Rgba8Unorm);
        assert_eq!(
            ledger.admit(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Rgba8Unorm,
                actual: ExternalSurfaceFormat::Bgra8Unorm,
            })
        );
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
        let mut ledger = ExternalSurfaceLedger::new(
            ExternalSurfaceBudget {
                max_pixels: 1_024,
                ..small_budget()
            },
            ExternalSurfaceFormat::Bgra8Unorm,
        );
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
        let mut ledger = ExternalSurfaceLedger::new(
            ExternalSurfaceBudget {
                max_bytes: 1_024,
                ..small_budget()
            },
            ExternalSurfaceFormat::Bgra8Unorm,
        );
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
        let mut ledger = ExternalSurfaceLedger::new(
            ExternalSurfaceBudget {
                max_size: size_of(256, 256),
                max_pixels: 256 * 256,
                max_bytes: 128 * 128 * BYTES_PER_PIXEL,
                max_in_flight_surfaces: 4,
                max_total_bytes: 3 * 128 * 128 * BYTES_PER_PIXEL / 2,
            },
            ExternalSurfaceFormat::Bgra8Unorm,
        );
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
        assert_eq!(budget.max_size, size_of(4096, 4096));
        assert_eq!(budget.max_in_flight_surfaces, 3);
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

    /// A device that cannot allocate the provisional extent lowers every budget with it, so the
    /// registry never admits a surface the device would refuse.
    #[test]
    fn the_provisional_budget_is_lowered_to_the_device_limit() {
        let budget = ExternalSurfaceBudget::provisional_for(2048);
        assert_eq!(budget.max_size, size_of(2048, 2048));
        assert_eq!(budget.max_pixels, 2048 * 2048);
        assert_eq!(budget.max_bytes, 2048 * 2048 * BYTES_PER_PIXEL);
        assert_eq!(
            budget.max_total_bytes,
            budget.max_bytes * budget.max_in_flight_surfaces as u64
        );

        // A device above the provisional ceiling keeps the provisional one.
        assert_eq!(
            ExternalSurfaceBudget::provisional_for(16_384),
            ExternalSurfaceBudget::PROVISIONAL
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
        let Some(registry) = registry() else {
            return;
        };
        let caps = registry.capabilities();
        let budget = ExternalSurfaceBudget::provisional_for(
            registry.device.limits().max_texture_dimension_2d,
        );

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
        assert_eq!(caps.max_size, budget.max_size);
        assert_eq!(caps.max_pixels, budget.max_pixels);
        assert_eq!(caps.max_bytes, budget.max_bytes);
        assert_eq!(caps.max_in_flight_surfaces, budget.max_in_flight_surfaces);
        assert!(caps.supports_affine && caps.supports_crop && caps.supports_clip);
        assert!(!caps.allow_stale_reuse);
    }

    /// WebGL2 has no queue of its own: what S2 proved there is the command order of the one shared
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
        // The budget came back, so the next registration is admitted.
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

    #[test]
    fn a_budget_overrun_reaches_the_registry_unchanged() {
        let Some((device, _queue)) = test_device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::with_budget(
            device,
            ExternalSurfaceFormat::Bgra8Unorm,
            false,
            small_budget(),
        );

        assert_eq!(
            registry.register(size_of(512, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 512,
                limit: 256,
            })
        );
        assert_eq!(
            registry.register(size_of(16, 16), ExternalSurfaceFormat::Rgba8Unorm),
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
        let Some(mut registry) = registry() else {
            return;
        };
        assert_eq!(registry.note_skipped_draw(), 1);
        assert_eq!(registry.note_skipped_draw(), 2);
    }

    #[test]
    fn a_producer_from_a_dead_generation_reports_device_lost() {
        let Some((device, queue)) = test_device() else {
            return;
        };
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
            .handle_device_lost(Arc::clone(&device));

        assert_eq!(
            producer.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::DeviceLost)
        );
        // Retiring through a stale producer is still harmless.
        producer.retire(handle);
        assert_eq!(registry.borrow().device_generation(), 1);
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
}
