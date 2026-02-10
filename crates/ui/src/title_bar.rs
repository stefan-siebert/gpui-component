use std::rc::Rc;

use crate::{
    ActiveTheme, Icon, IconName, Sizable as _, StyledExt, h_flex,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Point, Render, RenderOnce, StatefulInteractiveElement as _,
    StyleRefinement, Styled, TitlebarOptions, Window, WindowControlArea, div,
    prelude::FluentBuilder as _, px,
};
use smallvec::SmallVec;

pub const TITLE_BAR_HEIGHT: Pixels = px(34.);
#[cfg(target_os = "macos")]
const TITLE_BAR_LEFT_PADDING: Pixels = px(80.);
#[cfg(not(target_os = "macos"))]
const TITLE_BAR_LEFT_PADDING: Pixels = px(12.);

/// TitleBar used to customize the appearance of the title bar.
///
/// We can put some elements inside the title bar.
#[derive(IntoElement)]
pub struct TitleBar {
    style: StyleRefinement,
    children: SmallVec<[AnyElement; 1]>,
    on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>,
}

impl TitleBar {
    /// Create a new TitleBar.
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            children: SmallVec::new(),
            on_close_window: None,
        }
    }

    /// Returns the default title bar options for compatible with the [`crate::TitleBar`].
    pub fn title_bar_options() -> TitlebarOptions {
        TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
        }
    }

    /// Add custom for close window event, default is None, then click X button will call `window.remove_window()`.
    /// This works on Linux and Windows. On macOS, the native traffic lights handle window close.
    pub fn on_close_window(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if !cfg!(target_os = "macos") {
            self.on_close_window = Some(Rc::new(Box::new(f)));
        }
        self
    }
}

// The Windows control buttons have a fixed width of 35px.
//
// We don't need implementation the click event for the control buttons.
// If user clicked in the bounds, the window event will be triggered.
#[derive(IntoElement, Clone)]
enum ControlIcon {
    Minimize,
    Restore,
    Maximize,
    Close {
        on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>,
    },
}

impl ControlIcon {
    fn minimize() -> Self {
        Self::Minimize
    }

    fn restore() -> Self {
        Self::Restore
    }

    fn maximize() -> Self {
        Self::Maximize
    }

    fn close(on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>) -> Self {
        Self::Close { on_close_window }
    }

    fn id(&self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Close { .. } => "close",
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Restore => IconName::WindowRestore,
            Self::Maximize => IconName::WindowMaximize,
            Self::Close { .. } => IconName::WindowClose,
        }
    }

    fn window_control_area(&self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Restore | Self::Maximize => WindowControlArea::Max,
            Self::Close { .. } => WindowControlArea::Close,
        }
    }

    fn is_close(&self) -> bool {
        matches!(self, Self::Close { .. })
    }

    #[inline]
    fn hover_fg(&self, cx: &App) -> Hsla {
        if self.is_close() {
            cx.theme().danger_foreground
        } else {
            cx.theme().secondary_foreground
        }
    }

    #[inline]
    fn hover_bg(&self, cx: &App) -> Hsla {
        if self.is_close() {
            cx.theme().danger
        } else {
            cx.theme().secondary_hover
        }
    }

    #[inline]
    fn active_bg(&self, cx: &mut App) -> Hsla {
        if self.is_close() {
            cx.theme().danger_active
        } else {
            cx.theme().secondary_active
        }
    }
}

impl RenderOnce for ControlIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_linux = cfg!(target_os = "linux");
        let is_macos = cfg!(target_os = "macos");
        let is_windows = cfg!(target_os = "windows");
        let hover_fg = self.hover_fg(cx);
        let hover_bg = self.hover_bg(cx);
        let active_bg = self.active_bg(cx);
        let icon = self.clone();
        let fg_muted = cx.theme().muted_foreground;
        let on_close_window = match &self {
            ControlIcon::Close { on_close_window } => on_close_window.clone(),
            _ => None,
        };

        div()
            .id(self.id())
            .flex()
            .flex_shrink_0()
            .justify_center()
            .content_center()
            .items_center()
            // Windows: tall rectangular buttons spanning the full title bar height
            .when(!is_linux, |this| {
                this.w(TITLE_BAR_HEIGHT)
                    .h_full()
                    .text_color(cx.theme().foreground)
                    .hover(|style| style.bg(hover_bg).text_color(hover_fg))
                    .active(|style| style.bg(active_bg).text_color(hover_fg))
            })
            // Linux: small rounded circle buttons (20×20px) with uniform muted colors
            .when(is_linux, |this| {
                let bg_muted = cx.theme().muted;
                let fg = cx.theme().foreground;
                this.size_5()
                    .rounded_2xl()
                    .cursor_pointer()
                    .text_color(fg_muted)
                    .hover(|style| style.bg(bg_muted).text_color(fg))
                    .active(|style| style.bg(bg_muted).text_color(fg))
            })
            .when(is_windows, |this| {
                this.window_control_area(self.window_control_area())
            })
            .when(!is_macos, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match icon {
                        Self::Minimize => window.minimize_window(),
                        Self::Restore | Self::Maximize => {
                            #[cfg(target_os = "windows")]
                            toggle_maximize_win32(window);
                            #[cfg(not(target_os = "windows"))]
                            window.zoom_window();
                        }
                        Self::Close { .. } => {
                            if let Some(f) = on_close_window.clone() {
                                f(&ClickEvent::default(), window, cx);
                            } else {
                                window.remove_window();
                            }
                        }
                    }
                })
            })
            .child(
                Icon::new(self.icon())
                    .when(is_linux, |this| this.size_4().text_color(fg_muted))
                    .when(!is_linux, |this| this.small())
            )
    }
}

#[derive(IntoElement)]
struct WindowControls {
    on_close_window: Option<Rc<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>>,
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let is_linux = cfg!(target_os = "linux");

        if cfg!(target_os = "macos") {
            return div().id("window-controls");
        }

        h_flex()
            .id("window-controls")
            .items_center()
            .flex_shrink_0()
            .h_full()
            // Linux: spaced rounded buttons with padding
            .when(is_linux, |this| this.gap_2().pl_2().pr_3())
            .child(ControlIcon::minimize())
            .child(if window.is_maximized() {
                ControlIcon::restore()
            } else {
                ControlIcon::maximize()
            })
            .child(ControlIcon::close(self.on_close_window))
    }
}

impl Styled for TitleBar {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for TitleBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

struct TitleBarState {
    should_move: bool,
    drag_start_pos: Option<Point<Pixels>>,
    #[cfg(target_os = "windows")]
    last_mousedown_time: Option<std::time::Instant>,
    #[cfg(target_os = "windows")]
    last_mousedown_pos: Option<Point<Pixels>>,
}

// TODO: Remove this when GPUI has released v0.2.3
impl Render for TitleBarState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_macos = cfg!(target_os = "macos");

        let state = window.use_state(cx, |_, _| TitleBarState {
            should_move: false,
            drag_start_pos: None,
            #[cfg(target_os = "windows")]
            last_mousedown_time: None,
            #[cfg(target_os = "windows")]
            last_mousedown_pos: None,
        });

        // Main title bar container - all event handlers go here (like Zed's approach)
        h_flex()
            .id("title-bar")
            .flex_shrink_0()
            // Mark as drag zone for the platform
            .window_control_area(WindowControlArea::Drag)
            .w_full()
            .h(TITLE_BAR_HEIGHT)
            .pl(TITLE_BAR_LEFT_PADDING)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .bg(cx.theme().title_bar)
            .refine_style(&self.style)
            // Mouse event handlers for drag
            .on_mouse_down_out(window.listener_for(&state, |state, _, _, _| {
                state.should_move = false;
                state.drag_start_pos = None;
            }))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&state, |state, event: &gpui::MouseDownEvent, window, cx| {
                    // On Windows, a focusable parent element's auto-focus handler
                    // calls prevent_default() on every mouse-down, which blocks
                    // DefWindowProc from handling NC events (drag, resize, etc.).
                    // We must stop propagation for ALL title bar clicks and handle
                    // everything ourselves via the Win32 API.
                    #[cfg(target_os = "windows")]
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                    }

                    // On Windows, handle the top resize zone (~8px) by posting
                    // WM_NCLBUTTONDOWN + HTTOP directly, since DefWindowProc can't.
                    #[cfg(target_os = "windows")]
                    if event.position.y < px(8.0) {
                        start_top_resize_win32(window);
                        return;
                    }

                    // On Windows, detect double-clicks ourselves because GPUI's
                    // click_count() doesn't work reliably with WindowControlArea::Drag
                    // (WM_NCHITTEST returns HTCAPTION, bypassing normal click tracking).
                    #[cfg(target_os = "windows")]
                    {
                        let now = std::time::Instant::now();
                        let is_double_click = match (state.last_mousedown_time, state.last_mousedown_pos) {
                            (Some(last_time), Some(last_pos)) => {
                                let elapsed = now.duration_since(last_time);
                                let delta = event.position - last_pos;
                                let threshold = px(4.0);
                                elapsed.as_millis() < 500
                                    && delta.x > -threshold && delta.x < threshold
                                    && delta.y > -threshold && delta.y < threshold
                            }
                            _ => false,
                        };

                        if is_double_click {
                            state.should_move = false;
                            state.drag_start_pos = None;
                            state.last_mousedown_time = None;
                            state.last_mousedown_pos = None;
                            toggle_maximize_win32(window);
                            return;
                        }

                        state.last_mousedown_time = Some(now);
                        state.last_mousedown_pos = Some(event.position);
                    }

                    let _ = (window, &cx); // suppress unused warnings on non-Windows
                    state.should_move = true;
                    state.drag_start_pos = Some(event.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&state, |state, _, _, _| {
                    state.should_move = false;
                    state.drag_start_pos = None;
                }),
            )
            .on_mouse_move(window.listener_for(&state, |state, _event: &gpui::MouseMoveEvent, window, _| {
                if state.should_move {
                    #[cfg(not(target_os = "windows"))]
                    {
                        state.should_move = false;
                        window.start_window_move();
                    }
                    #[cfg(target_os = "windows")]
                    {
                        // Only start drag after exceeding a movement threshold (4px),
                        // so that double-clicks aren't swallowed by the drag modal loop.
                        if let Some(start) = state.drag_start_pos {
                            let delta = event.position - start;
                            let threshold = px(4.0);
                            if delta.x > threshold
                                || delta.x < -threshold
                                || delta.y > threshold
                                || delta.y < -threshold
                            {
                                state.should_move = false;
                                state.drag_start_pos = None;
                                start_window_move_win32(window);
                            }
                        }
                    }
                }
            }))
            // Double-click to maximize/restore
            // Linux: use GPUI's click_count(). Windows: handled in on_mouse_down above.
            .when(cfg!(target_os = "linux"), |this| {
                this.on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                })
            })
            .when(is_macos, |this| {
                this.on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.titlebar_double_click();
                    }
                })
            })
            // Right-click context menu
            .when(!is_macos, |this| {
                this.on_mouse_down(MouseButton::Right, move |ev, window, _| {
                    window.show_window_menu(ev.position)
                })
            })
            // content_stretch ensures empty space in children is still clickable
            .content_stretch()
            // Children container - plain div, no event handlers
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .overflow_x_hidden()
                    .w_full()
                    .h_full()
                    .when(window.is_fullscreen(), |this| this.pl_3())
                    .children(self.children),
            )
            // Window controls (minimize, maximize, close)
            .child(WindowControls {
                on_close_window: self.on_close_window,
            })
    }
}

/// Toggle between maximized and restored window state on Windows.
///
/// GPUI's `zoom_window()` only maximizes on Windows (calls SW_MAXIMIZE),
/// it does not toggle like the macOS equivalent. This helper checks
/// `is_maximized()` and calls SW_RESTORE or SW_MAXIMIZE accordingly.
#[cfg(target_os = "windows")]
fn toggle_maximize_win32(window: &mut gpui::Window) {
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = window.window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_ref() {
            unsafe {
                use windows::Win32::Foundation::*;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let hwnd = HWND(win32.hwnd.get() as *mut _);
                let cmd = if window.is_maximized() {
                    SW_RESTORE
                } else {
                    SW_MAXIMIZE
                };
                let _ = ShowWindowAsync(hwnd, cmd);
            }
        }
    }
}

/// Send WM_NCLBUTTONDOWN + HTTOP to initiate a top-edge resize on Windows.
///
/// When GPUI dispatches NC mouse events through the element tree, a focusable
/// parent element's auto-focus handler calls `prevent_default()`, which prevents
/// `DefWindowProc` from being called. Since `DefWindowProc` is what initiates
/// the native resize, we must post the message ourselves.
#[cfg(target_os = "windows")]
fn start_top_resize_win32(window: &mut gpui::Window) {
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = window.window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_ref() {
            unsafe {
                use windows::Win32::Foundation::*;
                use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let hwnd = HWND(win32.hwnd.get() as *mut _);
                let _ = ReleaseCapture();
                let _ = PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, WPARAM(HTTOP as usize), LPARAM(0));
            }
        }
    }
}

/// Send WM_NCLBUTTONDOWN + HTCAPTION to initiate a window drag on Windows.
///
/// GPUI's `start_window_move()` is a no-op on Windows, so we post the
/// message directly to the HWND via the Win32 API.
#[cfg(target_os = "windows")]
fn start_window_move_win32(window: &mut gpui::Window) {
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = window.window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_ref() {
            unsafe {
                use windows::Win32::Foundation::*;
                use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let hwnd = HWND(win32.hwnd.get() as *mut _);
                let _ = ReleaseCapture();
                let _ = PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
            }
        }
    }
}
