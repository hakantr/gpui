//! The D3D11 half of the bounded external-surface bridge: the registry that owns the GPU
//! resources an externally produced surface is drawn from, and the single privileged producer
//! accessor that lets the external compositor draw into one.
//!
//! Two things live here, and the contract keeps them apart on purpose (`KOPRU_SOZLESMESI.md`
//! §6c, decision D-K16):
//!
//! * [`ExternalSurfaceRegistry`] — crate-private storage. It maps the opaque
//!   [`ExternalSurfaceHandle`] onto the texture, shader resource view, size, byte order and byte
//!   cost of a surface, and it enforces the capability budgets. The renderer resolves handles
//!   against it at draw time. It never hands out the device.
//! * [`ExternalSurfaceProducer`] — the **producer** face, reached through
//!   [`external_surface_producer`]. It is for the one privileged external compositor, and it
//!   carries exactly what producing a surface needs: the [`ID3D11Device`] (unavoidable — a
//!   producer cannot create a pipeline or a render target view without it), plus registering,
//!   retiring, and reading the device generation. No consumer shader source, no arbitrary render
//!   pass, no swap-chain target, and no way to invalidate the registry out from under GPUI.
//!
//! Ordinary GPUI consumers never see any of this: they see `Window::paint_external_surface` and
//! `Window::external_surface_capabilities`, and an opaque handle. Reaching the producer requires
//! taking a direct dependency on this platform crate.
//!
//! Every surface lives in GPUI's own device (D-K12), so the sync model of this step is
//! `SameQueueOrdered`: the producer's work is submitted on the same immediate context before
//! GPUI's frame, and submission order is the guarantee. The cross-device shared / keyed-mutex
//! path is deliberately **not** part of this step and is not claimed in the capability snapshot.
//!
//! Losing the device kills every handle at once, not one by one: the S1 spike showed a TDR is
//! adapter-wide. That is what [`ExternalSurfaceRegistry::invalidate_all`] expresses, and why
//! `generation` is not optional in the handle.

use std::{cell::RefCell, rc::Rc};

use collections::FxHashMap;
use gpui::{
    DevicePixels, EXTERNAL_CONTRACT_VERSION, ExternalBudgetResource, ExternalSurfaceCapabilities,
    ExternalSurfaceError, ExternalSurfaceFormat, ExternalSurfaceHandle, Size,
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::{
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
        },
        Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
        },
    },
};

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
    /// The provisional budgets of the D3D11 backend (A-K05, closes in G1b).
    ///
    /// `max_size` is 4096x4096: the guaranteed D3D11 texture dimension limit at feature level
    /// 10.1, which is the floor GPUI's own device creation accepts. `max_pixels` and `max_bytes`
    /// are derived from it rather than chosen separately, so the three cannot drift apart.
    /// `max_in_flight_surfaces` matches `BUFFER_COUNT`, the swap chain's buffer count: one
    /// in-flight surface per frame the swap chain can have outstanding.
    pub(crate) const PROVISIONAL: Self = Self {
        max_size: Size {
            width: DevicePixels(4096),
            height: DevicePixels(4096),
        },
        max_pixels: 4096 * 4096,
        max_bytes: 4096 * 4096 * BYTES_PER_PIXEL,
        max_in_flight_surfaces: crate::BUFFER_COUNT as u32,
        max_total_bytes: 4096 * 4096 * BYTES_PER_PIXEL * crate::BUFFER_COUNT as u64,
    };
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
            // Generation 0 is a legal generation: a handle minted before any device loss is fresh
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
        // and only `Bgra8Unorm`, which is the byte order GPUI's own D3D11 render targets use.
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
    /// so the producer's clone can never leave the renderer holding a dangling view.
    _texture: ID3D11Texture2D,
    /// The view the consumer samples through.
    srv: ID3D11ShaderResourceView,
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
    device: ID3D11Device,
    ledger: ExternalSurfaceLedger,
    surfaces: FxHashMap<u64, RegisteredExternalSurface>,
}

impl ExternalSurfaceRegistry {
    /// Builds an empty registry on GPUI's own device, with the provisional budgets.
    pub(crate) fn new(device: &ID3D11Device) -> Self {
        Self::with_budget(device, ExternalSurfaceBudget::PROVISIONAL)
    }

    fn with_budget(device: &ID3D11Device, budget: ExternalSurfaceBudget) -> Self {
        Self {
            device: device.clone(),
            ledger: ExternalSurfaceLedger::new(budget),
            surfaces: FxHashMap::default(),
        }
    }

    /// Registers a surface and returns its identity together with a clone of the texture.
    ///
    /// The texture is created on GPUI's own device with `RENDER_TARGET | SHADER_RESOURCE` bind
    /// flags, no misc flags and `DEFAULT` usage: no shared handle and no keyed mutex, because this
    /// step is deliberately the same-device path. The clone is what makes the producer able to
    /// create its own render target view over the surface; the shader resource view stays here,
    /// because sampling is the consumer's side of the contract.
    pub(crate) fn register(
        &mut self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, ID3D11Texture2D), ExternalSurfaceError> {
        let (handle, bytes) = self.ledger.admit(size, format)?;

        let created = create_surface_texture(&self.device, size, dxgi_format(format));
        let (texture, srv) = match created {
            Ok(created) => created,
            Err(error) => {
                // An allocation failure is retryable and must not leave the budget consumed. The
                // contract allows a re-registration attempt on a later frame, at most once per
                // device generation; it never allows a retry inside the frame.
                self.ledger.withdraw(bytes);
                log::error!("Failed to allocate a {size:?} external surface: {error:?}");
                return Err(ExternalSurfaceError::TransientFailure);
            }
        };

        self.surfaces.insert(
            handle.id,
            RegisteredExternalSurface {
                _texture: texture.clone(),
                srv,
                size,
                _format: format,
                bytes,
            },
        );
        Ok((handle, texture))
    }

    /// The view a fresh handle samples through, or `None` when the handle is stale or unknown.
    ///
    /// A stale handle is not an error here and never a panic: the draw path skips the surface and
    /// counts it, and the consumer learns of the death from the device generation.
    pub(crate) fn resolve(
        &self,
        handle: ExternalSurfaceHandle,
    ) -> Option<&ID3D11ShaderResourceView> {
        self.entry(handle).map(|surface| &surface.srv)
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
    pub(crate) fn handle_device_lost(&mut self, device: &ID3D11Device) {
        self.device = device.clone();
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
            // GPUI's D3D11 render target is `DXGI_FORMAT_B8G8R8A8_UNORM`, so BGRA is the native
            // byte order here and the RGBA fallback is not offered: a mismatch has to be an
            // observable `FormatMismatch`, not a silent conversion.
            format_bgra8_unorm: true,
            format_rgba8_unorm: false,
            sampling_nearest: true,
            sampling_linear: true,
            // The producer draws on GPUI's own device and immediate context, so submission order
            // is the whole synchronization story of this step. The cross-device keyed-mutex fast
            // path is proven but deliberately out of scope here, and an unclaimed capability is
            // better than an unbacked one.
            sync_same_queue_ordered: true,
            sync_context_ordered: false,
            sync_fence: false,
            sync_keyed_mutex: false,
            // A CPU producer can fill a `DEFAULT` texture with `UpdateSubresource` on the same
            // immediate context, which is exactly what `CpuReady` reports.
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
/// because a producer physically cannot create a pipeline or a render target view without a
/// device (decision D-K16), and it is confined to this platform crate so that reaching it takes a
/// direct dependency on `gpui_windows` rather than on `gpui`.
///
/// What it carries:
///
/// * [`device`](Self::device) — GPUI's own `ID3D11Device`. The producer creates its pipelines and
///   its render target views over registered surfaces on it, and submits on the same immediate
///   context, which is what makes `SameQueueOrdered` true.
/// * [`register`](Self::register) / [`retire`](Self::retire) — the surface lifecycle.
/// * [`device_generation`](Self::device_generation) — the liveness of every handle at once.
///
/// What it deliberately does not carry: consumer shader source, an arbitrary render pass, the
/// swap chain or any other GPUI render target, the shader resource views GPUI samples through, and
/// any way to invalidate the registry — raising the generation is GPUI's device-loss action, not
/// the producer's.
///
/// A producer is bound to one window's renderer and to the device generation it was acquired in.
/// After a device loss, `register` reports [`ExternalSurfaceError::DeviceLost`]: the recovery is a
/// full rebuild, which starts by acquiring a new producer from
/// [`external_surface_producer`]. The type is neither `Send` nor `Sync`, because the device,
/// its immediate context and the window it belongs to all live on the window's own thread.
pub struct ExternalSurfaceProducer {
    device: ID3D11Device,
    generation: u64,
    registry: Rc<RefCell<ExternalSurfaceRegistry>>,
}

impl ExternalSurfaceProducer {
    pub(crate) fn new(
        device: ID3D11Device,
        registry: Rc<RefCell<ExternalSurfaceRegistry>>,
    ) -> Self {
        let generation = registry.borrow().device_generation();
        Self {
            device,
            generation,
            registry,
        }
    }

    /// GPUI's device, on which the producer creates its own pipelines and render target views.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
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
    /// The texture carries `RENDER_TARGET | SHADER_RESOURCE`, so the producer can create its own
    /// render target view over it; GPUI keeps the shader resource view. The content is expected
    /// **premultiplied** and without group opacity applied — GPUI's composite is the sole owner of
    /// group opacity (D-K14).
    pub fn register(
        &self,
        size: Size<DevicePixels>,
        format: ExternalSurfaceFormat,
    ) -> Result<(ExternalSurfaceHandle, ID3D11Texture2D), ExternalSurfaceError> {
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

/// Acquires the producer face of the external-surface bridge for one GPUI window.
///
/// `window` is the `HWND` of a GPUI window — the same handle `raw_window_handle` reports for it.
/// The result is `None` when the handle is not a live GPUI window, when the renderer has no device
/// at this instant (a device-lost recovery in flight), or when the renderer is busy drawing, so
/// this is acquired outside the frame rather than from inside paint.
///
/// This is the one entry point named by decision D-K16, and it is intended for the single
/// privileged external compositor. See [`ExternalSurfaceProducer`] for what it does and does not
/// grant. Ordinary GPUI consumers want `Window::paint_external_surface` and
/// `Window::external_surface_capabilities` instead, which never expose a device.
pub fn external_surface_producer(window: HWND) -> Option<ExternalSurfaceProducer> {
    let window = crate::window_from_hwnd(window)?;
    // `try_borrow` rather than `borrow`: a producer acquired from inside GPUI's own draw would
    // otherwise panic on the renderer's outstanding mutable borrow.
    let renderer = window.state.renderer.try_borrow().ok()?;
    renderer.external_surface_producer()
}

/// The DXGI byte order of a contract format.
///
/// Only `Bgra8Unorm` is reported by this backend and `ExternalSurfaceLedger::admit` refuses
/// everything else before a texture is ever created; the second arm is the truthful mapping of the
/// fallback byte order rather than a silent substitution, so enabling it later stays additive.
fn dxgi_format(format: ExternalSurfaceFormat) -> DXGI_FORMAT {
    match format {
        ExternalSurfaceFormat::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        ExternalSurfaceFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
    }
}

/// Creates the texture and the sampling view of one external surface.
///
/// `MiscFlags` is zero and usage is `DEFAULT`: no shared NT handle and no keyed mutex, because the
/// surface lives in GPUI's own device. `RENDER_TARGET` is what lets the producer draw into it and
/// `SHADER_RESOURCE` is what lets GPUI sample it.
fn create_surface_texture(
    device: &ID3D11Device,
    size: Size<DevicePixels>,
    format: DXGI_FORMAT,
) -> windows::core::Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.width.0 as u32,
        Height: size.height.0 as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let texture = unsafe {
        let mut texture = None;
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        texture.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "CreateTexture2D returned no texture",
            )
        })?
    };
    let srv = unsafe {
        let mut srv = None;
        device.CreateShaderResourceView(&texture, None, Some(&mut srv))?;
        srv.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "CreateShaderResourceView returned no view",
            )
        })?
    };
    Ok((texture, srv))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        },
    };

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

    /// A WARP device, or `None` where no D3D11 device can be created at all. Device-backed tests
    /// return early in that case, following the same convention as the atlas tests.
    fn warp_device() -> Option<ID3D11Device> {
        let mut device: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .ok()?;
        device
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
        let Some(device) = warp_device() else {
            return;
        };
        let registry = ExternalSurfaceRegistry::new(&device);
        let caps = registry.capabilities();

        assert!(caps.supported);
        assert_eq!(caps.contract_version, EXTERNAL_CONTRACT_VERSION);
        assert_eq!(caps.device_generation, 0);
        assert!(caps.format_bgra8_unorm);
        assert!(!caps.format_rgba8_unorm);
        assert!(caps.sampling_nearest && caps.sampling_linear);
        assert!(caps.sync_same_queue_ordered);
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
        assert!(caps.cpu_fallback);
        assert!(!caps.allow_stale_reuse);
    }

    // --- Registry: the parts that need a real device ------------------------------------------

    #[test]
    fn a_registered_surface_resolves_until_it_is_retired() {
        let Some(device) = warp_device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(&device);

        let (handle, _texture) = registry
            .register(size_of(64, 32), ExternalSurfaceFormat::Bgra8Unorm)
            .expect("registration should succeed on a WARP device");

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
        let Some(device) = warp_device() else {
            return;
        };
        let registry = ExternalSurfaceRegistry::new(&device);
        assert!(registry.resolve(ExternalSurfaceHandle::new(7, 0)).is_none());
    }

    #[test]
    fn invalidate_all_makes_every_handle_stale_at_once() {
        let Some(device) = warp_device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(&device);

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
        let Some(device) = warp_device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::with_budget(&device, small_budget());

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
        let Some(device) = warp_device() else {
            return;
        };
        let mut registry = ExternalSurfaceRegistry::new(&device);
        assert_eq!(registry.note_skipped_draw(), 1);
        assert_eq!(registry.note_skipped_draw(), 2);
    }

    #[test]
    fn a_producer_from_a_dead_generation_reports_device_lost() {
        let Some(device) = warp_device() else {
            return;
        };
        let registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(&device)));
        let producer = ExternalSurfaceProducer::new(device.clone(), registry.clone());

        assert_eq!(producer.device_generation(), 0);
        let (handle, _texture) = producer
            .register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm)
            .unwrap();
        assert!(registry.borrow().resolve(handle).is_some());

        registry.borrow_mut().handle_device_lost(&device);

        assert_eq!(
            producer.register(size_of(16, 16), ExternalSurfaceFormat::Bgra8Unorm),
            Err(ExternalSurfaceError::DeviceLost)
        );
        // Retiring through a stale producer is still harmless.
        producer.retire(handle);
        assert_eq!(registry.borrow().device_generation(), 1);
    }
}
