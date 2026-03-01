use std::ffi::c_ulong;

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

/// Wraps a window handle, presenting XCB handles as Xlib handles for wry compatibility.
///
/// XCB and Xlib both use the same underlying X11 window IDs (XIDs), so conversion
/// is a simple cast. wry's Linux backend (`build_as_child`) only accepts Xlib handles,
/// but GPUI exposes XCB handles on X11.
pub(crate) struct XlibWindowWrapper {
    window: c_ulong,
    visual_id: c_ulong,
}

impl XlibWindowWrapper {
    pub fn from_window(window: &impl HasWindowHandle) -> Result<Self, HandleError> {
        let handle = window.window_handle()?;
        match handle.as_raw() {
            RawWindowHandle::Xcb(xcb) => Ok(Self {
                window: xcb.window.get() as c_ulong,
                visual_id: xcb.visual_id.map(|v| v.get() as c_ulong).unwrap_or(0),
            }),
            RawWindowHandle::Xlib(xlib) => Ok(Self {
                window: xlib.window,
                visual_id: xlib.visual_id,
            }),
            _ => Err(HandleError::Unavailable),
        }
    }
}

impl HasWindowHandle for XlibWindowWrapper {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let mut handle = XlibWindowHandle::new(self.window);
        handle.visual_id = self.visual_id;
        Ok(unsafe { WindowHandle::borrow_raw(handle.into()) })
    }
}

/// Initialize GTK if it hasn't been initialized yet.
///
/// WebKitGTK (wry's Linux backend) requires GTK to be initialized.
/// This is safe to call multiple times — subsequent calls are no-ops.
pub(crate) fn ensure_gtk_initialized() {
    use std::sync::Once;

    static GTK_INIT: Once = Once::new();
    GTK_INIT.call_once(|| {
        gtk::init().expect("Failed to initialize GTK for WebView");
    });
}
