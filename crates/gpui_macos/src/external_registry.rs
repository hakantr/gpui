//! macOS window lookup for the bounded external-surface producer.
//!
//! Resource ownership moved with Zed's shared Metal renderer into `gpui_apple`; this module keeps
//! only the AppKit-specific step that resolves a live GPUI `NSView` to its window renderer.

use std::{ffi::c_void, ptr::NonNull};

pub use gpui_apple::ExternalSurfaceProducer;

/// Acquires the producer face of the external-surface bridge for one GPUI window.
///
/// `ns_view` is the live `NSView` pointer reported by the window's `AppKitWindowHandle`. The result
/// is `None` when the pointer does not name a GPUI window or its state is already locked (including
/// during `PlatformWindow::draw`).
///
/// # Safety
///
/// `ns_view` must point to a live `NSView`. AppKit has no safe validation operation for an
/// arbitrary Objective-C pointer, so a dangling or non-Objective-C pointer is undefined behavior.
pub unsafe fn external_surface_producer(
    ns_view: NonNull<c_void>,
) -> Option<ExternalSurfaceProducer> {
    // SAFETY: forwarded to the caller's obligation above; the lookup additionally checks that the
    // view belongs to a GPUI window before reading its state ivar.
    let state = unsafe { crate::window_state_from_view(ns_view) }?;
    // Avoid deadlocking when called while GPUI itself holds the window state during a draw.
    let state = state.try_lock()?;
    Some(state.renderer.external_surface_producer())
}
