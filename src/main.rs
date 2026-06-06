#![allow(unused, non_snake_case, dead_code, non_camel_case_types, unpredictable_function_pointer_comparisons, clippy::all)]

pub mod definitions;
pub mod global;
pub mod utils;
pub mod chars;
pub mod history;
pub mod help;
pub mod move_;   // C: move.c (move is a Rust keyword)
pub mod cut;
pub mod search;
pub mod files;
pub mod winio;
pub mod prompt;
pub mod text;
pub mod rcfile;
pub mod color;
pub mod browser;
pub mod nano;
mod installer;

/// Minimal gettext pass-through macro.
/// C: _("string") or P_("singular","plural",n)
///
/// With literal strings it returns the literal; with format arguments it
/// returns an owned String via format!().
#[macro_export]
macro_rules! tr {
    // Single literal: no allocation needed.
    ($s:literal) => { $s };
    // Format string with arguments: produce an owned String.
    ($fmt:literal, $($arg:tt)*) => {
        format!($fmt, $($arg)*)
    };
}

fn main() {
    nano::nano_main();
}
