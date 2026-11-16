//! A `wl_surface` as the raw handles wgpu makes a surface from (RFC-0068 §8).
//!
//! wgpu takes anything that has a window handle and a display handle. winit's
//! window has them; a Wayland surface made with smithay-client-toolkit does
//! not, so this is the one type that says what they are: the `wl_display` of
//! the connection and the `wl_surface` proxy, both as the C pointers
//! libwayland-client holds, which is what Mesa's Wayland support takes.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy};

/// What wgpu needs to draw into a `wl_surface`.
pub(crate) struct WaylandTarget {
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
    // Holding the proxy and the connection keeps both pointers valid for as
    // long as this value lives, which is as long as the wgpu surface does.
    _surface: WlSurface,
    _connection: Connection,
}

impl WaylandTarget {
    /// The target for `surface` on `connection`; `None` if either has no
    /// libwayland object (a connection that is not libwayland's, or a proxy
    /// that is already dead).
    pub(crate) fn new(connection: &Connection, surface: &WlSurface) -> Option<Self> {
        let display = NonNull::new(connection.backend().display_ptr().cast::<c_void>())?;
        let proxy = NonNull::new(surface.id().as_ptr().cast::<c_void>())?;
        Some(Self {
            display,
            surface: proxy,
            _surface: surface.clone(),
            _connection: connection.clone(),
        })
    }
}

// SAFETY: the pointers name libwayland objects, which the connection keeps
// alive and whose use is serialised by libwayland's own display lock; the
// proxy and connection that own them are `Send` and `Sync`.
unsafe impl Send for WaylandTarget {}
// SAFETY: as above.
unsafe impl Sync for WaylandTarget {}

impl HasDisplayHandle for WaylandTarget {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.display));
        // SAFETY: the display outlives `self`, which holds the connection.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl HasWindowHandle for WaylandTarget {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(self.surface));
        // SAFETY: the surface proxy outlives `self`, which holds it.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}
