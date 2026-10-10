mod cosmic_text_system;
mod external_registry;
mod wgpu_atlas;
mod wgpu_context;
mod wgpu_renderer;

pub use cosmic_text_system::*;
/// The producer face of the external surface. It is exported from
/// this platform crate on purpose: an ordinary GPUI consumer, which depends on `gpui` alone,
/// cannot reach it, and what it grants is documented on the type itself.
pub use external_registry::ExternalSurfaceProducer;
pub use wgpu;
pub use wgpu_atlas::*;
pub use wgpu_context::*;
#[cfg(all(
    not(target_family = "wasm"),
    any(test, feature = "bench-support", feature = "test-support")
))]
pub use wgpu_renderer::WgpuHeadlessRenderer;
pub use wgpu_renderer::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};
