use std::{
    cell::RefCell,
    ops::Range,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{HighlightStyle, SharedString, Task};
use gpui_base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit as BaseInputEdit, InputHighlighter,
    InputHighlighterFactory,
};
use ropey::Rope;
use tree_sitter::{InputEdit, ParseOptions, Parser, Point};

use super::{LanguageRegistry, SyntaxHighlighter};

pub(crate) fn input_highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        let config = LanguageRegistry::singleton().language(language)?;
        config.has_grammar().then(|| {
            Box::new(TreeSitterInputHighlighter::new(language)) as Box<dyn InputHighlighter>
        })
    })
}

struct TreeSitterInputHighlighter {
    inner: Rc<RefCell<SyntaxHighlighter>>,
    parse_task: Rc<RefCell<Option<Task<()>>>>,
}

impl TreeSitterInputHighlighter {
    fn new(language: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(SyntaxHighlighter::new(language))),
            parse_task: Rc::new(RefCell::new(None)),
        }
    }
}

impl SyntaxHighlighter {
    pub(crate) fn update_input(
        &mut self,
        edit: Option<BaseInputEdit>,
        text: &Rope,
        timeout: Option<Duration>,
    ) -> bool {
        self.update(edit.map(to_tree_sitter_edit), text, timeout)
    }
}

impl InputHighlighter for TreeSitterInputHighlighter {
    fn language(&self) -> SharedString {
        self.inner.borrow().language().clone()
    }

    fn update(
        &mut self,
        edit: Option<BaseInputEdit>,
        text: &Rope,
        folding: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<EditorState>,
    ) {
        const SYNC_PARSE_TIMEOUT: Duration = Duration::from_millis(2);
        const SYNC_PARSE_MAX_BYTES: usize = 256 * 1024;
        const PARSE_DEBOUNCE: Duration = Duration::from_millis(150);

        let edit = edit.map(to_tree_sitter_edit);
        let completed = {
            let mut highlighter = self.inner.borrow_mut();
            if text.len() > SYNC_PARSE_MAX_BYTES {
                highlighter.edit_tree(edit, text);
                false
            } else {
                highlighter.update(edit, text, Some(SYNC_PARSE_TIMEOUT))
            }
        };
        if completed {
            self.parse_task.borrow_mut().take();
            return;
        }

        let highlighter = self.inner.clone();
        let parse_task = self.parse_task.clone();
        let language = highlighter.borrow().language().clone();
        let old_tree = highlighter.borrow().tree().cloned();
        let injection_data = highlighter.borrow().injection_parse_data();
        let text = text.clone();
        let text_for_apply = text.clone();
        let cancel = Arc::new(AtomicBool::new(false));

        let task = cx.spawn_in(window, async move |entity, cx| {
            struct CancelOnDrop(Arc<AtomicBool>);
            impl Drop for CancelOnDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            let _cancel_guard = CancelOnDrop(cancel.clone());
            cx.background_executor().timer(PARSE_DEBOUNCE).await;

            let parse_cancel = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let config = LanguageRegistry::singleton().language(&language)?;
                    let grammar = config.language.as_ref()?;
                    let mut parser = Parser::new();
                    parser.set_language(grammar).ok()?;
                    let mut progress = |_: &tree_sitter::ParseState| {
                        if parse_cancel.load(Ordering::Relaxed) {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    };
                    let options = ParseOptions::new().progress_callback(&mut progress);
                    let tree = parser.parse_with_options(
                        &mut |offset, _| {
                            if offset >= text.len() {
                                ""
                            } else {
                                let (chunk, chunk_byte_ix) = text.chunk(offset);
                                &chunk[offset - chunk_byte_ix..]
                            }
                        },
                        old_tree.as_ref(),
                        Some(options),
                    )?;
                    if parse_cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    let injections = injection_data.map_or_else(Default::default, |data| {
                        SyntaxHighlighter::compute_injection_layers(data, &tree, &text)
                    });
                    let folds = if folding {
                        extract_fold_ranges(&tree)
                    } else {
                        Vec::new()
                    };
                    Some((tree, injections, folds))
                })
                .await;

            if let Some((tree, injections, folds)) = result {
                highlighter
                    .borrow_mut()
                    .apply_background_tree(tree, &text_for_apply, injections);
                let _ = entity.update(cx, |state, cx| {
                    state.apply_highlighter_fold_candidates(folds, cx);
                });
            }
        });
        parse_task.borrow_mut().replace(task);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.inner.borrow().styles(range, resolver)
    }
    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        self.inner
            .borrow()
            .tree()
            .map(extract_fold_ranges)
            .unwrap_or_default()
    }

    fn fold_ranges_for_edit(&self, range: Range<usize>, _: &Rope) -> Vec<FoldRange> {
        self.inner
            .borrow()
            .tree()
            .map(|tree| extract_fold_ranges_in_range(tree, range))
            .unwrap_or_default()
    }
}

fn to_tree_sitter_edit(edit: BaseInputEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: Point::new(edit.start_position.row, edit.start_position.column),
        old_end_position: Point::new(edit.old_end_position.row, edit.old_end_position.column),
        new_end_position: Point::new(edit.new_end_position.row, edit.new_end_position.column),
    }
}

/// A node spanning fewer lines than this cannot be folded, and neither can any
/// of its descendants — which is what makes pruning below sound.
const MIN_FOLD_LINES: usize = 2;

fn extract_fold_ranges(tree: &tree_sitter::Tree) -> Vec<FoldRange> {
    let mut ranges = Vec::new();
    collect_foldable_nodes(tree, None, &mut ranges);
    finish_fold_ranges(ranges)
}

fn extract_fold_ranges_in_range(
    tree: &tree_sitter::Tree,
    byte_range: Range<usize>,
) -> Vec<FoldRange> {
    let mut ranges = Vec::new();
    collect_foldable_nodes(tree, Some(&byte_range), &mut ranges);
    finish_fold_ranges(ranges)
}

fn finish_fold_ranges(mut ranges: Vec<FoldRange>) -> Vec<FoldRange> {
    ranges.sort_by_key(|range| range.start_line);
    ranges.dedup_by_key(|range| range.start_line);
    ranges
}

/// Walk the tree with a `TreeCursor` rather than by recursion, pruning
/// single-line subtrees and — when `byte_range` is given — subtrees outside it.
///
/// This fork carried its own iterative copy of this traversal since before the
/// 2026-09-01 upstream merge; upstream's version prunes the same way but
/// recurses, so a deeply nested tree is a stack-depth question rather than a
/// bounded walk. Keeping the cursor form costs nothing and removes that limit.
fn collect_foldable_nodes(
    tree: &tree_sitter::Tree,
    byte_range: Option<&Range<usize>>,
    ranges: &mut Vec<FoldRange>,
) {
    let mut cursor = tree.walk();

    loop {
        let node = cursor.node();

        if let Some(bytes) = byte_range
            && (node.end_byte() <= bytes.start || node.start_byte() >= bytes.end)
        {
            if !advance_cursor_to_next(&mut cursor) {
                return;
            }
            continue;
        }

        let start_row = node.start_position().row;
        let end_row = node.end_position().row;
        let multi_line = end_row.saturating_sub(start_row) >= MIN_FOLD_LINES;

        // `parent().is_some()` skips the root, which is never foldable.
        if multi_line && node.is_named() && node.parent().is_some() {
            ranges.push(FoldRange::new(start_row, end_row));
        }

        // Descend only into subtrees that span enough lines to hold a fold.
        if multi_line && cursor.goto_first_child() {
            continue;
        }

        if !advance_cursor_to_next(&mut cursor) {
            return;
        }
    }
}

/// Move to the next sibling, backtracking through ancestors as needed.
/// Returns false once the traversal is back at the root with nothing left.
fn advance_cursor_to_next(cursor: &mut tree_sitter::TreeCursor) -> bool {
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
    }
}
