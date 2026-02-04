// From:
// https://github.com/zed-industries/zed/blob/56daba28d40301ee4c05546fadb691d070b7b2b6/crates/gpui/examples/window_shadow.rs
use gpui::{
    AnyElement, App, Bounds, CursorStyle, Decorations, Edges, HitboxBehavior, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce,
    ResizeEdge, Size, Styled as _, Window, canvas, div, point, prelude::FluentBuilder as _, px,
};

use crate::ActiveTheme;

#[cfg(not(target_os = "linux"))]
const SHADOW_SIZE: Pixels = px(0.0);
#[cfg(target_os = "linux")]
const SHADOW_SIZE: Pixels = px(12.0);
const BORDER_SIZE: Pixels = px(1.0);

/// Default border radius (0 for backwards compatibility)
const DEFAULT_BORDER_RADIUS: Pixels = px(0.0);

/// Create a new window border.
pub fn window_border() -> WindowBorder {
    WindowBorder::new()
}

/// Window border use to render a custom window border and shadow for Linux.
///
/// # Example
///
/// ```rust
/// use gpui_component::window_border;
/// use gpui::px;
///
/// // Default (no rounded corners)
/// window_border().child(my_content);
///
/// // With rounded corners
/// window_border().border_radius(px(10.0)).child(my_content);
/// ```
#[derive(IntoElement)]
pub struct WindowBorder {
    children: Vec<AnyElement>,
    border_radius: Pixels,
}

impl Default for WindowBorder {
    fn default() -> Self {
        Self {
            children: Vec::new(),
            border_radius: DEFAULT_BORDER_RADIUS,
        }
    }
}

impl WindowBorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the border radius for the window corners.
    ///
    /// This controls the rounding of window corners when using client-side decorations.
    /// The radius is only applied to non-tiled edges.
    ///
    /// Default is `px(0.0)` (no rounding) for backwards compatibility.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gpui_component::window_border;
    /// use gpui::px;
    ///
    /// window_border()
    ///     .border_radius(px(10.0))
    ///     .child(my_content);
    /// ```
    pub fn border_radius(mut self, radius: impl Into<Pixels>) -> Self {
        self.border_radius = radius.into();
        self
    }
}

/// Get the window paddings.
pub fn window_paddings(window: &Window) -> Edges<Pixels> {
    match window.window_decorations() {
        Decorations::Server => Edges::all(px(0.0)),
        Decorations::Client { tiling } => {
            let mut paddings = Edges::all(SHADOW_SIZE);
            if tiling.top {
                paddings.top = px(0.0);
            }
            if tiling.bottom {
                paddings.bottom = px(0.0);
            }
            if tiling.left {
                paddings.left = px(0.0);
            }
            if tiling.right {
                paddings.right = px(0.0);
            }
            paddings
        }
    }
}

impl ParentElement for WindowBorder {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for WindowBorder {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let decorations = window.window_decorations();
        let border_radius = self.border_radius;
        window.set_client_inset(SHADOW_SIZE);

        div()
            .id("window-backdrop")
            .bg(gpui::transparent_black())
            .map(|div| match decorations {
                Decorations::Server => div,
                Decorations::Client { tiling, .. } => div
                    .bg(gpui::transparent_black())
                    .child(
                        canvas(
                            |_bounds, window, _| {
                                window.insert_hitbox(
                                    Bounds::new(
                                        point(px(0.0), px(0.0)),
                                        window.window_bounds().get_bounds().size,
                                    ),
                                    HitboxBehavior::Normal,
                                )
                            },
                            move |_bounds, hitbox, window, _| {
                                let mouse = window.mouse_position();
                                let size = window.window_bounds().get_bounds().size;
                                // Use actual shadow sizes based on tiling state
                                let top_shadow = if tiling.top { px(0.0) } else { SHADOW_SIZE };
                                let bottom_shadow = if tiling.bottom { px(0.0) } else { SHADOW_SIZE };
                                let left_shadow = if tiling.left { px(0.0) } else { SHADOW_SIZE };
                                let right_shadow = if tiling.right { px(0.0) } else { SHADOW_SIZE };

                                let Some(edge) = resize_edge_with_insets(
                                    mouse, size, top_shadow, bottom_shadow, left_shadow, right_shadow
                                ) else {
                                    return;
                                };
                                window.set_cursor_style(
                                    match edge {
                                        ResizeEdge::Top | ResizeEdge::Bottom => {
                                            CursorStyle::ResizeUpDown
                                        }
                                        ResizeEdge::Left | ResizeEdge::Right => {
                                            CursorStyle::ResizeLeftRight
                                        }
                                        ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                            CursorStyle::ResizeUpLeftDownRight
                                        }
                                        ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                            CursorStyle::ResizeUpRightDownLeft
                                        }
                                    },
                                    &hitbox,
                                );
                            },
                        )
                        .size_full()
                        .absolute(),
                    )
                    .when(!(tiling.top || tiling.right), |div| {
                        div.rounded_tr(border_radius)
                    })
                    .when(!(tiling.top || tiling.left), |div| {
                        div.rounded_tl(border_radius)
                    })
                    .when(!(tiling.bottom || tiling.right), |div| {
                        div.rounded_br(border_radius)
                    })
                    .when(!(tiling.bottom || tiling.left), |div| {
                        div.rounded_bl(border_radius)
                    })
                    .when(!tiling.top, |div| div.pt(SHADOW_SIZE))
                    .when(!tiling.bottom, |div| div.pb(SHADOW_SIZE))
                    .when(!tiling.left, |div| div.pl(SHADOW_SIZE))
                    .when(!tiling.right, |div| div.pr(SHADOW_SIZE))
                    .on_mouse_down(MouseButton::Left, move |_, window, _| {
                        let size = window.window_bounds().get_bounds().size;
                        let pos = window.mouse_position();

                        // Use actual shadow sizes based on tiling state
                        let top_shadow = if tiling.top { px(0.0) } else { SHADOW_SIZE };
                        let bottom_shadow = if tiling.bottom { px(0.0) } else { SHADOW_SIZE };
                        let left_shadow = if tiling.left { px(0.0) } else { SHADOW_SIZE };
                        let right_shadow = if tiling.right { px(0.0) } else { SHADOW_SIZE };

                        match resize_edge_with_insets(pos, size, top_shadow, bottom_shadow, left_shadow, right_shadow) {
                            Some(edge) => window.start_window_resize(edge),
                            None => {}
                        };
                    }),
            })
            .size_full()
            .child(
                div()
                    .cursor(CursorStyle::default())
                    .map(|div| match decorations {
                        Decorations::Server => div,
                        Decorations::Client { tiling } => div
                            .when(!(tiling.top || tiling.right), |div| {
                                div.rounded_tr(border_radius)
                            })
                            .when(!(tiling.top || tiling.left), |div| {
                                div.rounded_tl(border_radius)
                            })
                            .when(!(tiling.bottom || tiling.right), |div| {
                                div.rounded_br(border_radius)
                            })
                            .when(!(tiling.bottom || tiling.left), |div| {
                                div.rounded_bl(border_radius)
                            })
                            .border_color(cx.theme().window_border)
                            .when(!tiling.top, |div| div.border_t(BORDER_SIZE))
                            .when(!tiling.bottom, |div| div.border_b(BORDER_SIZE))
                            .when(!tiling.left, |div| div.border_l(BORDER_SIZE))
                            .when(!tiling.right, |div| div.border_r(BORDER_SIZE))
                            .when(!tiling.is_tiled(), |div| {
                                div.shadow(vec![gpui::BoxShadow {
                                    color: Hsla {
                                        h: 0.,
                                        s: 0.,
                                        l: 0.,
                                        a: 0.3,
                                    },
                                    blur_radius: SHADOW_SIZE / 2.,
                                    spread_radius: px(0.),
                                    offset: point(px(0.0), px(0.0)),
                                }])
                            }),
                    })
                    .on_mouse_move(|_e, _, cx| {
                        cx.stop_propagation();
                    })
                    .bg(gpui::transparent_black())
                    .size_full()
                    .overflow_hidden()
                    .children(self.children),
            )
    }
}

fn resize_edge_with_insets(
    pos: Point<Pixels>,
    size: Size<Pixels>,
    top: Pixels,
    bottom: Pixels,
    left: Pixels,
    right: Pixels,
) -> Option<ResizeEdge> {
    let in_top = top > px(0.0) && pos.y < top;
    let in_bottom = bottom > px(0.0) && pos.y > size.height - bottom;
    let in_left = left > px(0.0) && pos.x < left;
    let in_right = right > px(0.0) && pos.x > size.width - right;

    let edge = if in_top && in_left {
        ResizeEdge::TopLeft
    } else if in_top && in_right {
        ResizeEdge::TopRight
    } else if in_top {
        ResizeEdge::Top
    } else if in_bottom && in_left {
        ResizeEdge::BottomLeft
    } else if in_bottom && in_right {
        ResizeEdge::BottomRight
    } else if in_bottom {
        ResizeEdge::Bottom
    } else if in_left {
        ResizeEdge::Left
    } else if in_right {
        ResizeEdge::Right
    } else {
        return None;
    };
    Some(edge)
}
