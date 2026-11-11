use gpui::App;

mod column;
mod data_table;
mod delegate;
mod loading;
mod row_window;
mod state;
mod table;

pub use column::*;
pub use data_table::*;
pub use delegate::*;
pub use row_window::RowWindowHandle;
pub use state::*;
pub use table::*;

pub(crate) fn init(cx: &mut App) {
    data_table::init(cx);
}
