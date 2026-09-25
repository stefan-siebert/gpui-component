use std::rc::Rc;

use gpui::{
    App, Bounds, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, RenderOnce, Role, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder as _,
};

use crate::{ActiveTheme, Icon, IconName, StyledExt, h_flex};

/// A breadcrumb navigation element.
#[derive(IntoElement)]
pub struct Breadcrumb {
    style: StyleRefinement,
    items: Vec<BreadcrumbItem>,
}

/// Maximum number of characters before a breadcrumb label is middle-truncated.
///
/// Inside a [`CollapsibleBreadcrumb`] this is a *last resort*, applied only
/// once the items no longer fit the container. Hiding whole middle segments
/// is the primary mechanism; shortening the surviving labels comes after it.
/// Truncating while there is room to spare hides characters for no reason —
/// a 27-character label would lose its middle in a half-empty bar.
const DEFAULT_MAX_LABEL_CHARS: usize = 24;

/// Item for the [`Breadcrumb`].
#[derive(IntoElement)]
pub struct BreadcrumbItem {
    id: ElementId,
    style: StyleRefinement,
    label: SharedString,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    on_hover: Option<Rc<dyn Fn(&bool, &mut Window, &mut App)>>,
    disabled: bool,
    is_last: bool,
    max_label_chars: Option<usize>,
}

impl BreadcrumbItem {
    /// Create a new BreadcrumbItem with the given id and label.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            id: ElementId::Integer(0),
            style: StyleRefinement::default(),
            label: label.into(),
            on_click: None,
            on_hover: None,
            disabled: false,
            is_last: false,
            max_label_chars: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set the maximum number of characters before the label is
    /// middle-truncated (e.g. `very_long_na…me_here`).
    ///
    /// Set explicitly, the limit always applies. Left unset, a
    /// [`CollapsibleBreadcrumb`] falls back to [`DEFAULT_MAX_LABEL_CHARS`]
    /// *only while its items overflow the container*, and a regular
    /// [`Breadcrumb`] never truncates at all.
    pub fn max_label_chars(mut self, max: usize) -> Self {
        self.max_label_chars = Some(max);
        self
    }

    pub fn on_click(
        mut self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(on_click));
        self
    }

    pub fn on_hover(mut self, on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(on_hover));
        self
    }

    fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }

    /// For internal use only.
    fn is_last(mut self, is_last: bool) -> Self {
        self.is_last = is_last;
        self
    }

    /// Return the display label, applying middle truncation if needed.
    fn display_label(&self) -> SharedString {
        if let Some(max) = self.max_label_chars {
            middle_truncate(&self.label, max).into()
        } else {
            self.label.clone()
        }
    }
}

/// Truncate a string in the middle, keeping the first and last portions
/// with an ellipsis (`…`) in between.
/// e.g. `middle_truncate("folder_klkrfel_fdfkjdlkjdf_kfk_111", 20)`
///   → `"folder_klkr…kfk_111"`
fn middle_truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars || max_chars < 5 {
        return s.to_string();
    }
    // Keep slightly more at the start than the end
    let keep_end = (max_chars - 1) / 3;
    let keep_start = max_chars - 1 - keep_end;

    let start: String = s.chars().take(keep_start).collect();
    let end: String = s.chars().skip(char_count - keep_end).collect();
    format!("{start}\u{2026}{end}")
}

impl Styled for BreadcrumbItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl From<&'static str> for BreadcrumbItem {
    fn from(value: &'static str) -> Self {
        Self::new(value)
    }
}

impl From<String> for BreadcrumbItem {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<SharedString> for BreadcrumbItem {
    fn from(value: SharedString) -> Self {
        Self::new(value)
    }
}

impl RenderOnce for BreadcrumbItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = self.display_label();
        div()
            .id(self.id)
            .flex_shrink_0()
            .role(if self.on_click.is_some() && !self.disabled {
                Role::Link
            } else {
                Role::ListItem
            })
            .child(label)
            .text_color(cx.theme().muted_foreground)
            .when(self.is_last, |this| this.text_color(cx.theme().foreground))
            .when(self.disabled, |this| {
                this.text_color(cx.theme().muted_foreground)
            })
            .refine_style(&self.style)
            .when_some(self.on_hover, |this, on_hover| {
                this.on_hover(move |hovered, window, cx| {
                    on_hover(hovered, window, cx);
                })
            })
            .when(!self.disabled, |this| {
                this.when_some(self.on_click, |this, on_click| {
                    this.cursor_pointer().on_click(move |event, window, cx| {
                        on_click(event, window, cx);
                    })
                })
            })
    }
}

impl Breadcrumb {
    /// Create a new breadcrumb.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    /// Add an [`BreadcrumbItem`] to the breadcrumb.
    pub fn child(mut self, item: impl Into<BreadcrumbItem>) -> Self {
        self.items.push(item.into());
        self
    }

    /// Add multiple [`BreadcrumbItem`] items to the breadcrumb.
    pub fn children(mut self, items: impl IntoIterator<Item = impl Into<BreadcrumbItem>>) -> Self {
        self.items.extend(items.into_iter().map(Into::into));
        self
    }
}

#[derive(IntoElement)]
struct BreadcrumbSeparator;
impl RenderOnce for BreadcrumbSeparator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        Icon::new(IconName::ChevronRight)
            .text_color(cx.theme().muted_foreground)
            .flex_shrink_0()
            .size_3p5()
            .into_any_element()
    }
}

impl Styled for Breadcrumb {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Breadcrumb {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let items_count = self.items.len();

        let mut children = vec![];
        for (ix, item) in self.items.into_iter().enumerate() {
            let is_last = ix == items_count - 1;

            let item = item.id(ix);
            children.push(item.is_last(is_last).into_any_element());
            if !is_last {
                children.push(BreadcrumbSeparator.into_any_element());
            }
        }

        h_flex()
            .gap_1p5()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .refine_style(&self.style)
            .children(children)
    }
}

// ---------------------------------------------------------------------------
// CollapsibleBreadcrumb
// ---------------------------------------------------------------------------

/// Shared state for [`CollapsibleBreadcrumb`] that persists across frames.
///
/// Create one instance via [`CollapsibleBreadcrumbState::new`] and store it
/// in your view/entity. Pass it to each [`CollapsibleBreadcrumb`] you render.
/// The state tracks measured child widths from the previous frame so that
/// the breadcrumb can decide which items to collapse.
#[derive(Clone)]
pub struct CollapsibleBreadcrumbState {
    inner: Rc<std::cell::RefCell<CollapseInner>>,
}

#[derive(Default)]
struct CollapseInner {
    /// Measured widths of each child element (items + separators) from the
    /// *uncollapsed* layout. Only updated when all items are shown, so the
    /// collapsing decision stays stable and doesn't oscillate.
    child_widths: Vec<f32>,
    /// Actual gap between flex children, measured from bounds positions.
    gap: f32,
    /// Total content width computed from child_widths + gaps.
    total_content_width: f32,
    /// Available container width from the previous frame.
    container_width: f32,
    /// Number of items in the last measurement. Used to detect content changes
    /// (e.g. navigation to a different path) and reset stale measurements.
    measured_items_count: usize,
    /// Combined label length in the last measurement, as a cheap fingerprint of
    /// the content itself.
    ///
    /// The item count alone does not identify a path: `/home/a/b` and
    /// `/home/very_long_name/another_long_one` both have three segments. Since
    /// `total_content_width` only ever grows within one measurement generation,
    /// a swap between two equally deep paths would otherwise carry the wider
    /// one's width over to the narrower one and keep it looking overflowed.
    /// Collisions are harmless: equal total length means near-equal width.
    measured_label_chars: usize,
}

impl CollapseInner {
    /// Whether the last *uncollapsed* measurement exceeded the container.
    ///
    /// Both widths are zero until the first measured frame lands, so this
    /// reads `false` on the first pass — which is what we want: that frame
    /// must render full labels for the measurement to mean anything.
    ///
    /// Stable across frames, and so free of the truncate/untruncate flapping
    /// one would expect: `total_content_width` is a high-water mark of the
    /// *untruncated* layout (the prepaint callback rejects any smaller
    /// measurement), so shortening the labels never talks itself out of the
    /// overflow that caused it. Only a genuine change — a wider container or
    /// a different path — moves the answer back.
    fn is_overflowing(&self) -> bool {
        self.container_width > 0.0 && self.total_content_width > self.container_width
    }

    /// Discard measurements unless they describe exactly this content.
    ///
    /// The container width is deliberately kept: it belongs to the element,
    /// not to the labels, and is still the best estimate available for the
    /// frame that re-measures.
    fn invalidate_unless(&mut self, items_count: usize, label_chars: usize) {
        if self.measured_items_count == items_count && self.measured_label_chars == label_chars {
            return;
        }
        self.total_content_width = 0.0;
        self.child_widths.clear();
        self.measured_items_count = items_count;
        self.measured_label_chars = label_chars;
    }
}

impl CollapsibleBreadcrumbState {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(std::cell::RefCell::new(CollapseInner::default())),
        }
    }
}

/// A collapsible breadcrumb that automatically hides middle segments behind
/// an ellipsis (`…`) when items overflow the available width.
///
/// Uses a two-frame approach:
/// 1. First frame: renders all items; `on_children_prepainted` measures widths.
///    The parent container should use `overflow_hidden` to clip overflow.
/// 2. Subsequent frames: uses measured widths to show only
///    first item + `…` + trailing items that fit.
///
/// # Example
///
/// ```ignore
/// // In your entity struct:
/// breadcrumb_state: CollapsibleBreadcrumbState,
///
/// // In new():
/// breadcrumb_state: CollapsibleBreadcrumbState::new(),
///
/// // In render():
/// CollapsibleBreadcrumb::new("my-breadcrumb", &self.breadcrumb_state)
///     .child(BreadcrumbItem::new("/").on_click(|_, _, _| {}))
///     .child(BreadcrumbItem::new("home").on_click(|_, _, _| {}))
///     .child(BreadcrumbItem::new("deep").on_click(|_, _, _| {}))
///     .child(BreadcrumbItem::new("path").on_click(|_, _, _| {}))
/// ```
pub struct CollapsibleBreadcrumb {
    id: ElementId,
    state: CollapsibleBreadcrumbState,
    items: Vec<BreadcrumbItem>,
    style: StyleRefinement,
    on_ellipsis_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl CollapsibleBreadcrumb {
    pub fn new(id: impl Into<ElementId>, state: &CollapsibleBreadcrumbState) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            items: Vec::new(),
            style: StyleRefinement::default(),
            on_ellipsis_click: None,
        }
    }

    /// Add a [`BreadcrumbItem`].
    pub fn child(mut self, item: impl Into<BreadcrumbItem>) -> Self {
        self.items.push(item.into());
        self
    }

    /// Add multiple [`BreadcrumbItem`] items.
    pub fn children(mut self, items: impl IntoIterator<Item = impl Into<BreadcrumbItem>>) -> Self {
        self.items.extend(items.into_iter().map(Into::into));
        self
    }

    /// Set a click handler for the ellipsis item shown when items are collapsed.
    pub fn on_ellipsis_click(
        mut self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_ellipsis_click = Some(Rc::new(on_click));
        self
    }

    /// Determine how many trailing items to show based on measured child widths
    /// and available container width. Returns `None` if all items fit.
    fn compute_visible_tail(inner: &CollapseInner, items_count: usize) -> Option<usize> {
        if inner.container_width <= 0.0 || inner.child_widths.is_empty() || items_count <= 2 {
            return None;
        }

        // Use directly measured total content width for the overflow check
        if inner.total_content_width <= inner.container_width {
            return None; // Everything fits
        }

        let gap = inner.gap;

        // child_widths layout: [item0, sep0, item1, sep1, ..., itemN]
        // Collapsed layout children:
        //   [item0] gap [sep] gap […] gap [sep] gap [tail1] gap [sep] gap [tail2]
        //
        // Fixed prefix = item0 + gap + sep + gap + ellipsis + gap + sep
        //              = item0 + 3*gap + 2*sep + ellipsis
        let ellipsis_width = 12.0_f32;
        let first_item_w = inner.child_widths.first().copied().unwrap_or(0.0);
        let sep_w = inner.child_widths.get(1).copied().unwrap_or(0.0);

        let prefix = first_item_w + 3.0 * gap + 2.0 * sep_w + ellipsis_width;
        let budget = inner.container_width - prefix;

        if budget <= 0.0 {
            return Some(1);
        }

        // Count trailing items that fit (from last to first).
        // First tail item costs: gap + item_w
        // Each additional: gap + sep + gap + item_w
        let mut used = 0.0_f32;
        let mut tail = 0_usize;

        for i in (1..items_count).rev() {
            let child_idx = i * 2; // index into child_widths (items interleaved with seps)
            let item_w = inner.child_widths.get(child_idx).copied().unwrap_or(0.0);
            let cost = if tail > 0 {
                2.0 * gap + sep_w + item_w
            } else {
                gap + item_w
            };

            if used + cost > budget {
                break;
            }
            used += cost;
            tail += 1;
        }

        Some(tail.max(1))
    }

    fn make_ellipsis(
        on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let el = div()
            .id("breadcrumb-ellipsis")
            .flex_shrink_0()
            .child("\u{2026}")
            .text_color(cx.theme().muted_foreground);

        if let Some(on_click) = on_click {
            el.cursor_pointer()
                .on_click(move |event, window, cx| {
                    on_click(event, window, cx);
                })
                .into_any_element()
        } else {
            el.into_any_element()
        }
    }
}

impl Styled for CollapsibleBreadcrumb {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for CollapsibleBreadcrumb {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for CollapsibleBreadcrumb {
    type RequestLayoutState = gpui::AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let items_count = self.items.len();
        let state = self.state.clone();

        // Determine collapsing from previous frame's measurements. Measurements
        // describe one specific set of labels, so they are dropped as soon as
        // the content changes — by item count or by total label length.
        let label_chars: usize = self
            .items
            .iter()
            .map(|item| item.label.chars().count())
            .sum();
        state
            .inner
            .borrow_mut()
            .invalidate_unless(items_count, label_chars);

        let (visible_tail, overflowing) = {
            let inner = state.inner.borrow();
            (
                Self::compute_visible_tail(&inner, items_count),
                inner.is_overflowing(),
            )
        };
        let is_collapsed = visible_tail.is_some();

        // Build children. The default label limit is a response to overflow,
        // not a constant — see [`DEFAULT_MAX_LABEL_CHARS`]. An explicit limit
        // from the caller is an instruction and always stands.
        //
        // `overflowing` is checked instead of `is_collapsed` on purpose: a
        // breadcrumb of two items never collapses (there is no middle to
        // hide) but can still overrun its container, and that case needs the
        // labels shortened just as much.
        let items: Vec<BreadcrumbItem> = std::mem::take(&mut self.items)
            .into_iter()
            .map(|item| {
                if item.max_label_chars.is_some() || !overflowing {
                    item
                } else {
                    BreadcrumbItem {
                        max_label_chars: Some(DEFAULT_MAX_LABEL_CHARS),
                        ..item
                    }
                }
            })
            .collect();
        let mut children: Vec<gpui::AnyElement> = Vec::new();

        match visible_tail {
            None => {
                // Show all items
                for (ix, item) in items.into_iter().enumerate() {
                    let is_last = ix == items_count - 1;
                    children.push(item.id(ix).is_last(is_last).into_any_element());
                    if !is_last {
                        children.push(BreadcrumbSeparator.into_any_element());
                    }
                }
            }
            Some(tail_count) => {
                let tail_count = tail_count.min(items_count.saturating_sub(1));
                let tail_start = items_count - tail_count;
                let mut ellipsis_inserted = false;

                for (ix, item) in items.into_iter().enumerate() {
                    let is_last = ix == items_count - 1;

                    if ix == 0 {
                        // Always show first item
                        children.push(item.id(ix).is_last(is_last).into_any_element());
                        if !is_last {
                            children.push(BreadcrumbSeparator.into_any_element());
                        }
                    } else if ix >= tail_start {
                        // Tail items — insert ellipsis before the first one
                        if !ellipsis_inserted && tail_start > 1 {
                            children.push(Self::make_ellipsis(self.on_ellipsis_click.clone(), cx));
                            children.push(BreadcrumbSeparator.into_any_element());
                            ellipsis_inserted = true;
                        }
                        children.push(item.id(ix).is_last(is_last).into_any_element());
                        if !is_last {
                            children.push(BreadcrumbSeparator.into_any_element());
                        }
                    }
                    // else: hidden middle items — skip
                }
            }
        }

        // Build the inner flex container with measurement callback.
        // Only update child_widths when NOT collapsed — collapsed layout has
        // fewer children, so its widths would cause oscillation.
        let measure_state = self.state.clone();
        let mut inner = h_flex()
            .w_full()
            .overflow_hidden()
            .flex_nowrap()
            .gap_1p5()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .refine_style(&self.style)
            .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, _window, _cx| {
                let mut inner = measure_state.inner.borrow_mut();
                // Only update when this frame rendered ALL items.
                // Collapsed frames have fewer children, so their measurements
                // would cause the next frame to incorrectly uncollapse.
                if !is_collapsed && bounds.len() >= 2 {
                    let widths: Vec<f32> = bounds.iter().map(|b| f32::from(b.size.width)).collect();

                    // Measure actual gap from the distance between first two children
                    let first_right =
                        f32::from(bounds[0].origin.x) + f32::from(bounds[0].size.width);
                    let second_left = f32::from(bounds[1].origin.x);
                    let gap = (second_left - first_right).max(0.0);

                    let sum: f32 = widths.iter().sum();
                    let num_gaps = widths.len().saturating_sub(1) as f32;
                    let new_total = sum + num_gaps * gap;

                    // Only accept measurements that are >= the previous total.
                    // A smaller total means overflow_hidden clipped the last
                    // child — those measurements are unreliable.
                    if new_total >= inner.total_content_width - 1.0 {
                        inner.child_widths = widths;
                        inner.gap = gap;
                        inner.total_content_width = new_total;
                    }
                }
            })
            .children(children)
            .into_any();

        let layout_id = inner.request_layout(window, cx);
        (layout_id, inner)
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        inner: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Store container width for next frame's collapsing decision
        {
            let mut state = self.state.inner.borrow_mut();
            state.container_width = f32::from(bounds.size.width);
        }

        inner.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        inner: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        inner.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measurement as the prepaint callback would leave it.
    fn measured(total_content_width: f32, container_width: f32) -> CollapseInner {
        CollapseInner {
            child_widths: vec![total_content_width],
            gap: 6.0,
            total_content_width,
            container_width,
            measured_items_count: 1,
            measured_label_chars: 10,
        }
    }

    #[test]
    fn nothing_overflows_before_the_first_measurement() {
        // The opening frame must render full labels — otherwise the width it
        // measures is the width of already-shortened text.
        assert!(!CollapseInner::default().is_overflowing());
    }

    #[test]
    fn a_container_of_unknown_width_never_forces_truncation() {
        // Container width only becomes known in prepaint.
        assert!(!measured(900.0, 0.0).is_overflowing());
    }

    #[test]
    fn overflow_needs_the_content_to_exceed_the_container() {
        assert!(!measured(300.0, 800.0).is_overflowing());
        assert!(!measured(800.0, 800.0).is_overflowing());
        assert!(measured(801.0, 800.0).is_overflowing());
    }

    #[test]
    fn a_long_label_survives_whole_in_a_roomy_bar() {
        // The regression this guards: a 27-character label lost its middle in
        // a half-empty breadcrumb, because the limit counted characters and
        // never consulted the width.
        let label = "stefan.siebert.de@gmail.com";
        assert_eq!(label.chars().count(), 27);
        assert!(!measured(420.0, 900.0).is_overflowing());
    }

    #[test]
    fn unchanged_content_keeps_its_measurement() {
        let mut inner = measured(900.0, 500.0);
        inner.invalidate_unless(1, 10);
        assert_eq!(inner.total_content_width, 900.0);
        assert!(inner.is_overflowing());
    }

    #[test]
    fn a_different_path_of_the_same_depth_drops_the_measurement() {
        // The regression this guards: `/home/very_long_name/another_long_one`
        // and `/home/a/b` have the same segment count, so the item count alone
        // could not tell them apart — and because the stored width only grows,
        // the short path inherited the long one's overflow and stayed
        // truncated.
        let mut inner = measured(900.0, 500.0);
        assert!(inner.is_overflowing());
        inner.invalidate_unless(1, 4);
        assert_eq!(inner.total_content_width, 0.0);
        assert!(inner.child_widths.is_empty());
        assert!(!inner.is_overflowing());
        // The container width outlives the content — it describes the element.
        assert_eq!(inner.container_width, 500.0);
    }

    #[test]
    fn a_different_segment_count_drops_the_measurement() {
        let mut inner = measured(900.0, 500.0);
        inner.invalidate_unless(4, 10);
        assert_eq!(inner.total_content_width, 0.0);
        assert!(!inner.is_overflowing());
    }

    #[test]
    fn middle_truncate_keeps_both_ends() {
        assert_eq!(
            middle_truncate("stefan.siebert.de@gmail.com", 24),
            "stefan.siebert.d…ail.com"
        );
    }

    #[test]
    fn middle_truncate_leaves_short_labels_alone() {
        assert_eq!(middle_truncate("Documents", 24), "Documents");
        // Exactly at the limit is still untouched.
        assert_eq!(middle_truncate("123456789012", 12), "123456789012");
    }

    #[test]
    fn middle_truncate_declines_limits_too_small_to_be_useful() {
        // Below five characters an ellipsis costs more than it saves.
        assert_eq!(middle_truncate("abcdefgh", 4), "abcdefgh");
    }

    #[test]
    fn middle_truncate_slices_on_character_boundaries() {
        let out = middle_truncate("Ordner_ÄÖÜ_äöü_ßßß_Ende_lang_genug", 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.contains('…'));
    }
}
