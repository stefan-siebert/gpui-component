use std::ops::Range;

use gpui::{
    Along, App, Axis, Bounds, Context, ElementId, EventEmitter, IsZero, Pixels, Window, px,
};

mod panel;
mod resize_handle;
pub use panel::*;
#[doc(hidden)]
pub use resize_handle::*;

#[doc(hidden)]
pub const PANEL_MIN_SIZE: Pixels = px(100.);

/// Create a [`ResizablePanelGroup`] with horizontal resizing
pub fn h_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    ResizablePanelGroup::new(id).axis(Axis::Horizontal)
}

/// Create a [`ResizablePanelGroup`] with vertical resizing
pub fn v_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    ResizablePanelGroup::new(id).axis(Axis::Vertical)
}

/// Create a [`ResizablePanel`].
pub fn resizable_panel() -> ResizablePanel {
    ResizablePanel::new()
}

/// State for a [`ResizablePanel`]
#[derive(Debug, Clone)]
pub struct ResizableState {
    /// The `axis` will sync to actual axis of the ResizablePanelGroup in use.
    axis: Axis,
    panels: Vec<ResizablePanelState>,
    sizes: Vec<Pixels>,
    resizing_panel_ix: Option<usize>,
    bounds: Bounds<Pixels>,
}

impl Default for ResizableState {
    fn default() -> Self {
        Self {
            axis: Axis::Horizontal,
            panels: vec![],
            sizes: vec![],
            resizing_panel_ix: None,
            bounds: Bounds::default(),
        }
    }
}

impl ResizableState {
    /// Get the size of the panels.
    pub fn sizes(&self) -> &Vec<Pixels> {
        &self.sizes
    }

    /// Whether one of the group's handles is being dragged right now.
    ///
    /// A host that draws something for the duration of a drag — a size
    /// readout, a highlighted bar — should ask this instead of inferring a
    /// drag from the sizes changing: they change on a window resize too.
    pub fn is_resizing(&self) -> bool {
        self.resizing_panel_ix.is_some()
    }

    /// Programmatically resize the panel at `ix` to `size`, redistributing
    /// space among siblings using the same logic as a drag.
    ///
    /// Sizes are clamped to the panel's `size_range` and to the container.
    /// Emits `ResizablePanelEvent::Resized` so subscribers (e.g. preference
    /// persistence) see the change just as if the user had dragged a handle.
    ///
    /// Out-of-range indices are a no-op. For the last panel, space is taken
    /// from the previous sibling (the last panel has no handle of its own).
    pub fn resize_panel(
        &mut self,
        ix: usize,
        size: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.sizes.len() {
            return;
        }
        if ix + 1 < self.sizes.len() {
            self.resize_panel_at_handle(ix, size, window, cx);
        } else if ix > 0 {
            // Last panel: drive its size by resizing the previous sibling so
            // the freed space lands here.
            let delta = self.sizes[ix] - size;
            let prev = self.sizes[ix - 1];
            self.resize_panel_at_handle(ix - 1, prev + delta, window, cx);
        }
        self.done_resizing(cx);
    }

    /// Insert a panel state at `ix`, or append it when no index is supplied.
    ///
    /// Existing panel sizes are redistributed so their total remains equal to
    /// the current container size.
    pub fn insert_panel(
        &mut self,
        size: Option<Pixels>,
        ix: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let panel_state = ResizablePanelState {
            size,
            ..Default::default()
        };

        let size = size.unwrap_or(PANEL_MIN_SIZE);

        // We make sure that the size always sums up to the container size
        // by reducing the size of all other panels first.
        let container_size = self.container_size().max(px(1.));
        let total_leftover_size = (container_size - size).max(px(1.));

        for (i, panel) in self.panels.iter_mut().enumerate() {
            let ratio = self.sizes[i] / container_size;
            self.sizes[i] = total_leftover_size * ratio;
            panel.size = Some(self.sizes[i]);
        }

        if let Some(ix) = ix {
            self.panels.insert(ix, panel_state);
            self.sizes.insert(ix, size);
        } else {
            self.panels.push(panel_state);
            self.sizes.push(size);
        };

        cx.notify();
    }

    /// Adopt slot sizes decided by an owner that keeps its own record of the
    /// layout — the dock's pane tree does.
    ///
    /// Unlike [`Self::insert_panel`], nothing is redistributed: the caller has
    /// already decided how the space divides, and re-normalizing here would
    /// undo exactly that decision. Slots the caller left unconstrained keep
    /// whatever they had.
    pub(crate) fn adopt_sizes(&mut self, sizes: &[Option<Pixels>], cx: &mut Context<Self>) {
        let mut changed = false;
        for (ix, size) in sizes.iter().enumerate() {
            // The preference is mirrored exactly, `None` included. That is the
            // load-bearing half: `insert_panel` resolves every existing
            // panel's `None` into a concrete value as a side effect of
            // redistributing, so after inserting one slot the caller's "these
            // two are equally unconstrained" has quietly become "that one is
            // pinned, this one is the only flexible slot" — and the flexible
            // one then swallows whatever the pinned ones leave over.
            if let Some(panel) = self.panels.get_mut(ix) {
                if panel.size != *size {
                    panel.size = *size;
                    changed = true;
                }
            }

            // The measurement only moves when the tree names a size; an
            // unconstrained slot keeps whatever it was last laid out at until
            // the next pass recomputes it.
            let Some(size) = size else { continue };
            if let Some(slot) = self.sizes.get_mut(ix) {
                if *slot != *size {
                    *slot = *size;
                    changed = true;
                }
            }
        }

        if changed {
            cx.notify();
        }
    }

    pub(crate) fn sync_panels_count(
        &mut self,
        axis: Axis,
        panels_count: usize,
        cx: &mut Context<Self>,
    ) {
        let mut changed = self.axis != axis;
        self.axis = axis;

        if panels_count > self.panels.len() {
            let diff = panels_count - self.panels.len();
            self.panels
                .extend(vec![ResizablePanelState::default(); diff]);
            self.sizes.extend(vec![PANEL_MIN_SIZE; diff]);
            changed = true;
        }

        if panels_count < self.panels.len() {
            self.panels.truncate(panels_count);
            self.sizes.truncate(panels_count);
            changed = true;
        }

        if changed {
            // We need to make sure the total size is in line with the container size.
            self.adjust_to_container_size(cx);
        }
    }

    pub(crate) fn update_panel_size(
        &mut self,
        panel_ix: usize,
        bounds: Bounds<Pixels>,
        size_range: Range<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let size = bounds.size.along(self.axis);

        if self.sizes[panel_ix].as_f32() == PANEL_MIN_SIZE.as_f32() {
            // First real layout: adopt Taffy's computed size as our basis.
            self.sizes[panel_ix] = size;
            self.panels[panel_ix].size = Some(size);
            cx.notify();
        } else if self.resizing_panel_ix.is_some() {
            // Active splitter drag: sync sizes from Taffy bounds so the drag
            // calculation in resize_panel() has accurate values.
            self.sizes[panel_ix] = size;
            self.panels[panel_ix].size = Some(size);
        }
        // When NOT dragging, keep the stored sizes (flex_basis) unchanged.
        // Taffy's integer rounding gives the first panel an extra pixel on
        // odd container widths. Feeding that back as flex_basis causes
        // cumulative drift on every window resize frame.

        // Always update bounds (used for drag hit-testing and positioning).
        self.panels[panel_ix].bounds = bounds;
        self.panels[panel_ix].size_range = size_range;
    }

    /// Remove the panel at `panel_ix` and redistribute the remaining space.
    pub fn remove_panel(&mut self, panel_ix: usize, cx: &mut Context<Self>) {
        self.panels.remove(panel_ix);
        self.sizes.remove(panel_ix);
        if let Some(resizing_panel_ix) = self.resizing_panel_ix {
            if resizing_panel_ix > panel_ix {
                self.resizing_panel_ix = Some(resizing_panel_ix - 1);
            }
        }
        self.adjust_to_container_size(cx);
    }

    /// Reset the panel at `panel_ix` while preserving its current size.
    pub fn reset_panel(&mut self, panel_ix: usize, cx: &mut Context<Self>) {
        let old_size = self.sizes[panel_ix];

        self.panels[panel_ix] = ResizablePanelState::default();
        self.sizes[panel_ix] = old_size;
        self.adjust_to_container_size(cx);
    }

    /// Remove all panel state.
    pub fn clear(&mut self) {
        self.panels.clear();
        self.sizes.clear();
    }

    /// The container's extent along the group's axis. Zero until the group has
    /// laid out once.
    #[inline]
    pub fn container_size(&self) -> Pixels {
        self.bounds.size.along(self.axis)
    }

    pub(crate) fn done_resizing(&mut self, cx: &mut Context<Self>) {
        self.resizing_panel_ix = None;
        cx.emit(ResizablePanelEvent::Resized);
    }

    fn panel_size_range(&self, ix: usize) -> Range<Pixels> {
        let Some(panel) = self.panels.get(ix) else {
            return PANEL_MIN_SIZE..Pixels::MAX;
        };

        panel.size_range.clone()
    }

    fn sync_real_panel_sizes(&mut self, _: &App) {
        for (i, panel) in self.panels.iter().enumerate() {
            self.sizes[i] = panel.bounds.size.along(self.axis);
        }
    }

    /// Resize the panel at `ix` by treating `ix` as the drag-handle position
    /// (the handle that sits between panel `ix` and panel `ix + 1`). Returns
    /// early on the last panel since there is no handle below it.
    ///
    /// This is the worker behind drag interactions and the public
    /// [`Self::resize_panel`] API.
    fn resize_panel_at_handle(
        &mut self,
        ix: usize,
        size: Pixels,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old_sizes = self.sizes.clone();

        let mut ix = ix;
        // Only resize the left panels.
        if ix >= old_sizes.len() - 1 {
            return;
        }
        let container_size = self.container_size();
        self.sync_real_panel_sizes(cx);

        let move_changed = size - old_sizes[ix];
        if move_changed == px(0.) {
            return;
        }

        let size_range = self.panel_size_range(ix);
        let new_size = size.clamp(size_range.start, size_range.end);
        let is_expand = move_changed > px(0.);

        let main_ix = ix;
        let mut new_sizes = old_sizes.clone();

        if is_expand {
            let mut changed = new_size - old_sizes[ix];
            new_sizes[ix] = new_size;

            while changed > px(0.) && ix < old_sizes.len() - 1 {
                ix += 1;
                let size_range = self.panel_size_range(ix);
                let available_size = (new_sizes[ix] - size_range.start).max(px(0.));
                let to_reduce = changed.min(available_size);
                new_sizes[ix] -= to_reduce;
                changed -= to_reduce;
            }
        } else {
            let mut changed = new_size - size;
            new_sizes[ix] = new_size;

            while changed > px(0.) && ix > 0 {
                ix -= 1;
                let size_range = self.panel_size_range(ix);
                let available_size = (new_sizes[ix] - size_range.start).max(px(0.));
                let to_reduce = changed.min(available_size);
                changed -= to_reduce;
                new_sizes[ix] -= to_reduce;
            }

            new_sizes[main_ix + 1] += old_sizes[main_ix] - size - changed;
        }

        // If total size exceeds container size, adjust the main panel
        let total_size: Pixels = new_sizes.iter().map(|s| s.as_f32()).sum::<f32>().into();
        if total_size > container_size {
            let overflow = total_size - container_size;
            new_sizes[main_ix] = (new_sizes[main_ix] - overflow).max(size_range.start);
        }

        for (i, _) in old_sizes.iter().enumerate() {
            let size = new_sizes[i];
            self.panels[i].size = Some(size);
        }
        self.sizes = new_sizes;
        cx.notify();
    }

    /// Record the group's container bounds, re-deriving the panel sizes when
    /// the container itself changed size.
    ///
    /// The stored sizes are absolute pixels, so without this a panel keeps the
    /// size it was dragged to while the window grows around it: the flex pass
    /// hands the extra space to every `flex_grow` panel in equal parts, and a
    /// 70/30 split walks towards 50/50 (a small panel in a large window ends up
    /// a sliver of what its fraction says). Nothing else re-derives the
    /// fractions, because from the group's point of view nothing was dragged.
    ///
    /// It is deliberately *only* a real size change. Proportional rescaling and
    /// Taffy's flex pass are two different algorithms; feeding one back into the
    /// other on every steady-state prepaint drifts by Taffy's rounding and
    /// flickers at flex boundaries (a panel's `min_size` threshold). A frame
    /// where the container did not move must not touch the sizes at all.
    ///
    /// Returns whether the sizes were re-derived, i.e. whether the frame being
    /// drawn is stale and a settling frame has to follow.
    pub(crate) fn set_container_bounds(
        &mut self,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous = self.container_size();
        self.bounds = bounds;
        let current = self.container_size();

        // Sub-pixel jitter is not a resize. The first layout is, though —
        // `previous` is zero there and the described sizes still have to be
        // scaled to the window, which is what upstream's dock test
        // `a_dumped_split_writes_the_sizes_it_is_actually_drawn_at` pins.
        if (current - previous).abs() < px(1.) {
            return false;
        }
        // A drag in flight computes the sizes itself, from the bounds Taffy
        // just produced — leave them alone.
        if self.resizing_panel_ix.is_some() {
            return false;
        }

        self.adjust_to_container_size(cx);
        true
    }

    /// Adjust panel sizes according to the container size.
    ///
    /// When the container size changes, the panels should take up the same percentage as they did before.
    fn adjust_to_container_size(&mut self, cx: &mut Context<Self>) {
        if self.container_size().is_zero() {
            return;
        }

        // A panel with no size preference is laid out by flex, and its entry
        // in `sizes` is a placeholder until something measures it. Rescaling
        // by a ratio computed from that placeholder drags the panels that
        // *do* have a preference along with it: a 200px sidebar beside one
        // flexible panel comes back 587px wide on the frame after the first,
        // which reads as the layout jumping once for no reason. Flex already
        // fits the container, so there is nothing here to adjust.
        if self.panels.iter().any(|panel| panel.size.is_none()) {
            return;
        }

        let container_size = self.container_size();
        let total = self.sizes.iter().map(|s| s.as_f32()).sum::<f32>();
        if !total.is_finite() || total <= 0. {
            return;
        }
        let total_size = px(total);

        for i in 0..self.panels.len() {
            let size = self.sizes[i];
            let ratio = size / total_size;
            let new_size = container_size * ratio;
            // Keep the stored size inside the panel's own range. Rendering
            // clamps anyway (`min_h`/`max_h` plus a clamped `flex_basis`), so
            // an out-of-range value would not show up now — it would show up
            // one resize later, as a ratio taken from a size the panel never
            // had. A range is only meaningful once the panel has laid out
            // once; before that it is `0..0`.
            let range = &self.panels[i].size_range;
            let new_size = if range.end > range.start {
                new_size.clamp(range.start, range.end)
            } else {
                new_size
            };

            self.sizes[i] = new_size;
            self.panels[i].size = Some(new_size);
        }
        cx.notify();
    }
}

impl EventEmitter<ResizablePanelEvent> for ResizableState {}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResizablePanelState {
    pub size: Option<Pixels>,
    pub size_range: Range<Pixels>,
    bounds: Bounds<Pixels>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Entity, TestAppContext, point, size};

    fn container(height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(400.), px(height)),
        }
    }

    /// A two-panel vertical group that has laid out once: `first` and `second`
    /// are the heights Taffy reported for the panels.
    fn laid_out(
        cx: &mut TestAppContext,
        height: f32,
        first: f32,
        second: f32,
    ) -> Entity<ResizableState> {
        let state = cx.new(|_| ResizableState::default());
        state.update(cx, |state, cx| {
            state.sync_panels_count(Axis::Vertical, 2, cx);
            state.set_container_bounds(container(height), cx);
            state.update_panel_size(0, container(first), px(50.)..px(10000.), cx);
            state.update_panel_size(1, container(second), px(50.)..px(10000.), cx);
        });
        state
    }

    #[gpui::test]
    fn container_growth_keeps_the_panel_proportions(cx: &mut TestAppContext) {
        // 25 % of a 600 px column.
        let state = laid_out(cx, 600., 450., 150.);

        state.update(cx, |state, cx| {
            state.set_container_bounds(container(1200.), cx);
            assert_eq!(state.sizes(), &vec![px(900.), px(300.)]);
        });
    }

    #[gpui::test]
    fn container_shrink_keeps_the_panel_proportions(cx: &mut TestAppContext) {
        let state = laid_out(cx, 600., 450., 150.);

        state.update(cx, |state, cx| {
            state.set_container_bounds(container(300.), cx);
            assert_eq!(state.sizes(), &vec![px(225.), px(75.)]);
        });
    }

    #[gpui::test]
    fn a_frame_without_a_size_change_leaves_the_sizes_alone(cx: &mut TestAppContext) {
        let state = laid_out(cx, 600., 450., 150.);

        state.update(cx, |state, cx| {
            // Same height, and sub-pixel jitter: neither is a resize. Rescaling
            // here would fight Taffy's own rounding, one prepaint at a time.
            state.set_container_bounds(container(600.), cx);
            state.set_container_bounds(container(600.4), cx);
            assert_eq!(state.sizes(), &vec![px(450.), px(150.)]);
        });
    }

    #[gpui::test]
    fn a_panel_never_shrinks_below_its_range(cx: &mut TestAppContext) {
        let state = cx.new(|_| ResizableState::default());
        state.update(cx, |state, cx| {
            state.sync_panels_count(Axis::Vertical, 2, cx);
            state.set_container_bounds(container(600.), cx);
            state.update_panel_size(0, container(450.), px(50.)..px(10000.), cx);
            state.update_panel_size(1, container(150.), px(120.)..px(10000.), cx);

            // A quarter of 200 px is below the pane's 120 px minimum. The
            // stored size must not go there: rendering would clamp it anyway,
            // and the next resize would take its ratio from a height the panel
            // never had.
            state.set_container_bounds(container(200.), cx);
            assert_eq!(state.sizes(), &vec![px(150.), px(120.)]);
        });
    }
}

/// Upstream's own resizable tests. They live in a module of their own so
/// their imports — a full window harness — do not collide with the
/// container-proportion tests above, which drive `ResizableState` directly.
#[cfg(test)]
mod upstream_tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use gpui::{
        App, AppContext as _, Context, InteractiveElement as _, IntoElement, Modifiers,
        MouseButton, ParentElement as _, Pixels, Render, Styled as _, TestAppContext,
        VisualTestContext, Window, div, point, prelude::FluentBuilder as _, px, size,
    };

    use super::{
        ResizableState, ResizeHandleContext, ResizeHandleState, h_resizable, resizable_panel,
    };

    struct MixedSizingHarness {
        width: Pixels,
    }

    impl Render for MixedSizingHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.width).h(px(100.)).child(
                h_resizable("mixed-sizing")
                    .child(
                        resizable_panel()
                            .size(px(240.))
                            .child(div().size_full().debug_selector(|| "fixed-sidebar".into())),
                    )
                    .child(
                        resizable_panel().child(
                            div()
                                .size_full()
                                .debug_selector(|| "flexible-content".into()),
                        ),
                    ),
            )
        }
    }

    #[gpui::test]
    fn mixed_sizing_is_stable_between_resize_and_followup_frame(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| MixedSizingHarness { width: px(800.) });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        let before = cx.debug_bounds("fixed-sidebar").unwrap().size.width;

        view.update(cx, |view, cx| {
            view.width = px(1200.);
            cx.notify();
        });
        cx.run_until_parked();
        let settled_frame = cx.debug_bounds("fixed-sidebar").unwrap().size.width;
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let followup_frame = cx.debug_bounds("fixed-sidebar").unwrap().size.width;

        // Resizable panels preserve their proportional sizing across a
        // container resize; the important invariant is that applying the
        // state on the follow-up frame does not move the divider again.
        assert_ne!(settled_frame, before);
        assert_eq!(followup_frame, settled_frame);
    }

    struct CallerStateHarness {
        width: Pixels,
        state: gpui::Entity<ResizableState>,
    }

    impl Render for CallerStateHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.width).h(px(100.)).child(
                h_resizable("caller-state")
                    .with_state(&self.state)
                    .child(
                        resizable_panel()
                            .size(px(240.))
                            .child(div().size_full().debug_selector(|| "cs-sidebar".into())),
                    )
                    .child(resizable_panel().child(div().size_full())),
            )
        }
    }

    /// A group whose state the caller owns (`with_state`, as the dock does)
    /// has no `use_keyed_state` observer behind it, so the settling frame has
    /// to be scheduled by the deferred notify rather than by that observer.
    #[gpui::test]
    fn caller_owned_state_settles_on_the_same_frame(cx: &mut TestAppContext) {
        let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
        let (view, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| CallerStateHarness {
                width: px(800.),
                state,
            }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });

        view.update(cx, |view, cx| {
            view.width = px(1200.);
            cx.notify();
        });
        cx.run_until_parked();
        let settled = cx.debug_bounds("cs-sidebar").unwrap().size.width;
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let followup = cx.debug_bounds("cs-sidebar").unwrap().size.width;

        assert_eq!(followup, settled, "settling frame must not be pending");
    }

    struct ResizableHarness {
        state: gpui::Entity<ResizableState>,
        resizes: Rc<Cell<usize>>,
    }

    impl Render for ResizableHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(400.)).h(px(100.)).child(
                h_resizable("resizable")
                    .with_state(&self.state)
                    .on_resize({
                        let resizes = self.resizes.clone();
                        move |_, _, _| resizes.set(resizes.get() + 1)
                    })
                    .child(
                        resizable_panel()
                            .size(px(150.))
                            .child(div().size_full().debug_selector(|| "first-panel".into())),
                    )
                    .child(
                        resizable_panel()
                            .size(px(250.))
                            .child(div().size_full().debug_selector(|| "second-panel".into())),
                    ),
            )
        }
    }

    fn harness(
        cx: &mut TestAppContext,
    ) -> (
        &mut VisualTestContext,
        gpui::Entity<ResizableState>,
        Rc<Cell<usize>>,
    ) {
        let state = cx.update(|cx| cx.new(|_| ResizableState::default()));
        let resizes = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            let resizes = resizes.clone();
            move |_, _| ResizableHarness { state, resizes }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, state, resizes)
    }

    #[gpui::test]
    fn dynamic_panel_lifecycle_is_owned_by_resizable_state(cx: &mut TestAppContext) {
        let state = cx.update(|cx| cx.new(|_| ResizableState::default()));

        cx.update(|cx| {
            state.update(cx, |state, cx| {
                state.bounds.size = size(px(400.), px(100.));
                state.panels.push(Default::default());
                state.sizes.push(px(400.));
                state.insert_panel(Some(px(200.)), None, cx);
                assert_eq!(state.sizes(), &vec![px(200.), px(200.)]);

                state.reset_panel(0, cx);
                assert_eq!(state.sizes(), &vec![px(200.), px(200.)]);

                state.remove_panel(0, cx);
                assert_eq!(state.sizes(), &vec![px(400.)]);

                state.clear();
                assert!(state.sizes().is_empty());
            });
        });
    }

    #[gpui::test]
    fn group_measures_panels_and_programmatic_resize_uses_drag_rules(cx: &mut TestAppContext) {
        let (cx, state, _) = harness(cx);
        let first = cx.debug_bounds("first-panel").unwrap();
        let second = cx.debug_bounds("second-panel").unwrap();
        assert_eq!(first.size.width + second.size.width, px(400.));

        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.resize_panel(0, px(220.), window, cx);
            });
            window.draw(cx).clear(cx);
        });

        state.read_with(cx, |state, _| {
            assert_eq!(state.sizes(), &vec![px(220.), px(180.)]);
        });
    }

    #[gpui::test]
    fn dragging_the_handle_resizes_and_emits_once(cx: &mut TestAppContext) {
        let (cx, state, resizes) = harness(cx);
        let boundary = cx.debug_bounds("second-panel").unwrap().left();

        cx.simulate_mouse_down(
            point(boundary - px(2.), px(50.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(boundary + px(10.), px(50.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(220.), px(50.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(220.), px(50.)),
            MouseButton::Left,
            Modifiers::default(),
        );

        state.read_with(cx, |state, _| {
            assert_eq!(state.sizes(), &vec![px(220.), px(180.)]);
        });
        assert_eq!(resizes.get(), 1);
    }

    /// Reports every state its divider is rendered in, so a drag can be watched
    /// from outside the handle.
    ///
    /// `covered` lays an occluding overlay over the whole group after it, the
    /// way a sheet's backdrop or a toast would sit over a dock.
    struct HandleStateHarness {
        seen: Rc<RefCell<Vec<ResizeHandleState>>>,
        covered: bool,
    }

    impl Render for HandleStateHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let seen = self.seen.clone();
            div()
                .relative()
                .w(px(400.))
                .h(px(100.))
                .child(
                    h_resizable("handle-state")
                        .with_handle_appearance(Rc::new(
                            move |handle: &ResizeHandleContext, _: &mut Window, _: &mut App| {
                                let mut seen = seen.borrow_mut();
                                if seen.last() != Some(&handle.state()) {
                                    seen.push(handle.state());
                                }
                                // Nothing painted: this renderer is here to watch.
                                None
                            },
                        ))
                        .child(resizable_panel().size(px(150.)).child(div().size_full()))
                        .child(
                            resizable_panel()
                                .size(px(250.))
                                .child(div().size_full().debug_selector(|| "hs-second".into())),
                        ),
                )
                .when(self.covered, |this| {
                    this.child(div().absolute().inset_0().occlude())
                })
        }
    }

    /// A handle under something else does not answer the pointer.
    ///
    /// The listeners used to test the pointer against the handle's bounds,
    /// which read true through anything painted over it: a divider under a
    /// sheet's backdrop lit up as the pointer crossed where it lay, and a
    /// press there counted as a press on the handle. They ask the hitbox now,
    /// and an occluding element in front of it answers for it.
    #[gpui::test]
    fn a_covered_handle_stays_idle(cx: &mut TestAppContext) {
        let seen: Rc<RefCell<Vec<ResizeHandleState>>> = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let seen = seen.clone();
            move |_, _| HandleStateHarness {
                seen,
                covered: true,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let boundary = cx.debug_bounds("hs-second").unwrap().left();
        let on_handle = point(boundary - px(2.), px(50.));
        let draw = |cx: &mut VisualTestContext| cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_mouse_move(on_handle, None, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_down(on_handle, MouseButton::Left, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_up(on_handle, MouseButton::Left, Modifiers::default());
        draw(cx);

        assert_eq!(*seen.borrow(), vec![ResizeHandleState::Idle]);
    }

    #[gpui::test]
    fn a_handle_reports_the_press_and_the_drag_to_its_renderer(cx: &mut TestAppContext) {
        let seen: Rc<RefCell<Vec<ResizeHandleState>>> = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let seen = seen.clone();
            move |_, _| HandleStateHarness {
                seen,
                covered: false,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let boundary = cx.debug_bounds("hs-second").unwrap().left();
        let on_handle = point(boundary - px(2.), px(50.));
        let draw = |cx: &mut VisualTestContext| cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_mouse_move(on_handle, None, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_down(on_handle, MouseButton::Left, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_move(
            point(px(260.), px(50.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        draw(cx);
        cx.simulate_mouse_up(
            point(px(390.), px(90.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);

        // Every one of these frames used to render `Idle`: the listeners wrote
        // their progress into a copy of the handle's state, so no renderer ever
        // saw a press, and `is_active` never once read true.
        assert_eq!(
            *seen.borrow(),
            vec![
                ResizeHandleState::Idle,
                ResizeHandleState::Hovered,
                ResizeHandleState::Pressed,
                ResizeHandleState::Dragging,
                ResizeHandleState::Idle,
            ]
        );
    }

    struct SizedGroupHarness;

    impl Render for SizedGroupHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(400.))
                .h(px(100.))
                .child(
                    h_resizable("sized-resizable").size(px(40.)).child(
                        resizable_panel()
                            .child(div().size_full().debug_selector(|| "sized-panel".into())),
                    ),
                )
        }
    }

    #[gpui::test]
    fn a_group_size_binds_the_cross_axis(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| SizedGroupHarness);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let panel = cx.debug_bounds("sized-panel").unwrap();
        assert_eq!(panel.size.width, px(400.));
        assert_eq!(panel.size.height, px(40.));
    }
}
