//! Character selection over a virtualized list of text lines: a code or log
//! view built on `uniform_list` (or any list that paints only its visible
//! rows) whose text the mouse selects across lines and Ctrl/Cmd+C copies,
//! as it would a `SelectableText`.
//!
//! A `SelectableText` per line does not do for such a list: a line scrolled
//! out of view is not painted, so it is no participant of the window's
//! selection, and a selection longer than the view would lose its middle.
//! Here the whole list is one participant. Its endpoints are content keys —
//! a line and a byte offset into it — so what is selected, highlighted and
//! copied comes from the lines themselves, whichever rows are painted when
//! the gesture ends. The diff viewer does the same for its two sides
//! (`diff::selection`); this is that bridge without the diff.
//!
//! It also has a caret, the keyboard's place in the text: a press puts it
//! where the mouse is, [`LinesSelection::move_caret`] moves it by a
//! character, a word, a line or a page, and with `extend` selects from where
//! it was — a selection of the participant's own, as the diff viewer's line
//! selection is, which Ctrl/Cmd+C copies like the mouse's. A selection made
//! with the mouse goes on under Shift and the arrows. The owner routes the
//! keys (its view decides which keys are the caret's) and says when the
//! caret shows ([`LinesSelection::set_caret_shown`]: while it has the
//! keyboard).
//!
//! Use: keep one [`LinesSelection`] per view in its state and give it the
//! lines with [`LinesSelection::set_source`]. Wrap the list in
//! [`LinesSelection::layer`], and paint each row's text with
//! [`LinesSelection::line`] instead of a `StyledText`.

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, HighlightStyle, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, SharedString, StyledText,
    TextLayout, Window, fill, px, size,
};
use gpui_base::{
    TextSelection, TextSelectionContentKey, TextSelectionHandle, TextSelectionRegistration,
    TextSelectionRun,
};

use crate::ActiveTheme as _;
use crate::diff::selection::{line_distance, merge_highlights, word_range};

/// The lines a [`LinesSelection`] selects in, by index.
pub trait LineSource: 'static {
    /// How many lines there are.
    fn line_count(&self) -> usize;

    /// The text of line `index`, without its line break; `None` past the
    /// last line.
    fn line(&self, index: usize) -> Option<&str>;
}

impl LineSource for Vec<SharedString> {
    fn line_count(&self) -> usize {
        self.len()
    }

    fn line(&self, index: usize) -> Option<&str> {
        self.get(index).map(SharedString::as_ref)
    }
}

/// How [`LinesSelection::move_caret`] moves the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretMotion {
    /// A character to the left; from a line's start, to the end of the one
    /// before.
    Left,
    /// A character to the right; from a line's end, to the next line.
    Right,
    /// To the start of the word before, over the spaces before it.
    WordLeft,
    /// Past the word, and the spaces after it.
    WordRight,
    /// `n` lines down (up when negative), keeping the column it aims for.
    Lines(isize),
    /// To the line's first character that is not a space; from there, to
    /// its very start.
    LineStart,
    /// To the end of the line.
    LineEnd,
    /// To the start of the first line.
    Start,
    /// To the end of the last line.
    End,
}

/// A place in the lines: line `line`, byte `offset` into it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinePoint {
    pub line: usize,
    pub offset: usize,
}

/// Bits of a content key that hold the byte offset: lines up to 16 MiB,
/// and 2^40 lines above them.
const OFFSET_BITS: u32 = 24;
const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1;

fn encode_key(point: LinePoint) -> u64 {
    ((point.line as u64) << OFFSET_BITS) | (point.offset as u64 & OFFSET_MASK)
}

fn decode_key(key: u64) -> LinePoint {
    LinePoint {
        line: (key >> OFFSET_BITS) as usize,
        offset: (key & OFFSET_MASK) as usize,
    }
}

/// One painted line's text, where it was laid out this frame.
struct LineRun {
    line: usize,
    text: SharedString,
    layout: TextLayout,
    /// The part of the line inside the list's clip, in window coordinates.
    visible: Bounds<Pixels>,
}

#[derive(Default)]
struct Geometry {
    bounds: Bounds<Pixels>,
    scroll_offset: Point<Pixels>,
    /// A double or triple click being resolved: the click count, where it
    /// was, and whether its anchor is already answered (the next key is the
    /// unit's other end).
    multi_click: Option<(usize, Point<Pixels>, bool)>,
    runs: Vec<LineRun>,
}

impl Geometry {
    /// The painted line nearest `point` (window coordinates): the one whose
    /// band holds it, else the closest.
    fn run_at(&self, point: Point<Pixels>) -> Option<&LineRun> {
        self.runs
            .iter()
            .min_by(|a, b| line_distance(point, a.visible).cmp(&line_distance(point, b.visible)))
    }

    /// The place under `point`, a window position.
    fn point_at(&self, point: Point<Pixels>) -> Option<LinePoint> {
        let run = self.run_at(point)?;
        let index = run
            .layout
            .index_for_position(point)
            .unwrap_or_else(|index| index)
            .min(run.text.len());
        Some(LinePoint {
            line: run.line,
            offset: floor_char_boundary(&run.text, index),
        })
    }

    /// The content key under `content_point` — the window selection's
    /// participant-relative point — or, while a double or triple click is
    /// resolved, an end of the word or line it hit.
    fn key_at(&mut self, content_point: Point<Pixels>) -> Option<u64> {
        if let Some((count, position, anchored)) = self.multi_click {
            let place = self.point_at(position)?;
            let text = &self.run_at(position)?.text;
            let unit = if count >= 3 {
                0..text.len()
            } else {
                word_range(text, place.offset)?
            };
            self.multi_click = (!anchored).then_some((count, position, true));
            let offset = if anchored { unit.end } else { unit.start };
            return Some(encode_key(LinePoint {
                line: place.line,
                offset,
            }));
        }
        let point = content_point + self.bounds.origin + self.scroll_offset;
        self.point_at(point).map(encode_key)
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Characters before byte `offset` of `text`: the column Up/Down aim for.
fn column(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(0, |before| before.chars().count())
}

/// The byte offset of column `column` of `text`, or its end.
fn offset_of_column(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map_or(text.len(), |(offset, _)| offset)
}

/// What a word motion stops between: spaces, word characters, the rest.
fn char_class(character: char) -> u8 {
    if character.is_whitespace() {
        0
    } else if character == '_' || character.is_alphanumeric() {
        1
    } else {
        2
    }
}

/// Where `motion` takes the caret `at` in `source`, and the column the next
/// vertical move aims for (`goal`, kept across vertical moves).
fn moved(
    source: &dyn LineSource,
    at: LinePoint,
    goal: Option<usize>,
    motion: CaretMotion,
) -> (LinePoint, Option<usize>) {
    let count = source.line_count();
    if count == 0 {
        return (LinePoint { line: 0, offset: 0 }, None);
    }
    let line_at = at.line.min(count - 1);
    let text = source.line(line_at).unwrap_or("");
    let offset = floor_char_boundary(text, at.offset.min(text.len()));
    let place = |line: usize, offset: usize| LinePoint { line, offset };
    let end_of = |line: usize| source.line(line).map_or(0, str::len);
    match motion {
        CaretMotion::Left => match text[..offset].chars().next_back() {
            Some(character) => (place(line_at, offset - character.len_utf8()), None),
            None if line_at > 0 => (place(line_at - 1, end_of(line_at - 1)), None),
            None => (place(line_at, 0), None),
        },
        CaretMotion::Right => match text[offset..].chars().next() {
            Some(character) => (place(line_at, offset + character.len_utf8()), None),
            None if line_at + 1 < count => (place(line_at + 1, 0), None),
            None => (place(line_at, offset), None),
        },
        CaretMotion::WordLeft => {
            if offset == 0 {
                return if line_at > 0 {
                    (place(line_at - 1, end_of(line_at - 1)), None)
                } else {
                    (place(0, 0), None)
                };
            }
            let before = &text[..offset];
            let mut chars = before.char_indices().rev().peekable();
            let mut stop = offset;
            while let Some(&(index, character)) = chars.peek()
                && char_class(character) == 0
            {
                stop = index;
                chars.next();
            }
            if let Some(&(_, character)) = chars.peek() {
                let class = char_class(character);
                while let Some(&(index, character)) = chars.peek()
                    && char_class(character) == class
                {
                    stop = index;
                    chars.next();
                }
            }
            (place(line_at, stop), None)
        }
        CaretMotion::WordRight => {
            if offset == text.len() {
                return if line_at + 1 < count {
                    (place(line_at + 1, 0), None)
                } else {
                    (place(line_at, offset), None)
                };
            }
            let mut chars = text[offset..].char_indices().peekable();
            let mut stop = text.len() - offset;
            if let Some(&(_, character)) = chars.peek() {
                let class = char_class(character);
                if class != 0 {
                    while let Some(&(_, character)) = chars.peek()
                        && char_class(character) == class
                    {
                        chars.next();
                    }
                }
            }
            while let Some(&(_, character)) = chars.peek()
                && char_class(character) == 0
            {
                chars.next();
            }
            if let Some(&(index, _)) = chars.peek() {
                stop = index;
            }
            (place(line_at, offset + stop), None)
        }
        CaretMotion::Lines(delta) => {
            let goal = goal.unwrap_or_else(|| column(text, offset));
            let target = line_at.saturating_add_signed(delta).min(count - 1);
            let target_text = source.line(target).unwrap_or("");
            (
                place(target, offset_of_column(target_text, goal)),
                Some(goal),
            )
        }
        CaretMotion::LineStart => {
            let indent = text.len() - text.trim_start().len();
            let target = if offset == indent { 0 } else { indent };
            (place(line_at, target), None)
        }
        CaretMotion::LineEnd => (place(line_at, text.len()), None),
        CaretMotion::Start => (place(0, 0), None),
        CaretMotion::End => (place(count - 1, end_of(count - 1)), None),
    }
}

/// The keyboard's place in the lines.
#[derive(Default)]
struct Caret {
    at: LinePoint,
    /// The other end of a selection made with the keyboard; it counts only
    /// while the participant's own selection is active (the window clears
    /// that when the mouse starts another).
    anchor: Option<LinePoint>,
    /// The column Up/Down aim for, kept across vertical moves.
    goal: Option<usize>,
    /// Drawn: the owner's view has the keyboard.
    shown: bool,
}

/// The selection of one virtualized list of lines. Cheap to clone: the
/// clones share it.
#[derive(Clone)]
pub struct LinesSelection {
    handle: TextSelectionHandle,
    geometry: Rc<RefCell<Geometry>>,
    source: Rc<RefCell<Option<Rc<dyn LineSource>>>>,
    caret: Rc<RefCell<Caret>>,
}

impl LinesSelection {
    /// A selection over no lines yet.
    pub fn new(cx: &mut App) -> Self {
        let handle = TextSelectionHandle::new("", cx);
        let geometry = Rc::new(RefCell::new(Geometry::default()));
        let source: Rc<RefCell<Option<Rc<dyn LineSource>>>> = Rc::new(RefCell::new(None));
        let this = Self {
            handle,
            geometry,
            source,
            caret: Rc::new(RefCell::new(Caret::default())),
        };
        this.handle.resolve_content_key_with(
            {
                let geometry = this.geometry.clone();
                move |point, _| {
                    geometry
                        .borrow_mut()
                        .key_at(point)
                        .map(TextSelectionContentKey::new)
                }
            },
            cx,
        );
        this.handle.copy_with(
            {
                let this = this.clone();
                move |cx| this.selected_text(cx)
            },
            cx,
        );
        this
    }

    /// The participant handle, for what the window selection offers beyond
    /// this type (a focus callback, a subscription).
    pub fn handle(&self) -> &TextSelectionHandle {
        &self.handle
    }

    /// Select in `source` from now on. A selection still open is read
    /// against the new lines: an owner that replaces the content clears it
    /// (`TextSelection::clear`) when the old places mean nothing in the new.
    pub fn set_source(&self, source: Rc<dyn LineSource>) {
        *self.source.borrow_mut() = Some(source);
    }

    /// The selected span, start before end, when both its ends are in these
    /// lines and it is not empty: the keyboard's, else the mouse's.
    pub fn selected_range(&self, cx: &App) -> Option<Range<LinePoint>> {
        let (anchor, cursor) = self.keyboard_ends(cx).or_else(|| self.mouse_ends(cx))?;
        (anchor != cursor).then(|| anchor.min(cursor)..anchor.max(cursor))
    }

    /// The ends of a selection made with the keyboard, anchor first.
    fn keyboard_ends(&self, cx: &App) -> Option<(LinePoint, LinePoint)> {
        if !self.handle.has_local_selection(cx) {
            return None;
        }
        let caret = self.caret.borrow();
        Some((caret.anchor?, caret.at))
    }

    /// The ends of the window's selection when both are in these lines,
    /// anchor first.
    fn mouse_ends(&self, cx: &App) -> Option<(LinePoint, LinePoint)> {
        let snapshot = self.handle.snapshot(cx)?;
        let own = Some(self.handle.entity_id());
        if snapshot.anchor().entity_id() != own || snapshot.cursor().entity_id() != own {
            return None;
        }
        Some((
            decode_key(snapshot.anchor().content_key()?.value()),
            decode_key(snapshot.cursor().content_key()?.value()),
        ))
    }

    /// Where the caret is: the moving end of a selection, else where it was
    /// put.
    pub fn caret(&self, cx: &App) -> LinePoint {
        self.keyboard_ends(cx)
            .or_else(|| self.mouse_ends(cx))
            .map_or_else(|| self.caret.borrow().at, |(_, cursor)| cursor)
    }

    /// Put the caret at `place`, ending a selection made with the keyboard
    /// (the owner's: a search hit, a line its plugin marked).
    pub fn set_caret(&self, place: LinePoint, cx: &mut App) {
        let mut caret = self.caret.borrow_mut();
        caret.at = place;
        caret.anchor = None;
        caret.goal = None;
        drop(caret);
        self.handle.set_local_selection(false, cx);
    }

    /// Show the caret (while the owner's view has the keyboard) or not.
    pub fn set_caret_shown(&self, shown: bool) {
        self.caret.borrow_mut().shown = shown;
    }

    /// Move the caret by `motion`; with `extend`, select from where it was
    /// (or go on with the selection there is, the mouse's too). Answers
    /// where it went, for the owner to scroll there.
    pub fn move_caret(
        &self,
        motion: CaretMotion,
        extend: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> LinePoint {
        let Some(source) = self.source.borrow().clone() else {
            return self.caret.borrow().at;
        };
        // A selection the mouse made becomes the keyboard's: the window's
        // ends are taken over and the window's selection let go.
        let mouse = self.mouse_ends(cx);
        let keyboard = self.keyboard_ends(cx);
        if mouse.is_some() {
            TextSelection::clear(window, cx);
        }
        let (anchor, at) = match keyboard.or(mouse) {
            Some((anchor, cursor)) => (Some(anchor), cursor),
            None => (None, self.caret.borrow().at),
        };
        let goal = self.caret.borrow().goal;
        let (to, goal) = moved(source.as_ref(), at, goal, motion);
        let anchor = if extend { anchor.or(Some(at)) } else { None };
        let mut caret = self.caret.borrow_mut();
        caret.at = to;
        caret.anchor = anchor;
        caret.goal = goal;
        drop(caret);
        self.handle
            .set_local_selection(anchor.is_some_and(|anchor| anchor != to), cx);
        to
    }

    /// Select all the lines, the caret at their end.
    pub fn select_all(&self, window: &mut Window, cx: &mut App) {
        let Some(source) = self.source.borrow().clone() else {
            return;
        };
        if self.mouse_ends(cx).is_some() {
            TextSelection::clear(window, cx);
        }
        let (end, _) = moved(
            source.as_ref(),
            LinePoint::default(),
            None,
            CaretMotion::End,
        );
        let mut caret = self.caret.borrow_mut();
        caret.anchor = Some(LinePoint::default());
        caret.at = end;
        caret.goal = None;
        drop(caret);
        self.handle
            .set_local_selection(end != LinePoint::default(), cx);
    }

    /// Whether something is selected in these lines.
    pub fn has_selection(&self, cx: &App) -> bool {
        self.selected_range(cx).is_some()
    }

    /// The selected text, lines joined with `\n`; empty when nothing is
    /// selected.
    pub fn selected_text(&self, cx: &App) -> String {
        let (Some(range), Some(source)) = (self.selected_range(cx), self.source.borrow().clone())
        else {
            return String::new();
        };
        let mut text = String::new();
        for line in range.start.line..=range.end.line {
            let Some(content) = source.line(line) else {
                break;
            };
            let start = if line == range.start.line {
                floor_char_boundary(content, range.start.offset.min(content.len()))
            } else {
                0
            };
            let end = if line == range.end.line {
                floor_char_boundary(content, range.end.offset.min(content.len()))
            } else {
                content.len()
            };
            if line > range.start.line {
                text.push('\n');
            }
            text.push_str(content.get(start..end.max(start)).unwrap_or(""));
        }
        text
    }

    /// The selected bytes of line `line`, `len` long, if any.
    fn selected_in_line(&self, line: usize, len: usize, cx: &App) -> Option<Range<usize>> {
        let range = self.selected_range(cx)?;
        if line < range.start.line || line > range.end.line {
            return None;
        }
        let start = if line == range.start.line {
            range.start.offset.min(len)
        } else {
            0
        };
        let end = if line == range.end.line {
            range.end.offset.min(len)
        } else {
            len
        };
        (start < end).then_some(start..end)
    }

    /// The place under the window position `point`, among the lines painted
    /// last frame: for a host that acts on a click itself (a link in the
    /// line).
    pub fn point_at(&self, point: Point<Pixels>) -> Option<LinePoint> {
        self.geometry.borrow().point_at(point)
    }

    /// Wrap the list that paints the lines: the participant's hitbox and
    /// registration. `scroll` is the list's scroll handle (a uniform list's
    /// `base_handle`), which maps a painted line back to its place.
    pub fn layer(&self, child: impl IntoElement, scroll: ScrollHandle) -> SelectableLinesLayer {
        SelectableLinesLayer {
            child: child.into_any_element(),
            scroll,
            selection: self.clone(),
        }
    }

    /// Line `line`'s text, painted with `highlights` (byte ranges into
    /// `text`, sorted and not overlapping) and with its selected part.
    pub fn line(
        &self,
        id: impl Into<ElementId>,
        line: usize,
        text: impl Into<SharedString>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> SelectableLine {
        SelectableLine {
            id: id.into(),
            line,
            text: text.into(),
            highlights,
            selection: self.clone(),
            styled_text: None,
        }
    }
}

/// One line of a [`LinesSelection`]: its text, which it reports to the
/// selection where it is painted. Made by [`LinesSelection::line`].
pub struct SelectableLine {
    id: ElementId,
    line: usize,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selection: LinesSelection,
    styled_text: Option<StyledText>,
}

#[cfg(not(feature = "gpui-fast"))]
impl IntoElement for SelectableLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectableLine {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut layers = std::mem::take(&mut self.highlights);
        if let Some(selected) = self
            .selection
            .selected_in_line(self.line, self.text.len(), cx)
        {
            layers.push((
                selected,
                HighlightStyle {
                    background_color: Some(cx.theme().selection),
                    ..Default::default()
                },
            ));
        }
        let text = StyledText::new(self.text.clone())
            .with_highlights(merge_highlights(self.text.len(), layers));
        let text = self.styled_text.insert(text);
        text.request_layout(id, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(text) = self.styled_text.as_mut() else {
            return;
        };
        text.prepaint(id, inspector, bounds, &mut (), window, cx);
        self.selection.geometry.borrow_mut().runs.push(LineRun {
            line: self.line,
            text: self.text.clone(),
            layout: text.layout().clone(),
            visible: bounds.intersect(&window.content_mask().bounds),
        });
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(text) = self.styled_text.as_mut() else {
            return;
        };
        text.paint(id, inspector, bounds, &mut (), &mut (), window, cx);
        // The caret: as an input's, 1 px in the theme's caret colour.
        if !self.selection.caret.borrow().shown {
            return;
        }
        let caret = self.selection.caret(cx);
        if caret.line != self.line {
            return;
        }
        let layout = text.layout();
        let offset = floor_char_boundary(&self.text, caret.offset.min(self.text.len()));
        let Some(origin) = layout.position_for_index(offset) else {
            return;
        };
        window.paint_quad(fill(
            Bounds::new(origin, size(px(1.), layout.line_height())),
            cx.theme().caret,
        ));
    }
}

/// The list of a [`LinesSelection`]: registers it with the window's
/// selection each frame. Made by [`LinesSelection::layer`].
pub struct SelectableLinesLayer {
    child: AnyElement,
    scroll: ScrollHandle,
    selection: LinesSelection,
}

#[cfg(not(feature = "gpui-fast"))]
impl IntoElement for SelectableLinesLayer {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectableLinesLayer {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::NamedInteger(
            "selectable-lines".into(),
            self.selection.handle.entity_id().as_u64(),
        ))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        {
            let mut geometry = self.selection.geometry.borrow_mut();
            geometry.runs.clear();
            geometry.bounds = bounds;
        }
        // Behind the rows: whatever they handle themselves comes first.
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.child.prepaint(window, cx);
        let offset = self.scroll.offset();
        let text_bounds = {
            let mut geometry = self.selection.geometry.borrow_mut();
            geometry.scroll_offset = offset;
            geometry.runs.iter().map(|run| run.visible).collect()
        };
        self.selection.handle.register(
            TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_scroll_offset(offset)
                .with_text_bounds(text_bounds)
                .with_rendered_element(&self.selection.handle, window, cx),
            window,
            cx,
        );
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let runs: Vec<TextSelectionRun> = self
            .selection
            .geometry
            .borrow()
            .runs
            .iter()
            .map(|run| {
                TextSelectionRun::new(run.text.clone(), run.layout.clone(), run.visible)
                    .with_document_order(encode_key(LinePoint {
                        line: run.line,
                        offset: 0,
                    }))
            })
            .collect();
        self.selection.handle.update_runs(&runs, cx);
        // A double or triple click selects a word or a line: the window
        // selection asks for both ends through the content keys, and these
        // listeners tell `key_at` which unit it is resolving.
        // A press in the lines puts the caret there.
        window.on_mouse_event({
            let geometry = self.selection.geometry.clone();
            let caret = self.selection.caret.clone();
            move |event: &MouseDownEvent, phase, _, _| {
                if !phase.capture()
                    || event.button != MouseButton::Left
                    || event.click_count != 1
                    || event.modifiers.shift
                {
                    return;
                }
                let geometry = geometry.borrow();
                if !geometry.bounds.contains(&event.position) {
                    return;
                }
                if let Some(place) = geometry.point_at(event.position) {
                    let mut caret = caret.borrow_mut();
                    caret.at = place;
                    caret.anchor = None;
                    caret.goal = None;
                }
            }
        });
        window.on_mouse_event({
            let geometry = self.selection.geometry.clone();
            move |event: &MouseDownEvent, phase, _, _| {
                if phase.capture() && event.button == MouseButton::Left {
                    geometry.borrow_mut().multi_click = (event.click_count >= 2).then_some((
                        event.click_count,
                        event.position,
                        false,
                    ));
                }
            }
        });
        window.on_mouse_event({
            let geometry = self.selection.geometry.clone();
            move |_: &MouseMoveEvent, phase, _, _| {
                if phase.capture() {
                    geometry.borrow_mut().multi_click = None;
                }
            }
        });
        window.on_mouse_event({
            let geometry = self.selection.geometry.clone();
            move |_: &MouseUpEvent, phase, _, _| {
                if phase.capture() {
                    geometry.borrow_mut().multi_click = None;
                }
            }
        });
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::{
        Context, IntoElement, Modifiers, MouseButton, ParentElement as _, Render, ScrollHandle,
        SharedString, Styled as _, TestAppContext, Window, div, point, px, uniform_list,
    };
    use gpui_base::{TextSelection, TextSelectionLayer};

    use super::{CaretMotion, LinePoint, LinesSelection, decode_key, encode_key, moved};

    fn at(line: usize, offset: usize) -> LinePoint {
        LinePoint { line, offset }
    }

    fn source() -> Vec<SharedString> {
        ["  int main(void)", "{", "  return add(1, two);", "}"]
            .into_iter()
            .map(SharedString::from)
            .collect()
    }

    /// Each motion, at the edges too: lines wrap at their ends, a vertical
    /// move keeps the column it aims for across a short line, Home goes to
    /// the indent first.
    #[test]
    fn the_caret_moves_by_characters_words_lines_and_ends() {
        let lines = source();
        let go = |from: LinePoint, motion| moved(&lines, from, None, motion).0;
        assert_eq!(go(at(0, 0), CaretMotion::Left), at(0, 0));
        assert_eq!(go(at(1, 0), CaretMotion::Left), at(0, 16));
        assert_eq!(go(at(0, 16), CaretMotion::Right), at(1, 0));
        assert_eq!(go(at(2, 2), CaretMotion::WordRight), at(2, 9));
        assert_eq!(go(at(2, 9), CaretMotion::WordRight), at(2, 12));
        assert_eq!(go(at(2, 12), CaretMotion::WordLeft), at(2, 9));
        assert_eq!(go(at(2, 9), CaretMotion::WordLeft), at(2, 2));
        assert_eq!(go(at(2, 0), CaretMotion::WordLeft), at(1, 1));
        assert_eq!(go(at(2, 10), CaretMotion::LineStart), at(2, 2));
        assert_eq!(go(at(2, 2), CaretMotion::LineStart), at(2, 0));
        assert_eq!(go(at(2, 3), CaretMotion::LineEnd), at(2, 21));
        assert_eq!(go(at(2, 3), CaretMotion::Start), at(0, 0));
        assert_eq!(go(at(0, 3), CaretMotion::End), at(3, 1));
        // Down from column 9 through "{" and on: the column is kept.
        let (one, goal) = moved(&lines, at(0, 9), None, CaretMotion::Lines(1));
        assert_eq!((one, goal), (at(1, 1), Some(9)));
        let (two, _) = moved(&lines, one, goal, CaretMotion::Lines(1));
        assert_eq!(two, at(2, 9));
        assert_eq!(go(at(1, 0), CaretMotion::Lines(10)), at(3, 0));
        assert_eq!(go(at(1, 0), CaretMotion::Lines(-10)), at(0, 0));
    }

    /// Shift and the arrows select from the caret; the copy is that text.
    #[gpui::test]
    fn the_keyboard_selects_from_the_caret(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.update(|window, cx| {
            let selection = view.read(cx).selection.clone();
            selection.set_caret(at(1, 5), cx);
            selection.move_caret(CaretMotion::LineEnd, true, window, cx);
            selection.move_caret(CaretMotion::Right, true, window, cx);
            selection.move_caret(CaretMotion::Right, true, window, cx);
            assert_eq!(
                selection.selected_text(cx),
                "01 text
l"
            );
            assert!(TextSelection::has_selection(window, cx));
            assert_eq!(
                TextSelection::selected_text(window, cx),
                "01 text
l"
            );
            // An arrow without Shift ends it.
            selection.move_caret(CaretMotion::Left, false, window, cx);
            assert!(!selection.has_selection(cx));
            assert_eq!(selection.caret(cx), at(2, 0));
        });
    }

    /// A press puts the caret where it was; a selection the mouse made goes
    /// on under Shift and the arrows.
    #[gpui::test]
    fn the_mouse_places_the_caret_and_shift_extends_its_selection(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.simulate_click(point(px(1.), px(30.)), Modifiers::default());
        let caret = view.read_with(cx, |view, cx| view.selection.caret(cx));
        assert_eq!(caret, at(1, 0));
        cx.simulate_mouse_down(
            point(px(1.), px(10.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(290.), px(10.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(290.), px(10.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            let selection = view.read(cx).selection.clone();
            assert_eq!(selection.selected_text(cx), "line 00 text");
            selection.move_caret(CaretMotion::Lines(1), true, window, cx);
            assert_eq!(
                selection.selected_text(cx),
                "line 00 text
line 01 text"
            );
        });
    }

    struct Lines {
        selection: LinesSelection,
        lines: Rc<Vec<SharedString>>,
        scroll: gpui::UniformListScrollHandle,
    }

    impl Render for Lines {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let lines = self.lines.clone();
            let selection = self.selection.clone();
            let base: ScrollHandle = self.scroll.0.borrow().base_handle.clone();
            div().size_full().child(TextSelectionLayer).child(
                div().w(px(300.)).h(px(60.)).child(
                    self.selection.layer(
                        uniform_list("lines", lines.len(), move |range, _, _| {
                            range
                                .map(|line| {
                                    div().h(px(20.)).child(selection.line(
                                        ("line", line),
                                        line,
                                        lines[line].clone(),
                                        Vec::new(),
                                    ))
                                })
                                .collect()
                        })
                        .track_scroll(&self.scroll)
                        .size_full(),
                        base,
                    ),
                ),
            )
        }
    }

    fn open(cx: &mut TestAppContext) -> (gpui::Entity<Lines>, &mut gpui::VisualTestContext) {
        cx.update(crate::init);
        let lines: Vec<SharedString> = (0..40)
            .map(|line| SharedString::from(format!("line {line:02} text")))
            .collect();
        let lines = Rc::new(lines);
        let (view, cx) = cx.add_window_view(|_, cx| {
            let selection = LinesSelection::new(cx);
            selection.set_source(Rc::new(lines.as_ref().clone()));
            Lines {
                selection,
                lines: lines.clone(),
                scroll: gpui::UniformListScrollHandle::new(),
            }
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (view, cx)
    }

    #[test]
    fn a_key_holds_its_line_and_offset() {
        let place = LinePoint {
            line: 123_456,
            offset: 789,
        };
        assert_eq!(decode_key(encode_key(place)), place);
    }

    /// A drag across lines selects from the press to the release, and the
    /// copy is those characters, line breaks between.
    #[gpui::test]
    fn a_drag_selects_characters_across_lines(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        // "line 00 text" from its 6th character to the 2nd line's 4th.
        cx.simulate_mouse_down(
            point(px(40.), px(10.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(25.), px(30.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(25.), px(30.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (text, range) = view.read_with(cx, |view, cx| {
            (
                view.selection.selected_text(cx),
                view.selection.selected_range(cx),
            )
        });
        let Some(range) = range else {
            panic!("nothing selected");
        };
        assert_eq!((range.start.line, range.end.line), (0, 1));
        assert!(text.contains('\n'), "two lines: {text:?}");
        assert!(text.starts_with(&"line 00 text"[range.start.offset..]));
        assert!(text.ends_with(&"line 01 text"[..range.end.offset]));
        cx.update(|window, cx| {
            assert_eq!(TextSelection::selected_text(window, cx), text);
        });
    }

    /// The copy comes from the lines, not from what is painted: a selection
    /// whose start scrolled away still copies from its first line.
    #[gpui::test]
    fn the_copy_does_not_depend_on_the_painted_rows(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.simulate_mouse_down(
            point(px(1.), px(10.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(290.), px(50.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(290.), px(50.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        view.update(cx, |view, _| {
            view.scroll.scroll_to_item(30, gpui::ScrollStrategy::Top);
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let text = view.read_with(cx, |view, cx| view.selection.selected_text(cx));
        assert!(text.starts_with("line 00"), "{text:?}");
        assert!(text.contains("line 01 text\nline 02"), "{text:?}");
    }
}
