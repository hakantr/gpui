#![cfg(target_os = "windows")]

mod clipboard;
mod destination_list;
mod direct_manipulation;
mod direct_write;
mod directx_atlas;
mod directx_devices;
mod directx_renderer;
mod dispatcher;
mod display;
mod events;
mod external_registry;
mod keyboard;
mod platform;
mod system_notifications;
mod system_settings;
mod util;
mod vsync;
mod window;
mod wrapper;

pub(crate) use clipboard::*;
pub(crate) use destination_list::*;
pub(crate) use direct_write::*;
pub(crate) use directx_atlas::*;
pub(crate) use directx_devices::*;
pub(crate) use directx_renderer::*;
pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub(crate) use events::*;
pub(crate) use external_registry::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use system_notifications::*;
pub(crate) use system_settings::*;
pub(crate) use util::*;
pub(crate) use vsync::*;
pub(crate) use window::*;
pub(crate) use wrapper::*;

pub use platform::WindowsPlatform;

/// The producer face of the bounded external-surface bridge.
///
/// This is the one public entry point of this crate that carries a GPU device, and it exists only
/// for the single privileged external compositor: `gpui`'s own public API never exposes a device,
/// a queue, an encoder or a swap-chain target, and reaching this requires depending on this
/// platform crate directly. See [`ExternalSurfaceProducer`] for the exact limits.
pub use external_registry::{ExternalSurfaceProducer, external_surface_producer};

pub(crate) use windows::Win32::Foundation::HWND;
