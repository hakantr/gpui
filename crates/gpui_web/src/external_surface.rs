//! The producer face of the external-surface bridge on the browser platform.
//!
//! The bridge itself — the registry, the budgets and the composite — lives in `gpui_wgpu`, which
//! is shared by every wgpu-backed platform. What is missing there is a way to *reach* one window's
//! renderer from outside GPUI: [`crate::WebWindow`] owns its [`gpui_wgpu::WgpuRenderer`] privately,
//! so `WgpuRenderer::external_surface_producer` is unreachable for an application. This module is
//! the browser half of that path, and it mirrors `gpui_windows::external_surface_producer`:
//!
//! * the consumer side is `Window::paint_external_surface` in `gpui`, which never exposes a device;
//! * the producer side is [`external_surface_producer`], which is for the one privileged external
//!   compositor and is deliberately confined to this platform crate (decision D-K16).
//!
//! The window identity is the [`web_sys::HtmlCanvasElement`] GPUI renders into, which is what
//! `HasWindowHandle` reports for a `WebWindow` (as a `WebCanvasWindowHandle`) and what
//! `document.querySelector("canvas")` finds. Because the browser platform supports exactly one
//! top-level window (see the crate docs), the registry behind the lookup is a single weak slot
//! rather than a map; the canvas is still compared, so a foreign canvas resolves to `None` exactly
//! as a foreign `HWND` does on Windows.

use crate::window::WebWindowInner;
use gpui_wgpu::ExternalSurfaceProducer;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, WindowHandle};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use wasm_bindgen::{JsCast, JsValue};

thread_local! {
    /// The currently open GPUI browser window, held weakly so that a missed unregistration cannot
    /// keep the window (and with it the device, the surface and the canvas) alive.
    static OPEN_WINDOW: RefCell<Weak<WebWindowInner>> = const { RefCell::new(Weak::new()) };
}

/// Publishes a freshly created window so that [`external_surface_producer`] can find it.
pub(crate) fn register_window(window: &Rc<WebWindowInner>) {
    OPEN_WINDOW.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            *slot = Rc::downgrade(window);
        }
    });
}

/// Withdraws a closing window. Only the entry that still points at `window` is cleared, so a
/// late drop cannot unpublish a window that replaced it.
pub(crate) fn unregister_window(window: &Rc<WebWindowInner>) {
    OPEN_WINDOW.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut()
            && slot.as_ptr() == Rc::as_ptr(window)
        {
            *slot = Weak::new();
        }
    });
}

/// Resolves a canvas to the live GPUI window rendering into it, mirroring
/// `gpui_windows::window_from_hwnd`.
fn window_from_canvas(canvas: &web_sys::HtmlCanvasElement) -> Option<Rc<WebWindowInner>> {
    let window = OPEN_WINDOW.with(|slot| slot.try_borrow().ok()?.upgrade())?;
    // JS reference equality (`===`): the canvas a consumer holds is the very DOM node GPUI
    // created, whether it came from the raw window handle or from the document.
    let requested: &JsValue = canvas.as_ref();
    let owned: &JsValue = window.canvas.as_ref();
    (requested == owned).then_some(window)
}

/// Acquires the producer face of the external-surface bridge for one GPUI window.
///
/// `canvas` is the `HtmlCanvasElement` of a GPUI window — the same canvas `raw_window_handle`
/// reports for it as a `WebCanvasWindowHandle`. The result is `None` when the canvas is not the one
/// a live GPUI window renders into, when the renderer has no device at this instant (a device-lost
/// recovery in flight), or when the window state is already mutably borrowed, which on this
/// platform is exactly the span of `PlatformWindow::draw`. An element's paint runs before that
/// span, so acquiring a producer from inside paint is fine; acquiring one from a callback that
/// GPUI invokes during the platform draw is what would yield `None` here rather than panic.
///
/// This is the one entry point named by decision D-K16, and it is intended for the single
/// privileged external compositor. See [`ExternalSurfaceProducer`] for what it does and does not
/// grant. Ordinary GPUI consumers want `Window::paint_external_surface` and
/// `Window::external_surface_capabilities` instead, which never expose a device.
pub fn external_surface_producer(
    canvas: &web_sys::HtmlCanvasElement,
) -> Option<ExternalSurfaceProducer> {
    let window = window_from_canvas(canvas)?;
    // `try_borrow` rather than `borrow`: a producer acquired from inside GPUI's own draw would
    // otherwise panic on the renderer's outstanding mutable borrow.
    let state = window.state.try_borrow().ok()?;
    state.renderer.external_surface_producer()
}

/// Recovers the canvas object a `WebCanvasWindowHandle` points at, for as long as `handle`
/// borrows the window it came from.
///
/// The returned reference is tied to `'a` on purpose: `NonNull::as_ref` would otherwise hand out
/// an unbounded lifetime, and the pointer is only known to be live while the `HasWindowHandle`
/// borrow that produced it is.
fn canvas_object<'a>(handle: &WindowHandle<'a>) -> Option<&'a JsValue> {
    let RawWindowHandle::WebCanvas(web) = handle.as_raw() else {
        return None;
    };
    // SAFETY: `web.obj` is the pointer `WebWindow::window_handle` built, and it built it from
    // `&JsValue` — a borrow of the `HtmlCanvasElement` the live `WebWindowInner` owns (see
    // `crate::window`'s `impl HasWindowHandle for WebWindow`). Casting it back to `JsValue` is
    // therefore the exact inverse of that construction, and `'a` bounds the result to the window
    // borrow `handle` holds, so the reference cannot outlive the canvas. Nothing here stores the
    // pointer or the reference.
    Some(unsafe { web.obj.cast::<JsValue>().as_ref() })
}

/// Acquires the producer face of the external-surface bridge for a GPUI window, without the caller
/// having to hold the window's canvas.
///
/// This is [`external_surface_producer`] reached through `HasWindowHandle` instead of through the
/// `HtmlCanvasElement`: it resolves the window's `WebCanvasWindowHandle` back to that canvas and
/// then defers to the canvas entry point, so the canvas-identity check there stays the final gate.
/// A window whose handle is not a `WebCanvasWindowHandle`, or whose object is not an
/// `HtmlCanvasElement`, yields `None` rather than a producer.
///
/// The result is `None` for every reason [`external_surface_producer`] returns `None` — a foreign
/// or stale canvas, a device-lost recovery in flight, or window state already mutably borrowed —
/// and `None` on its own does not tell those apart. Consumers that need that distinction take a
/// capability snapshot around the call; this function classifies nothing.
///
/// Like [`external_surface_producer`] this is confined to the browser platform crate by decision
/// D-K16 and is for the one privileged external compositor. It hands out no canvas, no raw window
/// handle, no device and no queue — only the bounded [`ExternalSurfaceProducer`].
#[cfg(target_family = "wasm")]
pub fn external_surface_producer_for_window(
    window: &gpui::Window,
) -> Option<gpui_wgpu::ExternalSurfaceProducer> {
    // `HasWindowHandle::window_handle` explicitly: `gpui::Window` has an inherent
    // `window_handle` of its own that returns GPUI's `AnyWindowHandle`, which would shadow the
    // trait method here.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let canvas = canvas_object(&handle)?.dyn_ref::<web_sys::HtmlCanvasElement>()?;
    external_surface_producer(canvas)
}
