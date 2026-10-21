use gpui_base::TestSupportExt as _;
use std::{ops::Range, time::Duration};

use crate::{
    ActiveTheme, ElementExt, Icon, IconName, StyleSized as _, StyledExt, VirtualListScrollHandle,
    actions::{
        Cancel, SelectDown, SelectFirst, SelectLast, SelectNextColumn, SelectPageDown,
        SelectPageUp, SelectPrevColumn, SelectUp,
    },
    h_flex,
    menu::{ContextMenuExt, PopupMenu},
    scroll::{ScrollableMask, Scrollbar},
    v_flex,
};
use gpui::{
    AnyElement, AppContext, Axis, Bounds, ClickEvent, Context, Div, DragMoveEvent, ElementId,
    EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ListSizingBehavior,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, Task, TextAlign,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder, px, uniform_list,
};
use rust_i18n::t;

use super::*;

/// How far behind the pointer a resize drag trails the column edge.
const HANDLE_SIZE: Pixels = px(2.);

/// Grab room on each side of a resize handle's hairline, so the divider can be
/// caught without pixel-hunting. Both halves of the band are this wide, so
/// widening the grab area stays symmetric about the boundary. `resizable`'s
/// handle pads a hairline the same way, and by the same amount, see
/// `crates/base/src/resizable/resize_handle.rs`.
const HANDLE_PADDING: Pixels = px(4.);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SelectionMode {
    Column,
    Row,
    Cell,
}

impl SelectionMode {
    #[inline(always)]
    fn is_row(&self) -> bool {
        matches!(self, SelectionMode::Row)
    }

    #[inline(always)]
    fn is_column(&self) -> bool {
        matches!(self, SelectionMode::Column)
    }

    #[inline(always)]
    fn is_cell(&self) -> bool {
        matches!(self, SelectionMode::Cell)
    }
}

/// The current selection of a table, as one value.
///
/// Mirrors [`TableEvent::SelectRow`], [`TableEvent::SelectColumn`] and
/// [`TableEvent::SelectCell`]. Match on it instead of combining
/// [`TableState::selected_row`], [`TableState::selected_col`] and
/// [`TableState::selected_cell`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TableSelection {
    /// Nothing is selected.
    #[default]
    None,
    /// A row is selected.
    Row(usize),
    /// A column is selected.
    Column(usize),
    /// A cell is selected, as `(row_ix, col_ix)`.
    Cell(usize, usize),
}

/// The Table event.
#[derive(Clone)]
pub enum TableEvent {
    /// Single click or move to selected row.
    SelectRow(usize),
    /// Double click on the row.
    DoubleClickedRow(usize),
    /// Selected column.
    SelectColumn(usize),
    /// A cell has been selected (clicked or navigated to via keyboard).
    ///
    /// Emitted when a cell is selected in cell selection mode.
    /// The first `usize` is the row index, and the second `usize` is the column index.
    ///
    /// This event is also emitted when navigating between cells using keyboard shortcuts.
    SelectCell(usize, usize),
    /// A cell has been double-clicked.
    ///
    /// Emitted when a cell is double-clicked in cell selection mode.
    /// The first `usize` is the row index, and the second `usize` is the column index.
    ///
    /// Use this event to trigger actions like opening a detail view or editing the cell content.
    DoubleClickedCell(usize, usize),
    /// The column widths have changed.
    ///
    /// The `Vec<Pixels>` contains the new widths of all columns.
    ColumnWidthsChanged(Vec<Pixels>),
    /// A column has been moved.
    ///
    /// The first `usize` is the original index of the column,
    /// and the second `usize` is the new index of the column.
    MoveColumn(usize, usize),
    /// A row was left-clicked with full context: row index, column index
    /// (from click position), click count (1 = single, 2 = double), and
    /// whether the row was already selected before this click.
    ///
    /// This is a richer alternative to `SelectRow` + `DoubleClickedRow` for
    /// consumers that need column-aware or re-click-aware logic (e.g.
    /// click-to-rename on a specific column).
    RowClick {
        row_ix: usize,
        col_ix: Option<usize>,
        click_count: usize,
        was_already_selected: bool,
    },
    /// A row has been right-clicked.
    ///
    /// Contains the row index, or `None` if right-clicked on an empty area.
    /// Use this event to show context menus for rows.
    RightClickedRow(Option<usize>),
    /// A cell has been right-clicked.
    ///
    /// Emitted when a cell is right-clicked in cell selection mode.
    /// The first `usize` is the row index, and the second `usize` is the column index.
    ///
    /// Use this event to show context menus specific to the cell content.
    /// The right-clicked cell is highlighted with a subtle border until another cell is clicked.
    RightClickedCell(usize, usize),
    /// The selection has been cleared.
    ///
    /// This event is emitted when the selection is cleared.
    ClearSelection,
}

/// The visible range of the rows and columns.
#[derive(Debug, Default)]
pub struct TableVisibleRange {
    /// The visible range of the rows.
    rows: Range<usize>,
    /// The visible range of the columns.
    cols: Range<usize>,
}

impl TableVisibleRange {
    /// Returns the visible range of the rows.
    pub fn rows(&self) -> &Range<usize> {
        &self.rows
    }

    /// Returns the visible range of the columns.
    pub fn cols(&self) -> &Range<usize> {
        &self.cols
    }
}

/// The state for [`DataTable`].
///
/// # Selection Modes
///
/// The table supports three selection modes:
/// - **Row Selection**: Select entire rows (default mode)
/// - **Column Selection**: Select entire columns
/// - **Cell Selection**: Select individual cells
///
/// ## Cell Selection
///
/// When `cell_selectable` is enabled, users can:
/// - Click on cells to select them
/// - Right-click on cells to mark them for context menus
/// - Double-click on cells to trigger actions
/// - Navigate between cells using keyboard (arrow keys, Home, End, PageUp, PageDown, Tab)
///
/// When in cell selection mode, a row selector column appears on the left side,
/// allowing users to select entire rows by clicking on it.
///
/// # Events
///
/// The table emits the following events related to cell selection:
/// - [`TableEvent::SelectCell`]: Emitted when a cell is selected
/// - [`TableEvent::DoubleClickedCell`]: Emitted when a cell is double-clicked
/// - [`TableEvent::RightClickedCell`]: Emitted when a cell is right-clicked
///
/// # Example
///
/// ```rust,ignore
/// let table_state = cx.new(|cx| {
///     TableState::new(delegate, window, cx)
///         .cell_selectable(true)
///         .row_selectable(true)
/// });
///
/// // Subscribe to cell events
/// cx.subscribe(&table_state, |this, table, event, cx| {
///     match event {
///         TableEvent::SelectCell(row_ix, col_ix) => {
///             println!("Selected cell: ({}, {})", row_ix, col_ix);
///         }
///         TableEvent::DoubleClickedCell(row_ix, col_ix) => {
///             println!("Double-clicked cell: ({}, {})", row_ix, col_ix);
///         }
///         _ => {}
///     }
/// });
/// ```
#[derive(Clone)]
pub(crate) struct HeaderCell {
    pub label: SharedString,
    pub width: Pixels,
    col_span: usize,
    is_leaf: bool,
    leaf_col_ix: Option<usize>,
    start_leaf_col_ix: usize,
}

pub struct TableState<D: TableDelegate> {
    focus_handle: FocusHandle,
    delegate: D,
    pub(super) options: TableOptions,
    /// The bounds of the table container.
    bounds: Bounds<Pixels>,
    /// The bounds of the fixed head cols.
    fixed_head_cols_bounds: Bounds<Pixels>,

    col_groups: Vec<ColGroup>,
    header_layout: Vec<Vec<HeaderCell>>,

    /// Whether the table can loop selection, default is true.
    ///
    /// When the prev/next selection is out of the table bounds, the selection will loop to the other side.
    pub loop_selection: bool,
    /// Whether the table can select column.
    pub col_selectable: bool,
    /// Whether the table can select row.
    pub row_selectable: bool,
    /// Whether a row click with the secondary modifier (⌘ on macOS, Ctrl
    /// elsewhere) or Shift held selects the row, default is `true`.
    ///
    /// Set to `false` when the delegate keeps its own multi-row selection and
    /// handles those clicks itself. The table then leaves its selected row
    /// alone on a modified click, so a click that takes a row out of the
    /// delegate's selection is not undone by the table selecting it.
    ///
    /// In cell selection without a row header, a modified click on the
    /// selected cell does not escalate it to its row either.
    select_on_modifier_click: bool,
    /// Whether the table can select cell, default is false.
    ///
    /// When enabled:
    /// - Users can click on individual cells to select them
    /// - A row header column appears on the left for selecting entire rows
    ///   (can be hidden via [`Self::row_header`])
    /// - Keyboard navigation works at the cell level (arrow keys move between cells)
    /// - Right-click and double-click events are supported for cells
    pub cell_selectable: bool,
    /// Whether the row header column is visible when `cell_selectable` is enabled,
    /// default is `true`.
    ///
    /// Set to `false` to hide the narrow leftmost header column while keeping cell
    /// selection — useful when you want to put your own content (e.g. a row index
    /// column) on the left.
    pub row_header: bool,
    /// Whether the table can sort.
    pub sortable: bool,
    /// Whether the table can resize columns.
    pub col_resizable: bool,
    /// Whether the table can move columns.
    pub col_movable: bool,
    /// Enable/disable fixed columns feature.
    pub col_fixed: bool,

    pub vertical_scroll_handle: UniformListScrollHandle,
    pub horizontal_scroll_handle: VirtualListScrollHandle,

    selected_row: Option<usize>,
    selection_mode: SelectionMode,
    right_clicked_row: Option<usize>,
    right_clicked_cell: Option<(usize, usize)>,
    /// Column index hit by the last left-click (resolved from mouse position).
    last_clicked_col: Option<usize>,
    selected_col: Option<usize>,
    selected_cell: Option<(usize, usize)>,

    /// The column index that is being resized.
    resizing_col: Option<usize>,
    /// The column a double-click on its resize handle just fitted.
    ///
    /// The double-click's second press also starts a drag on the handle, which
    /// would move the fitted edge back under the pointer, so resize drags on
    /// this column are ignored until the next single press.
    fitted_col: Option<usize>,

    /// The table width the auto-width columns were last laid out for; zero
    /// until the first layout, which is what [`Self::auto_widths_ready`]
    /// reads.
    prev_table_width: Pixels,
    /// Fork: the width of the flat header row on the last frame, and of the
    /// delegate's trailing empty column in it. The difference is the room the
    /// columns of a table with an `auto_width` column share
    /// ([`Self::fill_room`]). Zero until measured.
    head_row_width: Pixels,
    last_empty_col_width: Pixels,
    /// The insertion gap index (`0..=cols_count`) while dragging a column
    /// header: the dragged column will be inserted between the columns
    /// `gap - 1` and `gap` on drop.
    col_drag_gap: Option<usize>,

    /// The visible range of the rows and columns.
    visible_range: TableVisibleRange,

    _measure: Vec<Duration>,
    _load_more_task: Task<()>,
}

impl<D> TableState<D>
where
    D: TableDelegate,
{
    /// Create a new TableState with the given delegate.
    pub fn new(delegate: D, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            options: TableOptions::default(),
            delegate,
            col_groups: Vec::new(),
            header_layout: Vec::new(),
            horizontal_scroll_handle: VirtualListScrollHandle::new(),
            vertical_scroll_handle: UniformListScrollHandle::new(),
            selection_mode: SelectionMode::Row,
            selected_row: None,
            right_clicked_row: None,
            last_clicked_col: None,
            right_clicked_cell: None,
            selected_col: None,
            selected_cell: None,
            resizing_col: None,
            prev_table_width: px(0.),
            head_row_width: px(0.),
            last_empty_col_width: px(0.),
            fitted_col: None,
            col_drag_gap: None,
            bounds: Bounds::default(),
            fixed_head_cols_bounds: Bounds::default(),
            visible_range: TableVisibleRange::default(),
            loop_selection: true,
            col_selectable: true,
            row_selectable: true,
            select_on_modifier_click: true,
            cell_selectable: false,
            row_header: true,
            sortable: true,
            col_movable: true,
            col_resizable: true,
            col_fixed: true,
            _load_more_task: Task::ready(()),
            _measure: Vec::new(),
        };

        this.prepare_col_groups(cx);
        this
    }

    /// Returns a reference to the delegate.
    pub fn delegate(&self) -> &D {
        &self.delegate
    }

    /// Returns a mutable reference to the delegate.
    pub fn delegate_mut(&mut self) -> &mut D {
        &mut self.delegate
    }

    /// Set to loop selection, default to true.
    pub fn loop_selection(mut self, loop_selection: bool) -> Self {
        self.loop_selection = loop_selection;
        self
    }

    /// Set to enable/disable column movable, default to true.
    pub fn col_movable(mut self, col_movable: bool) -> Self {
        self.col_movable = col_movable;
        self
    }

    /// Set to enable/disable column resizable, default to true.
    pub fn col_resizable(mut self, col_resizable: bool) -> Self {
        self.col_resizable = col_resizable;
        self
    }

    /// Set to enable/disable column sortable, default true
    pub fn sortable(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }

    /// Set to enable/disable row selectable, default true
    pub fn row_selectable(mut self, row_selectable: bool) -> Self {
        self.row_selectable = row_selectable;
        self
    }

    /// Set whether a row click with the secondary modifier or Shift held
    /// selects the row, default is `true`.
    pub fn select_on_modifier_click(mut self, select_on_modifier_click: bool) -> Self {
        self.select_on_modifier_click = select_on_modifier_click;
        self
    }

    /// Set to enable/disable column selectable, default true
    pub fn col_selectable(mut self, col_selectable: bool) -> Self {
        self.col_selectable = col_selectable;
        self
    }

    /// Set to enable/disable cell selection, default is false.
    ///
    /// When enabled:
    /// - Individual cells become selectable by clicking
    /// - A row header column appears on the left side (can be hidden via [`Self::row_header`])
    /// - Keyboard navigation operates at the cell level
    /// - Cell-specific events (SelectCell, DoubleClickedCell, RightClickedCell) are emitted
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let table_state = cx.new(|cx| {
    ///     TableState::new(delegate, window, cx)
    ///         .cell_selectable(true)  // Enable cell selection
    ///         .row_selectable(true)   // Also allow row selection via row header
    /// });
    /// ```
    pub fn cell_selectable(mut self, cell_selectable: bool) -> Self {
        self.cell_selectable = cell_selectable;
        self
    }

    /// Set whether the row header column is shown, default is `true`.
    ///
    /// Only effective when `cell_selectable` is `true` — otherwise the row header
    /// column is never rendered. Hide it when you want to use the leftmost column
    /// for your own content (e.g. a row index column).
    pub fn row_header(mut self, row_header: bool) -> Self {
        self.row_header = row_header;
        self
    }

    /// When we update columns or rows, we need to refresh the table.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.prepare_col_groups(cx);
    }

    /// Scroll to the row at the given index.
    pub fn scroll_to_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        self.vertical_scroll_handle
            .scroll_to_item(row_ix, ScrollStrategy::Top);
        cx.notify();
    }

    // Scroll to the column at the given index.
    pub fn scroll_to_col(&mut self, col_ix: usize, cx: &mut Context<Self>) {
        let col_ix = col_ix.saturating_sub(self.fixed_left_cols_count());

        // Resolve the offset here instead of deferring it to the virtual list.
        //
        // The header and the rows share `horizontal_scroll_handle`, but only
        // the rows are a `VirtualList`, and a deferred `scroll_to_item` is
        // applied during that list's prepaint — which runs *after* the header
        // has already been rendered and prepainted with the old offset. The
        // header would therefore stay one frame behind the rows.
        match self.horizontal_offset_for_col(col_ix) {
            Some(offset_x) => {
                let mut offset = self.horizontal_scroll_handle.offset();
                offset.x = offset_x;
                self.horizontal_scroll_handle.set_offset(offset);
            }
            // Before the first layout the viewport size is unknown, let the
            // virtual list resolve the offset once it has been laid out.
            None => self
                .horizontal_scroll_handle
                .scroll_to_item(col_ix, ScrollStrategy::Top),
        }

        cx.notify();
    }

    /// The horizontal scroll offset that brings the scrollable (non-fixed)
    /// column at `col_ix` fully into view.
    ///
    /// This mirrors the nearest-edge behavior of [`VirtualListScrollHandle::scroll_to_item`]
    /// with [`ScrollStrategy::Top`]: an already visible column keeps the current
    /// offset, otherwise the column is aligned to the edge it overflows. Both
    /// of those move the offset towards a column that exists, so the result
    /// never runs past the content and needs no extra clamping.
    ///
    /// Returns `None` when the viewport size is not known yet, or when `col_ix`
    /// is out of range.
    fn horizontal_offset_for_col(&self, col_ix: usize) -> Option<Pixels> {
        let viewport_width = self.horizontal_scroll_handle.bounds().size.width;
        if viewport_width <= px(0.) {
            return None;
        }

        let cols = self.col_groups.get(self.fixed_left_cols_count()..)?;
        let col_left: Pixels = cols.get(..col_ix)?.iter().map(|col| col.width).sum();
        let col_right = col_left + cols.get(col_ix)?.width;
        let offset_x = self.horizontal_scroll_handle.offset().x;

        Some(if col_left + offset_x < px(0.) {
            -col_left
        } else if col_right + offset_x > viewport_width {
            viewport_width - col_right
        } else {
            offset_x
        })
    }

    /// Returns the current selection as one value.
    ///
    /// A selected cell reports as [`TableSelection::Cell`] only; use
    /// `selected_cell().map(|(row_ix, _)| row_ix)` when the cell's row is wanted.
    pub fn selection(&self) -> TableSelection {
        if let Some(row_ix) = self.selected_row() {
            TableSelection::Row(row_ix)
        } else if let Some(col_ix) = self.selected_col() {
            TableSelection::Column(col_ix)
        } else if let Some((row_ix, col_ix)) = self.selected_cell() {
            TableSelection::Cell(row_ix, col_ix)
        } else {
            TableSelection::None
        }
    }

    /// Returns the selected row index.
    ///
    /// `Some` only when a row itself is selected; a selected cell does not
    /// count, see [`TableState::selection`].
    pub fn selected_row(&self) -> Option<usize> {
        self.selected_row.filter(|_| self.selection_mode.is_row())
    }

    /// Sets the selected row to the given index.
    pub fn set_selected_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let is_down = match self.selected_row {
            Some(selected_row) => row_ix > selected_row,
            None => true,
        };

        self.selection_mode = SelectionMode::Row;
        self.right_clicked_row = None;
        self.selected_row = Some(row_ix);
        if let Some(row_ix) = self.selected_row {
            self.vertical_scroll_handle.scroll_to_item(
                row_ix,
                if is_down {
                    ScrollStrategy::Bottom
                } else {
                    ScrollStrategy::Top
                },
            );
        }
        cx.emit(TableEvent::SelectRow(row_ix));
        cx.emit(TableEvent::RightClickedRow(None));
        cx.notify();
    }

    /// Returns the row that has been right clicked.
    pub fn right_clicked_row(&self) -> Option<usize> {
        self.right_clicked_row
    }

    /// Returns the column index hit by the last left-click, resolved from
    /// the mouse position against column bounds. `None` if no click occurred
    /// yet or the position didn't match any column.
    pub fn last_clicked_col(&self) -> Option<usize> {
        self.last_clicked_col
    }

    /// Returns the cell that has been right clicked, as `(row_ix, col_ix)`.
    pub fn right_clicked_cell(&self) -> Option<(usize, usize)> {
        self.right_clicked_cell
    }

    /// Set or clear the right-clicked row state.
    ///
    /// Pass `None` to clear — useful when opening a header context menu
    /// to prevent the row context menu from appearing simultaneously.
    pub fn set_right_clicked_row(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        self.right_clicked_row = row;
        cx.notify();
    }

    /// Returns the selected column index.
    ///
    /// `Some` only when a column itself is selected; a selected cell does not
    /// count, see [`TableState::selection`].
    pub fn selected_col(&self) -> Option<usize> {
        self.selected_col
            .filter(|_| self.selection_mode.is_column())
    }

    /// Sets the selected col to the given index.
    pub fn set_selected_col(&mut self, col_ix: usize, cx: &mut Context<Self>) {
        self.selection_mode = SelectionMode::Column;
        self.selected_col = Some(col_ix);
        if let Some(col_ix) = self.selected_col {
            self.scroll_to_col(col_ix, cx);
        }
        cx.emit(TableEvent::SelectColumn(col_ix));
        cx.notify();
    }

    /// Returns the selected cell as `(row_ix, col_ix)`.
    ///
    /// `Some` only when a cell is selected; see [`TableState::selection`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some((row_ix, col_ix)) = table_state.read(cx).selected_cell() {
    ///     println!("Selected cell: ({}, {})", row_ix, col_ix);
    /// }
    /// ```
    pub fn selected_cell(&self) -> Option<(usize, usize)> {
        self.selected_cell.filter(|_| self.selection_mode.is_cell())
    }

    /// Sets the selected cell to the given row and column indices.
    ///
    /// This method:
    /// - Switches the table to cell selection mode
    /// - Scrolls to make the cell visible (centered vertically)
    /// - Emits a [`TableEvent::SelectCell`] event
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Select the cell at row 5, column 3
    /// table_state.update(cx, |state, cx| {
    ///     state.set_selected_cell(5, 3, cx);
    /// });
    /// ```
    pub fn set_selected_cell(&mut self, row_ix: usize, col_ix: usize, cx: &mut Context<Self>) {
        self.selection_mode = SelectionMode::Cell;
        self.selected_cell = Some((row_ix, col_ix));
        self.right_clicked_cell = None;

        // Scroll to the cell
        self.vertical_scroll_handle
            .scroll_to_item(row_ix, ScrollStrategy::Center);
        self.scroll_to_col(col_ix, cx);

        cx.emit(TableEvent::SelectCell(row_ix, col_ix));
        cx.notify();
    }

    /// Sets the selection as one value; [`TableSelection::None`] clears it.
    ///
    /// Scrolls and emits exactly as the matching `set_selected_*` or
    /// [`TableState::clear_selection`] call would.
    pub fn set_selection(&mut self, selection: TableSelection, cx: &mut Context<Self>) {
        match selection {
            TableSelection::None => self.clear_selection(cx),
            TableSelection::Row(row_ix) => self.set_selected_row(row_ix, cx),
            TableSelection::Column(col_ix) => self.set_selected_col(col_ix, cx),
            TableSelection::Cell(row_ix, col_ix) => self.set_selected_cell(row_ix, col_ix, cx),
        }
    }

    /// Clear the selection of the table.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selection_mode = SelectionMode::Row;
        self.selected_row = None;
        self.selected_col = None;
        self.selected_cell = None;
        cx.emit(TableEvent::ClearSelection);
        cx.notify();
    }

    /// Returns the visible range of the rows and columns.
    ///
    /// See [`TableVisibleRange`].
    pub fn visible_range(&self) -> &TableVisibleRange {
        &self.visible_range
    }

    /// Dump the header row of the table.
    ///
    /// Batched exporters can read the headers once with this, then stream the
    /// rows with [`Self::dump_range`].
    pub fn headers(&self, cx: &App) -> Vec<String> {
        let columns_count = self.delegate.columns_count(cx);
        let mut headers = Vec::with_capacity(columns_count);
        for col_ix in 0..columns_count {
            let column = self.delegate.column(col_ix, cx);
            headers.push(column.name.to_string());
        }

        headers
    }

    /// Dump table data.
    ///
    /// Returns a tuple of (headers, rows) where each row is a vector of cell values.
    ///
    /// This materializes the complete table in memory. For large tables, prefer
    /// [`Self::dump_range`] and process rows in batches.
    pub fn dump(&self, cx: &App) -> (Vec<String>, Vec<Vec<String>>) {
        self.dump_range(0..self.delegate.rows_count(cx), cx)
    }

    /// Dump table data for the specified row range.
    ///
    /// Returns the same `(headers, rows)` shape as [`Self::dump`], with only
    /// the rows inside the clamped range.
    ///
    /// The requested range is clamped to the table's current row count. For
    /// large tables, callers can invoke this repeatedly with bounded ranges.
    pub fn dump_range(&self, range: Range<usize>, cx: &App) -> (Vec<String>, Vec<Vec<String>>) {
        let columns_count = self.delegate.columns_count(cx);
        let rows_count = self.delegate.rows_count(cx);
        let start = range.start.min(rows_count);
        let end = range.end.min(rows_count).max(start);

        let mut rows = Vec::with_capacity(end - start);
        for row_ix in start..end {
            let mut row = Vec::with_capacity(columns_count);
            for col_ix in 0..columns_count {
                row.push(self.delegate.cell_text(row_ix, col_ix, cx));
            }
            rows.push(row);
        }

        (self.headers(cx), rows)
    }

    /// Re-compute the header layout from the current delegate.
    ///
    /// Call this after changing delegate state that affects `group_headers`.
    pub fn refresh_header_layout(&mut self, cx: &mut Context<Self>) {
        self.update_header_layout(cx);
        cx.notify();
    }

    fn prepare_col_groups(&mut self, cx: &mut Context<Self>) {
        self.set_col_groups_from_delegate(cx);
        cx.notify();
    }

    /// Rebuild col_groups from the delegate without calling cx.notify().
    /// Use this when updating column widths during an already-in-progress render
    /// to avoid triggering an extra render cycle (which causes resize flicker).
    pub fn set_col_groups_from_delegate(&mut self, cx: &mut Context<Self>) {
        self.col_groups = (0..self.delegate.columns_count(cx))
            .map(|col_ix| {
                let column = self.delegate().column(col_ix, cx);
                ColGroup {
                    width: column.width,
                    preferred: column.width,
                    bounds: Bounds::default(),
                    original_min_width: column.min_width,
                    column,
                }
            })
            .collect();

        self.update_header_layout(cx);
    }

    /// Lays out the columns of a table with an `auto_width` column for this
    /// frame, and returns whether a width changed.
    ///
    /// Called at the start of each render, with the room measured on the
    /// previous frame ([`Self::fill_room`]); see [`Self::layout_fill_widths`]
    /// for what it does with it. While idle the auto columns are drawn with
    /// `flex_1` instead ([`Self::auto_columns_flex`]), which comes to the same
    /// widths without trailing a pane resize by a frame. Both agree only
    /// because the room here is the room flex has: before 2026-10-21 it was the
    /// table's whole width, borders and the trailing empty column included, so
    /// a drag's first frame widened the auto column by those 14 px and its
    /// release narrowed it again.
    fn update_auto_widths(&mut self) -> bool {
        let table_width = self.bounds.size.width;

        // Skip on the very first frame (bounds not yet measured).
        if table_width <= px(0.) || !self.has_auto_width_cols() {
            return false;
        }

        self.prev_table_width = table_width;
        let room = self.fill_room();
        self.layout_fill_widths(room)
    }

    fn has_auto_width_cols(&self) -> bool {
        self.col_groups.iter().any(|c| c.column.auto_width)
    }

    /// The room the columns share: the flat header row less its trailing
    /// empty column, plus the fixed columns beside it. The table's own width
    /// until the header has been measured, and always for a grouped header,
    /// which is not.
    fn fill_room(&self) -> Pixels {
        if self.head_row_width <= px(0.) {
            return self.bounds.size.width;
        }
        let fixed = if self.fixed_left_cols_count() > 0 {
            self.fixed_head_cols_bounds.size.width
        } else {
            px(0.)
        };
        (self.head_row_width - self.last_empty_col_width + fixed).max(px(0.))
    }

    /// Fork: sets each column's `width` from its `preferred` width so that
    /// all of them fill `room`, and returns whether a width changed.
    ///
    /// The other columns keep their preferred widths and the `auto_width`
    /// columns share what is left, each at least its `min_width`. When that
    /// does not fit, the resizable columns give way in proportion to their
    /// preferred widths, none below its own `min_width`; a column that cannot
    /// be resized keeps its width. Their preferred widths stay as they are, so
    /// they come back as soon as the room does.
    fn layout_fill_widths(&mut self, room: Pixels) -> bool {
        let auto_count = self
            .col_groups
            .iter()
            .filter(|c| c.column.auto_width)
            .count();
        if auto_count == 0 {
            return false;
        }
        let auto_min: Pixels = self
            .col_groups
            .iter()
            .filter(|c| c.column.auto_width)
            .map(|c| c.column.min_width)
            .sum();
        let shrinks = |c: &ColGroup| !c.column.auto_width && c.is_resizable();
        let rigid: Pixels = self
            .col_groups
            .iter()
            .filter(|c| !c.column.auto_width && !c.is_resizable())
            .map(|c| c.preferred)
            .sum();
        let budget = (room - auto_min - rigid).max(px(0.));

        let mut widths: Vec<Pixels> = self.col_groups.iter().map(|c| c.preferred).collect();
        let wanted: Pixels = self
            .col_groups
            .iter()
            .filter(|c| shrinks(c))
            .map(|c| c.preferred)
            .sum();
        if wanted > budget {
            // Shrink in proportion; a column that reaches its minimum stays
            // there and the others share the rest, until none is below it.
            let mut at_min = vec![false; self.col_groups.len()];
            loop {
                let held: Pixels = self
                    .col_groups
                    .iter()
                    .zip(&at_min)
                    .filter(|(c, at_min)| shrinks(c) && **at_min)
                    .map(|(c, _)| c.column.min_width)
                    .sum();
                let free: Pixels = self
                    .col_groups
                    .iter()
                    .zip(&at_min)
                    .filter(|(c, at_min)| shrinks(c) && !**at_min)
                    .map(|(c, _)| c.preferred)
                    .sum();
                let scale = if free > px(0.) {
                    ((budget - held).max(px(0.)) / free).min(1.)
                } else {
                    0.
                };
                let mut settled = true;
                for (ix, col) in self.col_groups.iter().enumerate() {
                    if !shrinks(col) {
                        continue;
                    }
                    if at_min[ix] {
                        widths[ix] = col.column.min_width;
                        continue;
                    }
                    let width = col.preferred * scale;
                    if width < col.column.min_width {
                        at_min[ix] = true;
                        settled = false;
                    }
                    widths[ix] = width.max(col.column.min_width);
                }
                if settled {
                    break;
                }
            }
        }

        let others: Pixels = self
            .col_groups
            .iter()
            .zip(&widths)
            .filter(|(c, _)| !c.column.auto_width)
            .map(|(_, width)| *width)
            .sum();
        let per_auto = (room - others).max(px(0.)) / auto_count as f32;
        let mut changed = false;
        for (col, width) in self.col_groups.iter_mut().zip(widths) {
            let width = if col.column.auto_width {
                per_auto.max(col.column.min_width)
            } else {
                width
            };
            if (col.width - width).abs() > px(0.01) {
                col.width = width;
                changed = true;
            }
        }
        changed
    }

    /// Fork: the column a drag of column `ix`'s right edge trades width with
    /// in a table with an `auto_width` column — the next one to the right
    /// that is resizable or fills. `None` when there is none, or when both
    /// fill: the edge between two fill columns has nothing to move.
    fn fill_partner(&self, ix: usize) -> Option<usize> {
        let this = self.col_groups.get(ix)?;
        let partner = (ix + 1..self.col_groups.len()).find(|&next| {
            self.col_groups
                .get(next)
                .is_some_and(|c| c.column.auto_width || c.is_resizable())
        })?;
        let partner_fills = self
            .col_groups
            .get(partner)
            .is_some_and(|c| c.column.auto_width);
        (!(this.column.auto_width && partner_fills)).then_some(partner)
    }

    /// Fork: moves the right edge of column `ix` so that the column becomes
    /// `target` wide, in a table with an `auto_width` column, and returns
    /// whether a width changed.
    ///
    /// The width comes from or goes to [`Self::fill_partner`], so the edge
    /// stays under the pointer and no other edge moves; an `auto_width`
    /// column on either side takes part by filling, as it always does. Both
    /// columns stay within their `min_width` (the original one) and
    /// `max_width`. Upstream's resize instead pushes the columns on the right
    /// outward, which a table that fills its width can only show by letting
    /// the auto column take the change back on release.
    fn drag_col_edge(&mut self, ix: usize, target: Pixels, cx: &mut Context<Self>) -> bool {
        let Some(partner) = self.fill_partner(ix) else {
            return false;
        };
        let (Some(left), Some(right)) = (self.col_groups.get(ix), self.col_groups.get(partner))
        else {
            return false;
        };
        let range = |c: &ColGroup| {
            let max = if c.column.auto_width {
                px(f32::MAX)
            } else {
                c.column.max_width
            };
            (c.original_min_width, max)
        };
        let (left_min, left_max) = range(left);
        let (right_min, right_max) = range(right);
        let lowest = (left_min - left.width).max(right.width - right_max);
        let highest = (left_max - left.width).min(right.width - right_min);
        if lowest > highest {
            return false;
        }
        let delta = (target - left.width).max(lowest).min(highest);
        if delta.abs() < px(0.01) {
            return false;
        }
        let (left_auto, left_width) = (left.column.auto_width, left.width);
        let (right_auto, right_width) = (right.column.auto_width, right.width);
        if !left_auto {
            self.col_groups[ix].preferred = left_width + delta;
        }
        if !right_auto {
            self.col_groups[partner].preferred = right_width - delta;
        }
        let room = self.fill_room();
        self.layout_fill_widths(room);
        self.update_header_layout(cx);
        cx.notify();
        true
    }

    /// Whether auto-width columns have been computed at least once.
    /// On the very first frame, bounds aren't available yet.
    fn auto_widths_ready(&self) -> bool {
        self.prev_table_width > px(0.)
    }

    /// Whether auto-width (`flex_grow`) columns should render with live
    /// `flex_1()` this frame instead of their computed pixel width.
    ///
    /// Flex is the default: the layout engine sizes the auto column every frame
    /// to fill remaining space, so a window/pane resize has NO one-frame lag
    /// (the computed width comes from the *previous* frame's bounds). This is
    /// the long-standing behaviour that clamped correctly — as long as BOTH the
    /// row wrapper and the inner cell flex together; if only the cell flexes,
    /// the non-flex wrapper content-sizes around it and the column grows to the
    /// widest cell's content.
    ///
    /// The one time we must NOT flex is while a column is actively resized:
    /// there the auto column must sit at its computed pixel width so the drag
    /// math is stable and resizing a *neighbour* doesn't make the lone flex
    /// column greedily absorb the change (a "mirrored" resize).
    fn auto_columns_flex(&self) -> bool {
        self.resizing_col.is_none()
    }

    fn update_header_layout(&mut self, cx: &mut Context<Self>) {
        let group_rows = self.delegate.group_headers(cx);

        let mut layout = match group_rows.as_ref() {
            Some(rows) => Vec::with_capacity(rows.len() + 1),
            None => Vec::with_capacity(1),
        };

        if let Some(group_rows) = group_rows {
            for row in group_rows {
                let mut cell_row = Vec::with_capacity(row.len());
                let mut current_leaf_ix = 0;
                for group in row {
                    let mut width = px(0.);
                    let start_leaf_col_ix = current_leaf_ix;
                    for i in 0..group.span {
                        if current_leaf_ix + i < self.col_groups.len() {
                            width += self.col_groups[current_leaf_ix + i].width;
                        }
                    }
                    current_leaf_ix += group.span;
                    cell_row.push(HeaderCell {
                        label: group.label.clone(),
                        width,
                        col_span: group.span,
                        is_leaf: false,
                        leaf_col_ix: None,
                        start_leaf_col_ix,
                    });
                }
                layout.push(cell_row);
            }
        }

        let mut leaf_row = Vec::with_capacity(self.col_groups.len());
        for (ix, group) in self.col_groups.iter().enumerate() {
            leaf_row.push(HeaderCell {
                label: group.column.name.clone(),
                width: group.width,
                col_span: 1,
                is_leaf: true,
                leaf_col_ix: Some(ix),
                start_leaf_col_ix: ix,
            });
        }
        layout.push(leaf_row);

        self.header_layout = layout;
    }

    fn fixed_left_cols_count(&self) -> usize {
        if !self.col_fixed {
            return 0;
        }

        self.col_groups
            .iter()
            .filter(|col| col.column.fixed == Some(ColumnFixed::Left))
            .count()
    }

    /// One header row. An unset header height matches the body rows.
    fn header_row_height(&self) -> Pixels {
        self.options
            .header_row_height
            .unwrap_or(self.options.size.table_row_height())
    }

    /// Every header row, including column-group rows. An empty layout still
    /// reserves one row, matching the vertical scrollbar's top inset.
    fn header_band_height(&self) -> Pixels {
        self.header_row_height() * self.header_layout.len().max(1) as f32
    }

    fn page_item_count(&self) -> usize {
        let row_height = self.options.size.table_row_height();
        let body_height = (self.bounds.size.height - self.header_band_height()).max(px(0.));
        let count = (body_height / row_height).floor() as usize;
        count.max(1)
    }

    fn on_row_right_click(
        &mut self,
        _: &MouseDownEvent,
        row_ix: Option<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_clicked_row = row_ix;
        self.right_clicked_cell = None;
        cx.emit(TableEvent::RightClickedRow(row_ix));
    }

    fn on_cell_right_click(
        &mut self,
        _: &MouseDownEvent,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cell_selectable {
            return;
        }

        cx.stop_propagation();
        self.right_clicked_cell = Some((row_ix, col_ix));
        self.right_clicked_row = None;
        cx.emit(TableEvent::RightClickedCell(row_ix, col_ix));
    }

    fn on_row_left_click(
        &mut self,
        e: &ClickEvent,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.row_selectable {
            return;
        }

        cx.stop_propagation();
        // If a right-click row was active, this click is dismissing the
        // context menu — don't count as "was already selected" to avoid
        // triggering click-to-rename.
        let was_already_selected =
            self.right_clicked_row.is_none() && self.selected_row == Some(row_ix);

        // Determine which column was clicked from the mouse position
        let click_x = e.position().x;
        let col_ix = self.col_groups.iter().position(|cg| {
            let left = cg.bounds.origin.x;
            let right = left + cg.bounds.size.width;
            click_x >= left && click_x < right
        });
        self.last_clicked_col = col_ix;

        if self.selects_row_on_click(e) {
            self.set_selected_row(row_ix, cx);
        }

        let click_count = e.click_count();
        if click_count == 2 {
            cx.emit(TableEvent::DoubleClickedRow(row_ix));
        }

        cx.emit(TableEvent::RowClick {
            row_ix,
            col_ix,
            click_count,
            was_already_selected,
        });
    }

    /// Whether a click on a row selects it, see `select_on_modifier_click`.
    fn selects_row_on_click(&self, e: &ClickEvent) -> bool {
        let modifiers = e.modifiers();
        self.select_on_modifier_click || !(modifiers.secondary() || modifiers.shift)
    }

    fn on_col_head_click(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        if !self.col_selectable {
            return;
        }

        let Some(col_group) = self.col_groups.get(col_ix) else {
            return;
        };

        if !col_group.column.selectable {
            return;
        }

        self.set_selected_col(col_ix, cx)
    }

    fn on_cell_click(
        &mut self,
        e: &ClickEvent,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cell_selectable {
            return;
        }

        cx.stop_propagation();

        let is_double_click = e.click_count() == 2;

        // When the row header column is hidden, a single click on the
        // already-selected cell escalates the selection to the entire row —
        // giving users a way to pick rows without the dedicated header column.
        // Double-clicks are passed through to `DoubleClickedCell` and never
        // trigger the escalation.
        let is_reselect = self.selected_cell() == Some((row_ix, col_ix));
        let should_escalate_to_row = !self.row_header
            && self.row_selectable
            && is_reselect
            && !is_double_click
            && self.selects_row_on_click(e);
        if should_escalate_to_row {
            self.set_selected_row(row_ix, cx);
            return;
        }

        self.set_selected_cell(row_ix, col_ix, cx);

        if e.click_count() == 2 {
            cx.emit(TableEvent::DoubleClickedCell(row_ix, col_ix));
        }
    }

    fn has_selection(&self) -> bool {
        self.selection() != TableSelection::None
    }

    pub(super) fn action_cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_selection() {
            self.clear_selection(cx);
            return;
        }
        cx.propagate();
    }

    pub(super) fn action_select_prev(
        &mut self,
        _: &SelectUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows_count = self.delegate.rows_count(cx);
        if rows_count < 1 {
            return;
        }

        // Cell selection mode: move up within the same column
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let new_row = if row_ix > 0 {
                    row_ix.saturating_sub(1)
                } else if self.loop_selection {
                    rows_count.saturating_sub(1)
                } else {
                    row_ix
                };
                self.set_selected_cell(new_row, col_ix, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Row selection mode
        if !self.row_selectable {
            return;
        }
        let mut selected_row = self.selected_row.unwrap_or(0);
        if selected_row > 0 {
            selected_row = selected_row.saturating_sub(1);
        } else {
            if self.loop_selection {
                selected_row = rows_count.saturating_sub(1);
            }
        }

        self.set_selected_row(selected_row, cx);
    }

    pub(super) fn action_select_next(
        &mut self,
        _: &SelectDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows_count = self.delegate.rows_count(cx);
        if rows_count < 1 {
            return;
        }

        // Cell selection mode: move down within the same column
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let new_row = if row_ix < rows_count.saturating_sub(1) {
                    row_ix + 1
                } else if self.loop_selection {
                    0
                } else {
                    row_ix
                };
                self.set_selected_cell(new_row, col_ix, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Row selection mode
        if !self.row_selectable {
            return;
        }
        let selected_row = match self.selected_row {
            Some(selected_row) if selected_row < rows_count.saturating_sub(1) => selected_row + 1,
            Some(selected_row) => {
                if self.loop_selection {
                    0
                } else {
                    selected_row
                }
            }
            _ => 0,
        };

        self.set_selected_row(selected_row, cx);
    }

    pub(super) fn action_select_first_column(
        &mut self,
        _: &SelectFirst,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Cell selection mode: move to first cell in current row
        if self.selection_mode.is_cell() {
            if let Some((row_ix, _)) = self.selected_cell {
                self.set_selected_cell(row_ix, 0, cx);
            } else {
                // No cell selected, select first cell of first row
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Row selection mode: Home goes to the first row, as in a list
        if self.selection_mode.is_row() && self.row_selectable {
            if self.delegate.rows_count(cx) > 0 {
                self.set_selected_row(0, cx);
            }
            return;
        }

        // Column selection mode
        self.set_selected_col(0, cx);
    }

    pub(super) fn action_select_last_column(
        &mut self,
        _: &SelectLast,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let columns_count = self.delegate.columns_count(cx);

        // Cell selection mode: move to last cell in current row
        if self.selection_mode.is_cell() {
            if let Some((row_ix, _)) = self.selected_cell {
                self.set_selected_cell(row_ix, columns_count.saturating_sub(1), cx);
            } else {
                // No cell selected, select last cell of first row
                self.set_selected_cell(0, columns_count.saturating_sub(1), cx);
            }
            return;
        }

        // Row selection mode: End goes to the last row, as in a list
        if self.selection_mode.is_row() && self.row_selectable {
            let rows_count = self.delegate.rows_count(cx);
            if rows_count > 0 {
                self.set_selected_row(rows_count - 1, cx);
            }
            return;
        }

        // Column selection mode
        self.set_selected_col(columns_count.saturating_sub(1), cx);
    }

    pub(super) fn action_select_page_up(
        &mut self,
        _: &SelectPageUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let step = self.page_item_count();

        // Cell selection mode: move up by page within the same column
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let target = row_ix.saturating_sub(step);
                self.set_selected_cell(target, col_ix, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Row selection mode
        if !self.row_selectable {
            return;
        }
        let current = self.selected_row.unwrap_or(0);
        let target = current.saturating_sub(step);
        self.set_selected_row(target, cx);
    }

    pub(super) fn action_select_page_down(
        &mut self,
        _: &SelectPageDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows_count = self.delegate.rows_count(cx);
        if rows_count == 0 {
            return;
        }

        let step = self.page_item_count();

        // Cell selection mode: move down by page within the same column
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let max_row = rows_count.saturating_sub(1);
                let target = (row_ix + step).min(max_row);
                self.set_selected_cell(target, col_ix, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Row selection mode
        if !self.row_selectable {
            return;
        }
        let current = self.selected_row.unwrap_or(0);
        let max_row = rows_count.saturating_sub(1);
        let target = (current + step).min(max_row);
        self.set_selected_row(target, cx);
    }

    pub(super) fn action_select_prev_col(
        &mut self,
        _: &SelectPrevColumn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let columns_count = self.delegate.columns_count(cx);

        // Cell selection mode: move left within the same row
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let new_col = if col_ix > 0 {
                    col_ix.saturating_sub(1)
                } else if self.loop_selection {
                    columns_count.saturating_sub(1)
                } else {
                    col_ix
                };
                self.set_selected_cell(row_ix, new_col, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Column selection mode
        let mut selected_col = self.selected_col.unwrap_or(0);
        if selected_col > 0 {
            selected_col = selected_col.saturating_sub(1);
        } else {
            if self.loop_selection {
                selected_col = columns_count.saturating_sub(1);
            }
        }
        self.set_selected_col(selected_col, cx);
    }

    pub(super) fn action_select_next_col(
        &mut self,
        _: &SelectNextColumn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let columns_count = self.delegate.columns_count(cx);

        // Cell selection mode: move right within the same row
        if self.selection_mode.is_cell() {
            if let Some((row_ix, col_ix)) = self.selected_cell {
                let new_col = if col_ix < columns_count.saturating_sub(1) {
                    col_ix + 1
                } else if self.loop_selection {
                    0
                } else {
                    col_ix
                };
                self.set_selected_cell(row_ix, new_col, cx);
            } else {
                // No cell selected, select first cell
                self.set_selected_cell(0, 0, cx);
            }
            return;
        }

        // Column selection mode
        let mut selected_col = self.selected_col.unwrap_or(0);
        if selected_col < columns_count.saturating_sub(1) {
            selected_col += 1;
        } else {
            if self.loop_selection {
                selected_col = 0;
            }
        }

        self.set_selected_col(selected_col, cx);
    }

    /// Scroll table when mouse position is near the edge of the table bounds.
    fn scroll_table_by_col_resizing(
        &mut self,
        mouse_position: Point<Pixels>,
        col_group: &ColGroup,
    ) {
        // Do nothing if pos out of the table bounds right for avoid scroll to the right.
        if mouse_position.x > self.bounds.right() {
            return;
        }

        let mut offset = self.horizontal_scroll_handle.offset();
        let col_bounds = col_group.bounds;

        if mouse_position.x < self.bounds.left()
            && col_bounds.right() < self.bounds.left() + px(20.)
        {
            offset.x += px(1.);
        } else if mouse_position.x > self.bounds.right()
            && col_bounds.right() > self.bounds.right() - px(20.)
        {
            offset.x -= px(1.);
        }

        self.horizontal_scroll_handle.set_offset(offset);
    }

    /// The width column `col_ix` is drawn at now, or `None` past the last
    /// column.
    ///
    /// This is the table's own width, which follows a drag or a fit as it
    /// happens; the delegate's [`Column::width`] is read again only on
    /// [`Self::refresh`].
    pub fn col_width(&self, col_ix: usize) -> Option<Pixels> {
        self.col_groups.get(col_ix).map(|group| group.width)
    }

    /// Fits column `col_ix` to its content, as a double-click on its resize
    /// handle does.
    ///
    /// The width is [`TableDelegate::fit_column`] plus the cell padding,
    /// clamped to the column's `min_width` and `max_width`, and
    /// [`TableEvent::ColumnWidthsChanged`] is emitted when it changed. Returns
    /// whether the delegate gave a width: `false` when the column cannot be
    /// resized or the delegate answered `None`.
    pub fn fit_col(&mut self, col_ix: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_col_resizable(col_ix) {
            return false;
        }
        let Some(content) = self.delegate.fit_column(col_ix, window, cx) else {
            return false;
        };
        let padding = self
            .col_groups
            .get(col_ix)
            .and_then(|group| group.column.paddings)
            .unwrap_or_else(|| self.options.size.table_cell_padding());
        // Layout rounds a width to the nearest device pixel, and text even a
        // fraction of a pixel wider than its cell is truncated, so round every
        // part up to a whole device pixel.
        let scale_factor = window.scale_factor();
        let device_pixels = |length: Pixels| (f32::from(length) * scale_factor).ceil();
        let mut content_pixels = device_pixels(content);
        // Dividing back can land one ulp below the content at a fractional
        // scale, and the text is cut for that ulp too.
        if px(content_pixels / scale_factor) < content {
            content_pixels += 1.;
        }
        let width = content_pixels + device_pixels(padding.left) + device_pixels(padding.right);
        if self.resize_cols(col_ix, px(width / scale_factor), window, cx) {
            let new_widths = self.col_groups.iter().map(|g| g.width).collect();
            cx.emit(TableEvent::ColumnWidthsChanged(new_widths));
        }
        true
    }

    /// The `ix`` is the index of the col to resize,
    /// and the `size` is the new size for the col.
    ///
    /// Returns whether the width changed.
    fn resize_cols(
        &mut self,
        ix: usize,
        size: Pixels,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.col_resizable {
            return false;
        }

        let mut changed = false;
        if let Some(col_group) = self.col_groups.get_mut(ix) {
            if col_group.is_resizable() {
                // Clamp against the original min_width (not the potentially raised one)
                // so the user can always drag back down to the original minimum.
                // `max`/`min` rather than `clamp`: an inverted range resolves to
                // `max_width` instead of panicking (upstream #3472).
                let new_width = size
                    .max(col_group.original_min_width)
                    .min(col_group.column.max_width);

                if col_group.width != new_width {
                    col_group.width = new_width;
                    col_group.preferred = new_width;
                    // For auto_width columns, update min_width so the column doesn't
                    // auto-shrink below the user's chosen size. If the user drags
                    // smaller, min_width follows down (but not below original).
                    if col_group.column.auto_width {
                        col_group.column.min_width = new_width;
                    }
                    changed = true;
                }
            }
        }

        if changed {
            if self.has_auto_width_cols() {
                let room = self.fill_room();
                self.layout_fill_widths(room);
            }
            self.update_header_layout(cx);
            cx.notify();
        }
        changed
    }

    fn perform_sort(&mut self, col_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sortable {
            return;
        }

        let sort = self.col_groups.get(col_ix).and_then(|g| g.column.sort);
        if sort.is_none() {
            return;
        }

        let sort = sort.unwrap();
        let sort = match sort {
            ColumnSort::Ascending => ColumnSort::Default,
            ColumnSort::Descending => ColumnSort::Ascending,
            ColumnSort::Default => ColumnSort::Descending,
        };

        for (ix, col_group) in self.col_groups.iter_mut().enumerate() {
            if ix == col_ix {
                col_group.column.sort = Some(sort);
            } else {
                if col_group.column.sort.is_some() {
                    col_group.column.sort = Some(ColumnSort::Default);
                }
            }
        }

        self.delegate_mut().perform_sort(col_ix, sort, window, cx);

        cx.notify();
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if col_ix == to_ix {
            return;
        }

        self.delegate.move_column(col_ix, to_ix, window, cx);
        let col_group = self.col_groups.remove(col_ix);
        self.col_groups.insert(to_ix, col_group);

        cx.emit(TableEvent::MoveColumn(col_ix, to_ix));
        cx.notify();
    }

    /// Resolve the insertion gap for a column-header drag at the window
    /// coordinate `x`, or `None` when dropping there would not move the
    /// dragged column at `drag_col_ix`.
    fn drag_gap_at(&self, x: Pixels, drag_col_ix: usize) -> Option<usize> {
        let fixed_count = self.fixed_left_cols_count();

        // A column can only be reordered within its own region: rendering
        // pins the first `fixed_count` columns, so a cross-region move would
        // change which columns are pinned without updating their `fixed`
        // flags.
        let drag_in_fixed = drag_col_ix < fixed_count;
        let pointer_in_fixed = fixed_count > 0 && x < self.fixed_head_cols_bounds.right();
        if drag_in_fixed != pointer_in_fixed {
            return None;
        }

        // Columns scrolled beneath the fixed region keep stale bounds, so
        // resolve `x` against the fixed columns alone when it falls in that
        // region, and against the visible scrollable columns otherwise.
        let candidates = if pointer_in_fixed {
            0..fixed_count
        } else {
            self.calculate_visible_leaf_col_range(fixed_count).0
        };

        // The gap sits after the last candidate column whose center is left of `x`.
        let mut gap = candidates.start;
        for ix in candidates {
            if x < self.col_groups[ix].bounds.center().x {
                break;
            }
            gap = ix + 1;
        }

        // No gap if dropping there would put the dragged column back to
        // where it already is.
        if gap == drag_col_ix || gap == drag_col_ix + 1 {
            None
        } else {
            Some(gap)
        }
    }

    /// Dispatch delegate's `load_more` method when the visible range is near the end.
    fn load_more_if_need(
        &mut self,
        rows_count: usize,
        visible_end: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let threshold = self.delegate.load_more_threshold();
        // Securely handle subtract logic to prevent attempt to subtract with overflow
        if visible_end >= rows_count.saturating_sub(threshold) {
            if !self.delegate.has_more(cx) {
                return;
            }

            self._load_more_task = cx.spawn_in(window, async move |view, window| {
                _ = view.update_in(window, |view, window, cx| {
                    view.delegate.load_more(window, cx);
                });
            });
        }
    }

    fn update_visible_range_if_need(
        &mut self,
        visible_range: Range<usize>,
        items_count: usize,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Skip when visible range is only 1 item, unless there is at most 1 item.
        // The visual_list will use first item to measure.
        if visible_range.len() <= 1 && items_count > 1 {
            return;
        }

        // Stripe filler rows are rendered past the last row; never report them.
        let end = visible_range.end.min(items_count);
        let visible_range = visible_range.start.min(end)..end;
        // A range wholly past the last item is stale; the list scrolls back next frame.
        if visible_range.is_empty() && items_count > 0 {
            return;
        }

        if axis == Axis::Vertical {
            if self.visible_range.rows == visible_range {
                return;
            }
            self.delegate_mut()
                .visible_rows_changed(visible_range.clone(), window, cx);
            self.visible_range.rows = visible_range;
        } else {
            if self.visible_range.cols == visible_range {
                return;
            }
            self.delegate_mut()
                .visible_columns_changed(visible_range.clone(), window, cx);
            self.visible_range.cols = visible_range;
        }
    }

    fn render_cell(
        &self,
        _row_ix: Option<usize>,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Div {
        self.render_cell_inner(col_ix)
    }

    /// Retained as a thin alias of `render_cell` for the body row path.
    fn render_cell_flex(&self, col_ix: usize) -> Div {
        self.render_cell_inner(col_ix)
    }

    fn render_cell_inner(&self, col_ix: usize) -> Div {
        let Some(col_group) = self.col_groups.get(col_ix) else {
            return div();
        };

        let col_width = col_group.width;
        let col_padding = col_group.column.paddings;
        // Auto_width columns flex while idle and on the first frame, and render
        // at their computed pixel width during a column drag
        // (`auto_columns_flex`); `update_auto_widths` computes the width flex
        // arrives at, so the switch does not move anything.
        let use_flex =
            col_group.column.auto_width && (self.auto_columns_flex() || !self.auto_widths_ready());

        div()
            .when(use_flex, |this| {
                this.flex_1().min_w(col_group.column.min_width)
            })
            .when(!use_flex, |this| this.w(col_width).flex_shrink_0())
            .h_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .table_cell_size(self.options.size)
            // Text alignment is inherited, so this aligns the header name and
            // the delegate's cell text without changing how cells lay out.
            .map(|this| match col_group.column.align {
                TextAlign::Center => this.text_center(),
                TextAlign::Right => this.text_right(),
                _ => this,
            })
            .map(|this| match col_padding {
                Some(padding) => this
                    .pl(padding.left)
                    .pr(padding.right)
                    .pt(padding.top)
                    .pb(padding.bottom),
                None => this,
            })
    }

    /// Show Column selection style, when the column is selected and the selection state is Column.
    /// Note: When a cell is selected, column selection style is not shown.
    fn render_col_wrap(
        &self,
        _row_ix: Option<usize>,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let el = h_flex().h_full();
        let selectable = self.col_selectable
            && self
                .col_groups
                .get(col_ix)
                .map(|col_group| col_group.column.selectable)
                .unwrap_or(false);

        // `selected_col()` is `None` outside column mode, so a selected cell
        // never leaves its column highlighted.
        if selectable && self.selected_col() == Some(col_ix) {
            el.bg(cx.theme().tokens.table_active)
        } else {
            el
        }
    }

    /// The other half of that band, for the boundary on the *left* of column
    /// `ix` — the one owned by column `ix - 1`.
    ///
    /// It is positioned absolutely so that reaching back over the boundary
    /// costs the header row no width, and it paints after this column's own
    /// cell so that it, and not a column reorder, receives the drag.
    fn render_leading_resize_handle(
        &self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(prev) = ix.checked_sub(1).filter(|ix| self.is_col_resizable(*ix)) else {
            return div().into_any_element();
        };

        self.resize_handle_band(prev, ("resizable-handle-leading", ix).into(), cx)
            .absolute()
            .top_0()
            .left_0()
            .h_full()
            .w(HANDLE_PADDING)
            .into_any_element()
    }

    /// The half of column `ix`'s resize handle that lies inside column `ix`,
    /// along with the hairline that marks the boundary.
    ///
    /// It is [`HANDLE_PADDING`] wide, like the half on the far side, so the
    /// grab area is symmetric about the boundary. The negative margin keeps
    /// the band's net contribution to the header row zero, and `justify_end`
    /// pins the hairline to the column edge.
    fn render_resize_handle(
        &self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !self.is_col_resizable(ix) {
            return div().into_any_element();
        }

        let group_id = SharedString::from(format!("resizable-handle:{}", ix));

        self.resize_handle_band(ix, ("resizable-handle", ix).into(), cx)
            .group(group_id.clone())
            .h_full()
            .w(HANDLE_PADDING)
            .ml(-HANDLE_PADDING)
            .justify_end()
            .items_center()
            .child(
                div()
                    .h_full()
                    .justify_center()
                    .bg(cx.theme().table_row_border)
                    .group_hover(&group_id, |this| this.bg(cx.theme().border).h_full())
                    .w(px(1.)),
            )
            .into_any_element()
    }

    fn is_col_resizable(&self, ix: usize) -> bool {
        self.col_resizable
            && self
                .col_groups
                .get(ix)
                .map(|col| col.is_resizable())
                .unwrap_or(false)
            // Fork: in a table that fills its width an edge with nothing to
            // trade with (the last column's, against the empty column) has no
            // handle.
            && (!self.has_auto_width_cols() || self.fill_partner(ix).is_some())
    }

    /// The interactive band that drags the boundary on the right of column
    /// `ix`, carrying no styling that places or paints it.
    ///
    /// Paint order decides which element is offered a drag first: later
    /// siblings win, and a header cell starts a column reorder. So a single
    /// band straddling the boundary would lose its outer half to the next
    /// column's cell. Each boundary is covered by two bands instead, one in
    /// the `th` on either side, each rendered after that `th`'s own cell and
    /// so above everything it overlaps.
    fn resize_handle_band(
        &self,
        ix: usize,
        id: ElementId,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(id)
            .occlude()
            .cursor_col_resize()
            .on_drag_move(
                cx.listener(move |view, e: &DragMoveEvent<ResizeColumn>, window, cx| {
                    match e.drag(cx) {
                        ResizeColumn((entity_id, ix)) => {
                            if cx.entity_id() != *entity_id {
                                return;
                            }

                            // sync col widths into real widths
                            // TODO: Consider to remove this, this may not need now.
                            // for (_, col_group) in view.col_groups.iter_mut().enumerate() {
                            //     col_group.width = col_group.bounds.size.width;
                            // }

                            let ix = *ix;
                            if view.fitted_col == Some(ix) {
                                return;
                            }
                            let fills = view.has_auto_width_cols();
                            if fills && view.resizing_col.is_none() {
                                // A drag starts from what is shown: columns
                                // squeezed for lack of room are asked for
                                // their squeezed width from now on.
                                for col_group in &mut view.col_groups {
                                    if !col_group.column.auto_width {
                                        col_group.preferred = col_group.width;
                                    }
                                }
                            }
                            view.resizing_col = Some(ix);

                            let col_group = view
                                .col_groups
                                .get(ix)
                                .expect("BUG: invalid col index")
                                .clone();

                            let target = e.event.position.x - HANDLE_SIZE - col_group.bounds.left();
                            if fills {
                                view.drag_col_edge(ix, target, cx);
                            } else {
                                view.resize_cols(ix, target, window, cx);
                            }

                            // scroll the table if the drag is near the edge
                            view.scroll_table_by_col_resizing(e.event.position, &col_group);
                        }
                    };
                }),
            )
            .on_drag(ResizeColumn((cx.entity_id(), ix)), |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, e: &MouseDownEvent, window, cx| {
                    match e.click_count {
                        1 => view.fitted_col = None,
                        2 => {
                            // Fork: a double-click resets an auto_width column
                            // to its original min_width; other columns fit their
                            // content (upstream #3489).
                            let auto_width = view
                                .col_groups
                                .get(ix)
                                .is_some_and(|col_group| col_group.column.auto_width);
                            if auto_width {
                                if let Some(col_group) = view.col_groups.get_mut(ix) {
                                    col_group.column.min_width = col_group.original_min_width;
                                    col_group.width = col_group.original_min_width;
                                }
                                view.prev_table_width = px(0.); // force recalc
                                view.fitted_col = None;
                                cx.notify();
                            } else if view.fit_col(ix, window, cx) {
                                view.fitted_col = Some(ix);
                            }
                        }
                        _ => {}
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if view.resizing_col.is_none() {
                        return;
                    }

                    view.resizing_col = None;

                    let new_widths = view.col_groups.iter().map(|g| g.width).collect();
                    cx.emit(TableEvent::ColumnWidthsChanged(new_widths));
                    cx.notify();
                }),
            )
    }

    /// The border on the right edge of the fixed columns, unless turned off
    /// with [`DataTable::fixed_cols_border`].
    fn render_fixed_cols_border(&self, cx: &App) -> Option<Div> {
        self.options.fixed_cols_border.then(|| {
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w_0()
                .flex_shrink_0()
                .border_r_1()
                .border_color(cx.theme().border)
        })
    }

    /// Render the row header cell (when cell_selectable is enabled)
    fn render_row_header_cell(
        &self,
        row_ix: usize,
        is_head: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(("row-header", row_ix))
            .w_3()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().table_row_border)
            .bg(cx.theme().tokens.table_head)
            .flex_shrink_0()
            .table_cell_size(self.options.size)
            .when(!is_head, |this| {
                this.when(self.row_selectable, |this| {
                    this.on_click(cx.listener(move |table, e: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        if table.selects_row_on_click(e) {
                            table.set_selected_row(row_ix, cx);
                        }
                    }))
                })
            })
    }

    fn render_sort_icon(
        &self,
        col_ix: usize,
        col_group: &ColGroup,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.sortable {
            return None;
        }

        let Some(sort) = col_group.column.sort else {
            return None;
        };

        let (icon, is_on) = match sort {
            ColumnSort::Ascending => (IconName::SortAscending, true),
            ColumnSort::Descending => (IconName::SortDescending, true),
            ColumnSort::Default => (IconName::ChevronsUpDown, false),
        };

        Some(
            div()
                .id(("icon-sort", col_ix))
                .test_support()
                .role(gpui::Role::Button)
                .aria_label(t!("Table.SortBy", column = col_group.column.name).to_string())
                .p(px(2.))
                .rounded(cx.theme().radius / 2.)
                .map(|this| match is_on {
                    true => this,
                    false => this.opacity(0.5),
                })
                .hover(|this| this.bg(cx.theme().tokens.secondary).opacity(7.))
                .active(|this| this.bg(cx.theme().tokens.secondary_active).opacity(1.))
                .on_click(
                    cx.listener(move |table, _, window, cx| table.perform_sort(col_ix, window, cx)),
                )
                .child(
                    Icon::new(icon)
                        .size_3()
                        .text_color(cx.theme().secondary_foreground),
                ),
        )
    }

    /// Returns `(col_ix, colspan)` pairs for header rendering within the given
    /// column index range, skipping columns consumed by a preceding colspan.
    fn header_col_spans(&self, range: Range<usize>) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        let mut i = range.start;
        while i < range.end {
            let span = self
                .col_groups
                .get(i)
                .map(|cg| cg.column.header_colspan)
                .unwrap_or(1)
                .min(range.end - i); // clamp to section boundary
            result.push((i, span));
            i += span;
        }
        result
    }

    /// Render the column header.
    /// The children must be one by one items.
    /// Because the horizontal scroll handle will use the child_item_bounds to
    /// calculate the item position for itself's `scroll_to_item` method.
    fn render_th(
        &mut self,
        col_ix: usize,
        colspan: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let entity_id = cx.entity_id();
        let col_group = self.col_groups.get(col_ix).expect("BUG: invalid col index");

        // Disable drag for spanned headers to avoid complex multi-column reorder.
        let movable = self.col_movable && col_group.column.movable && colspan == 1;
        let paddings = col_group.column.paddings;
        let name = col_group.column.name.clone();

        // Compute combined properties across the span.
        let is_auto = self.col_groups[col_ix..col_ix + colspan]
            .iter()
            .any(|cg| cg.column.auto_width);
        // Mirrors `render_cell_inner` and the body row wrapper: flex to fill
        // remaining space live (no resize lag) except while a column is being
        // dragged; also flex on the pre-measurement first frame. All three flip
        // together so header and body stay in sync.
        let auto_flex = is_auto && (self.auto_columns_flex() || !self.auto_widths_ready());

        // Build the inner sizing div for the spanned header cell.
        let cell_div = if colspan == 1 {
            self.render_cell_flex(col_ix)
        } else {
            // Sum widths and min_widths across all spanned columns.
            let total_width: Pixels = self.col_groups[col_ix..col_ix + colspan]
                .iter()
                .map(|cg| cg.width)
                .sum();
            let total_min_width: Pixels = self.col_groups[col_ix..col_ix + colspan]
                .iter()
                .map(|cg| cg.column.min_width)
                .sum();

            let cell = h_flex()
                .h_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .table_cell_size(self.options.size);

            if auto_flex {
                cell.flex_1().min_w(total_min_width)
            } else {
                cell.w(total_width).flex_shrink_0()
            }
        };

        // Resize handle on the last column of the span.
        let resize_col_ix = col_ix + colspan - 1;

        h_flex()
            .h_full()
            // Flex the auto column only until its width is measured; thereafter
            // it sizes to its computed width, matching the body.
            .when(auto_flex, |this| this.flex_1())
            .child(
                cell_div
                    .id(("col-header", col_ix))
                    .test_support()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.on_col_head_click(col_ix, window, cx);
                    }))
                    .child(
                        h_flex()
                            .h_full()
                            .flex_1()
                            .justify_between()
                            .items_center()
                            .child(self.delegate.render_th(col_ix, window, cx))
                            .when_some(paddings, |this, paddings| {
                                // Leave right space for the sort icon, if this column have custom padding
                                let offset_pr =
                                    self.options.size.table_cell_padding().right - paddings.right;
                                this.pr(offset_pr.max(px(0.)))
                            })
                            .children(self.render_sort_icon(col_ix, &col_group, window, cx)),
                    )
                    .when(movable, |this| {
                        this.on_drag(
                            DragColumn {
                                entity_id,
                                col_ix,
                                name,
                                width: col_group.width,
                            },
                            |drag, _, _, cx| {
                                cx.stop_propagation();
                                cx.new(|_| drag.clone())
                            },
                        )
                    })
                    .map(|this| {
                        // Draw the insertion indicator on the left edge of the gap
                        // column, or on the right edge of the last column for the
                        // trailing gap. Use an absolutely positioned overlay instead
                        // of a border, to avoid shifting the cell content.
                        let last_gap = col_ix + 1 == self.col_groups.len();
                        match self.col_drag_gap {
                            Some(gap)
                                if cx.has_active_drag()
                                    && (gap == col_ix || (last_gap && gap == col_ix + 1)) =>
                            {
                                let right_side = gap == col_ix + 1;
                                this.relative().child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .w(px(2.))
                                        .map(|d| if right_side { d.right_0() } else { d.left_0() })
                                        .bg(cx.theme().drag_border),
                                )
                            }
                            _ => this,
                        }
                    }),
            )
            // Trailing handle. For a spanned header this must target the LAST
            // spanned column, not the leader — the fork's colspan fix
            // (7a622ced); upstream passes `col_ix`, which is the same thing
            // whenever colspan is 1.
            .child(self.render_resize_handle(resize_col_ix, window, cx))
            // resize handle cell left side
            .child(self.render_leading_resize_handle(col_ix, window, cx))
            // to save the bounds of this col.
            .on_prepaint({
                let view = cx.entity().clone();
                move |bounds, _, cx| {
                    view.update(cx, |r, _| {
                        r.col_groups[col_ix].bounds = bounds;
                        // For a spanned header (colspan > 1) the resize handle targets the
                        // LAST spanned column (`resize_col_ix`), not the leader. The drag math
                        // reads `col_groups[resize_col_ix].bounds.left()`, so that column needs
                        // bounds whose left edge matches its real on-screen position. Without
                        // this it stays at the default `origin.x = 0` and the resize computes a
                        // width relative to the window's left edge, making the column jump to
                        // full width. Its left edge = span left + widths of the preceding
                        // spanned columns.
                        if resize_col_ix != col_ix {
                            if let Some(slice) = r.col_groups.get(col_ix..resize_col_ix) {
                                let preceding: Pixels = slice.iter().map(|c| c.width).sum();
                                if let Some(resize_group) = r.col_groups.get_mut(resize_col_ix) {
                                    let mut resize_bounds = bounds;
                                    resize_bounds.origin.x = bounds.left() + preceding;
                                    resize_bounds.size.width =
                                        (bounds.right() - resize_bounds.origin.x).max(px(0.));
                                    resize_group.bounds = resize_bounds;
                                }
                            }
                        }
                    })
                }
            })
    }

    /// Compute the visible non-fixed leaf-column range for header rendering.
    ///
    /// Returns `(visible_range, left_spacer_width)` where:
    /// - `visible_range` is the column-index range that should be rendered.
    /// - `left_spacer_width` is the total width of the off-screen left columns,
    ///   used as a spacer div to keep visible columns at the correct position.
    ///
    /// On the first frame `self.bounds` is zero, so a fallback that covers all
    /// columns is returned to avoid a blank header on initial paint.
    fn calculate_visible_leaf_col_range(
        &self,
        left_columns_count: usize,
    ) -> (Range<usize>, Pixels) {
        let total_cols = self.col_groups.len();

        if self.bounds.size.width == px(0.) {
            return (left_columns_count..total_cols, px(0.));
        }

        let fixed_width = self.fixed_head_cols_bounds.size.width;
        let available_width = (self.bounds.size.width - fixed_width).max(px(0.));
        // The scroll handle offset is negative when scrolled right; negate it
        // to obtain a positive distance from the left edge of the scroll area.
        let scroll_x = (-self.horizontal_scroll_handle.offset().x).max(px(0.));

        // Walk left-to-right through non-fixed columns to find the first one
        // whose right edge enters the viewport. The accumulated width of the
        // skipped columns becomes the left spacer width.
        let mut range_start = left_columns_count;
        let mut left_spacer = px(0.);
        let mut cumulative = px(0.);
        for i in left_columns_count..total_cols {
            let right_edge = cumulative + self.col_groups[i].width;
            if right_edge > scroll_x {
                range_start = i;
                left_spacer = cumulative;
                break;
            }
            cumulative = right_edge;
        }

        // Continue from `range_start` (skipping already-scanned columns) to
        // find the last column still within the viewport. The 200 px overdraw
        // buffer prevents a visible flash when the user scrolls quickly.
        let right_bound = scroll_x + available_width + px(200.);
        let mut range_end = total_cols;
        let mut cumulative = left_spacer; // already summed widths before `range_start`
        for i in range_start..total_cols {
            cumulative += self.col_groups[i].width;
            if cumulative > right_bound {
                range_end = (i + 1).min(total_cols);
                break;
            }
        }

        (range_start..range_end, left_spacer)
    }

    fn render_table_header(
        &mut self,
        left_columns_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().clone();

        // Header leaf-column virtualization.
        //
        // `render_th` creates interactive elements with resize-handle listeners.
        // Calling it for every column every frame is O(n) in column count; with
        // 1000+ columns this alone drops FPS below 60 even in release mode.
        //
        // We restrict rendering to the columns currently visible inside the
        // overflow-scroll viewport, surrounding them with inert spacer divs:
        //
        //   [left_spacer] [visible columns…] [right_spacer] [last_empty_col]
        //
        // The spacers preserve the flex container's total content width so that
        // the scrollbar range stays correct.
        let total_cols = self.col_groups.len();
        let (visible_col_range, left_spacer) =
            self.calculate_visible_leaf_col_range(left_columns_count);

        let layout_len = self.header_layout.len();

        // Reset fixed head columns bounds, if no fixed columns are present
        if left_columns_count == 0 {
            self.fixed_head_cols_bounds = Bounds::default();
        }

        let mut header = self.delegate_mut().render_header(window, cx);
        let style = header.style().clone();
        let has_group_headers = self.header_layout.len() > 1;

        header
            .h_flex()
            .w_full()
            .flex_shrink_0()
            // Single-row header: restore the explicit height + bottom border
            // that the multi-row header refactor removed. The multi-row path
            // draws per-row borders inside its v_flex, so we only apply here
            // when there are no group headers.
            .when(!has_group_headers, |this| {
                this.h(self.header_row_height())
                    .border_b_1()
                    .border_color(cx.theme().border)
            })
            .bg(cx.theme().table_head)
            .text_color(cx.theme().table_head_foreground)
            .refine_style(&style)
            .on_drag_move(cx.listener(|table, e: &DragMoveEvent<DragColumn>, _, cx| {
                let drag = e.drag(cx);
                let (drag_entity_id, drag_col_ix) = (drag.entity_id, drag.col_ix);

                let gap =
                    if drag_entity_id == cx.entity_id() && e.bounds.contains(&e.event.position) {
                        table.drag_gap_at(e.event.position.x, drag_col_ix)
                    } else {
                        None
                    };

                if table.col_drag_gap != gap {
                    table.col_drag_gap = gap;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|table, drag: &DragColumn, window, cx| {
                if drag.entity_id != cx.entity_id() {
                    return;
                }

                // Insert the dragged column into the indicated gap.
                let Some(gap) = table.col_drag_gap.take() else {
                    return;
                };
                let to_ix = if drag.col_ix < gap { gap - 1 } else { gap };
                table.move_column(drag.col_ix, to_ix, window, cx);
            }))
            .when(self.cell_selectable && self.row_header, |this| {
                this.child(self.render_row_header_cell(0, true, cx))
            })
            .when(left_columns_count > 0, |this| {
                let view = view.clone();
                // Render left fixed columns
                this.child(
                    h_flex()
                        .relative()
                        .h_full()
                        .bg(cx.theme().table_head)
                        .children(if has_group_headers {
                            self.render_group_header_cols(0..left_columns_count, window, cx)
                        } else {
                            self.header_col_spans(0..left_columns_count)
                                .into_iter()
                                .map(|(col_ix, colspan)| {
                                    self.render_th(col_ix, colspan, window, cx)
                                        .into_any_element()
                                })
                                .collect()
                        })
                        .children(self.render_fixed_cols_border(cx))
                        .on_prepaint(move |bounds, _, cx| {
                            view.update(cx, |r, _| r.fixed_head_cols_bounds = bounds)
                        }),
                )
            })
            .child(
                // Columns
                // overflow_hidden (not overflow_scroll): clips columns that
                // extend past the container when one is resized wider, and lets
                // the auto column's first-frame remaining-space fill work.
                h_flex()
                    .id("table-head")
                    .size_full()
                    .overflow_hidden()
                    .relative()
                    .bg(cx.theme().table_head)
                    .map(|this| {
                        if has_group_headers {
                            // Grouped header: multi-row v_flex. Leaf row uses upstream's spacer
                            // virtualization for tables with many leaf columns; group header rows
                            // render every cell (cell count is small per group row).
                            let layout = self.header_layout.clone();
                            this.child(v_flex().min_w_full().flex_shrink_0().children(
                                layout.iter().enumerate().map(|(row_ix, row_cells)| {
                                    let is_leaf_row = row_ix + 1 == layout_len;
                                    h_flex()
                                        .min_w_full()
                                        .h(self.header_row_height())
                                        .border_b_1()
                                        .border_color(cx.theme().border)
                                        .map(|this| {
                                            if is_leaf_row {
                                                this.when(left_spacer > px(0.), |r| {
                                                    r.child(
                                                        div()
                                                            .w(left_spacer)
                                                            .h_full()
                                                            .flex_shrink_0(),
                                                    )
                                                })
                                                .children(row_cells.iter().filter_map(|cell| {
                                                    if cell.start_leaf_col_ix >= left_columns_count
                                                        && cell.is_leaf
                                                    {
                                                        let ix = cell.leaf_col_ix?;
                                                        if !visible_col_range.contains(&ix) {
                                                            return None;
                                                        }
                                                        Some(
                                                            self.render_th(ix, 1, window, cx)
                                                                .into_any_element(),
                                                        )
                                                    } else {
                                                        None
                                                    }
                                                }))
                                                .when(visible_col_range.end < total_cols, |r| {
                                                    let right_spacer: Pixels = self.col_groups
                                                        [visible_col_range.end..total_cols]
                                                        .iter()
                                                        .map(|g| g.width)
                                                        .sum();
                                                    r.child(
                                                        div()
                                                            .w(right_spacer)
                                                            .h_full()
                                                            .flex_shrink_0(),
                                                    )
                                                })
                                                .child(
                                                    self.delegate.render_last_empty_col(window, cx),
                                                )
                                            } else {
                                                this.children(row_cells.iter().filter_map(|cell| {
                                                    if cell.start_leaf_col_ix >= left_columns_count
                                                    {
                                                        if cell.is_leaf {
                                                            if let Some(ix) = cell.leaf_col_ix {
                                                                return Some(
                                                                    self.render_th(
                                                                        ix, 1, window, cx,
                                                                    )
                                                                    .into_any_element(),
                                                                );
                                                            }
                                                        } else {
                                                            return Some(
                                                                self.delegate_mut()
                                                                    .render_group_th(
                                                                        &cell.label,
                                                                        cell.col_span,
                                                                        cell.width,
                                                                        window,
                                                                        cx,
                                                                    )
                                                                    .into_any_element(),
                                                            );
                                                        }
                                                    }
                                                    None
                                                }))
                                                .child(
                                                    self.delegate.render_last_empty_col(window, cx),
                                                )
                                            }
                                        })
                                }),
                            ))
                        } else {
                            // Flat header: use header_col_spans to support column spans even
                            // without group headers (a local feature beyond upstream's design).
                            //
                            // Fork: the row and its trailing empty column are
                            // measured for the room auto-width columns fill
                            // (`fill_room`).
                            let view = cx.entity();
                            this.child(
                                h_flex()
                                    .relative()
                                    .size_full()
                                    .children(
                                        self.header_col_spans(left_columns_count..total_cols)
                                            .into_iter()
                                            .map(|(col_ix, colspan)| {
                                                self.render_th(col_ix, colspan, window, cx)
                                            }),
                                    )
                                    .child(
                                        div()
                                            .h_full()
                                            .flex_shrink_0()
                                            .child(self.delegate.render_last_empty_col(window, cx))
                                            .on_prepaint({
                                                let view = view.clone();
                                                move |bounds, _, cx| {
                                                    view.update(cx, |table, _| {
                                                        table.last_empty_col_width =
                                                            bounds.size.width
                                                    })
                                                }
                                            }),
                                    )
                                    .on_prepaint(move |bounds, _, cx| {
                                        view.update(cx, |table, _| {
                                            table.head_row_width = bounds.size.width
                                        })
                                    }),
                            )
                        }
                    }),
            )
    }

    /// Render multi-row group header columns for the given column range.
    /// Used only when `group_headers()` returns Some.
    fn render_group_header_cols(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let layout = self.header_layout.clone();
        vec![
            v_flex()
                .min_w_full()
                .flex_shrink_0()
                .children(layout.iter().enumerate().map(|(_row_ix, row_cells)| {
                    h_flex()
                        .min_w_full()
                        .h(self.header_row_height())
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .children(row_cells.iter().filter_map(|cell| {
                            if cell.start_leaf_col_ix >= range.start
                                && cell.start_leaf_col_ix < range.end
                            {
                                if cell.is_leaf {
                                    if let Some(ix) = cell.leaf_col_ix {
                                        return Some(
                                            self.render_th(ix, 1, window, cx).into_any_element(),
                                        );
                                    }
                                } else {
                                    return Some(
                                        self.delegate_mut()
                                            .render_group_th(
                                                &cell.label,
                                                cell.col_span,
                                                cell.width,
                                                window,
                                                cx,
                                            )
                                            .into_any_element(),
                                    );
                                }
                            }
                            None
                        }))
                }))
                .into_any_element(),
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn render_table_row(
        &mut self,
        row_ix: usize,
        rows_count: usize,
        left_columns_count: usize,
        columns_count: usize,
        is_filled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_stripe_row = self.options.stripe && row_ix % 2 != 0;
        // `selected_row()` is `None` outside row mode, so a selected cell or
        // column never highlights its row.
        let is_selected = self.selected_row() == Some(row_ix);
        let row_height = self.options.size.table_row_height();

        if row_ix < rows_count {
            let is_last_row = row_ix + 1 == rows_count;
            let need_render_border = is_selected || !is_last_row || !is_filled;

            let mut tr = self.delegate.render_tr(row_ix, window, cx);
            let style = tr.style().clone();

            tr.test_support()
                .role(gpui::Role::Row)
                .aria_selected(is_selected)
                .h_flex()
                .w_full()
                .h(row_height)
                .when(need_render_border, |this| {
                    this.border_b_1().border_color(cx.theme().table_row_border)
                })
                .when(is_stripe_row, |this| this.bg(cx.theme().tokens.table_even))
                .refine_style(&style)
                .hover(|this| {
                    if is_selected || self.right_clicked_row == Some(row_ix) {
                        this
                    } else {
                        this.bg(cx.theme().tokens.table_hover)
                    }
                })
                .when(self.cell_selectable && self.row_header, |this| {
                    this.child(self.render_row_header_cell(row_ix, false, cx))
                })
                .when(left_columns_count > 0, |this| {
                    // Left fixed columns
                    this.child(
                        h_flex()
                            .relative()
                            .h_full()
                            .children({
                                let mut items = Vec::with_capacity(left_columns_count);

                                (0..left_columns_count).for_each(|col_ix| {
                                    let is_cell_selected =
                                        self.selected_cell() == Some((row_ix, col_ix));
                                    let is_cell_right_clicked =
                                        self.right_clicked_cell == Some((row_ix, col_ix));

                                    items.push(
                                        self.render_col_wrap(Some(row_ix), col_ix, window, cx)
                                            .child(
                                                self.render_cell(Some(row_ix), col_ix, window, cx)
                                                    .id(format!("table-cell:{}:{}", row_ix, col_ix))
                                                    .relative()
                                                    .child(self.measure_render_td(
                                                        row_ix, col_ix, window, cx,
                                                    ))
                                                    .when(is_cell_selected, |this| {
                                                        this.child(
                                                            div()
                                                                .absolute()
                                                                .inset_0()
                                                                .bg(cx.theme().tokens.table_active),
                                                        )
                                                    })
                                                    .when(
                                                        is_cell_right_clicked && !is_cell_selected,
                                                        |this| {
                                                            this.child(
                                                                div()
                                                                    .absolute()
                                                                    .inset_0()
                                                                    .border_1()
                                                                    .border_color(
                                                                        cx.theme()
                                                                            .table_active_border
                                                                            .opacity(0.5),
                                                                    ),
                                                            )
                                                        },
                                                    )
                                                    .when(self.cell_selectable, |this| {
                                                        this.on_click(cx.listener(
                                                            move |table, e, window, cx| {
                                                                table.on_cell_click(
                                                                    e, row_ix, col_ix, window, cx,
                                                                );
                                                            },
                                                        ))
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            cx.listener(
                                                                move |table, e, window, cx| {
                                                                    table.on_cell_right_click(
                                                                        e, row_ix, col_ix, window,
                                                                        cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                    }),
                                            ),
                                    );
                                });

                                items
                            })
                            .children(self.render_fixed_cols_border(cx)),
                    )
                })
                .children({
                    // Render scrollable columns directly in this h_flex (no
                    // horizontal virtual_list). This is the jitter fix: it avoids
                    // the virtual_list's 1-frame column-width lag during resize.
                    // Auto columns size via their computed width (see
                    // render_cell_inner), so they stay in sync with the header.
                    let mut cells = Vec::with_capacity(columns_count - left_columns_count);

                    (left_columns_count..columns_count).for_each(|col_ix| {
                        let is_cell_selected = self.selected_cell() == Some((row_ix, col_ix));
                        let is_cell_right_clicked =
                            self.right_clicked_cell == Some((row_ix, col_ix));
                        let is_auto = self
                            .col_groups
                            .get(col_ix)
                            .map(|c| c.column.auto_width)
                            .unwrap_or(false);
                        // The row WRAPPER must flex whenever the inner cell does,
                        // otherwise the non-flex wrapper content-sizes around the
                        // flexing cell and the column grows to the widest filename.
                        // So: flex live when idle (lag-free window/pane resize),
                        // pin to the computed pixel width only during an active
                        // column drag (stable drag, no neighbour "mirroring").
                        let auto_flex =
                            is_auto && (self.auto_columns_flex() || !self.auto_widths_ready());
                        let col_min_w = self
                            .col_groups
                            .get(col_ix)
                            .map(|c| c.column.min_width)
                            .unwrap_or(px(0.));

                        let el = self
                            .render_col_wrap(Some(row_ix), col_ix, window, cx)
                            // Before measurement: fill remaining row space via flex.
                            .when(auto_flex, |this| {
                                this.flex_1().min_w(col_min_w).overflow_hidden()
                            })
                            .child(
                                self.render_cell_flex(col_ix)
                                    .id(format!("table-cell-{}:{}", row_ix, col_ix))
                                    .relative()
                                    .child(self.measure_render_td(row_ix, col_ix, window, cx))
                                    .when(is_cell_selected, |this| {
                                        this.child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .bg(cx.theme().table_active)
                                                .border_1()
                                                .border_color(cx.theme().table_active_border),
                                        )
                                    })
                                    .when(is_cell_right_clicked && !is_cell_selected, |this| {
                                        this.child(
                                            div().absolute().inset_0().border_1().border_color(
                                                cx.theme().table_active_border.opacity(0.5),
                                            ),
                                        )
                                    })
                                    .when(self.cell_selectable, |this| {
                                        this.on_click(cx.listener(move |table, e, window, cx| {
                                            cx.stop_propagation();
                                            table.on_cell_click(e, row_ix, col_ix, window, cx);
                                        }))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(move |table, e, window, cx| {
                                                table.on_cell_right_click(
                                                    e, row_ix, col_ix, window, cx,
                                                );
                                            }),
                                        )
                                    }),
                            );

                        cells.push(el);
                    });

                    cells
                })
                // Row selected / right-clicked style.
                //
                // The overlay extends 1px above/below the row so its
                // top/bottom borders coincide with neighbouring rows'
                // separator lines — makes the selection look like one
                // continuous frame instead of a box stacked on top of
                // borders.  BUT this trick breaks at the viewport edges:
                // a `top: -1` on the first visible row or a `bottom: -1`
                // on the last visible row extends 1px past the clipping
                // surface (inner_table has `overflow_hidden`), so that
                // side of the border disappears.  The fix is to clamp to
                // 0 when we're at an edge.
                //
                // Horizontally the same thing happened at `left: 0`/
                // `right: 0` — the border sat exactly at the clip
                // boundary and was eaten.  `inner_table`'s 1px horizontal
                // padding (below) gives the overlay 1px of breathing
                // room on both sides, so callers no longer need the
                // `.px_px()` wrapper workaround.
                .when(is_selected, |this| {
                    this.map(|this| {
                        if cx.theme().list.active_highlight {
                            let is_first_visible = row_ix <= self.visible_range.rows.start;
                            let is_last_visible = row_ix + 1 >= self.visible_range.rows.end;
                            this.border_color(gpui::transparent_white()).child(
                                div()
                                    .top(if is_first_visible { px(0.) } else { px(-1.) })
                                    .left(px(0.))
                                    .right(px(0.))
                                    .bottom(if is_last_visible { px(0.) } else { px(-1.) })
                                    .absolute()
                                    .bg(cx.theme().tokens.table_active)
                                    .border_1()
                                    .border_color(cx.theme().table_active_border),
                            )
                        } else {
                            this.bg(cx.theme().tokens.accent)
                        }
                    })
                })
                // Row right click row style
                .when(self.right_clicked_row == Some(row_ix), |this| {
                    let is_first_visible = row_ix <= self.visible_range.rows.start;
                    let is_last_visible = row_ix + 1 >= self.visible_range.rows.end;
                    this.border_color(gpui::transparent_white()).child(
                        div()
                            .top(if is_first_visible { px(0.) } else { px(-1.) })
                            .left(px(0.))
                            .right(px(0.))
                            .bottom(if is_last_visible { px(0.) } else { px(-1.) })
                            .absolute()
                            .border_1()
                            .border_color(cx.theme().selection),
                    )
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e, window, cx| {
                        this.on_row_right_click(e, Some(row_ix), window, cx);
                    }),
                )
                .on_click(cx.listener(move |this, e, window, cx| {
                    this.on_row_left_click(e, row_ix, window, cx);
                }))
                .into_any_element()
        } else {
            let cols_width: Pixels = self
                .col_groups
                .iter()
                .take(columns_count)
                .map(|col_group| col_group.width)
                .sum();

            // Render fake rows to fill the rest table space
            self.delegate
                .render_tr(row_ix, window, cx)
                .h_flex()
                .w_full()
                .h(row_height)
                .border_b_1()
                .border_color(cx.theme().table_row_border)
                .when(is_stripe_row, |this| this.bg(cx.theme().tokens.table_even))
                .when(self.cell_selectable && self.row_header, |this| {
                    // Render empty row header cell for fake rows
                    this.child(
                        div()
                            .w(px(40.))
                            .h_full()
                            .flex_shrink_0()
                            .table_cell_size(self.options.size),
                    )
                })
                // The fake cells paint nothing, so a single spacer as wide as all
                // the columns keeps the last empty column in place without laying
                // out one element per column.
                .child(div().flex_shrink_0().h_full().w(cols_width))
                .child(self.delegate.render_last_empty_col(window, cx))
                .into_any_element()
        }
    }

    /// Calculate the extra rows needed to fill the table empty space when `stripe` is true.
    fn calculate_extra_rows_needed(
        &self,
        total_height: Pixels,
        actual_height: Pixels,
        row_height: Pixels,
    ) -> usize {
        let mut extra_rows_needed = 0;

        let remaining_height = total_height - actual_height;
        if remaining_height > px(0.) {
            extra_rows_needed = (remaining_height / row_height).floor() as usize;
        }

        extra_rows_needed
    }

    #[inline]
    fn measure_render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !crate::measure_enable() {
            return self
                .delegate
                .render_td(row_ix, col_ix, window, cx)
                .into_any_element();
        }

        let start = std::time::Instant::now();
        let el = self.delegate.render_td(row_ix, col_ix, window, cx);
        self._measure.push(start.elapsed());
        el.into_any_element()
    }

    fn measure(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        if !crate::measure_enable() {
            return;
        }

        // Print avg measure time of each td
        if self._measure.len() > 0 {
            let total = self
                ._measure
                .iter()
                .fold(Duration::default(), |acc, d| acc + *d);
            let avg = total / self._measure.len() as u32;
            eprintln!(
                "last render {} cells total: {:?}, avg: {:?}",
                self._measure.len(),
                total,
                avg,
            );
        }
        self._measure.clear();
    }

    fn render_vertical_scrollbar(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        Some(
            div()
                .absolute()
                .top(self.header_band_height())
                .right_0()
                .bottom_0()
                .w(Scrollbar::width())
                .child(
                    Scrollbar::vertical(&self.vertical_scroll_handle)
                        .viewport_from_layout()
                        .max_fps(60),
                ),
        )
    }

    fn render_horizontal_scrollbar(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .left(self.fixed_head_cols_bounds.size.width)
            .right_0()
            .bottom_0()
            .h(Scrollbar::width())
            .child(Scrollbar::horizontal(&self.horizontal_scroll_handle).viewport_from_layout())
    }
}

impl<D> Focusable for TableState<D>
where
    D: TableDelegate,
{
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl<D> EventEmitter<TableEvent> for TableState<D> where D: TableDelegate {}

impl<D> Render for TableState<D>
where
    D: TableDelegate,
{
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.measure(window, cx);
        if self.update_auto_widths() {
            self.update_header_layout(cx);
        }

        let columns_count = self.delegate.columns_count(cx);
        let left_columns_count = self
            .col_groups
            .iter()
            .filter(|col| self.col_fixed && col.column.fixed == Some(ColumnFixed::Left))
            .count();
        let rows_count = self.delegate.rows_count(cx);
        let loading = self.delegate.loading(cx);

        let row_height = self.options.size.table_row_height();
        let total_height = self
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height;
        let actual_height = row_height * rows_count as f32;
        let extra_rows_count =
            self.calculate_extra_rows_needed(total_height, actual_height, row_height);
        let render_rows_count = if self.options.stripe {
            rows_count + extra_rows_count
        } else {
            rows_count
        };
        let has_right_click = self.right_clicked_row.is_some() || self.right_clicked_cell.is_some();
        let is_filled = total_height > Pixels::ZERO && total_height <= actual_height;

        let loading_view = if loading {
            Some(
                self.delegate
                    .render_loading(self.options.size, window, cx)
                    .into_any_element(),
            )
        } else {
            None
        };

        let empty_view = if rows_count == 0 {
            // The rows list is not rendered, so report the empty range here.
            self.update_visible_range_if_need(0..0, 0, Axis::Vertical, window, cx);
            Some(
                div()
                    .size_full()
                    .child(self.delegate.render_empty(window, cx))
                    .into_any_element(),
            )
        } else {
            None
        };

        let inner_table = v_flex()
            .id("table-inner")
            .size_full()
            // 1px horizontal padding so the selected-row overlay's
            // left/right borders (`border_1()` drawn from `left: 0`/
            // `right: 0` on an absolute div) aren't eaten by this
            // container's `overflow_hidden`.  Header cells shift by
            // the same 1px so they stay aligned with body cells.
            .px_px()
            .overflow_hidden()
            .child(self.render_table_header(left_columns_count, window, cx))
            .context_menu({
                let view = cx.entity().clone();
                move |this, window: &mut Window, cx: &mut Context<PopupMenu>| {
                    if let Some(row_ix) = view.read(cx).right_clicked_row {
                        view.update(cx, |menu, cx| {
                            menu.delegate_mut().context_menu(row_ix, this, window, cx)
                        })
                    } else {
                        this
                    }
                }
            })
            .map(|this| {
                if rows_count == 0 {
                    this.children(empty_view)
                } else {
                    this.child(
                        // min_w_full() ensures the table body fills its parent
                        // without creating the layout oscillation that w_full()
                        // caused on Wayland (width: 100% vs flex_grow conflict).
                        h_flex()
                            .id("table-body")
                            .flex_grow_1()
                            .h_full()
                            .min_w_full()
                            .child(
                                uniform_list(
                                    "table-uniform-list",
                                    render_rows_count,
                                    cx.processor(
                                        move |table, visible_range: Range<usize>, window, cx| {
                                            // Column widths come from `update_auto_widths` at the
                                            // start of this render rather than `col.bounds.size` (set
                                            // by on_prepaint from the previous frame). Keeps body
                                            // columns in sync with the header on the same frame and
                                            // avoids column jitter during window resize.

                                            table.load_more_if_need(
                                                rows_count,
                                                visible_range.end,
                                                window,
                                                cx,
                                            );
                                            table.update_visible_range_if_need(
                                                visible_range.clone(),
                                                rows_count,
                                                Axis::Vertical,
                                                window,
                                                cx,
                                            );
                                            table.delegate_mut().visible_rows_rendered(
                                                visible_range.clone(),
                                                window,
                                                cx,
                                            );

                                            if visible_range.end > rows_count {
                                                // Scroll without cx.notify() — we are inside the
                                                // uniform_list processor (during layout), so calling
                                                // notify() would schedule another render mid-frame.
                                                table.vertical_scroll_handle.scroll_to_item(
                                                    std::cmp::min(
                                                        visible_range.start,
                                                        rows_count.saturating_sub(1),
                                                    ),
                                                    ScrollStrategy::Top,
                                                );
                                            }

                                            let mut items = Vec::with_capacity(
                                                visible_range
                                                    .end
                                                    .saturating_sub(visible_range.start),
                                            );

                                            // Render fake rows to fill the table
                                            visible_range.for_each(|row_ix| {
                                                // Render real rows for available data
                                                items.push(table.render_table_row(
                                                    row_ix,
                                                    rows_count,
                                                    left_columns_count,
                                                    columns_count,
                                                    is_filled,
                                                    window,
                                                    cx,
                                                ));
                                            });

                                            items
                                        },
                                    ),
                                )
                                .flex_grow_1()
                                .size_full()
                                .with_sizing_behavior(ListSizingBehavior::Auto)
                                .track_scroll(&self.vertical_scroll_handle)
                                .map(|rows| {
                                    gpui_base::ScrollBounce::new(
                                        "table-scroll-bounce",
                                        &self.vertical_scroll_handle,
                                        rows,
                                    )
                                    .when_some(self.options.scroll_bounce, |bounce, on| {
                                        bounce.enabled(on)
                                    })
                                })
                                .into_any_element(),
                            ),
                    )
                }
            });

        div()
            .size_full()
            .children(loading_view)
            .when(!loading, |this| {
                this.child(inner_table)
                    .child(ScrollableMask::new(
                        Axis::Horizontal,
                        &self.horizontal_scroll_handle,
                    ))
                    // Keep vertical wheel scrolling from leaking into an
                    // ancestor scroller. Skipped when the table is empty:
                    // the `uniform_list` is not rendered then, so the
                    // handle's offset and `max_offset` are stale.
                    .when(rows_count > 0, |this| {
                        this.child(ScrollableMask::new(
                            Axis::Vertical,
                            &self.vertical_scroll_handle.0.borrow().base_handle,
                        ))
                    })
                    .when(has_right_click, |this| {
                        this.on_mouse_down_out(cx.listener(|this, e, window, cx| {
                            this.on_row_right_click(e, None, window, cx);
                            cx.notify();
                        }))
                    })
            })
            .on_prepaint({
                let state = cx.entity();
                move |bounds, _, cx| state.update(cx, |state, _| state.bounds = bounds)
            })
            .when(!window.is_inspector_picking(cx), |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .size_full()
                        .when(self.options.scrollbar_visible.bottom, |this| {
                            this.child(self.render_horizontal_scrollbar(window, cx))
                        })
                        .when(
                            self.options.scrollbar_visible.right && rows_count > 0,
                            |this| this.children(self.render_vertical_scrollbar(window, cx)),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui::{App, Entity, TestAppContext};

    use super::*;

    struct WidthsDelegate;

    struct BounceDelegate;

    impl TableDelegate for BounceDelegate {
        fn columns_count(&self, _: &App) -> usize {
            1
        }
        fn rows_count(&self, _: &App) -> usize {
            40
        }
        fn column(&self, _: usize, _: &App) -> Column {
            Column::new("value", "Value")
        }
        fn render_td(
            &mut self,
            row: usize,
            _: usize,
            _: &mut Window,
            _: &mut Context<TableState<Self>>,
        ) -> impl IntoElement {
            div().child(row.to_string())
        }
    }

    struct BounceTable(Entity<TableState<BounceDelegate>>);

    impl Render for BounceTable {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(240.))
                .h(px(200.))
                .child(DataTable::new(&self.0).scroll_bounce(true))
        }
    }

    #[gpui::test]
    fn scroll_bounce_table_keeps_headers_fixed_and_accepts_new_wheel_input(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BounceTable(cx.new(|cx| TableState::new(BounceDelegate, window, cx)))
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = view.read_with(cx, |view, _| view.0.clone());
        let header = state.read_with(cx, |state, _| state.col_groups[0].bounds);
        let wheel = |cx: &mut gpui::VisualTestContext, dy| {
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(px(100.), px(100.)),
                delta: gpui::ScrollDelta::Lines(gpui::point(0., dy)),
                ..Default::default()
            });
        };
        for (dy, phase) in [
            (-10000., gpui::TouchPhase::Started),
            (0., gpui::TouchPhase::Ended),
        ] {
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(px(100.), px(100.)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(dy))),
                touch_phase: phase,
                ..Default::default()
            });
        }
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let end = state.read_with(cx, |state, _| {
            state
                .vertical_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
        });
        assert!(end < px(0.));
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            state.read_with(cx, |state, _| state.col_groups[0].bounds),
            header
        );
        wheel(cx, 2.);
        assert!(
            state.read_with(cx, |state, _| state
                .vertical_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y)
                > end
        );
    }

    /// A file table: an icon and a mark column that cannot be resized, a
    /// name that fills, and three resizable columns.
    struct FillDelegate;

    impl TableDelegate for FillDelegate {
        fn columns_count(&self, _: &App) -> usize {
            6
        }
        fn rows_count(&self, _: &App) -> usize {
            3
        }
        fn column(&self, col_ix: usize, _: &App) -> Column {
            match col_ix {
                0 => Column::new("icon", "").width(px(40.)).resizable(false),
                1 => Column::new("name", "Name")
                    .width(px(200.))
                    .min_width(px(120.))
                    .flex_grow(),
                2 => Column::new("mark", "").width(px(40.)).resizable(false),
                3 => Column::new("size", "Size").width(px(100.)),
                4 => Column::new("modified", "Modified").width(px(160.)),
                _ => Column::new("permissions", "Permissions").width(px(136.)),
            }
        }
        fn render_td(
            &mut self,
            row: usize,
            _: usize,
            _: &mut Window,
            _: &mut Context<TableState<Self>>,
        ) -> impl IntoElement {
            div().child(row.to_string())
        }
    }

    fn widths<D: TableDelegate>(table: &TableState<D>) -> Vec<Pixels> {
        table.col_groups.iter().map(|c| c.width).collect()
    }

    #[gpui::test]
    fn fill_layout_squeezes_and_restores_preferred_widths(cx: &mut TestAppContext) {
        cx.skip_drawing();
        let window = cx.add_window(|window, cx| TableState::new(FillDelegate, window, cx));
        window
            .update(cx, |table, _, _| {
                // Room to spare: the name takes the rest.
                table.layout_fill_widths(px(800.));
                assert_eq!(
                    widths(table),
                    [px(40.), px(324.), px(40.), px(100.), px(160.), px(136.)]
                );

                // 500 px: the name at its 120 px minimum, the rigid columns
                // as they are, and the three resizable ones share 300 px in
                // proportion to 100 : 160 : 136.
                table.layout_fill_widths(px(500.));
                let squeezed = widths(table);
                assert_eq!(squeezed[1], px(120.));
                assert_eq!((squeezed[0], squeezed[2]), (px(40.), px(40.)));
                let shared: f32 = squeezed[3..].iter().map(|w| f32::from(*w)).sum();
                assert!((shared - 300.).abs() < 0.01, "{squeezed:?}");
                assert!((f32::from(squeezed[4] / squeezed[3]) - 1.6).abs() < 0.01);

                // So little room that a column reaches its minimum: it stays
                // there, and the name stays at its own.
                table.layout_fill_widths(px(240.));
                assert!(widths(table)[3..].iter().all(|w| *w >= px(20.)));
                assert_eq!(widths(table)[1], px(120.));

                // The room comes back, and so do the widths asked for.
                table.layout_fill_widths(px(800.));
                assert_eq!(
                    widths(table),
                    [px(40.), px(324.), px(40.), px(100.), px(160.), px(136.)]
                );
            })
            .unwrap();
    }

    struct FillTable(Entity<TableState<FillDelegate>>);

    impl Render for FillTable {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(640.)).h(px(200.)).child(DataTable::new(&self.0))
        }
    }

    /// The right edge of each column's header cell.
    fn edges(
        state: &Entity<TableState<FillDelegate>>,
        cx: &mut gpui::VisualTestContext,
    ) -> Vec<Pixels> {
        state.read_with(cx, |state, _| {
            state.col_groups.iter().map(|c| c.bounds.right()).collect()
        })
    }

    /// Presses on the handle on the right of column `ix`, moves by `dx` in
    /// six steps and releases. Returns the edges after the last move and after
    /// the release.
    fn drag_edge(
        state: &Entity<TableState<FillDelegate>>,
        ix: usize,
        dx: f32,
        cx: &mut gpui::VisualTestContext,
    ) -> (Vec<Pixels>, Vec<Pixels>, Pixels) {
        let bounds = state.read_with(cx, |state, _| state.col_groups[ix].bounds);
        let mut position = gpui::point(bounds.right() - px(1.), bounds.center().y);
        cx.simulate_mouse_down(position, MouseButton::Left, gpui::Modifiers::none());
        for _ in 0..6 {
            position.x += px(dx / 6.);
            cx.simulate_mouse_move(position, MouseButton::Left, gpui::Modifiers::none());
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(state.read_with(cx, |state, _| state.resizing_col), Some(ix));
        let dragged = edges(state, cx);
        // Released where the handle now is, as a real drag ends.
        cx.simulate_mouse_up(position, MouseButton::Left, gpui::Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(state.read_with(cx, |state, _| state.resizing_col), None);
        (dragged, edges(state, cx), position.x)
    }

    /// Before 2026-10-21 the fill column was computed for the whole table
    /// width but flexed into 14 px less, so a drag's first frame pushed every
    /// edge on its right outward and the release pulled the dragged one back
    /// to where it started.
    #[gpui::test]
    fn dragging_an_edge_in_a_filling_table_moves_only_that_edge(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            FillTable(cx.new(|cx| TableState::new(FillDelegate, window, cx)))
        });
        for _ in 0..3 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        let state = view.read_with(cx, |view, _| view.0.clone());

        // Idle, the flexed name is exactly as wide as its computed width.
        let (computed, shown) = state.read_with(cx, |state, _| {
            (
                state.col_groups[1].width,
                state.col_groups[1].bounds.size.width,
            )
        });
        assert!(
            (computed - shown).abs() < px(0.5),
            "{computed:?} vs {shown:?}"
        );
        let start = edges(&state, cx);

        // The size column's edge: it and the modified column trade width.
        let (dragged, released, pointer) = drag_edge(&state, 3, 30., cx);
        assert!(
            (dragged[3] - (pointer - HANDLE_SIZE)).abs() < px(1.),
            "{dragged:?}"
        );
        for ix in [0, 1, 2, 4, 5] {
            assert!(
                (dragged[ix] - start[ix]).abs() < px(0.5),
                "{ix}: {start:?} -> {dragged:?}"
            );
        }
        assert_eq!(released, dragged);
        // Nothing reaches past the room: the last edge is where it was.
        assert!((released[5] - start[5]).abs() < px(0.5));

        // The name's edge: the name and the size column trade, the mark
        // column between them moves along, the size column's right edge
        // stays.
        let before = released;
        let (dragged, released, pointer) = drag_edge(&state, 1, -24., cx);
        assert!(
            (dragged[1] - (pointer - HANDLE_SIZE)).abs() < px(1.),
            "{dragged:?}"
        );
        assert!((dragged[2] - dragged[1] - px(40.)).abs() < px(0.5));
        for ix in [3, 4, 5] {
            assert!(
                (dragged[ix] - before[ix]).abs() < px(0.5),
                "{ix}: {before:?} -> {dragged:?}"
            );
        }
        assert_eq!(released, dragged);

        // The last column's edge has nothing to trade with, so no handle.
        assert!(!state.read_with(cx, |state, _| state.is_col_resizable(5)));
    }

    impl TableDelegate for WidthsDelegate {
        fn columns_count(&self, _: &App) -> usize {
            6
        }

        fn rows_count(&self, _: &App) -> usize {
            0
        }

        fn column(&self, col_ix: usize, _: &App) -> Column {
            match col_ix {
                0 => Column::new("narrow", "Narrow")
                    .min_width(px(80.))
                    .width(px(40.)),
                1 => Column::new("wide", "Wide")
                    .max_width(px(120.))
                    .width(px(300.)),
                // `width` alone below the default `min_width` of 20px.
                2 => Column::new("tiny", "Tiny").width(px(10.)),
                // The default resize protection does not limit initial width.
                3 => Column::new("wide_default", "Wide Default").width(px(2000.)),
                // Applications can explicitly allow wider resize ranges.
                4 => Column::new("wide_explicit", "Wide Explicit")
                    .max_width(px(2400.))
                    .width(px(2000.)),
                // `min_width` above `max_width` must not panic.
                _ => Column::new("inverted", "Inverted")
                    .min_width(px(200.))
                    .max_width(px(100.)),
            }
        }

        fn render_td(
            &mut self,
            _: usize,
            _: usize,
            _: &mut Window,
            _: &mut Context<TableState<Self>>,
        ) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn column_resize_bounds_preserve_initial_widths(cx: &mut TestAppContext) {
        cx.skip_drawing();
        let window = cx.add_window(|window, cx| TableState::new(WidthsDelegate, window, cx));

        window
            .update(cx, |table, window, cx| {
                let widths = |table: &TableState<WidthsDelegate>| {
                    table
                        .col_groups
                        .iter()
                        .map(|col_group| col_group.width)
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    widths(table),
                    [px(40.), px(300.), px(10.), px(2000.), px(2000.), px(100.)]
                );

                for ix in 0..table.col_groups.len() {
                    table.resize_cols(ix, px(10000.), window, cx);
                }
                assert_eq!(
                    widths(table),
                    [
                        px(1200.),
                        px(120.),
                        px(1200.),
                        px(1200.),
                        px(2400.),
                        px(100.)
                    ]
                );

                for ix in 0..table.col_groups.len() {
                    table.resize_cols(ix, px(-100.), window, cx);
                }
                assert_eq!(
                    widths(table),
                    [px(80.), px(20.), px(20.), px(20.), px(20.), px(100.)]
                );
            })
            .unwrap();
    }
}
