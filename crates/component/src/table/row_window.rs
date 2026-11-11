//! Fork: the vertical position of a windowed table body.
//!
//! A [`TableState`](super::TableState) made [`windowed`](super::TableState::windowed)
//! renders the rows from a top row index instead of through a `uniform_list`.
//! A `uniform_list` keeps its offset as `f32` pixels over every row; at 32 px a
//! row, `f32` stops counting whole pixels past 2²⁴ px, about 524 000 rows, and
//! below that a trackpad's few pixels a frame round away. Here the position is
//! the top row plus the pixels of it scrolled out at the top, the scrollbar sees
//! a fraction of the way through the rows, and the wheel sees only the distance
//! it moves — no pixel count over all rows exists anywhere.

use std::{cell::RefCell, rc::Rc};

use gpui::{Bounds, Pixels, Point, ScrollStrategy, Size, point, px, size};

use crate::scroll::ScrollbarHandle;

/// The tallest content the scrollbar is told about. Up to it the thumb is as
/// long as over real pixels and the offset is the real one; past it the thumb
/// is at its minimum length anyway, and its travel maps onto the rows by
/// fraction. Small enough that `f32` still keeps a sixteenth of a pixel.
const SCROLLBAR_CONTENT_LIMIT: f64 = 1_000_000.;

/// How far the wheel is told it can move from where it is, each way. It only
/// has to hold one event's delta, and stays small enough that `f32` keeps the
/// fractions of a pixel a trackpad sends.
const WHEEL_REACH: f64 = 100_000.;

#[derive(Debug, Default)]
struct RowWindow {
    top_row: usize,
    /// The pixels of `top_row` scrolled out at the top, `0..row_height`.
    row_offset: f64,
    rows_count: usize,
    /// Zero until the first render.
    row_height: f64,
    /// The bounds of the rows below the header; empty until the first prepaint.
    viewport: Bounds<Pixels>,
    /// A scroll asked for before the next render, which knows the row count.
    pending_scroll: Option<(usize, ScrollStrategy)>,
}

impl RowWindow {
    fn viewport_height(&self) -> f64 {
        self.viewport.size.height.to_f64()
    }

    /// Pixels from the top of the first row to the top of the viewport.
    fn position(&self) -> f64 {
        self.top_row as f64 * self.row_height + self.row_offset
    }

    fn max_position(&self) -> f64 {
        (self.rows_count as f64 * self.row_height - self.viewport_height()).max(0.)
    }

    fn set_position(&mut self, position: f64) {
        if self.row_height <= 0. {
            return;
        }
        let position = position.clamp(0., self.max_position());
        let top_row = (position / self.row_height).floor();
        self.top_row = top_row as usize;
        self.row_offset = (position - top_row * self.row_height).clamp(0., self.row_height);
    }

    /// Brings `row_ix` into view as `uniform_list` does: a row already fully
    /// visible stays where it is, any other is placed by `strategy`.
    fn scroll_to_row(&mut self, row_ix: usize, strategy: ScrollStrategy) {
        let row_ix = row_ix.min(self.rows_count.saturating_sub(1));
        let row_top = row_ix as f64 * self.row_height;
        let row_bottom = row_top + self.row_height;
        let position = self.position();
        let viewport_height = self.viewport_height();
        let is_above = row_top < position;
        let is_below = row_bottom > position + viewport_height;
        if !is_above && !is_below {
            return;
        }

        let target = match strategy {
            ScrollStrategy::Top => row_top,
            ScrollStrategy::Center => row_top + self.row_height / 2. - viewport_height / 2.,
            ScrollStrategy::Bottom => row_bottom - viewport_height,
            ScrollStrategy::Nearest if is_above => row_top,
            ScrollStrategy::Nearest => row_bottom - viewport_height,
        };
        self.set_position(target);
    }

    /// Resolves a pending scroll once both the rows and the viewport are known,
    /// else keeps the position inside the rows.
    fn settle(&mut self) {
        if self.row_height <= 0. || self.viewport_height() <= 0. {
            return;
        }
        match self.pending_scroll.take() {
            Some((row_ix, strategy)) => self.scroll_to_row(row_ix, strategy),
            None => self.set_position(self.position()),
        }
    }

    fn scrollbar_content_height(&self) -> f64 {
        (self.rows_count as f64 * self.row_height)
            .min(SCROLLBAR_CONTENT_LIMIT)
            .max(self.viewport_height())
    }

    /// The fraction of the way from the first to the last page, `0..=1`.
    fn fraction(&self) -> f64 {
        let max_position = self.max_position();
        if max_position <= 0. {
            0.
        } else {
            (self.position() / max_position).clamp(0., 1.)
        }
    }

    fn wheel_reach_up(&self) -> f32 {
        self.position().min(WHEEL_REACH) as f32
    }

    fn wheel_reach_down(&self) -> f32 {
        (self.max_position() - self.position()).clamp(0., WHEEL_REACH) as f32
    }
}

/// The vertical position of a windowed table body, see
/// [`TableState::windowed`](super::TableState::windowed).
///
/// As a [`ScrollbarHandle`] it drives the table's vertical scrollbar by
/// fraction; [`Self::wheel`] is the view the wheel scrolls through.
#[derive(Clone, Default)]
pub struct RowWindowHandle(Rc<RefCell<RowWindow>>);

impl RowWindowHandle {
    /// The first row drawn, at least partly visible.
    pub fn top_row(&self) -> usize {
        self.0.borrow().top_row
    }

    /// The pixels of [`Self::top_row`] scrolled out above the viewport.
    pub fn row_offset(&self) -> Pixels {
        px(self.0.borrow().row_offset as f32)
    }

    /// The fraction of the way from the first page to the last, `0..=1`.
    pub fn fraction(&self) -> f64 {
        self.0.borrow().fraction()
    }

    /// Scrolls `row_ix` into view on the next render, as
    /// `UniformListScrollHandle::scroll_to_item` does.
    pub fn scroll_to_row(&self, row_ix: usize, strategy: ScrollStrategy) {
        self.0.borrow_mut().pending_scroll = Some((row_ix, strategy));
    }

    /// The handle the vertical wheel mask moves the rows through.
    pub(super) fn wheel(&self) -> RowWindowWheel {
        RowWindowWheel(self.clone())
    }

    /// Takes the row count and height of this render.
    pub(super) fn lay_out(&self, rows_count: usize, row_height: Pixels) {
        let mut window = self.0.borrow_mut();
        window.rows_count = rows_count;
        let row_height = row_height.to_f64();
        if window.row_height != row_height {
            window.row_offset = 0.;
            window.row_height = row_height;
        }
        window.settle();
    }

    /// Takes the rows' bounds after layout. True when their height changed,
    /// so that the next frame renders as many rows as now fit.
    pub(super) fn set_viewport(&self, bounds: Bounds<Pixels>) -> bool {
        let mut window = self.0.borrow_mut();
        let height_changed = window.viewport.size.height != bounds.size.height;
        window.viewport = bounds;
        if height_changed {
            window.settle();
        }
        height_changed
    }

    /// The viewport height, or `None` before the first layout.
    pub(super) fn viewport_height(&self) -> Option<Pixels> {
        let height = self.0.borrow().viewport.size.height;
        (height > px(0.)).then_some(height)
    }

    fn move_to(&self, position: f64) {
        let mut window = self.0.borrow_mut();
        window.pending_scroll = None;
        window.set_position(position);
    }
}

impl ScrollbarHandle for RowWindowHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        let window = self.0.borrow();
        let span = window.scrollbar_content_height() - window.viewport_height();
        point(px(0.), px(-(window.fraction() * span) as f32))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let target = {
            let window = self.0.borrow();
            let span = window.scrollbar_content_height() - window.viewport_height();
            if span <= 0. {
                return;
            }
            (-offset.y.to_f64() / span).clamp(0., 1.) * window.max_position()
        };
        self.move_to(target);
    }

    fn content_size(&self) -> Size<Pixels> {
        let window = self.0.borrow();
        size(
            window.viewport.size.width,
            px(window.scrollbar_content_height() as f32),
        )
    }
}

/// The wheel's view of a [`RowWindowHandle`]: the content reaches only
/// [`WHEEL_REACH`] each way from the current position, so the distance an
/// event moves is exact however far down the rows are.
#[derive(Clone)]
pub(super) struct RowWindowWheel(RowWindowHandle);

impl ScrollbarHandle for RowWindowWheel {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        point(px(0.), px(-self.0.0.borrow().wheel_reach_up()))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let (position, reported) = {
            let window = self.0.0.borrow();
            (window.position(), -window.wheel_reach_up())
        };
        let moved = f64::from(reported) - offset.y.to_f64();
        if moved != 0. {
            self.0.move_to(position + moved);
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        let window = self.0.0.borrow();
        size(
            window.viewport.size.width,
            window.viewport.size.height
                + px(window.wheel_reach_up())
                + px(window.wheel_reach_down()),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, ScrollStrategy, point, px, size};

    use super::*;

    const ROW_HEIGHT: f32 = 32.;
    const VIEWPORT_HEIGHT: f32 = 320.;

    /// A handle laid out over `rows_count` rows of 32 px, ten of them visible.
    fn laid_out(rows_count: usize) -> RowWindowHandle {
        let handle = RowWindowHandle::default();
        handle.lay_out(rows_count, px(ROW_HEIGHT));
        handle.set_viewport(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(400.), px(VIEWPORT_HEIGHT)),
        });
        handle
    }

    fn scroll(
        handle: &RowWindowHandle,
        rows_count: usize,
        row_ix: usize,
        strategy: ScrollStrategy,
    ) {
        handle.scroll_to_row(row_ix, strategy);
        handle.lay_out(rows_count, px(ROW_HEIGHT));
    }

    #[test]
    fn a_scroll_waits_for_the_render_and_keeps_a_visible_row_in_place() {
        let rows_count = 100;
        let handle = laid_out(rows_count);
        handle.scroll_to_row(50, ScrollStrategy::Top);
        assert_eq!(handle.top_row(), 0, "nothing moves before the render");
        handle.lay_out(rows_count, px(ROW_HEIGHT));
        assert_eq!(handle.top_row(), 50);

        scroll(&handle, rows_count, 55, ScrollStrategy::Top);
        assert_eq!(handle.top_row(), 50, "a visible row does not scroll");
    }

    #[test]
    fn the_strategies_place_the_row_as_a_uniform_list_does() {
        let rows_count = 100;
        let handle = laid_out(rows_count);
        scroll(&handle, rows_count, 40, ScrollStrategy::Bottom);
        assert_eq!(handle.top_row(), 31, "row 40 is the last of ten");
        assert_eq!(handle.row_offset(), px(0.));

        scroll(&handle, rows_count, 70, ScrollStrategy::Center);
        // 70 * 32 + 16 - 160 = 2096 = 65 rows + 16 px.
        assert_eq!(handle.top_row(), 65);
        assert_eq!(handle.row_offset(), px(16.));

        scroll(&handle, rows_count, 10, ScrollStrategy::Nearest);
        assert_eq!(handle.top_row(), 10, "above the view: to the top");
        scroll(&handle, rows_count, 30, ScrollStrategy::Nearest);
        assert_eq!(handle.top_row(), 21, "below the view: to the bottom");
    }

    #[test]
    fn the_last_page_is_the_furthest_it_goes() {
        let rows_count = 100;
        let handle = laid_out(rows_count);
        scroll(&handle, rows_count, usize::MAX, ScrollStrategy::Top);
        assert_eq!(handle.top_row(), 90);
        assert_eq!(handle.fraction(), 1.);

        // Fewer rows (a filter, a reload) pull the window back up.
        handle.lay_out(50, px(ROW_HEIGHT));
        assert_eq!(handle.top_row(), 40);
        // So does a taller viewport.
        handle.set_viewport(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(400.), px(640.)),
        });
        assert_eq!(handle.top_row(), 30);
    }

    #[test]
    fn a_scroll_before_the_first_layout_is_kept_for_it() {
        let handle = RowWindowHandle::default();
        handle.scroll_to_row(500, ScrollStrategy::Top);
        handle.lay_out(1_000, px(ROW_HEIGHT));
        assert_eq!(handle.top_row(), 0, "no viewport yet");
        handle.set_viewport(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(400.), px(VIEWPORT_HEIGHT)),
        });
        assert_eq!(handle.top_row(), 500);
    }

    #[test]
    fn the_scrollbar_offset_is_the_real_one_for_a_short_table() {
        let rows_count = 100;
        let handle = laid_out(rows_count);
        assert_eq!(handle.content_size().height, px(3_200.));
        handle.set_offset(point(px(0.), px(-1_000.)));
        assert_eq!(handle.top_row(), 31);
        assert_eq!(handle.row_offset(), px(8.));
        assert_eq!(handle.offset().y, px(-1_000.));
    }

    #[test]
    fn the_scrollbar_moves_ten_million_rows_by_fraction() {
        let rows_count = 10_000_000;
        let handle = laid_out(rows_count);
        let content = handle.content_size().height;
        assert_eq!(content, px(SCROLLBAR_CONTENT_LIMIT as f32));
        let span = content - px(VIEWPORT_HEIGHT);

        handle.set_offset(point(px(0.), -span / 2.));
        // Half of the way to the last page, rows_count - 10.
        assert_eq!(handle.top_row(), 4_999_995);
        assert!((handle.fraction() - 0.5).abs() < 1e-9);

        handle.set_offset(point(px(0.), -span));
        assert_eq!(handle.top_row(), rows_count - 10);
        assert_eq!(handle.offset().y, -span);

        handle.set_offset(point(px(0.), px(10.)));
        assert_eq!(handle.top_row(), 0, "past the top is the top");
    }

    /// What the vertical `ScrollableMask` does with a wheel event.
    fn wheel(handle: &RowWindowHandle, delta_y: f32) -> bool {
        let wheel = handle.wheel();
        let axis_max =
            (wheel.content_size().height - wheel.viewport_bounds().size.height).max(px(0.));
        let current = wheel.offset().y.clamp(-axis_max, px(0.));
        let new_offset = (current + px(delta_y)).clamp(-axis_max, px(0.));
        if new_offset == current {
            return false;
        }
        wheel.set_offset(point(px(0.), new_offset));
        true
    }

    #[test]
    fn the_wheel_moves_exactly_however_far_down_it_is() {
        let rows_count = 10_000_000;
        let handle = laid_out(rows_count);
        scroll(&handle, rows_count, 9_000_000, ScrollStrategy::Top);
        assert_eq!(handle.top_row(), 9_000_000);

        // A trackpad's half pixel a frame, which `f32` pixels over all rows lose.
        for _ in 0..64 {
            assert!(wheel(&handle, -0.5));
        }
        assert_eq!(handle.top_row(), 9_000_001);
        assert_eq!(handle.row_offset(), px(0.));

        assert!(wheel(&handle, 48.));
        assert_eq!(handle.top_row(), 8_999_999);
        assert_eq!(handle.row_offset(), px(16.));
    }

    #[test]
    fn the_wheel_hands_over_at_both_ends() {
        let rows_count = 100;
        let handle = laid_out(rows_count);
        assert!(
            !wheel(&handle, 40.),
            "at the top a wheel up goes to the parent"
        );
        assert!(wheel(&handle, -100_000.));
        assert_eq!(handle.top_row(), 90);
        assert!(
            !wheel(&handle, -40.),
            "at the bottom a wheel down goes to the parent"
        );
    }
}
