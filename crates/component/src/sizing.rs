use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{App, Edges, Pixels, Styled, px};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Platform-aware control sizing (fork addition)
// ---------------------------------------------------------------------------
//
// Windows WinUI3 controls default to 32px height; macOS native controls are
// ~22-24px. These helpers add a per-platform boost so each `Size` tier maps to
// values that feel native on both platforms. They used to live in `styled.rs`
// beside the fork's own copies of `Sizable` / `StyleSized`; upstream moved
// those traits here in the 2026-09-01 merge, so the boosts moved with them.

/// Extra height for interactive controls on Windows.
#[inline(always)]
pub(crate) fn platform_control_h_boost() -> Pixels {
    if cfg!(target_os = "windows") {
        px(4.0)
    } else {
        px(0.0)
    }
}

/// Extra horizontal padding for interactive controls on Windows.
#[inline(always)]
pub(crate) fn platform_control_px_boost() -> Pixels {
    if cfg!(target_os = "windows") {
        px(2.0)
    } else {
        px(0.0)
    }
}

/// Extra vertical padding for interactive controls on Windows.
#[inline(always)]
pub(crate) fn platform_control_py_boost() -> Pixels {
    if cfg!(target_os = "windows") {
        px(2.0)
    } else {
        px(0.0)
    }
}

// ---------------------------------------------------------------------------
// Control scale (fork addition)
// ---------------------------------------------------------------------------
//
// One factor every size tier's geometry is multiplied by — the heights and
// paddings of buttons, inputs, menu items, table rows, tabs and the title bar —
// so an app can let its controls follow its own density and font-size
// settings. Text needs no factor: it is sized in rems and follows the window's
// rem size already, which is exactly why fixed-pixel boxes around it did not
// fit once the font grew.
//
// Process-wide rather than a `Theme` field, because the size helpers run in
// builder chains that have no `cx` (`StyleSized`, `Size::table_row_height`),
// and an app has one control scale the way it has one theme. A
// `Size::Size(px)` the caller chose stays as it is.

static CONTROL_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// The control scale [`set_control_scale`] accepts; values outside are clamped.
pub const CONTROL_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=3.0;

/// The factor every size tier's geometry is multiplied by, `1.0` by default.
pub fn control_scale() -> f32 {
    f32::from_bits(CONTROL_SCALE.load(Ordering::Relaxed))
}

/// Set the control scale, clamped to [`CONTROL_SCALE_RANGE`] (a value that is
/// not finite means `1.0`), and refresh every window if it changed.
pub fn set_control_scale(scale: f32, cx: &mut App) {
    let scale = if scale.is_finite() {
        scale.clamp(*CONTROL_SCALE_RANGE.start(), *CONTROL_SCALE_RANGE.end())
    } else {
        1.0
    };
    let previous = CONTROL_SCALE.swap(scale.to_bits(), Ordering::Relaxed);
    if previous != scale.to_bits() {
        cx.refresh_windows();
    }
}

/// `value` at the control scale, rounded to whole pixels.
///
/// For geometry that has to line up with a control without being one — a
/// bar's height, a slot beside a button.
pub fn scaled_px(value: Pixels) -> Pixels {
    scaled_by(value, control_scale())
}

fn scaled_by(value: Pixels, scale: f32) -> Pixels {
    px((value.as_f32() * scale).round())
}

/// A size for elements.
#[derive(Clone, Default, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub enum Size {
    Size(Pixels),
    XSmall,
    Small,
    #[default]
    Medium,
    Large,
}

impl Size {
    fn as_f32(&self) -> f32 {
        match self {
            Size::Size(val) => val.as_f32(),
            Size::XSmall => 0.,
            Size::Small => 1.,
            Size::Medium => 2.,
            Size::Large => 3.,
        }
    }

    /// Returns the size as a static string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Size::XSmall => "xs",
            Size::Small => "sm",
            Size::Medium => "md",
            Size::Large => "lg",
            Size::Size(_) => "custom",
        }
    }

    /// Create a Size from a static string.
    ///
    /// - "xs" or "xsmall"
    /// - "sm" or "small"
    /// - "md" or "medium"
    /// - "lg" or "large"
    ///
    /// Any other value will return Size::Medium.
    pub fn from_str(size: &str) -> Self {
        match size.to_lowercase().as_str() {
            "xs" | "xsmall" => Size::XSmall,
            "sm" | "small" => Size::Small,
            "md" | "medium" => Size::Medium,
            "lg" | "large" => Size::Large,
            _ => Size::Medium,
        }
    }

    /// Returns the height for table row, at the control scale.
    #[inline]
    pub fn table_row_height(&self) -> Pixels {
        match self {
            Size::Size(size) => *size,
            Size::XSmall => scaled_px(px(26.)),
            Size::Small => scaled_px(px(30.)),
            Size::Large => scaled_px(px(40.)),
            _ => scaled_px(px(32.)),
        }
    }

    /// The height of a `Button` of this size — the icon button's edge too —
    /// at the control scale, Windows boost included.
    pub fn button_height(&self) -> Pixels {
        let boost = platform_control_h_boost();
        match self {
            Size::Size(size) => *size,
            Size::XSmall => scaled_px(px(20.) + boost),
            Size::Small => scaled_px(px(24.) + boost),
            Size::Medium | Size::Large => scaled_px(px(32.) + boost),
        }
    }

    /// The height of an `Input`, `Select` and the like of this size, at the
    /// control scale, Windows boost included.
    pub fn input_height(&self) -> Pixels {
        let boost = platform_control_h_boost();
        match self {
            Size::Large => scaled_px(px(44.) + boost),
            Size::Medium => scaled_px(px(32.) + boost),
            Size::Small => scaled_px(px(24.) + boost),
            Size::XSmall => scaled_px(px(20.) + boost),
            Size::Size(_) => scaled_px(px(24.) + boost),
        }
    }

    /// The height of a `PopupMenu` item of this size, at the control scale.
    pub fn menu_item_height(&self) -> Pixels {
        match self {
            Size::XSmall | Size::Small => scaled_px(px(20.)),
            Size::Size(size) => *size,
            Size::Medium | Size::Large => scaled_px(px(26.)),
        }
    }

    /// Returns the padding for a table cell.
    #[inline]
    pub fn table_cell_padding(&self) -> Edges<Pixels> {
        match self {
            Size::XSmall => Edges {
                top: scaled_px(px(2.)),
                bottom: scaled_px(px(2.)),
                left: scaled_px(px(4.)),
                right: scaled_px(px(4.)),
            },
            Size::Small => Edges {
                top: scaled_px(px(3.)),
                bottom: scaled_px(px(3.)),
                left: scaled_px(px(6.)),
                right: scaled_px(px(6.)),
            },
            Size::Large => Edges {
                top: scaled_px(px(8.)),
                bottom: scaled_px(px(8.)),
                left: scaled_px(px(12.)),
                right: scaled_px(px(12.)),
            },
            _ => Edges {
                top: scaled_px(px(4.)),
                bottom: scaled_px(px(4.)),
                left: scaled_px(px(8.)),
                right: scaled_px(px(8.)),
            },
        }
    }

    /// Returns a smaller size.
    pub fn smaller(&self) -> Self {
        match self {
            Size::XSmall => Size::XSmall,
            Size::Small => Size::XSmall,
            Size::Medium => Size::Small,
            Size::Large => Size::Medium,
            Size::Size(val) => Size::Size(*val * 0.2),
        }
    }

    /// Returns a larger size.
    pub fn larger(&self) -> Self {
        match self {
            Size::XSmall => Size::Small,
            Size::Small => Size::Medium,
            Size::Medium => Size::Large,
            Size::Large => Size::Large,
            Size::Size(val) => Size::Size(*val * 1.2),
        }
    }

    /// Return the max size between two sizes.
    ///
    /// e.g. `Size::XSmall.max(Size::Small)` will return `Size::XSmall`.
    pub fn max(&self, other: Self) -> Self {
        match (self, other) {
            (Size::Size(a), Size::Size(b)) => Size::Size(px(a.as_f32().min(b.as_f32()))),
            (Size::Size(a), _) => Size::Size(*a),
            (_, Size::Size(b)) => Size::Size(b),
            (a, b) if a.as_f32() < b.as_f32() => *a,
            _ => other,
        }
    }

    /// Return the min size between two sizes.
    ///
    /// e.g. `Size::XSmall.min(Size::Small)` will return `Size::Small`.
    pub fn min(&self, other: Self) -> Self {
        match (self, other) {
            (Size::Size(a), Size::Size(b)) => Size::Size(px(a.as_f32().max(b.as_f32()))),
            (Size::Size(a), _) => Size::Size(*a),
            (_, Size::Size(b)) => Size::Size(b),
            (a, b) if a.as_f32() > b.as_f32() => *a,
            _ => other,
        }
    }

    /// Returns the horizontal input padding.
    pub fn input_px(&self) -> Pixels {
        let boost = platform_control_px_boost();
        scaled_px(match self {
            Self::Large => px(12.) + boost,
            Self::Medium => px(10.) + boost,
            Self::Small => px(8.) + boost,
            Self::XSmall => px(4.) + boost,
            _ => px(8.) + boost,
        })
    }

    /// Returns the vertical input padding.
    pub fn input_py(&self) -> Pixels {
        let boost = platform_control_py_boost();
        scaled_px(match self {
            Size::Large => px(10.) + boost,
            Size::Medium => px(8.) + boost,
            Size::Small => px(2.) + boost,
            Size::XSmall => px(0.) + boost,
            _ => px(2.) + boost,
        })
    }

    /// Returns the horizontal padding of a row in an input's dropdown list.
    ///
    /// The trigger insets its text by a 1px border plus [`Self::input_px`]; the
    /// popup insets a row by [`DROPDOWN_LIST_PADDING`]. The row pads the
    /// difference, so the chosen item's text lines up with the trigger's.
    pub(crate) fn list_px(&self) -> Pixels {
        self.input_px() + px(1.) - DROPDOWN_LIST_PADDING
    }
}

/// The inset between an input's dropdown popup and its list rows.
pub(crate) const DROPDOWN_LIST_PADDING: Pixels = px(4.);

impl From<Pixels> for Size {
    fn from(size: Pixels) -> Self {
        Size::Size(size)
    }
}

/// A trait for setting the size of an element.
/// Size::Medium is use by default.
#[allow(patterns_in_fns_without_body)]
pub trait Sizable: Sized {
    /// Set the ui::Size of this element.
    ///
    /// Also can receive a `ButtonSize` to convert to `IconSize`,
    /// Or a `Pixels` to set a custom size: `px(30.)`
    fn with_size(mut self, size: impl Into<Size>) -> Self;

    /// Apply control-specific presentation when this element is hosted by a
    /// toolbar. Most controls keep their normal presentation; command buttons
    /// override this hook to use the toolbar's quiet, compact treatment.
    #[doc(hidden)]
    fn prepare_for_toolbar(self) -> Self {
        self
    }

    /// Set to Size::XSmall
    #[inline(always)]
    fn xsmall(self) -> Self {
        self.with_size(Size::XSmall)
    }

    /// Set to Size::Small
    #[inline(always)]
    fn small(self) -> Self {
        self.with_size(Size::Small)
    }

    /// Set to Size::Large
    #[inline(always)]
    fn large(self) -> Self {
        self.with_size(Size::Large)
    }
}

#[allow(unused)]
pub trait StyleSized<T: Styled> {
    fn input_text_size(self, size: Size) -> Self;
    fn input_size(self, size: Size) -> Self;
    fn input_pl(self, size: Size) -> Self;
    fn input_pr(self, size: Size) -> Self;
    fn input_px(self, size: Size) -> Self;
    fn input_py(self, size: Size) -> Self;
    fn input_h(self, size: Size) -> Self;
    fn list_size(self, size: Size) -> Self;
    fn list_px(self, size: Size) -> Self;
    fn list_py(self, size: Size) -> Self;
    /// Apply size with the given `Size`.
    fn size_with(self, size: Size) -> Self;
    /// Apply the table cell size (Font size, padding) with the given `Size`.
    fn table_cell_size(self, size: Size) -> Self;
    fn button_text_size(self, size: Size) -> Self;
}

impl<T: Styled> StyleSized<T> for T {
    #[inline]
    fn input_text_size(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.text_xs(),
            Size::Small => self.text_sm(),
            Size::Medium => self.text_sm(),
            Size::Large => self.text_base(),
            Size::Size(size) => self.text_size(size * 0.875),
        }
    }

    #[inline]
    fn input_size(self, size: Size) -> Self {
        self.input_px(size).input_py(size).input_h(size)
    }

    #[inline]
    fn input_pl(self, size: Size) -> Self {
        self.pl(size.input_px())
    }

    #[inline]
    fn input_pr(self, size: Size) -> Self {
        self.pr(size.input_px())
    }

    #[inline]
    fn input_px(self, size: Size) -> Self {
        self.px(size.input_px())
    }

    #[inline]
    fn input_py(self, size: Size) -> Self {
        self.py(size.input_py())
    }

    #[inline]
    fn input_h(self, size: Size) -> Self {
        self.h(size.input_height())
    }

    #[inline]
    fn list_size(self, size: Size) -> Self {
        self.list_px(size).list_py(size).input_text_size(size)
    }

    #[inline]
    fn list_px(self, size: Size) -> Self {
        self.px(size.list_px())
    }

    #[inline]
    fn list_py(self, size: Size) -> Self {
        match size {
            Size::Large => self.py_2(),
            Size::Medium => self.py_1(),
            Size::Small => self.py_0p5(),
            _ => self.py_1(),
        }
    }

    #[inline]
    fn size_with(self, size: Size) -> Self {
        match size {
            Size::Large => self.size_11(),
            Size::Medium => self.size_8(),
            Size::Small => self.size_5(),
            Size::XSmall => self.size_4(),
            Size::Size(size) => self.size(size),
        }
    }

    #[inline]
    fn table_cell_size(self, size: Size) -> Self {
        let padding = size.table_cell_padding();
        match size {
            Size::XSmall => self.text_xs(),
            Size::Small | Size::Medium => self.text_sm(),
            Size::Large => self.text_base(),
            _ => self,
        }
        .pl(padding.left)
        .pr(padding.right)
        .pt(padding.top)
        .pb(padding.bottom)
    }

    fn button_text_size(self, size: Size) -> Self {
        match size {
            Size::XSmall => self.text_xs(),
            Size::Small | Size::Medium => self.text_sm(),
            _ => self.text_base(),
        }
    }
}
#[cfg(test)]
mod tests {
    use gpui::px;

    use super::scaled_by;
    use crate::Size;

    #[test]
    fn test_size_max_min() {
        assert_eq!(Size::Small.min(Size::XSmall), Size::Small);
        assert_eq!(Size::XSmall.min(Size::Small), Size::Small);
        assert_eq!(Size::Small.min(Size::Medium), Size::Medium);
        assert_eq!(Size::Medium.min(Size::Large), Size::Large);
        assert_eq!(Size::Large.min(Size::Small), Size::Large);

        assert_eq!(
            Size::Size(px(10.)).min(Size::Size(px(20.))),
            Size::Size(px(20.))
        );

        // Min
        assert_eq!(Size::Small.max(Size::XSmall), Size::XSmall);
        assert_eq!(Size::XSmall.max(Size::Small), Size::XSmall);
        assert_eq!(Size::Small.max(Size::Medium), Size::Small);
        assert_eq!(Size::Medium.max(Size::Large), Size::Medium);
        assert_eq!(Size::Large.max(Size::Small), Size::Small);

        assert_eq!(
            Size::Size(px(10.)).max(Size::Size(px(20.))),
            Size::Size(px(10.))
        );
    }

    #[test]
    fn test_size_as_str() {
        assert_eq!(Size::XSmall.as_str(), "xs");
        assert_eq!(Size::Small.as_str(), "sm");
        assert_eq!(Size::Medium.as_str(), "md");
        assert_eq!(Size::Large.as_str(), "lg");
        assert_eq!(Size::Size(px(15.)).as_str(), "custom");
    }

    #[test]
    fn test_table_row_height() {
        assert_eq!(Size::XSmall.table_row_height(), px(26.));
        assert_eq!(Size::Small.table_row_height(), px(30.));
        assert_eq!(Size::Medium.table_row_height(), px(32.));
        assert_eq!(Size::Large.table_row_height(), px(40.));
        assert_eq!(Size::Size(px(48.)).table_row_height(), px(48.));
    }

    #[test]
    fn scaled_rounds_to_whole_pixels() {
        assert_eq!(scaled_by(px(24.), 1.0), px(24.));
        assert_eq!(scaled_by(px(24.), 0.85), px(20.));
        assert_eq!(scaled_by(px(26.), 0.85), px(22.));
        assert_eq!(scaled_by(px(32.), 1.15), px(37.));
        assert_eq!(scaled_by(px(0.), 2.0), px(0.));
    }

    #[test]
    fn test_size_from_str() {
        assert_eq!(Size::from_str("xs"), Size::XSmall);
        assert_eq!(Size::from_str("xsmall"), Size::XSmall);
        assert_eq!(Size::from_str("sm"), Size::Small);
        assert_eq!(Size::from_str("small"), Size::Small);
        assert_eq!(Size::from_str("md"), Size::Medium);
        assert_eq!(Size::from_str("medium"), Size::Medium);
        assert_eq!(Size::from_str("lg"), Size::Large);
        assert_eq!(Size::from_str("large"), Size::Large);
        assert_eq!(Size::from_str("unknown"), Size::Medium);

        // Case insensitive
        assert_eq!(Size::from_str("XS"), Size::XSmall);
        assert_eq!(Size::from_str("SMALL"), Size::Small);
        assert_eq!(Size::from_str("Md"), Size::Medium);
    }
}
