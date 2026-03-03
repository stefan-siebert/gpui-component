use std::ops::Range;

#[cfg(not(target_family = "wasm"))]
pub use tree_sitter::Tree;

#[cfg(target_family = "wasm")]
/// Stub type for tree-sitter Tree on WASM (tree-sitter not available).
pub struct Tree;

#[cfg(not(target_family = "wasm"))]
/// Minimum line span for a node to be considered foldable.
const MIN_FOLD_LINES: usize = 2;

/// A fold range representing a foldable code region.
///
/// The fold range spans from start_line to end_line (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FoldRange {
    /// Start line (inclusive)
    pub start_line: usize,
    /// End line (inclusive)
    pub end_line: usize,
}

impl FoldRange {
    pub fn new(start_line: usize, end_line: usize) -> Self {
        assert!(
            start_line <= end_line,
            "fold start_line must be <= end_line"
        );
        Self {
            start_line,
            end_line,
        }
    }
}

// ==================== Native Implementation (with tree-sitter) ====================

#[cfg(not(target_family = "wasm"))]
/// Extract fold ranges from a tree-sitter syntax tree.
///
/// Uses iterative `TreeCursor` traversal and prunes single-line subtrees
/// (they cannot contain foldable nodes). This visits only multi-line nodes,
/// making it fast even for very large files (millions of AST nodes).
pub fn extract_fold_ranges(tree: &Tree) -> Vec<FoldRange> {
    let mut ranges = Vec::new();
    collect_foldable_nodes_iterative(tree, &mut ranges);

    ranges.sort_by_key(|r| r.start_line);
    ranges.dedup_by_key(|r| r.start_line);
    ranges
}

#[cfg(not(target_family = "wasm"))]
/// Extract fold ranges only within a byte range (for incremental updates after edits).
///
/// Skips subtrees entirely outside the range, making it O(nodes in range)
/// instead of O(all nodes in tree).
pub fn extract_fold_ranges_in_range(tree: &Tree, byte_range: Range<usize>) -> Vec<FoldRange> {
    let mut ranges = Vec::new();
    collect_foldable_nodes_in_range_iterative(tree, &byte_range, &mut ranges);

    ranges.sort_by_key(|r| r.start_line);
    ranges.dedup_by_key(|r| r.start_line);
    ranges
}

#[cfg(not(target_family = "wasm"))]
/// Iterative tree traversal that prunes single-line subtrees.
///
/// A node spanning fewer than MIN_FOLD_LINES cannot itself be foldable,
/// and neither can any of its descendants. Skipping those subtrees
/// reduces the visited set from O(all nodes) to O(multi-line nodes).
fn collect_foldable_nodes_iterative(tree: &Tree, ranges: &mut Vec<FoldRange>) {
    let mut cursor = tree.walk();

    loop {
        let node = cursor.node();
        let start_row = node.start_position().row;
        let end_row = node.end_position().row;
        let multi_line = end_row.saturating_sub(start_row) >= MIN_FOLD_LINES;

        if multi_line && node.is_named() && node.parent().is_some() {
            ranges.push(FoldRange {
                start_line: start_row,
                end_line: end_row,
            });
        }

        // Only descend into children if this subtree spans enough lines
        if multi_line && cursor.goto_first_child() {
            continue;
        }

        // Move to next sibling, or backtrack up
        if !advance_cursor_to_next(&mut cursor) {
            return;
        }
    }
}

#[cfg(not(target_family = "wasm"))]
/// Iterative tree traversal within a byte range, pruning single-line and
/// out-of-range subtrees.
fn collect_foldable_nodes_in_range_iterative(
    tree: &Tree,
    byte_range: &Range<usize>,
    ranges: &mut Vec<FoldRange>,
) {
    let mut cursor = tree.walk();

    loop {
        let node = cursor.node();

        // Skip subtrees entirely outside the byte range
        if node.end_byte() <= byte_range.start || node.start_byte() >= byte_range.end {
            if !advance_cursor_to_next(&mut cursor) {
                return;
            }
            continue;
        }

        let start_row = node.start_position().row;
        let end_row = node.end_position().row;
        let multi_line = end_row.saturating_sub(start_row) >= MIN_FOLD_LINES;

        if multi_line && node.is_named() && node.parent().is_some() {
            ranges.push(FoldRange {
                start_line: start_row,
                end_line: end_row,
            });
        }

        if multi_line && cursor.goto_first_child() {
            continue;
        }

        if !advance_cursor_to_next(&mut cursor) {
            return;
        }
    }
}

#[cfg(not(target_family = "wasm"))]
/// Advance the cursor to the next sibling, or backtrack to an ancestor's sibling.
/// Returns false when the traversal is complete (back at root with no more siblings).
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

// ==================== WASM Stub Implementation ====================

#[cfg(target_family = "wasm")]
/// Extract fold ranges - WASM stub (returns empty, no tree-sitter).
pub fn extract_fold_ranges(_tree: &Tree) -> Vec<FoldRange> {
    Vec::new()
}

#[cfg(target_family = "wasm")]
/// Extract fold ranges in range - WASM stub (returns empty, no tree-sitter).
pub fn extract_fold_ranges_in_range(_tree: &Tree, _byte_range: Range<usize>) -> Vec<FoldRange> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fold_range_ordering() {
        let mut ranges = vec![
            FoldRange {
                start_line: 10,
                end_line: 20,
            },
            FoldRange {
                start_line: 5,
                end_line: 15,
            },
            FoldRange {
                start_line: 5,
                end_line: 15,
            },
            FoldRange {
                start_line: 1,
                end_line: 30,
            },
        ];

        ranges.sort_by_key(|r| r.start_line);
        ranges.dedup_by_key(|r| r.start_line);

        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].start_line, 1);
        assert_eq!(ranges[1].start_line, 5);
        assert_eq!(ranges[2].start_line, 10);
    }
}
