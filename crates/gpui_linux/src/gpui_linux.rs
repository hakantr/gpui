#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
#[cfg(feature = "wayland")]
pub use linux::wayland::window::external_surface_producer;
#[cfg(feature = "x11")]
pub use linux::x11::window::external_surface_producer as external_surface_producer_x11;
