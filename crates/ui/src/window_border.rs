// From:
// https://github.com/zed-industries/zed/blob/56daba28d40301ee4c05546fadb691d070b7b2b6/crates/gpui/examples/window_shadow.rs
use gpui::{
    AnyElement, App, Bounds, CursorStyle, Decorations, Edges, HitboxBehavior, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce,
    ResizeEdge, Size, Styled as _, Window, canvas, div, point, prelude::FluentBuilder as _, px,
};

use crate::ActiveTheme;

#[cfg(not(target_os = "linux"))]
pub(crate) const SHADOW_SIZE: Pixels = px(0.0);
#[cfg(target_os = "linux")]
pub(crate) const SHADOW_SIZE: Pixels = px(12.0);
const BORDER_SIZE: Pixels = px(1.0);
pub(crate) const BORDER_RADIUS: Pixels = px(0.0);

/// Create a new window border.
pub fn window_border() -> WindowBorder {
    WindowBorder::new()
}

/// Window border use to render a custom window border and shadow for Linux.
#[derive(IntoElement)]
pub struct WindowBorder {
    shadow_size: Pixels,
    border_radius: Pixels,
    children: Vec<AnyElement>,
}

impl Default for WindowBorder {
    fn default() -> Self {
        Self {
            shadow_size: SHADOW_SIZE,
            border_radius: BORDER_RADIUS,
            children: Vec::new(),
        }
    }
}

impl WindowBorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the shadow size for typical Linux client-side decorations.
    ///
    /// Default: [`SHADOW_SIZE`]
    pub fn shadow_size(mut self, size: impl Into<Pixels>) -> Self {
        self.shadow_size = size.into();
        self
    }

    /// Set the border radius for the window corners.
    ///
    /// Default: [`BORDER_RADIUS`]
    pub fn border_radius(mut self, radius: impl Into<Pixels>) -> Self {
        self.border_radius = radius.into();
        self
    }
}

/// Get the window paddings.
pub fn window_paddings(window: &Window) -> Edges<Pixels> {
    let shadow_size = window.client_inset().unwrap_or(SHADOW_SIZE);
    match window.window_decorations() {
        Decorations::Server => Edges::all(px(0.0)),
        Decorations::Client { tiling } => {
            let mut paddings = Edges::all(shadow_size);
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
        let shadow_size = match decorations {
            Decorations::Client { tiling }
                if tiling.top && tiling.bottom && tiling.left && tiling.right =>
            {
                px(0.0)
            }
            _ => self.shadow_size,
        };
        window.set_client_inset(shadow_size);

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
                                let Decorations::Client { tiling } = window.window_decorations() else {
                                    return;
                                };
                                if tiling.top && tiling.bottom && tiling.left && tiling.right {
                                    return;
                                }
                                let Some(edge) = resize_edge(mouse, shadow_size, size) else {
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
                    .when(!tiling.top, |div| div.pt(shadow_size))
                    .when(!tiling.bottom, |div| div.pb(shadow_size))
                    .when(!tiling.left, |div| div.pl(shadow_size))
                    .when(!tiling.right, |div| div.pr(shadow_size))
                    .on_mouse_move(|_e, window, _cx| window.refresh())
                    .on_mouse_down(MouseButton::Left, move |_, window, _| {
                        let Decorations::Client { tiling } = window.window_decorations() else {
                            return;
                        };
                        if tiling.top && tiling.bottom && tiling.left && tiling.right {
                            return;
                        }
                        let size = window.window_bounds().get_bounds().size;
                        let pos = window.mouse_position();

                        match resize_edge(pos, shadow_size, size) {
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
                                    blur_radius: shadow_size / 2.,
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
                    .children(self.children),
            )
    }
}

fn resize_edge(pos: Point<Pixels>, shadow_size: Pixels, size: Size<Pixels>) -> Option<ResizeEdge> {
    // Corner zones extend further along each edge than the shadow area itself,
    // making corners much easier to grab (similar to GNOME/libadwaita behavior).
    let corner_size = shadow_size * 3.0;

    let near_top = pos.y < shadow_size;
    let near_bottom = pos.y > size.height - shadow_size;
    let near_left = pos.x < shadow_size;
    let near_right = pos.x > size.width - shadow_size;

    let edge = if near_top && pos.x < corner_size {
        ResizeEdge::TopLeft
    } else if near_top && pos.x > size.width - corner_size {
        ResizeEdge::TopRight
    } else if near_left && pos.y < corner_size {
        ResizeEdge::TopLeft
    } else if near_right && pos.y < corner_size {
        ResizeEdge::TopRight
    } else if near_bottom && pos.x < corner_size {
        ResizeEdge::BottomLeft
    } else if near_bottom && pos.x > size.width - corner_size {
        ResizeEdge::BottomRight
    } else if near_left && pos.y > size.height - corner_size {
        ResizeEdge::BottomLeft
    } else if near_right && pos.y > size.height - corner_size {
        ResizeEdge::BottomRight
    } else if near_top {
        ResizeEdge::Top
    } else if near_bottom {
        ResizeEdge::Bottom
    } else if near_left {
        ResizeEdge::Left
    } else if near_right {
        ResizeEdge::Right
    } else {
        return None;
    };
    Some(edge)
}
