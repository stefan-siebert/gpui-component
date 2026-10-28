use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window,
    prelude::FluentBuilder as _,
};
use smallvec::SmallVec;

use crate::{ActiveTheme, StyledExt, h_flex};

/// A horizontal status bar, usually placed at the bottom of a window or pane.
///
/// It is split into three regions — `left`, `center`, and `right`. This mirrors
/// the status bars found in native UI frameworks (Windows `StatusStrip`, WPF
/// `StatusBar`, macOS `NSStatusBar`): a container that holds a row of items
/// aligned to either end.
///
/// Each region accepts any [`IntoElement`], so a string, an [`Icon`](crate::Icon),
/// a ghost `Button`, a vertical `Separator`, a custom layout, etc. can be passed
/// directly. Use a plain string for a non-interactive label.
///
/// `left` and `right` pin items to each end. `child`/`children` add to the
/// center region, whose alignment follows the pinned ends: centered with both
/// `left` and `right`, end-aligned with only `left`, and start-aligned
/// otherwise (only `right`, or neither — like a plain container).
///
/// When the bar is too narrow, the `right` region keeps its width and the
/// others give way: a long message on the left is cut, the indicators at the
/// right end stay whole.
///
/// ```
/// # mod gpui_kit { pub extern crate gpui_component as component; }
/// use gpui_kit::component::status_bar::StatusBar;
///
/// let _ = StatusBar::new().left("Ln 1, Col 1").right("UTF-8");
/// ```
#[derive(IntoElement)]
pub struct StatusBar {
    style: StyleRefinement,
    left: SmallVec<[AnyElement; 1]>,
    right: SmallVec<[AnyElement; 1]>,
    children: SmallVec<[AnyElement; 1]>,
}

impl StatusBar {
    /// Create a new, empty [`StatusBar`].
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            left: SmallVec::new(),
            right: SmallVec::new(),
            children: SmallVec::new(),
        }
    }

    /// Append an element to the left region. Call multiple times to add more.
    pub fn left(mut self, child: impl IntoElement) -> Self {
        self.left.push(child.into_any_element());
        self
    }

    /// Append an element to the right region. Call multiple times to add more.
    pub fn right(mut self, child: impl IntoElement) -> Self {
        self.right.push(child.into_any_element());
        self
    }
}

/// `child` / `children` add to the center region, so a `StatusBar` without
/// `left`/`right` items behaves like a plain container.
impl ParentElement for StatusBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for StatusBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for StatusBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // The center aligns by which ends are pinned: centered with both left
        // and right, end-aligned with only left, otherwise start-aligned (only
        // right, or neither) — so a bar with just `child`s reads like a container.
        let has_left = !self.left.is_empty();
        let has_right = !self.right.is_empty();
        let region = || h_flex().overflow_hidden().items_center().gap_2();

        h_flex()
            .items_center()
            .gap_2()
            .py_1()
            .px_2()
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .bg(cx.theme().tokens.status_bar)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .refine_style(&self.style)
            .when(has_left, |this| this.child(region().children(self.left)))
            .child(
                region()
                    .flex_1()
                    .when(has_left && has_right, |this| this.justify_center())
                    .when(has_left && !has_right, |this| this.justify_end())
                    .children(self.children),
            )
            .when(has_right, |this| {
                this.child(region().flex_shrink_0().children(self.right))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AvailableSpace, Bounds, IntoElement as _, ParentElement as _, Pixels, Styled as _,
        TestAppContext, div, point, px, size,
    };

    use super::StatusBar;

    #[gpui::test]
    fn a_long_left_side_gives_way_to_the_right_side(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let cx = cx.add_empty_window();
        let right_item: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let record = right_item.clone();
        cx.draw(
            point(px(0.), px(0.)),
            size(
                AvailableSpace::Definite(px(200.)),
                AvailableSpace::MinContent,
            ),
            move |_, _| {
                StatusBar::new()
                    .w(px(200.))
                    .left(div().w(px(400.)).h(px(10.)))
                    .right(
                        div()
                            .on_children_prepainted(move |bounds, _, _| {
                                record.set(bounds.first().copied())
                            })
                            .child(div().w(px(80.)).h(px(10.))),
                    )
                    .into_any_element()
            },
        );
        let right_item = right_item.get().unwrap();
        assert_eq!(right_item.size.width, px(80.));
        assert!(
            right_item.right() <= px(200.),
            "the right item ends at {:?}, past the bar's edge",
            right_item.right()
        );
    }
}
