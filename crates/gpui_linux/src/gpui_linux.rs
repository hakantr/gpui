#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
pub use linux::wayland::window::external_surface_producer;
