use crate::{
    ActiveTheme, Sizable, Size,
    actions::{
        Cancel, SelectDown, SelectFirst, SelectLast, SelectNextColumn, SelectPageDown,
        SelectPageUp, SelectPrevColumn, SelectUp,
    },
    table::{TableDelegate, TableState},
};
use gpui::{
    App, Edges, Entity, Focusable, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    RenderOnce, Styled, Window, div, prelude::FluentBuilder,
};

const CONTEXT: &'static str = "DataTable";
pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("down", SelectDown, Some(CONTEXT)),
        KeyBinding::new("left", SelectPrevColumn, Some(CONTEXT)),
        KeyBinding::new("right", SelectNextColumn, Some(CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(CONTEXT)),
        KeyBinding::new("pageup", SelectPageUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", SelectPageDown, Some(CONTEXT)),
        KeyBinding::new("tab", SelectNextColumn, Some(CONTEXT)),
        KeyBinding::new("shift-tab", SelectPrevColumn, Some(CONTEXT)),
    ]);
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct TableOptions {
    pub(super) scrollbar_visible: Edges<bool>,
    /// Set stripe style of the table.
    pub(super) stripe: bool,
    /// Set to use border style of the table.
    pub(super) bordered: bool,
    /// The cell size of the table.
    pub(super) size: Size,
    /// When false, body rows render cells via h_flex (like the header) instead of
    /// a horizontal virtual_list. This avoids a 1-frame column-width mismatch
    /// between header and body during window resize, which causes visible flicker
    /// on Wayland compositors. Default: true.
    pub(super) horizontal_virtual: bool,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            scrollbar_visible: Edges::all(true),
            stripe: false,
            bordered: true,
            size: Size::default(),
            horizontal_virtual: true,
        }
    }
}

/// A table element with support for row, column, and cell selection.
///
/// # Features
///
/// - **Multiple Selection Modes**: Support for row, column, and cell selection
/// - **Cell Selection**: Click to select individual cells, with keyboard navigation
/// - **Virtual Scrolling**: Efficient rendering of large datasets
/// - **Resizable Columns**: Drag column borders to resize
/// - **Movable Columns**: Drag column headers to reorder
/// - **Fixed Columns**: Pin columns to the left side
/// - **Sortable Columns**: Click column headers to sort
/// - **Context Menus**: Right-click support for rows and cells
///
/// # Cell Selection Mode
///
/// When cell selection is enabled via [`TableState::cell_selectable()`]:
/// - Click on cells to select them
/// - A row selector column appears on the left for selecting entire rows
/// - Keyboard navigation (arrow keys, Tab, Home, End, PageUp, PageDown) works at cell level
/// - Right-click and double-click events are supported
///
/// See [`TableState`] for more details on cell selection.
///
/// # Example
///
/// ```rust,ignore
/// let table_state = cx.new(|cx| {
///     TableState::new(delegate, cx)
///         .cell_selectable(true)
///         .row_selectable(true)
/// });
///
/// DataTable::new(&table_state)
///     .stripe(true)
///     .bordered(true)
/// ```
#[derive(IntoElement)]
pub struct DataTable<D: TableDelegate> {
    state: Entity<TableState<D>>,
    options: TableOptions,
}

impl<D> DataTable<D>
where
    D: TableDelegate,
{
    /// Create a new DataTable element with the given [`TableState`].
    pub fn new(state: &Entity<TableState<D>>) -> Self {
        Self { state: state.clone(), options: TableOptions::default() }
    }

    /// Set to use stripe style of the table, default to false.
    pub fn stripe(mut self, stripe: bool) -> Self {
        self.options.stripe = stripe;
        self
    }

    /// Set to use border style of the table, default to true.
    pub fn bordered(mut self, bordered: bool) -> Self {
        self.options.bordered = bordered;
        self
    }

    /// Set scrollbar visibility.
    pub fn scrollbar_visible(mut self, vertical: bool, horizontal: bool) -> Self {
        self.options.scrollbar_visible =
            Edges { right: vertical, bottom: horizontal, ..Default::default() };
        self
    }

    /// Disable horizontal virtual_list for body rows.
    ///
    /// When set to `false`, body rows render cells in an `h_flex` (the same way
    /// the header does) instead of a horizontal `virtual_list` with absolute
    /// positioning. This eliminates a 1-frame column-width mismatch between
    /// header and body during window resize, which causes visible flicker on
    /// some Wayland compositors.
    ///
    /// Use this when the table has a small, fixed number of columns and
    /// horizontal virtualization is not needed.
    ///
    /// Default: `true` (horizontal virtualization enabled).
    pub fn horizontal_virtual(mut self, enabled: bool) -> Self {
        self.options.horizontal_virtual = enabled;
        self
    }
}

impl<D> Sizable for DataTable<D>
where
    D: TableDelegate,
{
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.options.size = size.into();
        self
    }
}

impl<D> RenderOnce for DataTable<D>
where
    D: TableDelegate,
{
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let bordered = self.options.bordered;
        let focus_handle = self.state.focus_handle(cx);
        // Only update options if they actually changed, to avoid marking the
        // entity dirty on every parent render (which causes a re-render cascade
        // during window resize).
        let new_options = self.options;
        self.state.update(cx, |state, _| {
            if state.options != new_options {
                state.options = new_options;
            }
        });

        div()
            .id("table")
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&focus_handle)
            .on_action(window.listener_for(&self.state, TableState::action_cancel))
            .on_action(window.listener_for(&self.state, TableState::action_select_next))
            .on_action(window.listener_for(&self.state, TableState::action_select_prev))
            .on_action(window.listener_for(&self.state, TableState::action_select_next_col))
            .on_action(window.listener_for(&self.state, TableState::action_select_prev_col))
            .on_action(window.listener_for(&self.state, TableState::action_select_first_column))
            .on_action(window.listener_for(&self.state, TableState::action_select_last_column))
            .on_action(window.listener_for(&self.state, TableState::action_select_page_up))
            .on_action(window.listener_for(&self.state, TableState::action_select_page_down))
            .bg(cx.theme().table)
            .when(bordered, |this| {
                this.rounded(cx.theme().radius).border_1().border_color(cx.theme().border)
            })
            .child(self.state)
    }
}
