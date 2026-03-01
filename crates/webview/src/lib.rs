use std::{ops::Deref, rc::Rc};

use wry::{
    Rect,
    dpi::{self, LogicalSize},
};

use gpui::{
    App, Bounds, ContentMask, Context, DismissEvent, Element, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, Hitbox, InteractiveElement, IntoElement, LayoutId,
    MouseDownEvent, ParentElement as _, Pixels, Render, Size, Style, Styled as _, Task, Window,
    canvas, div,
};

#[cfg(target_os = "linux")]
mod linux;

/// A webview based on wry WebView.
///
/// Supports macOS, Windows, and Linux (X11 only).
///
/// [experimental]
pub struct WebView {
    focus_handle: FocusHandle,
    webview: Rc<wry::WebView>,
    visible: bool,
    bounds: Bounds<Pixels>,
    #[cfg(target_os = "linux")]
    _gtk_pump_task: Option<Task<()>>,
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.hide();
    }
}

impl WebView {
    /// Create a new WebView from a wry WebView.
    ///
    /// On Linux, this starts a background task that pumps GTK events for WebKitGTK.
    /// The caller is responsible for GTK initialization and platform-specific webview
    /// construction. Consider using [`WebView::build`] instead for automatic handling.
    pub fn new(webview: wry::WebView, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = webview.set_bounds(Rect::default());

        #[cfg(target_os = "linux")]
        let gtk_pump_task = {
            let task = cx.spawn(async move |_this, cx| {
                loop {
                    smol::Timer::after(std::time::Duration::from_millis(16)).await;
                    cx.update(|_| {
                        while gtk::events_pending() {
                            gtk::main_iteration_do(false);
                        }
                    });
                }
            });
            Some(task)
        };

        Self {
            focus_handle: cx.focus_handle(),
            visible: true,
            bounds: Bounds::default(),
            webview: Rc::new(webview),
            #[cfg(target_os = "linux")]
            _gtk_pump_task: gtk_pump_task,
        }
    }

    /// Build a WebView as a child of the given GPUI window.
    ///
    /// This handles platform-specific construction:
    /// - On macOS/Windows: uses the native window handle directly
    /// - On Linux (X11): initializes GTK, converts XCB handle to Xlib, starts GTK event pumping
    ///
    /// Wayland is not currently supported (wry limitation).
    pub fn build(
        builder: wry::WebViewBuilder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let webview = Self::build_webview(builder, window)?;
        Ok(Self::new(webview, window, cx))
    }

    fn build_webview(
        builder: wry::WebViewBuilder,
        window: &mut Window,
    ) -> anyhow::Result<wry::WebView> {
        #[cfg(target_os = "linux")]
        {
            linux::ensure_gtk_initialized();
            let wrapper = linux::XlibWindowWrapper::from_window(window).map_err(|_| {
                anyhow::anyhow!(
                    "WebView requires an X11 window. Wayland is not supported. \
                     Set WAYLAND_DISPLAY=\"\" to force X11 mode."
                )
            })?;
            Ok(builder.build_as_child(&wrapper)?)
        }

        #[cfg(not(target_os = "linux"))]
        {
            use raw_window_handle::HasWindowHandle;
            let window_handle = window.window_handle()?;
            Ok(builder.build_as_child(&window_handle)?)
        }
    }

    /// Show the webview.
    pub fn show(&mut self) {
        let _ = self.webview.set_visible(true);
        self.visible = true;
    }

    /// Hide the webview.
    pub fn hide(&mut self) {
        _ = self.webview.focus_parent();
        _ = self.webview.set_visible(false);
        self.visible = false;
    }

    /// Get whether the webview is visible.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Get the current bounds of the webview.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// Go back in the webview history.
    pub fn back(&mut self) -> anyhow::Result<()> {
        Ok(self.webview.evaluate_script("history.back();")?)
    }

    /// Load a URL in the webview.
    pub fn load_url(&mut self, url: &str) {
        let _ = self.webview.load_url(url);
    }

    /// Get the raw wry webview.
    pub fn raw(&self) -> &wry::WebView {
        &self.webview
    }
}

impl Deref for WebView {
    type Target = wry::WebView;

    fn deref(&self) -> &Self::Target {
        &self.webview
    }
}

impl Focusable for WebView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for WebView {}

impl Render for WebView {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().clone();

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child({
                let view = cx.entity().clone();
                canvas(
                    move |bounds, _, cx| view.update(cx, |r, _| r.bounds = bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child(WebViewElement::new(self.webview.clone(), view, window, cx))
    }
}

/// A webview element can display a wry webview.
pub struct WebViewElement {
    parent: Entity<WebView>,
    view: Rc<wry::WebView>,
}

impl WebViewElement {
    /// Create a new webview element from a wry WebView.
    pub fn new(
        view: Rc<wry::WebView>,
        parent: Entity<WebView>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self {
        Self { view, parent }
    }
}

impl IntoElement for WebViewElement {
    type Element = WebViewElement;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };

        // If the parent view is no longer visible, we don't need to layout the webview
        let id = window.request_layout(style, [], cx);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if !self.parent.read(cx).visible() {
            return None;
        }

        let _ = self.view.set_bounds(Rect {
            size: dpi::Size::Logical(LogicalSize {
                width: bounds.size.width.into(),
                height: bounds.size.height.into(),
            }),
            position: dpi::Position::Logical(dpi::LogicalPosition::new(
                bounds.origin.x.into(),
                bounds.origin.y.into(),
            )),
        });

        // Create a hitbox to handle mouse event
        Some(window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let bounds = hitbox.clone().map(|h| h.bounds).unwrap_or(bounds);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let webview = self.view.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, _, _, _| {
                if !bounds.contains(&event.position) {
                    // Click white space to blur the input focus
                    let _ = webview.focus_parent();
                }
            });
        });
    }
}
