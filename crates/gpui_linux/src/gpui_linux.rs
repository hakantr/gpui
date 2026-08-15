#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
pub use linux::wayland::window::external_surface_producer;
pub use linux::x11::window::external_surface_producer as external_surface_producer_x11;
