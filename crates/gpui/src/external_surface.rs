//! The bounded external-surface bridge: the smallest public surface that lets an externally
//! produced GPU surface be sampled into a GPUI scene at the correct draw order.
//!
//! This module is the GPUI-side half of the frozen bridge contract (`contract v1.0`). It carries
//! only plain data and the frozen validation rules; there is no GPU resource, backend type, device
//! access, or shader here. Raw `MTLTexture`, `ID3D11Texture2D`, `wgpu::Texture` and WebGL texture
//! values never reach GPUI's public API — a surface is identified solely by the opaque
//! [`ExternalSurfaceHandle`] registry id, and resource ownership stays inside the renderer.
//!
//! The consumer-side mirror of these types lives in a separate repository and is deliberately a
//! separate set of definitions rather than a shared crate; the two sides agree on *semantics* and
//! *names*, not on a common dependency. Where the contract names a geometry type, this side uses
//! the GPUI-native equivalent instead: `SurfaceSize` becomes [`Size<DevicePixels>`], the crop
//! `SourceRect` becomes `Bounds<DevicePixels>`, target/clip rectangles become `Bounds<Pixels>` and
//! `ContentMask<ScaledPixels>`, and the 2x3 affine becomes [`TransformationMatrix`].
//!
//! Nothing here draws. A backend reports [`ExternalSurfaceCapabilities::unsupported()`] until its
//! own bridge step lands, and an unsupported backend rejects the paint call with an error rather
//! than inserting a primitive that no renderer draws: silently dropping the effect is forbidden by
//! the contract.

use crate::{Bounds, DevicePixels, Pixels, Size, TransformationMatrix};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use std::fmt::{self, Display};

/// The bridge contract version this build of GPUI speaks: **contract v1.1**.
///
/// A semantic change raises `major`; an additive capability raises `minor`. v1.1 adds the tracked
/// publication surface: a publication identity, a binding proof, a terminal state and a monotone
/// retire watermark. It is additive because every seam carrying it has a default body that reports
/// [`ExternalSurfaceError::UnsupportedCapability`], so a backend that has not implemented it
/// behaves exactly as it did under v1.0.
pub const EXTERNAL_CONTRACT_VERSION: ExternalContractVersion = ExternalContractVersion::new(1, 1);

/// The semantic contract version the bridge and the external renderer shake hands on.
///
/// An incompatible version is neither a panic nor undefined behavior; it produces
/// [`ExternalSurfaceError::ContractVersionMismatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalContractVersion {
    /// The semantic version. A change means the contract was rewritten and the two sides cannot
    /// talk to each other.
    pub major: u16,
    /// The additive capability version. A change only adds backward-compatible fields.
    pub minor: u16,
}

impl ExternalContractVersion {
    /// Builds a version from a `major`/`minor` pair.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Whether a side written against `self` can serve a side written against `other`.
    ///
    /// The rule is the same `major` and `self.minor >= other.minor`: a newer `minor` only ever
    /// adds capabilities, so it carries everything an older `minor` expects, but not the reverse.
    /// A `major` difference is a semantic difference and can be served in neither direction.
    pub const fn is_compatible_with(&self, other: &ExternalContractVersion) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

impl Display for ExternalContractVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The **opaque** identity of an external surface inside the renderer's registry.
///
/// This is not a downcastable `Any` and it carries no backend resource. Resource ownership stays
/// in the renderer's registry; the consumer only carries this identity.
///
/// `generation` is not optional. Losing the device or adapter invalidates every identity at once,
/// and drawing with a stale identity produces [`ExternalSurfaceError::StaleGeneration`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalSurfaceHandle {
    /// The identity within the registry. It is only meaningful inside the same device generation.
    pub id: u64,
    /// The device/context generation this identity belongs to.
    pub generation: u64,
}

impl ExternalSurfaceHandle {
    /// Builds a handle from the given id and generation.
    pub const fn new(id: u64, generation: u64) -> Self {
        Self { id, generation }
    }

    /// Whether the handle is still fresh for the given device generation.
    pub const fn is_fresh_for(&self, device_generation: u64) -> bool {
        self.generation == device_generation
    }
}

/// The byte order of an external surface.
///
/// There is a single logical format, 8 bits per channel `unorm`; only the byte order varies and it
/// is reported through the capability snapshot. There is no silent per-platform format
/// substitution: a mismatch is [`ExternalSurfaceError::FormatMismatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalSurfaceFormat {
    /// The first preference; the byte order GPUI's Metal, D3D11 and wgpu paths use.
    Bgra8Unorm,
    /// The fallback; the byte order GPUI's wasm+GL path uses.
    Rgba8Unorm,
}

/// The color space of an external surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalColorSpace {
    /// sRGB-encoded `unorm` values with **no hardware sRGB conversion**. The only valid value in
    /// contract v1.
    SrgbEncodedUnorm,
}

/// How the alpha channel of an external surface is to be interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalAlphaMode {
    /// Premultiplied alpha; the only valid value in contract v1.
    ///
    /// This is a technical requirement rather than a preference: the contract includes linear
    /// sampling and an affine transform, and straight alpha corrupts edge colors under both. Group
    /// opacity is also exactly a scalar multiply on premultiplied content.
    Premultiplied,
}

/// How an external surface is sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalSampling {
    /// Nearest neighbor.
    Nearest,
    /// Bilinear.
    Linear,
}

/// The producer's completion information.
///
/// The waiting policy is a hard rule of the contract: the GPUI render thread never waits
/// indefinitely and the default acquire budget is zero (try-acquire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalSyncToken {
    /// The producer and GPUI share a queue, so submission order is enough (Metal, WebGPU, Vulkan,
    /// GL).
    SameQueueOrdered,
    /// The command order of the same WebGL2 context is enough.
    ContextOrdered,
    /// A GPU fence/timeline value.
    Fence {
        /// The timeline value to wait for.
        value: u64,
    },
    /// A D3D11 shared-resource key, used only on the cross-device fast path.
    KeyedMutex {
        /// The acquire/release key.
        key: u64,
    },
    /// A completed CPU fallback upload.
    CpuReady,
    /// No safe sharing could be established.
    Unsupported,
}

impl ExternalSyncToken {
    /// Whether the token reports a safe form of sharing.
    pub const fn is_supported(&self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    /// Whether the consumer has to wait on a separate synchronization object.
    ///
    /// Order alone is enough for `SameQueueOrdered` and `ContextOrdered`, and `CpuReady` has
    /// already completed; `Fence` and `KeyedMutex` require an explicit object. There is nothing to
    /// wait on for `Unsupported`.
    pub const fn requires_explicit_wait(&self) -> bool {
        matches!(self, Self::Fence { .. } | Self::KeyedMutex { .. })
    }
}

/// The resource and semantics of an external surface.
///
/// A descriptor is frame-scoped; the resource storage itself is held by the renderer's registry.
/// Separating the resource and its semantics from the placement (bounds, crop, transform, clip,
/// opacity) is what lets cached content be reused without re-rendering when only the placement
/// changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalSurfaceDescriptor {
    /// The opaque registry identity.
    pub handle: ExternalSurfaceHandle,
    /// The physical size of the surface, in device pixels.
    pub size: Size<DevicePixels>,
    /// The byte order.
    pub format: ExternalSurfaceFormat,
    /// The color space.
    pub color_space: ExternalColorSpace,
    /// The alpha interpretation.
    pub alpha_mode: ExternalAlphaMode,
    /// The sampling mode.
    pub sampling: ExternalSampling,
    /// The producer's completion information.
    pub ready: ExternalSyncToken,
    /// The number of bytes allocated for this surface; the byte budget is checked against it.
    pub allocated_bytes: u64,
}

impl ExternalSurfaceDescriptor {
    /// Validates this descriptor against a capability snapshot, in the frozen order.
    ///
    /// 1. **Generation freshness:** a handle whose generation differs from the device generation
    ///    yields [`ExternalSurfaceError::StaleGeneration`]. This check comes first on purpose: a
    ///    stale handle points at a resource that is already dead, so a format or budget error
    ///    would be misleading, and the only thing the consumer can do is a full rebuild.
    /// 2. **Format byte order:** a byte order the backend does not report yields
    ///    [`ExternalSurfaceError::FormatMismatch`]; a backend reporting no byte order at all
    ///    yields [`ExternalSurfaceError::UnsupportedCapability`]. Alpha mode and color space are
    ///    not checked at runtime: each has exactly one valid value in v1 and the type system
    ///    already guarantees it.
    /// 3. **Sampling:** a mode the backend does not report yields
    ///    [`ExternalSurfaceError::UnsupportedCapability`].
    /// 4. **Size budget:** width first, then height; an overrun is
    ///    [`ExternalSurfaceError::BudgetExceeded`] with [`ExternalBudgetResource::Size`].
    /// 5. **Pixel budget:** [`ExternalBudgetResource::Pixels`].
    /// 6. **Byte budget:** [`ExternalBudgetResource::Bytes`].
    ///
    /// [`ExternalBudgetResource::InFlightSurfaces`] is not checked here. It cannot be observed
    /// from a single descriptor and belongs to the registry level.
    ///
    /// This does not check `caps.supported`; the caller checks that first, because an unsupported
    /// backend has no meaningful budgets to compare against.
    pub fn validate(&self, caps: &ExternalSurfaceCapabilities) -> Result<(), ExternalSurfaceError> {
        if !self.handle.is_fresh_for(caps.device_generation) {
            return Err(ExternalSurfaceError::StaleGeneration {
                expected: caps.device_generation,
                actual: self.handle.generation,
            });
        }

        if !caps.supports_format(self.format) {
            return Err(match caps.preferred_format() {
                Some(expected) => ExternalSurfaceError::FormatMismatch {
                    expected,
                    actual: self.format,
                },
                None => ExternalSurfaceError::UnsupportedCapability,
            });
        }

        if !caps.supports_sampling(self.sampling) {
            return Err(ExternalSurfaceError::UnsupportedCapability);
        }

        if self.size.width > caps.max_size.width {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: device_pixels_as_u64(self.size.width),
                limit: device_pixels_as_u64(caps.max_size.width),
            });
        }
        if self.size.height > caps.max_size.height {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: device_pixels_as_u64(self.size.height),
                limit: device_pixels_as_u64(caps.max_size.height),
            });
        }

        let pixels = device_pixels_as_u64(self.size.width) * device_pixels_as_u64(self.size.height);
        if pixels > caps.max_pixels {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Pixels,
                requested: pixels,
                limit: caps.max_pixels,
            });
        }

        if self.allocated_bytes > caps.max_bytes {
            return Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: self.allocated_bytes,
                limit: caps.max_bytes,
            });
        }

        Ok(())
    }
}

/// Reads a `DevicePixels` extent as an unsigned budget quantity.
///
/// `DevicePixels` is a signed `i32`; a negative extent is not a legal surface size and saturates to
/// zero here so it can never wrap into a huge budget request.
fn device_pixels_as_u64(value: DevicePixels) -> u64 {
    value.0.max(0) as u64
}

/// The budget item named by [`ExternalSurfaceError::BudgetExceeded`].
///
/// The items map one-to-one onto the four budget fields of the capability snapshot. The budget
/// **values** are not frozen by the contract; only the mechanism is — the fields exist, an overrun
/// produces an error, and the numbers are negotiated at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalBudgetResource {
    /// `max_size`: the width or height limit of a single surface, in device pixels.
    Size,
    /// `max_pixels`: the total pixel limit of a single surface.
    Pixels,
    /// `max_bytes`: the allocated byte limit of a single surface.
    Bytes,
    /// `max_in_flight_surfaces`: the limit on how many surfaces may be in flight at once.
    ///
    /// This cannot be validated from a single descriptor; it is produced at the registry level.
    InFlightSurfaces,
}

impl Display for ExternalBudgetResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Size => "size",
            Self::Pixels => "pixels",
            Self::Bytes => "bytes",
            Self::InFlightSurfaces => "in-flight surfaces",
        };
        f.write_str(name)
    }
}

/// The observable, classified error set of the bridge.
///
/// No violation of the contract is a panic, undefined behavior, or a silently dropped effect;
/// every one of them becomes an observable error here.
///
/// The consumer-side mirror of this enum also carries a `DestinationOutsideGroup` variant. That
/// one reports a group-routing failure — an observable destination dependency reaching GPUI pixels
/// outside the private group — which is decided entirely by the external compositor's own group
/// routing. GPUI never routes groups, so the variant is deliberately omitted here rather than
/// carried as an unreachable case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalSurfaceError {
    /// The backend does not carry the direct path. The effect is not silently dropped.
    UnsupportedCapability,
    /// A pixel, byte, size, or in-flight limit was exceeded.
    BudgetExceeded {
        /// The budget item that was exceeded.
        resource: ExternalBudgetResource,
        /// The requested value.
        requested: u64,
        /// The limit in force.
        limit: u64,
    },
    /// A cycle or some other group semantics error, including an invalid placement: a non-finite
    /// bound or transform, an out-of-range opacity, or an empty or out-of-surface crop. There is
    /// no silent correction.
    InvalidGroup,
    /// The resource generation is invalid; after device or context loss every identity is dead.
    DeviceLost,
    /// The surface could not be consumed safely. The acquire budget is zero, and with stale reuse
    /// disabled the group is skipped.
    SynchronizationFailed,
    /// A retryable allocation or submit failure.
    ///
    /// There is no retry within a frame; re-registration is attempted only on a later frame, and
    /// at most once per device generation.
    TransientFailure,
    /// An incompatible format or alpha mode. There is no silent per-platform conversion.
    FormatMismatch {
        /// The preferred byte order the backend reports.
        expected: ExternalSurfaceFormat,
        /// The byte order that arrived in the descriptor.
        actual: ExternalSurfaceFormat,
    },
    /// A draw was attempted with a stale identity.
    StaleGeneration {
        /// The device generation in force.
        expected: u64,
        /// The generation of the handle that was to be drawn.
        actual: u64,
    },
    /// An incompatible contract version.
    ContractVersionMismatch {
        /// This side.
        ours: ExternalContractVersion,
        /// The other side.
        theirs: ExternalContractVersion,
    },
}

impl Display for ExternalSurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCapability => {
                f.write_str("UnsupportedCapability: the backend does not carry the direct path")
            }
            Self::BudgetExceeded {
                resource,
                requested,
                limit,
            } => write!(
                f,
                "BudgetExceeded: the {resource} limit was exceeded (requested {requested}, limit \
                 {limit})"
            ),
            Self::InvalidGroup => {
                f.write_str("InvalidGroup: a cycle or some other group semantics error")
            }
            Self::DeviceLost => f.write_str("DeviceLost: the resource generation is invalid"),
            Self::SynchronizationFailed => {
                f.write_str("SynchronizationFailed: the surface could not be consumed safely")
            }
            Self::TransientFailure => {
                f.write_str("TransientFailure: a retryable allocation or submit failure")
            }
            Self::FormatMismatch { expected, actual } => write!(
                f,
                "FormatMismatch: incompatible format/alpha (expected {expected:?}, got {actual:?})"
            ),
            Self::StaleGeneration { expected, actual } => write!(
                f,
                "StaleGeneration: draw with a stale identity (expected generation {expected}, got \
                 {actual})"
            ),
            Self::ContractVersionMismatch { ours, theirs } => write!(
                f,
                "ContractVersionMismatch: incompatible contract version (ours {ours}, theirs \
                 {theirs})"
            ),
        }
    }
}

impl std::error::Error for ExternalSurfaceError {}

/// The capability and budget snapshot a renderer backend reports for the external-surface bridge.
///
/// The snapshot is cached over the window/device lifetime; no capability query is added to the
/// per-frame hot path. The budget **values** are not frozen by the contract: only the existence of
/// the fields, the fact that an overrun produces [`ExternalSurfaceError::BudgetExceeded`], and
/// that the numbers are negotiated at runtime.
///
/// Format, sampling, and sync support are plain `bool` fields rather than a bitflag type, so that
/// adding a capability stays an additive, source-compatible change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalSurfaceCapabilities {
    /// Whether this backend carries the external-surface path at all. When this is `false` every
    /// other field is meaningless and painting an external surface is rejected with
    /// [`ExternalSurfaceError::UnsupportedCapability`].
    pub supported: bool,
    /// The contract version this backend speaks.
    pub contract_version: ExternalContractVersion,
    /// The device/context generation in force. Raising it invalidates every handle.
    pub device_generation: u64,
    /// Whether [`ExternalSurfaceFormat::Bgra8Unorm`] is supported. This is the first preference.
    pub format_bgra8_unorm: bool,
    /// Whether [`ExternalSurfaceFormat::Rgba8Unorm`] is supported. This is the fallback byte order.
    pub format_rgba8_unorm: bool,
    /// Whether [`ExternalSampling::Nearest`] is supported.
    pub sampling_nearest: bool,
    /// Whether [`ExternalSampling::Linear`] is supported.
    pub sampling_linear: bool,
    /// Whether submission order on a shared queue is enough.
    pub sync_same_queue_ordered: bool,
    /// Whether the command order of a shared context is enough.
    pub sync_context_ordered: bool,
    /// Whether a GPU fence/timeline value can be used.
    pub sync_fence: bool,
    /// Whether a keyed mutex can be used; only on the cross-device fast path.
    pub sync_keyed_mutex: bool,
    /// Whether completion of a CPU fallback upload can be used.
    pub sync_cpu_ready: bool,
    /// The largest width/height of a single surface, in device pixels.
    pub max_size: Size<DevicePixels>,
    /// The largest total pixel count of a single surface.
    pub max_pixels: u64,
    /// The largest allocation of a single surface, in bytes.
    pub max_bytes: u64,
    /// The largest number of surfaces that may be in flight at once.
    pub max_in_flight_surfaces: u32,
    /// Whether an affine transform is supported.
    pub supports_affine: bool,
    /// Whether a crop (source rectangle) is supported.
    pub supports_crop: bool,
    /// Whether clipping by the GPUI content mask is supported.
    pub supports_clip: bool,
    /// Whether the CPU fallback path is reachable.
    pub cpu_fallback: bool,
    /// Whether the last valid generation is drawn when the resource is not ready in time.
    ///
    /// When this is off the group is skipped and [`ExternalSurfaceError::SynchronizationFailed`]
    /// is produced. The frame is presented either way.
    pub allow_stale_reuse: bool,
}

impl ExternalSurfaceCapabilities {
    /// The snapshot a backend reports while it does not carry the bridge: `supported: false` with
    /// every budget zeroed.
    ///
    /// Every backend returns this until its own bridge step lands. Because `supported` is `false`,
    /// the ordinary GPUI path pays no extra allocation, branch, pass, or sync cost, and every
    /// external paint call is rejected rather than silently dropped.
    pub const fn unsupported() -> Self {
        Self {
            supported: false,
            contract_version: EXTERNAL_CONTRACT_VERSION,
            device_generation: 0,
            format_bgra8_unorm: false,
            format_rgba8_unorm: false,
            sampling_nearest: false,
            sampling_linear: false,
            sync_same_queue_ordered: false,
            sync_context_ordered: false,
            sync_fence: false,
            sync_keyed_mutex: false,
            sync_cpu_ready: false,
            max_size: Size {
                width: DevicePixels(0),
                height: DevicePixels(0),
            },
            max_pixels: 0,
            max_bytes: 0,
            max_in_flight_surfaces: 0,
            supports_affine: false,
            supports_crop: false,
            supports_clip: false,
            cpu_fallback: false,
            allow_stale_reuse: false,
        }
    }

    /// Whether the given byte order is supported.
    pub const fn supports_format(&self, format: ExternalSurfaceFormat) -> bool {
        match format {
            ExternalSurfaceFormat::Bgra8Unorm => self.format_bgra8_unorm,
            ExternalSurfaceFormat::Rgba8Unorm => self.format_rgba8_unorm,
        }
    }

    /// The first supported byte order in preference order, `Bgra8Unorm` first.
    ///
    /// `None` means the direct path could not be established at all, which results in
    /// [`ExternalSurfaceError::UnsupportedCapability`].
    pub const fn preferred_format(&self) -> Option<ExternalSurfaceFormat> {
        if self.format_bgra8_unorm {
            Some(ExternalSurfaceFormat::Bgra8Unorm)
        } else if self.format_rgba8_unorm {
            Some(ExternalSurfaceFormat::Rgba8Unorm)
        } else {
            None
        }
    }

    /// Whether the given sampling mode is supported.
    pub const fn supports_sampling(&self, sampling: ExternalSampling) -> bool {
        match sampling {
            ExternalSampling::Nearest => self.sampling_nearest,
            ExternalSampling::Linear => self.sampling_linear,
        }
    }

    /// Whether the given sync token can be established.
    ///
    /// [`ExternalSyncToken::Unsupported`] is never a supported form; it reports that safe sharing
    /// could not be established.
    pub const fn supports_sync(&self, token: ExternalSyncToken) -> bool {
        match token {
            ExternalSyncToken::SameQueueOrdered => self.sync_same_queue_ordered,
            ExternalSyncToken::ContextOrdered => self.sync_context_ordered,
            ExternalSyncToken::Fence { .. } => self.sync_fence,
            ExternalSyncToken::KeyedMutex { .. } => self.sync_keyed_mutex,
            ExternalSyncToken::CpuReady => self.sync_cpu_ready,
            ExternalSyncToken::Unsupported => false,
        }
    }

    /// Validates the version handshake, where `ours` is this side and `self.contract_version` is
    /// the other side.
    ///
    /// The compatibility rule is [`ExternalContractVersion::is_compatible_with`]. An incompatible
    /// version is not a panic; it produces [`ExternalSurfaceError::ContractVersionMismatch`].
    pub const fn check_contract_version(
        &self,
        ours: ExternalContractVersion,
    ) -> Result<(), ExternalSurfaceError> {
        if ours.is_compatible_with(&self.contract_version) {
            Ok(())
        } else {
            Err(ExternalSurfaceError::ContractVersionMismatch {
                ours,
                theirs: self.contract_version,
            })
        }
    }
}

/// Validates the placement half of an external surface paint.
///
/// The frozen rules are:
///
/// 1. every component of `bounds` and `transform` must be finite;
/// 2. `opacity` must be within `0.0..=1.0`, which excludes NaN and infinities;
/// 3. a `source_bounds` crop, when given, must be non-empty and must lie inside `surface_size`;
///    `None` means the whole surface and is always valid.
///
/// Every violation produces [`ExternalSurfaceError::InvalidGroup`]: an invalid placement leaves the
/// observable group semantics undefined. There is no silent clamping.
pub(crate) fn validate_external_paint(
    bounds: Bounds<Pixels>,
    source_bounds: Option<Bounds<DevicePixels>>,
    surface_size: Size<DevicePixels>,
    transform: &TransformationMatrix,
    opacity: f32,
) -> Result<(), ExternalSurfaceError> {
    let bounds_finite = bounds.origin.x.0.is_finite()
        && bounds.origin.y.0.is_finite()
        && bounds.size.width.0.is_finite()
        && bounds.size.height.0.is_finite();
    let transform_finite = transform
        .rotation_scale
        .iter()
        .flatten()
        .chain(transform.translation.iter())
        .all(|component| component.is_finite());
    if !bounds_finite || !transform_finite {
        return Err(ExternalSurfaceError::InvalidGroup);
    }

    if !(0.0..=1.0).contains(&opacity) {
        return Err(ExternalSurfaceError::InvalidGroup);
    }

    if let Some(crop) = source_bounds {
        let (x, y) = (i64::from(crop.origin.x.0), i64::from(crop.origin.y.0));
        let (width, height) = (i64::from(crop.size.width.0), i64::from(crop.size.height.0));
        let (max_width, max_height) = (
            i64::from(surface_size.width.0),
            i64::from(surface_size.height.0),
        );
        let inside = width > 0
            && height > 0
            && x >= 0
            && y >= 0
            && x + width <= max_width
            && y + height <= max_height;
        if !inside {
            return Err(ExternalSurfaceError::InvalidGroup);
        }
    }

    Ok(())
}

/// A source of a surface's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceSource {
    /// A macOS image buffer from CoreVideo
    #[cfg(target_os = "macos")]
    Surface(CVPixelBuffer),
    /// An externally produced GPU surface, identified only by its opaque registry handle.
    ///
    /// The renderer resolves the handle against its own registry; nothing about the underlying GPU
    /// resource is visible here. A backend that reports
    /// [`ExternalSurfaceCapabilities::unsupported()`] never receives this variant, because
    /// [`crate::Window::paint_external_surface`] rejects the call before a primitive is inserted.
    External(ExternalSurfaceDescriptor),
}

#[cfg(target_os = "macos")]
impl From<CVPixelBuffer> for SurfaceSource {
    fn from(value: CVPixelBuffer) -> Self {
        SurfaceSource::Surface(value)
    }
}

/// The window/producer scope an identity or a watermark belongs to, so a foreign question is
/// refused rather than silently answered as "not retired yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WatermarkScope(pub(crate) u64);

/// The identity of a tracked publication.
///
/// Opaque by construction: it has no `Ord`, exposes no raw serial and cannot be compared against a
/// bare `u64`. A host that wants to know whether a publication is safe to retire asks
/// [`RetireWatermark::coverage`], which also validates scope; it cannot do the arithmetic itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PublicationId {
    serial: u64,
    generation: u64,
    scope: WatermarkScope,
}

impl PublicationId {
    /// Mints an identity. Crate-private: a publication is born in the registry, never outside it.
    pub(crate) fn new(serial: u64, generation: u64, scope: WatermarkScope) -> Self {
        Self {
            serial,
            generation,
            scope,
        }
    }

    /// The window/producer scope that minted this identity.
    pub(crate) fn scope(&self) -> WatermarkScope {
        self.scope
    }

    /// The device generation this identity belongs to.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The monotone serial. Crate-private: the host is never given serial arithmetic.
    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }
}

/// Which counter ran out. Naming the counter keeps exhaustion from being read as a budget refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PublicationCounter {
    /// The per-window publication serial.
    WindowPublication,
    /// The per-window scene generation.
    SceneGeneration,
}

/// Why a tracked publication was refused.
///
/// This is a separate type rather than new variants on [`ExternalSurfaceError`]: that enum is a
/// closed, exhaustive enum, so adding a variant would break every consumer that matches it without
/// a wildcard. Adding a new error type for a new method is genuinely additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrackedPublishError {
    /// The ordinary descriptor and placement validation refused the call. A backend that does not
    /// carry the tracked surface reports `UnsupportedCapability` here.
    Surface(ExternalSurfaceError),
    /// A monotone counter ran out. Not a budget refusal: no serial was consumed and none wrapped.
    CounterExhausted {
        /// Which counter ran out.
        counter: PublicationCounter,
    },
    /// The handle was already painted untracked, so it cannot be moved into the tracked space
    /// after the fact. A new handle is required.
    AlreadyPublishedUntracked,
    /// The publication was closed to future publication. Existing live occurrences and replay
    /// continuations are unaffected; only fresh paint is refused.
    ClosedPublication,
}

/// What the registry knows about a handle, for the untracked paint path.
///
/// The query is deliberately mutation-free: asking must never enroll, mint or close anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PublicationAdmission {
    /// Not tracked. The ordinary untracked paint proceeds unchanged.
    Untracked,
    /// Tracked and open: this occurrence counts towards the named publication.
    Tracked(PublicationId),
    /// Tracked and closed: fresh paint is refused.
    Closed,
}

/// Whether a publication was ever bound to a consumer draw command.
///
/// `Bound` is evidence of a *recorded draw command*, not of GPU completion or present. See the
/// deviation record for the normative boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BindingProof {
    /// At least one occurrence reached a successful consumer draw command. Never regresses.
    Bound,
    /// Still live, or still open to future publication. Not terminal.
    Pending,
    /// Closed to the future, never bound, and no live occurrence remains. Terminal.
    Superseded,
    /// The registry has never seen this identity.
    Unknown,
    /// The identity belongs to a device generation that is gone.
    StaleGeneration,
    /// This backend does not carry the tracked publication surface.
    Unsupported,
}

/// The answer to "is this publication behind the retire watermark?".
///
/// Deliberately not a `bool`: "not yet" and "not mine" are different answers, and collapsing them
/// into `false` would let a host read a foreign identity as merely un-retired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WatermarkCoverage {
    /// Behind the watermark: every occurrence is terminal.
    Covered,
    /// Within scope, but not behind the watermark yet.
    NotYet,
    /// Another window, producer or registry. Not an answer about retirement at all.
    ForeignScope,
    /// The identity belongs to a device generation that is gone.
    StaleGeneration,
}

/// A monotone retire threshold, scoped to one window's registry and device generation.
///
/// The host cannot compare it, order it or unwrap a serial from it; it can only ask
/// [`RetireWatermark::coverage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RetireWatermark {
    through_serial: u64,
    generation: u64,
    scope: WatermarkScope,
}

impl RetireWatermark {
    /// Builds a watermark. Crate-private: only a registry may state a threshold.
    pub(crate) fn new(through_serial: u64, generation: u64, scope: WatermarkScope) -> Self {
        Self {
            through_serial,
            generation,
            scope,
        }
    }

    /// Whether `id` is behind this threshold, and whether the question was even in scope.
    ///
    /// Scope is checked first, and deliberately so: an identity from another window or producer is
    /// not a question this watermark can answer at all. Comparing its serial against ours would be
    /// meaningless, and the two ways of getting it wrong are both unsafe — reading it as `Covered`
    /// licences a retire that was never proven, and reading it as `NotYet` makes a host wait for a
    /// threshold that will never move.
    pub fn coverage(&self, id: PublicationId) -> WatermarkCoverage {
        if id.scope() != self.scope {
            return WatermarkCoverage::ForeignScope;
        }
        if id.generation() != self.generation {
            return WatermarkCoverage::StaleGeneration;
        }
        if id.serial() <= self.through_serial {
            WatermarkCoverage::Covered
        } else {
            WatermarkCoverage::NotYet
        }
    }

    /// The scope this watermark speaks for; the registry uses it to reject foreign identities.
    pub(crate) fn scope(&self) -> WatermarkScope {
        self.scope
    }
}

/// How far a producer can safely retire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RetireSafety {
    /// Everything the watermark covers is terminal.
    Through(RetireWatermark),
    /// Nothing is terminal yet. Distinct from exhaustion: progress is still possible.
    NoneYet,
    /// The producer belongs to a device generation that is gone.
    StaleProducer,
    /// A counter ran out and the registry is in a sticky fail-closed state: no new threshold will
    /// be produced. Deliberately not hidden inside `NoneYet`, which would read as "keep waiting".
    CounterExhausted {
        /// Which counter ran out.
        counter: PublicationCounter,
    },
    /// This backend does not carry the tracked publication surface.
    Unsupported,
}

/// The result of closing a publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CloseOutcome {
    /// Closed by this call.
    Closed,
    /// Already closed. Closing is idempotent and cannot be undone.
    AlreadyClosed,
    /// The registry has never seen this handle.
    Unknown,
}

/// One window's scene generation. Checked, never reused, owned by the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneGeneration(pub(crate) u64);

/// The result of an atomic scene handover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SceneReplaceOutcome {
    /// The live set was replaced and the dropped set was evaluated for terminality.
    Replaced,
    /// Both the old and the new set were empty. The only true no-op.
    NoOp,
    /// The scene generation counter ran out. The registry is now sticky fail-closed.
    CounterExhausted,
    /// This backend does not carry the tracked publication surface.
    Unsupported,
}

/// The core-to-platform carrier for one atomic scene handover.
///
/// Its fields are private and its constructor is crate-private to `gpui`, so a platform can read a
/// handover but can never forge one. It carries only the distinct set of live handles: the scene
/// generation, liveness, the sticky exhaustion flag and the watermark are owned by the per-window
/// platform registry, not by core.
///
/// **N24 — the privacy sentinel.** The two doctests below are a *pair*, and only the pair
/// distinguishes "the constructor is private" from "the symbol does not exist".
///
/// The type is public and nameable from outside the crate, so the symbol is demonstrably present:
///
/// ```
/// use gpui::SceneHandover;
/// fn reads(handover: &SceneHandover) -> usize {
///     handover.live_handles().len()
/// }
/// ```
///
/// Its constructor is not, so a platform cannot forge a handover:
///
/// ```compile_fail
/// use gpui::SceneHandover;
/// let forged = SceneHandover::new(Vec::new());
/// ```
///
/// The first doctest compiling is what makes the second one evidence of privacy rather than of a
/// missing item. Note the deliberate absence of an `EXXXX` pin on the `compile_fail`: on rustc
/// 1.97.1 that pin is inert — a wrong code is accepted just as readily as the right one — so
/// writing one would claim a precision this sentinel does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneHandover {
    live: Vec<ExternalSurfaceHandle>,
}

impl SceneHandover {
    /// Builds a handover. Crate-private: only core may state which handles a scene holds.
    pub(crate) fn new(live: Vec<ExternalSurfaceHandle>) -> Self {
        Self { live }
    }

    /// The distinct set of handles the incoming scene holds. Read-only.
    pub fn live_handles(&self) -> &[ExternalSurfaceHandle] {
        &self.live
    }
}

/// What the ordinary — untracked — paint path must do with a handle, given what the registry knows
/// about it.
///
/// This is the seam binding described in the deviation record. It is a pure function so that the
/// decision can be sentinelled without standing up a platform window: the seam itself is
/// mutation-free, so everything that matters here is the mapping.
pub(crate) fn untracked_paint_decision(
    admission: PublicationAdmission,
) -> Result<Option<PublicationId>, ExternalSurfaceError> {
    match admission {
        // Nobody is tracking this handle: the ordinary path is unchanged.
        PublicationAdmission::Untracked => Ok(None),
        // Tracked and open. The occurrence counts towards the publication the registry already
        // minted, so the old path is never invisible to the watermark.
        PublicationAdmission::Tracked(id) => Ok(Some(id)),
        // Closed to the future. Fresh paint is refused here exactly as it is on the tracked path;
        // live scenes and replay continuations are untouched, because they are not fresh paint.
        PublicationAdmission::Closed => Err(ExternalSurfaceError::InvalidGroup),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **N24, positive twin.** The permitted access — core building a handover and reading it —
    /// compiles and works. Without this half, the `compile_fail` doctest above would be satisfied
    /// by a constructor that is broken for everyone, not merely private to the outside.
    #[test]
    fn n24_pozitif_es_core_icinden_handover_kurulabilir() {
        let birinci = ExternalSurfaceHandle::new(7, 1);
        let ikinci = ExternalSurfaceHandle::new(9, 1);
        let handover = SceneHandover::new(vec![birinci, ikinci]);

        assert_eq!(handover.live_handles(), &[birinci, ikinci]);
    }

    /// **N5, fresh-paint half.** A closed publication refuses fresh paint on the ordinary path
    /// too, not only on the tracked one. Without this the host could keep feeding occurrences into
    /// a publication it has already closed, and the watermark could never become terminal.
    #[test]
    fn n5_kapali_yayin_taze_boyayi_reddeder() {
        assert_eq!(
            untracked_paint_decision(PublicationAdmission::Closed),
            Err(ExternalSurfaceError::InvalidGroup),
            "kapali tracked handle'a taze boya InvalidGroup uretmeli"
        );
    }

    /// **N4.** Once a handle is tracked, an occurrence painted through the *existing*
    /// `paint_external_surface` path still counts towards that publication. Without this the old
    /// path would be invisible to the watermark, which is a use-after-release hole.
    #[test]
    fn n4_acik_tracked_handle_eski_yoldan_ayni_yayina_sayilir() {
        let kimlik = PublicationId::new(5, 1, WatermarkScope(1));

        assert_eq!(
            untracked_paint_decision(PublicationAdmission::Tracked(kimlik)),
            Ok(Some(kimlik)),
            "acik tracked handle'in eski yoldan boyasi ayni publication'a sayilmali"
        );
    }

    /// The control: an untracked handle is unaffected. The bridge's ordinary path must not change
    /// shape for surfaces nobody is tracking.
    #[test]
    fn untracked_handle_olagan_yolda_degismez() {
        assert_eq!(
            untracked_paint_decision(PublicationAdmission::Untracked),
            Ok(None)
        );
    }

    /// **N21.** `coverage` separates four answers. Collapsing `ForeignScope` into `NotYet` would
    /// let a host read someone else's identity as merely un-retired and keep waiting forever;
    /// collapsing it into `Covered` would licence a retire that was never proven. Neither is a
    /// `bool`, which is why the query does not return one.
    #[test]
    fn n21_coverage_dort_durumu_ayirir() {
        let bizim = WatermarkScope(1);
        let yabanci = WatermarkScope(2);
        let esik = RetireWatermark::new(10, 3, bizim);

        assert_eq!(
            esik.coverage(PublicationId::new(9, 3, bizim)),
            WatermarkCoverage::Covered,
            "esigin altindaki kendi kimligimiz kapsanmis olmali"
        );
        assert_eq!(
            esik.coverage(PublicationId::new(11, 3, bizim)),
            WatermarkCoverage::NotYet,
            "esigin ustundeki kendi kimligimiz henuz kapsanmamis olmali"
        );
        assert_eq!(
            esik.coverage(PublicationId::new(9, 3, yabanci)),
            WatermarkCoverage::ForeignScope,
            "baska pencere/producer kimligi ForeignScope olmali, NotYet ya da Covered DEGIL"
        );
        assert_eq!(
            esik.coverage(PublicationId::new(9, 2, bizim)),
            WatermarkCoverage::StaleGeneration,
            "olu nesilden kimlik StaleGeneration olmali"
        );
    }

    /// Scope is checked before generation: a foreign identity is not our question at all, so it
    /// must not be answered as a stale one of ours.
    #[test]
    fn n21_kapsam_nesilden_once_denetlenir() {
        let esik = RetireWatermark::new(10, 3, WatermarkScope(1));

        assert_eq!(
            esik.coverage(PublicationId::new(9, 99, WatermarkScope(2))),
            WatermarkCoverage::ForeignScope,
            "yabanci kapsam + olu nesil ForeignScope olmali"
        );
    }

    /// The other two crate-private constructors are reachable from core in the same way. A
    /// publication identity is minted by the registry and a watermark is stated by the registry;
    /// neither can be built by a host, and both must be buildable here.
    #[test]
    fn core_icinden_kimlik_ve_esik_kurulabilir() {
        let kimlik = PublicationId::new(4, 2, WatermarkScope(1));
        assert_eq!(kimlik.serial(), 4);
        assert_eq!(kimlik.generation(), 2);
        assert_eq!(kimlik.scope(), WatermarkScope(1));

        let esik = RetireWatermark::new(4, 2, WatermarkScope(1));
        assert_eq!(esik.scope(), WatermarkScope(1));
    }
    use crate::{
        Bounds, ContentMask, PaintSurface, PrimitiveBatch, Quad, ScaledPixels, Scene, point, px,
        size,
    };

    fn supported_capabilities() -> ExternalSurfaceCapabilities {
        ExternalSurfaceCapabilities {
            supported: true,
            contract_version: EXTERNAL_CONTRACT_VERSION,
            device_generation: 3,
            format_bgra8_unorm: true,
            format_rgba8_unorm: true,
            sampling_nearest: true,
            sampling_linear: true,
            sync_same_queue_ordered: true,
            sync_context_ordered: false,
            sync_fence: true,
            sync_keyed_mutex: false,
            sync_cpu_ready: true,
            max_size: size(DevicePixels(4096), DevicePixels(4096)),
            max_pixels: 16_777_216,
            max_bytes: 67_108_864,
            max_in_flight_surfaces: 3,
            supports_affine: true,
            supports_crop: true,
            supports_clip: true,
            cpu_fallback: true,
            allow_stale_reuse: false,
        }
    }

    fn sample_descriptor() -> ExternalSurfaceDescriptor {
        ExternalSurfaceDescriptor {
            handle: ExternalSurfaceHandle::new(7, 3),
            size: size(DevicePixels(256), DevicePixels(128)),
            format: ExternalSurfaceFormat::Bgra8Unorm,
            color_space: ExternalColorSpace::SrgbEncodedUnorm,
            alpha_mode: ExternalAlphaMode::Premultiplied,
            sampling: ExternalSampling::Linear,
            ready: ExternalSyncToken::SameQueueOrdered,
            allocated_bytes: 256 * 128 * 4,
        }
    }

    // --- Capability snapshot ---------------------------------------------------------------

    #[test]
    fn unsupported_capabilities_report_no_support() {
        let caps = ExternalSurfaceCapabilities::unsupported();
        assert!(!caps.supported);
        assert_eq!(caps.contract_version, EXTERNAL_CONTRACT_VERSION);
        assert_eq!(caps.device_generation, 0);
        assert_eq!(caps.preferred_format(), None);
        assert!(!caps.supports_format(ExternalSurfaceFormat::Bgra8Unorm));
        assert!(!caps.supports_format(ExternalSurfaceFormat::Rgba8Unorm));
        assert!(!caps.supports_sampling(ExternalSampling::Nearest));
        assert!(!caps.supports_sampling(ExternalSampling::Linear));
        assert!(!caps.supports_sync(ExternalSyncToken::SameQueueOrdered));
        assert_eq!(caps.max_size, size(DevicePixels(0), DevicePixels(0)));
        assert_eq!(caps.max_pixels, 0);
        assert_eq!(caps.max_bytes, 0);
        assert_eq!(caps.max_in_flight_surfaces, 0);
        assert!(!caps.supports_affine);
        assert!(!caps.supports_crop);
        assert!(!caps.supports_clip);
        assert!(!caps.cpu_fallback);
        assert!(!caps.allow_stale_reuse);
    }

    #[test]
    fn format_preference_puts_bgra_first() {
        let mut caps = supported_capabilities();
        assert_eq!(
            caps.preferred_format(),
            Some(ExternalSurfaceFormat::Bgra8Unorm)
        );

        caps.format_bgra8_unorm = false;
        assert_eq!(
            caps.preferred_format(),
            Some(ExternalSurfaceFormat::Rgba8Unorm)
        );
        assert!(!caps.supports_format(ExternalSurfaceFormat::Bgra8Unorm));
        assert!(caps.supports_format(ExternalSurfaceFormat::Rgba8Unorm));

        caps.format_rgba8_unorm = false;
        assert_eq!(caps.preferred_format(), None);
    }

    #[test]
    fn sync_support_is_read_as_reported() {
        let caps = supported_capabilities();
        assert!(caps.supports_sync(ExternalSyncToken::SameQueueOrdered));
        assert!(caps.supports_sync(ExternalSyncToken::Fence { value: 1 }));
        assert!(caps.supports_sync(ExternalSyncToken::CpuReady));
        assert!(!caps.supports_sync(ExternalSyncToken::ContextOrdered));
        assert!(!caps.supports_sync(ExternalSyncToken::KeyedMutex { key: 1 }));
        assert!(!caps.supports_sync(ExternalSyncToken::Unsupported));
    }

    #[test]
    fn sync_token_classes() {
        assert!(ExternalSyncToken::SameQueueOrdered.is_supported());
        assert!(ExternalSyncToken::ContextOrdered.is_supported());
        assert!(ExternalSyncToken::CpuReady.is_supported());
        assert!(!ExternalSyncToken::Unsupported.is_supported());

        assert!(ExternalSyncToken::Fence { value: 9 }.requires_explicit_wait());
        assert!(ExternalSyncToken::KeyedMutex { key: 1 }.requires_explicit_wait());
        assert!(!ExternalSyncToken::SameQueueOrdered.requires_explicit_wait());
        assert!(!ExternalSyncToken::ContextOrdered.requires_explicit_wait());
        assert!(!ExternalSyncToken::CpuReady.requires_explicit_wait());
        assert!(!ExternalSyncToken::Unsupported.requires_explicit_wait());
    }

    #[test]
    fn handle_freshness_is_decided_by_generation() {
        let handle = ExternalSurfaceHandle::new(1, 3);
        assert!(handle.is_fresh_for(3));
        assert!(!handle.is_fresh_for(4));
        assert!(!handle.is_fresh_for(2));
    }

    // --- Contract version ------------------------------------------------------------------

    #[test]
    fn frozen_contract_version_is_one_one() {
        assert_eq!(
            EXTERNAL_CONTRACT_VERSION,
            ExternalContractVersion::new(1, 1)
        );
        assert_eq!(EXTERNAL_CONTRACT_VERSION.to_string(), "1.1");
        assert!(EXTERNAL_CONTRACT_VERSION.is_compatible_with(&EXTERNAL_CONTRACT_VERSION));
    }

    /// The bump from 1.0 to 1.1 is only legitimate if it is additive, and the number alone does
    /// not say that. This is the property the freeze is actually protecting: a consumer written
    /// against 1.0 must still be served, and the major must not have moved.
    #[test]
    fn one_one_is_additive_over_one_zero() {
        let bir_sifir = ExternalContractVersion::new(1, 0);

        assert!(
            EXTERNAL_CONTRACT_VERSION.is_compatible_with(&bir_sifir),
            "1.1 bir 1.0 tuketicisine hizmet etmeli"
        );
        assert!(
            !bir_sifir.is_compatible_with(&EXTERNAL_CONTRACT_VERSION),
            "1.0 bir 1.1 tuketicisine hizmet EDEMEZ; yon asimetriktir"
        );
        assert_eq!(
            EXTERNAL_CONTRACT_VERSION.major, bir_sifir.major,
            "additive bir adimda major KIMILDAMAZ"
        );
    }

    #[test]
    fn a_newer_minor_serves_an_older_one() {
        let newer = ExternalContractVersion::new(1, 3);
        let older = ExternalContractVersion::new(1, 0);
        assert!(newer.is_compatible_with(&older));
        assert!(!older.is_compatible_with(&newer));
    }

    #[test]
    fn incompatible_version_produces_contract_version_mismatch() {
        let mut caps = supported_capabilities();
        caps.contract_version = ExternalContractVersion::new(2, 0);
        assert_eq!(
            caps.check_contract_version(EXTERNAL_CONTRACT_VERSION),
            Err(ExternalSurfaceError::ContractVersionMismatch {
                ours: EXTERNAL_CONTRACT_VERSION,
                theirs: ExternalContractVersion::new(2, 0),
            })
        );

        caps.contract_version = ExternalContractVersion::new(1, 4);
        assert_eq!(
            caps.check_contract_version(EXTERNAL_CONTRACT_VERSION),
            Err(ExternalSurfaceError::ContractVersionMismatch {
                ours: EXTERNAL_CONTRACT_VERSION,
                theirs: ExternalContractVersion::new(1, 4),
            })
        );

        caps.contract_version = EXTERNAL_CONTRACT_VERSION;
        assert_eq!(
            caps.check_contract_version(EXTERNAL_CONTRACT_VERSION),
            Ok(())
        );
    }

    // --- Descriptor validation -------------------------------------------------------------

    #[test]
    fn a_valid_descriptor_passes_validation() {
        assert_eq!(
            sample_descriptor().validate(&supported_capabilities()),
            Ok(())
        );
    }

    #[test]
    fn an_unreported_byte_order_produces_format_mismatch() {
        let mut caps = supported_capabilities();
        caps.format_rgba8_unorm = false;
        let mut descriptor = sample_descriptor();
        descriptor.format = ExternalSurfaceFormat::Rgba8Unorm;

        assert_eq!(
            descriptor.validate(&caps),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Bgra8Unorm,
                actual: ExternalSurfaceFormat::Rgba8Unorm,
            })
        );
    }

    #[test]
    fn a_backend_reporting_no_format_produces_unsupported_capability() {
        let mut caps = supported_capabilities();
        caps.format_bgra8_unorm = false;
        caps.format_rgba8_unorm = false;
        assert_eq!(
            sample_descriptor().validate(&caps),
            Err(ExternalSurfaceError::UnsupportedCapability)
        );
    }

    #[test]
    fn an_unreported_sampling_mode_produces_unsupported_capability() {
        let mut caps = supported_capabilities();
        caps.sampling_linear = false;
        let mut descriptor = sample_descriptor();
        descriptor.sampling = ExternalSampling::Linear;

        assert_eq!(
            descriptor.validate(&caps),
            Err(ExternalSurfaceError::UnsupportedCapability)
        );

        descriptor.sampling = ExternalSampling::Nearest;
        assert_eq!(descriptor.validate(&caps), Ok(()));
    }

    #[test]
    fn an_oversized_width_or_height_produces_the_size_budget() {
        let caps = supported_capabilities();
        let mut descriptor = sample_descriptor();
        descriptor.size = size(caps.max_size.width + DevicePixels(1), DevicePixels(8));
        assert_eq!(
            descriptor.validate(&caps),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 4097,
                limit: 4096,
            })
        );

        descriptor.size = size(DevicePixels(8), caps.max_size.height + DevicePixels(1));
        assert_eq!(
            descriptor.validate(&caps),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Size,
                requested: 4097,
                limit: 4096,
            })
        );
    }

    #[test]
    fn too_many_pixels_produce_the_pixel_budget() {
        let mut caps = supported_capabilities();
        caps.max_pixels = 1_024;
        assert_eq!(
            sample_descriptor().validate(&caps),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Pixels,
                requested: 256 * 128,
                limit: 1_024,
            })
        );
    }

    #[test]
    fn too_many_bytes_produce_the_byte_budget() {
        let mut caps = supported_capabilities();
        caps.max_bytes = 1_000;
        assert_eq!(
            sample_descriptor().validate(&caps),
            Err(ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Bytes,
                requested: 256 * 128 * 4,
                limit: 1_000,
            })
        );
    }

    #[test]
    fn a_stale_or_future_generation_produces_stale_generation() {
        let caps = supported_capabilities();
        for generation in [caps.device_generation - 1, caps.device_generation + 1] {
            let mut descriptor = sample_descriptor();
            descriptor.handle = ExternalSurfaceHandle::new(7, generation);
            assert_eq!(
                descriptor.validate(&caps),
                Err(ExternalSurfaceError::StaleGeneration {
                    expected: caps.device_generation,
                    actual: generation,
                })
            );
        }
    }

    /// The generation check runs before everything else: on a dead handle a format or budget error
    /// is misleading, and the only thing the consumer can do is a full rebuild.
    #[test]
    fn the_generation_check_runs_before_format_and_budget() {
        let mut caps = supported_capabilities();
        caps.device_generation = 9;
        caps.format_bgra8_unorm = false;
        caps.max_pixels = 1;
        let descriptor = sample_descriptor();

        assert_eq!(
            descriptor.validate(&caps),
            Err(ExternalSurfaceError::StaleGeneration {
                expected: 9,
                actual: descriptor.handle.generation,
            })
        );
    }

    #[test]
    fn the_format_check_runs_before_the_budget() {
        let mut caps = supported_capabilities();
        caps.format_bgra8_unorm = false;
        caps.max_pixels = 1;

        assert_eq!(
            sample_descriptor().validate(&caps),
            Err(ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Rgba8Unorm,
                actual: ExternalSurfaceFormat::Bgra8Unorm,
            })
        );
    }

    // --- Placement validation --------------------------------------------------------------

    fn valid_paint() -> (
        Bounds<Pixels>,
        Option<Bounds<DevicePixels>>,
        Size<DevicePixels>,
        TransformationMatrix,
        f32,
    ) {
        (
            Bounds::new(point(px(0.), px(0.)), size(px(128.), px(64.))),
            None,
            size(DevicePixels(256), DevicePixels(128)),
            TransformationMatrix::unit(),
            1.0,
        )
    }

    #[test]
    fn a_valid_placement_passes_validation() {
        let (bounds, crop, surface, transform, opacity) = valid_paint();
        assert_eq!(
            validate_external_paint(bounds, crop, surface, &transform, opacity),
            Ok(())
        );
        assert_eq!(
            validate_external_paint(bounds, crop, surface, &transform, 0.0),
            Ok(())
        );
    }

    #[test]
    fn a_non_finite_bound_produces_invalid_group() {
        let (_, crop, surface, transform, opacity) = valid_paint();
        for broken in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let bounds = Bounds::new(point(px(0.), px(0.)), size(px(broken), px(1.)));
            assert_eq!(
                validate_external_paint(bounds, crop, surface, &transform, opacity),
                Err(ExternalSurfaceError::InvalidGroup)
            );
        }
    }

    #[test]
    fn a_non_finite_transform_produces_invalid_group() {
        let (bounds, crop, surface, _, opacity) = valid_paint();
        for index in 0..4 {
            let mut transform = TransformationMatrix::unit();
            transform.rotation_scale[index / 2][index % 2] = f32::NAN;
            assert_eq!(
                validate_external_paint(bounds, crop, surface, &transform, opacity),
                Err(ExternalSurfaceError::InvalidGroup)
            );
        }
        for index in 0..2 {
            let mut transform = TransformationMatrix::unit();
            transform.translation[index] = f32::INFINITY;
            assert_eq!(
                validate_external_paint(bounds, crop, surface, &transform, opacity),
                Err(ExternalSurfaceError::InvalidGroup)
            );
        }
    }

    #[test]
    fn an_out_of_range_opacity_produces_invalid_group() {
        let (bounds, crop, surface, transform, _) = valid_paint();
        for broken in [-0.001f32, 1.001, f32::NAN, f32::INFINITY] {
            assert_eq!(
                validate_external_paint(bounds, crop, surface, &transform, broken),
                Err(ExternalSurfaceError::InvalidGroup)
            );
        }
    }

    #[test]
    fn an_empty_or_out_of_surface_crop_produces_invalid_group() {
        let (bounds, _, surface, transform, opacity) = valid_paint();
        let broken_crops = [
            Bounds::new(
                point(DevicePixels(0), DevicePixels(0)),
                size(DevicePixels(0), DevicePixels(128)),
            ),
            Bounds::new(
                point(DevicePixels(0), DevicePixels(0)),
                size(DevicePixels(256), DevicePixels(0)),
            ),
            Bounds::new(
                point(DevicePixels(-1), DevicePixels(0)),
                size(DevicePixels(8), DevicePixels(8)),
            ),
            Bounds::new(
                point(DevicePixels(0), DevicePixels(-1)),
                size(DevicePixels(8), DevicePixels(8)),
            ),
            Bounds::new(
                point(DevicePixels(250), DevicePixels(0)),
                size(DevicePixels(8), DevicePixels(8)),
            ),
            Bounds::new(
                point(DevicePixels(0), DevicePixels(124)),
                size(DevicePixels(8), DevicePixels(8)),
            ),
        ];
        for crop in broken_crops {
            assert_eq!(
                validate_external_paint(bounds, Some(crop), surface, &transform, opacity),
                Err(ExternalSurfaceError::InvalidGroup),
                "{crop:?}"
            );
        }

        let whole = Bounds::new(
            point(DevicePixels(0), DevicePixels(0)),
            size(DevicePixels(256), DevicePixels(128)),
        );
        assert_eq!(
            validate_external_paint(bounds, Some(whole), surface, &transform, opacity),
            Ok(())
        );
    }

    // --- Error surface ---------------------------------------------------------------------

    /// The consumer-side mirror also has `DestinationOutsideGroup`; it is a compositor-side
    /// group-routing error and has no GPUI counterpart, so it is absent here by design.
    #[test]
    fn every_error_variant_displays_its_own_name() {
        let errors = [
            ExternalSurfaceError::UnsupportedCapability,
            ExternalSurfaceError::BudgetExceeded {
                resource: ExternalBudgetResource::Pixels,
                requested: 9,
                limit: 4,
            },
            ExternalSurfaceError::InvalidGroup,
            ExternalSurfaceError::DeviceLost,
            ExternalSurfaceError::SynchronizationFailed,
            ExternalSurfaceError::TransientFailure,
            ExternalSurfaceError::FormatMismatch {
                expected: ExternalSurfaceFormat::Bgra8Unorm,
                actual: ExternalSurfaceFormat::Rgba8Unorm,
            },
            ExternalSurfaceError::StaleGeneration {
                expected: 4,
                actual: 3,
            },
            ExternalSurfaceError::ContractVersionMismatch {
                ours: EXTERNAL_CONTRACT_VERSION,
                theirs: ExternalContractVersion::new(2, 0),
            },
        ];
        let names = [
            "UnsupportedCapability",
            "BudgetExceeded",
            "InvalidGroup",
            "DeviceLost",
            "SynchronizationFailed",
            "TransientFailure",
            "FormatMismatch",
            "StaleGeneration",
            "ContractVersionMismatch",
        ];
        assert_eq!(errors.len(), names.len());
        for (error, name) in errors.iter().zip(names) {
            let message = error.to_string();
            assert!(message.starts_with(name), "{message}");
        }
    }

    #[test]
    fn the_error_type_implements_the_error_trait() {
        fn as_error(error: &ExternalSurfaceError) -> &dyn std::error::Error {
            error
        }

        let error = ExternalSurfaceError::DeviceLost;
        assert!(as_error(&error).source().is_none());
        assert_eq!(as_error(&error).to_string(), error.to_string());
    }

    #[test]
    fn budget_resource_names_every_item() {
        assert_eq!(ExternalBudgetResource::Size.to_string(), "size");
        assert_eq!(ExternalBudgetResource::Pixels.to_string(), "pixels");
        assert_eq!(ExternalBudgetResource::Bytes.to_string(), "bytes");
        assert_eq!(
            ExternalBudgetResource::InFlightSurfaces.to_string(),
            "in-flight surfaces"
        );
    }

    // --- Surface source --------------------------------------------------------------------

    #[test]
    fn external_surface_sources_compare_by_descriptor() {
        let source = SurfaceSource::External(sample_descriptor());
        assert_eq!(source, source.clone());
        assert_eq!(source, SurfaceSource::External(sample_descriptor()));

        let mut other = sample_descriptor();
        other.handle = ExternalSurfaceHandle::new(8, 3);
        assert_ne!(source, SurfaceSource::External(other));

        // `SurfaceSource` still derives `Eq`, so it can be used where full equality is required.
        fn assert_eq_bound<T: Eq>(_: &T) {}
        assert_eq_bound(&source);
    }

    // --- Scene integration -----------------------------------------------------------------

    fn test_bounds() -> Bounds<ScaledPixels> {
        Bounds::new(
            point(ScaledPixels(0.), ScaledPixels(0.)),
            size(ScaledPixels(100.), ScaledPixels(100.)),
        )
    }

    fn test_content_mask() -> ContentMask<ScaledPixels> {
        ContentMask {
            bounds: test_bounds(),
        }
    }

    /// The bridge deliberately reuses the existing `Surfaces` batch instead of adding a new
    /// primitive kind, so an external surface has to interleave with ordinary primitives by draw
    /// order exactly like any other surface. This proves it.
    #[test]
    fn external_surfaces_interleave_with_quads_in_draw_order() {
        let mut scene = Scene::default();

        // Overlapping bounds force strictly increasing draw orders: quad 1, surface 2, quad 3.
        scene.insert_primitive(Quad {
            bounds: test_bounds(),
            content_mask: test_content_mask(),
            ..Default::default()
        });
        scene.insert_primitive(PaintSurface {
            order: 0,
            bounds: test_bounds(),
            content_mask: test_content_mask(),
            source: SurfaceSource::External(sample_descriptor()),
            source_bounds: None,
            transform: TransformationMatrix::unit(),
            opacity: 1.0,
        });
        scene.insert_primitive(Quad {
            bounds: test_bounds(),
            content_mask: test_content_mask(),
            ..Default::default()
        });
        scene.finish();

        assert_eq!(scene.quads.len(), 2);
        assert_eq!(scene.surfaces.len(), 1);
        assert_eq!(scene.quads[0].order, 1);
        assert_eq!(scene.surfaces[0].order, 2);
        assert_eq!(scene.quads[1].order, 3);
        assert!(matches!(
            &scene.surfaces[0].source,
            SurfaceSource::External(descriptor) if *descriptor == sample_descriptor()
        ));

        let batches: Vec<_> = scene
            .batches()
            .map(|batch| match batch {
                PrimitiveBatch::Quads(range) => ("quads", range),
                PrimitiveBatch::Surfaces(range) => ("surfaces", range),
                other => panic!("unexpected batch: {other:?}"),
            })
            .collect();

        assert_eq!(
            batches,
            vec![("quads", 0..1), ("surfaces", 0..1), ("quads", 1..2)]
        );
    }
}
