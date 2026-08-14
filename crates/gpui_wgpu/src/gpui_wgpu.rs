mod cosmic_text_system;
mod external_registry;
mod wgpu_atlas;
mod wgpu_context;
mod wgpu_renderer;

pub use cosmic_text_system::*;
/// The producer face of the bounded external-surface bridge (decision D-K16). It is exported from
/// this platform crate on purpose: an ordinary GPUI consumer, which depends on `gpui` alone,
/// cannot reach it, and what it grants is documented on the type itself.
pub use external_registry::ExternalSurfaceProducer;
pub use wgpu;
pub use wgpu_atlas::*;
pub use wgpu_context::*;
pub use wgpu_renderer::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};
