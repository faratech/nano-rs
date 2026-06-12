#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/definitions.h from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2017, 2020-2022, 2024 Benno Schulenberg

use std::rc::{Rc, Weak};
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Version constants
// ---------------------------------------------------------------------------

/// The GNU nano release this port mirrors (shown as "GNU nano, version X" and
/// written into lock files / the credits screen for compatibility). This is
/// distinct from the nano-rs release version, which is the crate version
/// (`CARGO_PKG_VERSION`) and is what release tags and the self-updater track.
pub const GNU_NANO_VERSION: &str = "9.0.0";

// ---------------------------------------------------------------------------
// Platform / path constants
// ---------------------------------------------------------------------------

/// Default root UID (0 on POSIX; 65535 on Tandem NonStop).
pub const ROOT_UID: u32 = 0;

/// Maximum path length when the OS does not define one.
pub const PATH_MAX: usize = 4096;

// ---------------------------------------------------------------------------
// Boolean direction / mode helpers (C #define → typed const)
// ---------------------------------------------------------------------------

pub const BACKWARD: bool = false;
pub const FORWARD:  bool = true;

pub const YESORNO:     bool = false;
pub const YESORALLORNO: bool = true;

pub const YES:    i32 = 1;
pub const ALL:    i32 = 2;
pub const NO:     i32 = 0;
pub const CANCEL: i32 = -1;

pub const BLIND:   bool = false;
pub const VISIBLE: bool = true;

pub const JUSTFIND:  i32 = 0;
pub const REPLACING: i32 = 1;
pub const INREGION:  i32 = 2;

pub const NORMAL:    bool = true;
pub const SPECIAL:   bool = false;
pub const TEMPORARY: bool = false;

pub const ANNOTATE: bool = true;
pub const NONOTES:  bool = false;

pub const PRUNE_DUPLICATE:    bool = true;
pub const IGNORE_DUPLICATES:  bool = false;

// ---------------------------------------------------------------------------
// UTF-8 / character constants
// ---------------------------------------------------------------------------

/// Maximum byte length of a valid UTF-8 character (4 with UTF-8, 1 without).
#[cfg(feature = "utf8")]
pub const MAXCHARLEN: usize = 4;
#[cfg(not(feature = "utf8"))]
pub const MAXCHARLEN: usize = 1;

/// Default width (in columns) of a tab character.
pub const WIDTH_OF_TAB: usize = 8;

/// Default number of columns from end-of-line where soft-wrapping occurs.
pub const COLUMNS_FROM_EOL: usize = 8;

/// Number of columns the cursor should stay away from the edge (scrolling cushion).
pub const CUSHION: usize = 3;

/// The default comment character when a syntax does not specify any.
pub const GENERAL_COMMENT_CHARACTER: &str = "#";

/// The maximum number of search/replace history strings to keep.
pub const MAX_SEARCH_HISTORY: usize = 100;

/// The largest usize value that does not have its high bit set.
pub const HIGHEST_POSITIVE: usize = usize::MAX >> 1;

// ---------------------------------------------------------------------------
// Color-related constants
// ---------------------------------------------------------------------------

/// Represents the terminal's default color (no explicit color).
#[cfg(feature = "color")]
pub const THE_DEFAULT: i16 = -1;

/// Sentinel value for an invalid / unrecognised color.
#[cfg(feature = "color")]
pub const BAD_COLOR: i16 = -2;

/// Multiline-regex coverage flags — stored in `LineNode::multidata`.
/// The start/end regexes don't cover this line at all.
#[cfg(feature = "color")]
pub const NOTHING:    i16 = 1 << 1;
/// The start regex matches on this line; the end regex on a later one.
#[cfg(feature = "color")]
pub const STARTSHERE: i16 = 1 << 2;
/// Start matches on an earlier line; end matches on a later one.
#[cfg(feature = "color")]
pub const WHOLELINE:  i16 = 1 << 3;
/// Start matches on an earlier line; end matches on this line.
#[cfg(feature = "color")]
pub const ENDSHERE:   i16 = 1 << 4;
/// Both start and end match within this line.
#[cfg(feature = "color")]
pub const JUSTONTHIS: i16 = 1 << 5;

// ---------------------------------------------------------------------------
// Basic control codes
// ---------------------------------------------------------------------------

pub const ESC_CODE: u32 = 0x1B;
pub const DEL_CODE: u32 = 0x7F;

// ---------------------------------------------------------------------------
// Modified / extended key codes (beyond ncurses KEY_MAX)
// ---------------------------------------------------------------------------

pub const CONTROL_LEFT:   u32 = 0x401;
pub const CONTROL_RIGHT:  u32 = 0x402;
pub const CONTROL_UP:     u32 = 0x403;
pub const CONTROL_DOWN:   u32 = 0x404;
pub const CONTROL_HOME:   u32 = 0x405;
pub const CONTROL_END:    u32 = 0x406;
pub const CONTROL_DELETE: u32 = 0x40D;

pub const SHIFT_CONTROL_LEFT:   u32 = 0x411;
pub const SHIFT_CONTROL_RIGHT:  u32 = 0x412;
pub const SHIFT_CONTROL_UP:     u32 = 0x413;
pub const SHIFT_CONTROL_DOWN:   u32 = 0x414;
pub const SHIFT_CONTROL_HOME:   u32 = 0x415;
pub const SHIFT_CONTROL_END:    u32 = 0x416;
pub const CONTROL_SHIFT_DELETE: u32 = 0x41D;

pub const ALT_LEFT:     u32 = 0x421;
pub const ALT_RIGHT:    u32 = 0x422;
pub const ALT_UP:       u32 = 0x423;
pub const ALT_DOWN:     u32 = 0x424;
pub const ALT_HOME:     u32 = 0x425;
pub const ALT_END:      u32 = 0x426;
pub const ALT_PAGEUP:   u32 = 0x427;
pub const ALT_PAGEDOWN: u32 = 0x428;
pub const ALT_INSERT:   u32 = 0x42C;
pub const ALT_DELETE:   u32 = 0x42D;

pub const SHIFT_ALT_LEFT:  u32 = 0x431;
pub const SHIFT_ALT_RIGHT: u32 = 0x432;
pub const SHIFT_ALT_UP:    u32 = 0x433;
pub const SHIFT_ALT_DOWN:  u32 = 0x434;

pub const SHIFT_UP:       u32 = 0x453;
pub const SHIFT_DOWN:     u32 = 0x454;
pub const SHIFT_HOME:     u32 = 0x455;
pub const SHIFT_END:      u32 = 0x456;
pub const SHIFT_PAGEUP:   u32 = 0x457;
pub const SHIFT_PAGEDOWN: u32 = 0x458;
pub const SHIFT_DELETE:   u32 = 0x45D;
pub const SHIFT_TAB:      u32 = 0x45F;

pub const FOCUS_IN:  u32 = 0x491;
pub const FOCUS_OUT: u32 = 0x499;

/// Start-of-bracketed-paste signal.
pub const START_OF_PASTE: u32 = 0x4B5;
/// End-of-bracketed-paste signal.
pub const END_OF_PASTE:   u32 = 0x4BE;

/// A string bind has been partially planted or has an unpaired opening brace.
pub const MORE_PLANTS:       u32 = 0x4EA;
/// A string bind has an unpaired opening brace.
pub const MISSING_BRACE:     u32 = 0x4EB;
/// A function in a string bind needs to be executed.
pub const PLANTED_A_COMMAND: u32 = 0x4EC;
/// A specified function name in a string bind is invalid.
pub const NO_SUCH_FUNCTION:  u32 = 0x4EF;

/// Ctrl + centre key on the numeric keypad.
#[cfg(not(feature = "tiny"))]
pub const KEY_CENTER: u32 = 0x4F0;

/// Synthetic keycode sent when a SIGWINCH (window resize) is received.
#[cfg(not(feature = "tiny"))]
pub const THE_WINDOW_RESIZED: u32 = 0x4F7;

/// An unknown / unrecognised escape sequence was received.
pub const FOREIGN_SEQUENCE: u32 = 0x4FC;

/// A synthetic keycode for priming the input stream after suspension.
pub const KEY_FRESH: u32 = 0x4FE;

// ---------------------------------------------------------------------------
// Undo-record extra flags
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tiny"))]
pub const WAS_BACKSPACE_AT_EOF: i32 = 1 << 1;
#[cfg(not(feature = "tiny"))]
pub const WAS_WHOLE_LINE:       i32 = 1 << 2;
#[cfg(not(feature = "tiny"))]
pub const INCLUDED_LAST_LINE:   i32 = 1 << 3;
#[cfg(not(feature = "tiny"))]
pub const MARK_WAS_SET:         i32 = 1 << 4;
#[cfg(not(feature = "tiny"))]
pub const CURSOR_WAS_AT_HEAD:   i32 = 1 << 5;
#[cfg(not(feature = "tiny"))]
pub const HAD_ANCHOR_AT_START:  i32 = 1 << 6;

// ---------------------------------------------------------------------------
// Menu identifier bitmasks
// ---------------------------------------------------------------------------

pub const MMAIN:        u32 = 1 << 0;
pub const MWHEREIS:     u32 = 1 << 1;
pub const MREPLACE:     u32 = 1 << 2;
pub const MREPLACEWITH: u32 = 1 << 3;
pub const MGOTOLINE:    u32 = 1 << 4;
pub const MWRITEFILE:   u32 = 1 << 5;
pub const MINSERTFILE:  u32 = 1 << 6;
pub const MEXECUTE:     u32 = 1 << 7;
pub const MHELP:        u32 = 1 << 8;
pub const MSPELL:       u32 = 1 << 9;
pub const MBROWSER:     u32 = 1 << 10;
pub const MWHEREISFILE: u32 = 1 << 11;
pub const MGOTODIR:     u32 = 1 << 12;
pub const MYESNO:       u32 = 1 << 13;
pub const MLINTER:      u32 = 1 << 14;
pub const MFINDINHELP:  u32 = 1 << 15;

/// Abbreviation for all menus except Help, Browser, and YesNo.
pub const MMOST: u32 = MMAIN | MWHEREIS | MREPLACE | MREPLACEWITH | MGOTOLINE
    | MWRITEFILE | MINSERTFILE | MEXECUTE | MWHEREISFILE | MGOTODIR
    | MFINDINHELP | MSPELL | MLINTER;

/// Like MMOST but also includes the Browser menu (tiny: only MMAIN|MBROWSER).
#[cfg(not(feature = "tiny"))]
pub const MSOME: u32 = MMOST | MBROWSER;
#[cfg(feature = "tiny")]
pub const MSOME: u32 = MMAIN | MBROWSER;

// ---------------------------------------------------------------------------
// Enumeration types
// ---------------------------------------------------------------------------

/// The on-disk line-ending format of an open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatType {
    /// Not yet determined.
    Unspecified,
    /// Unix (LF) line endings.
    NixFile,
    /// DOS/Windows (CRLF) line endings.
    DosFile,
}

impl Default for FormatType {
    fn default() -> Self {
        FormatType::Unspecified
    }
}

/// Severity levels for status-bar messages.
/// C: typedef enum { VACUUM, HUSH, REMARK, INFO, NOTICE, AHEM, MILD, ALERT } message_type;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MessageType {
    Vacuum,
    Hush,
    Remark,
    Info,
    Notice,
    Ahem,
    Mild,
    Alert,
}

impl Default for MessageType {
    fn default() -> Self {
        MessageType::Vacuum
    }
}

/// How a file should be written.
/// C: typedef enum { OVERWRITE, APPEND, PREPEND, EMERGENCY } kind_of_writing_type;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindOfWritingType {
    Overwrite,
    Append,
    Prepend,
    Emergency,
}

impl Default for KindOfWritingType {
    fn default() -> Self {
        KindOfWritingType::Overwrite
    }
}

/// How the edit window should be refreshed after a cursor move.
/// C: typedef enum { CENTERING, FLOWING, STATIONARY } update_type;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    Centering,
    Flowing,
    Stationary,
}

impl Default for UpdateType {
    fn default() -> Self {
        UpdateType::Centering
    }
}

/// The kinds of undo/redo actions.  ADD…REPLACE must come first.
/// C: typedef enum { ADD, ENTER, BACK, DEL, JOIN, REPLACE, … } undo_type;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoType {
    Add,
    Enter,
    Back,
    Del,
    Join,
    Replace,
    #[cfg(feature = "wrapping")]
    SplitBegin,
    #[cfg(feature = "wrapping")]
    SplitEnd,
    Indent,
    Unindent,
    #[cfg(feature = "comment")]
    Comment,
    #[cfg(feature = "comment")]
    Uncomment,
    #[cfg(feature = "comment")]
    Preflight,
    Zap,
    Cut,
    CutToEof,
    Copy,
    Paste,
    Insert,
    CoupleBegin,
    CoupleEnd,
    Other,
}

impl Default for UndoType {
    fn default() -> Self {
        UndoType::Other
    }
}

// ---------------------------------------------------------------------------
// Interface-element color indices
// ---------------------------------------------------------------------------

/// Index into the color-pair array for the title bar.
pub const TITLE_BAR:      usize = 0;
/// Index for line numbers.
pub const LINE_NUMBER:     usize = 1;
/// Index for the guide stripe.
pub const GUIDE_STRIPE:    usize = 2;
/// Index for the scroll bar.
pub const SCROLL_BAR:      usize = 3;
/// Index for selected (marked) text.
pub const SELECTED_TEXT:   usize = 4;
/// Index for spotlighted (found) text.
pub const SPOTLIGHTED:     usize = 5;
/// Index for the mini info bar.
pub const MINI_INFOBAR:    usize = 6;
/// Index for the prompt bar.
pub const PROMPT_BAR:      usize = 7;
/// Index for the status bar.
pub const STATUS_BAR:      usize = 8;
/// Index for error messages.
pub const ERROR_MESSAGE:   usize = 9;
/// Index for key-combo display.
pub const KEY_COMBO:       usize = 10;
/// Index for function-tag display.
pub const FUNCTION_TAG:    usize = 11;
/// Total number of independently colorable interface elements.
pub const NUMBER_OF_ELEMENTS: usize = 12;

// ---------------------------------------------------------------------------
// Runtime flags (stored in the flags[] array).
// Each constant is the bit position; use the FLAGS/FLAGMASK logic in global.rs.
// ---------------------------------------------------------------------------

pub const DONTUSE:            u32 = 0;
pub const CASE_SENSITIVE:     u32 = 1;
pub const CONSTANT_SHOW:      u32 = 2;
pub const NO_HELP:            u32 = 3;
pub const NO_WRAP:            u32 = 4;
pub const AUTOINDENT:         u32 = 5;
pub const VIEW_MODE:          u32 = 6;
pub const USE_MOUSE:          u32 = 7;
pub const USE_REGEXP:         u32 = 8;
pub const SAVE_ON_EXIT:       u32 = 9;
pub const CUT_FROM_CURSOR:    u32 = 10;
pub const BACKWARDS_SEARCH:   u32 = 11;
pub const MULTIBUFFER:        u32 = 12;
pub const REBIND_DELETE:      u32 = 13;
pub const RAW_SEQUENCES:      u32 = 14;
pub const NO_CONVERT:         u32 = 15;
pub const MAKE_BACKUP:        u32 = 16;
pub const INSECURE_BACKUP:    u32 = 17;
pub const NO_SYNTAX:          u32 = 18;
pub const PRESERVE:           u32 = 19;
pub const HISTORYLOG:         u32 = 20;
pub const RESTRICTED:         u32 = 21;
pub const SMART_HOME:         u32 = 22;
pub const WHITESPACE_DISPLAY: u32 = 23;
pub const TABS_TO_SPACES:     u32 = 24;
pub const QUICK_BLANK:        u32 = 25;
pub const WORD_BOUNDS:        u32 = 26;
pub const NO_NEWLINES:        u32 = 27;
pub const BOLD_TEXT:          u32 = 28;
pub const SOFTWRAP:           u32 = 29;
pub const POSITIONLOG:        u32 = 30;
pub const LOCKING:            u32 = 31;
pub const NOREAD_MODE:        u32 = 32;
pub const MAKE_IT_UNIX:       u32 = 33;
pub const TRIM_BLANKS:        u32 = 34;
pub const SHOW_CURSOR:        u32 = 35;
pub const LINE_NUMBERS:       u32 = 36;
pub const AT_BLANKS:          u32 = 37;
pub const AFTER_ENDS:         u32 = 38;
pub const LET_THEM_ZAP:       u32 = 39;
pub const BREAK_LONG_LINES:   u32 = 40;
pub const JUMPY_SCROLLING:    u32 = 41;
pub const EMPTY_LINE:         u32 = 42;
pub const INDICATOR:          u32 = 43;
pub const BOOKSTYLE:          u32 = 44;
pub const COLON_PARSING:      u32 = 45;
pub const STATEFLAGS:         u32 = 46;
pub const USE_MAGIC:          u32 = 47;
pub const MINIBAR:            u32 = 48;
pub const ZERO:               u32 = 49;
pub const MODERN_BINDINGS:    u32 = 50;
pub const SOLO_SIDESCROLL:    u32 = 51;

// ---------------------------------------------------------------------------
// Linked-list type aliases
// ---------------------------------------------------------------------------

/// An owned (strong) reference to a `LineNode`.
pub type LinePtr  = Rc<RefCell<LineNode>>;
/// A weak (non-owning) reference to a `LineNode` (used for back-links).
pub type LineWeak = Weak<RefCell<LineNode>>;

// ---------------------------------------------------------------------------
// Structure types
// ---------------------------------------------------------------------------

// --- color / syntax structures (ENABLE_COLOR) ---

/// One foreground/background color pair plus the regexes that trigger it.
/// C: typedef struct colortype { … } colortype;
#[cfg(feature = "color")]
#[derive(Debug)]
pub struct ColorType {
    /// Ordinal number (for multiline regex pairs).
    pub id: i16,
    /// Foreground color index.
    pub fg: i16,
    /// Background color index.
    pub bg: i16,
    /// The ncurses/crossterm color-pair number for this combination.
    pub pairnum: i16,
    /// Pair number and brightness composed into ready-to-use attributes.
    pub attributes: i32,
    /// The compiled start regex (or the only regex for single-line rules).
    pub start: Option<regex::Regex>,
    /// The compiled end regex (for multiline rules), if any.
    pub end: Option<regex::Regex>,
    /// Next color combination in the syntax's list.
    pub next: Option<Box<ColorType>>,
}

#[cfg(feature = "color")]
impl Default for ColorType {
    fn default() -> Self {
        ColorType {
            id: 0,
            fg: THE_DEFAULT,
            bg: THE_DEFAULT,
            pairnum: 0,
            attributes: 0,
            start: None,
            end: None,
            next: None,
        }
    }
}

/// One regex used to recognise which files a syntax applies to.
/// C: typedef struct regexlisttype { … } regexlisttype;
#[cfg(feature = "color")]
#[derive(Debug)]
pub struct RegexListType {
    /// A compiled regex to match things that imply a certain syntax.
    pub one_rgx: Option<regex::Regex>,
    /// The next regex in the list.
    pub next: Option<Box<RegexListType>>,
}

#[cfg(feature = "color")]
impl Default for RegexListType {
    fn default() -> Self {
        RegexListType { one_rgx: None, next: None }
    }
}

/// One `extendsyntax` command recorded for deferred application.
/// C: typedef struct augmentstruct { … } augmentstruct;
#[cfg(feature = "color")]
#[derive(Debug)]
pub struct AugmentStruct {
    /// The file in which this `extendsyntax` command appears.
    pub filename: String,
    /// The line number of the command within that file.
    pub lineno: isize,
    /// The full text of the command line.
    pub data: String,
    /// Next augmentation node.
    pub next: Option<Box<AugmentStruct>>,
}

#[cfg(feature = "color")]
impl Default for AugmentStruct {
    fn default() -> Self {
        AugmentStruct {
            filename: String::new(),
            lineno: 0,
            data: String::new(),
            next: None,
        }
    }
}

/// A complete syntax definition (colors, file matchers, lint/format commands).
/// C: typedef struct syntaxtype { … } syntaxtype;
#[cfg(feature = "color")]
#[derive(Debug)]
pub struct SyntaxType {
    /// The name of this syntax (e.g. `"c"`, `"python"`).
    pub name: String,
    /// File from which this syntax was loaded, or empty if built-in.
    pub filename: String,
    /// The line number of the `syntax` command in `filename`.
    pub lineno: usize,
    /// List of deferred `extendsyntax` commands.
    pub augmentations: Option<Box<AugmentStruct>>,
    /// File-extension regexes that select this syntax.
    pub extensions: Option<Box<RegexListType>>,
    /// Header-line regexes that select this syntax.
    pub headers: Option<Box<RegexListType>>,
    /// libmagic-result regexes that select this syntax.
    pub magics: Option<Box<RegexListType>>,
    /// The linter command for this file type.
    pub linter: Option<String>,
    /// The formatter command for this file type.
    pub formatter: Option<String>,
    /// What the Tab key should insert; `None` means use the global default.
    pub tabstring: Option<String>,
    /// The line-comment prefix (and optional postfix) for this file type.
    #[cfg(feature = "comment")]
    pub comment: Option<String>,
    /// The list of color rules for this syntax.
    pub color: Option<Box<ColorType>>,
    /// How many multiline regex strings this syntax has.
    pub multiscore: i16,
    /// Next syntax in the global list.
    pub next: Option<Box<SyntaxType>>,
}

#[cfg(feature = "color")]
impl Default for SyntaxType {
    fn default() -> Self {
        SyntaxType {
            name: String::new(),
            filename: String::new(),
            lineno: 0,
            augmentations: None,
            extensions: None,
            headers: None,
            magics: None,
            linter: None,
            formatter: None,
            tabstring: None,
            #[cfg(feature = "comment")]
            comment: None,
            color: None,
            multiscore: 0,
            next: None,
        }
    }
}

/// One entry in the linter's list of errors/warnings.
/// C: typedef struct lintstruct { … } lintstruct;
#[cfg(feature = "color")]
#[derive(Debug)]
pub struct LintStruct {
    /// Line number of the diagnostic.
    pub lineno: isize,
    /// Column number of the diagnostic.
    pub colno: isize,
    /// Human-readable error/warning message.
    pub msg: String,
    /// The file to which this diagnostic belongs.
    pub filename: String,
    /// Next diagnostic.
    pub next: Option<Box<LintStruct>>,
    /// Previous diagnostic (back-link; raw pointer to avoid Rc cycle in a
    /// simple doubly-linked list; callers must uphold validity).
    pub prev: *mut LintStruct,
}

#[cfg(feature = "color")]
impl Default for LintStruct {
    fn default() -> Self {
        LintStruct {
            lineno: 0,
            colno: 0,
            msg: String::new(),
            filename: String::new(),
            next: None,
            prev: std::ptr::null_mut(),
        }
    }
}

// --- core buffer line ---

/// One line in the text buffer (doubly-linked list with Rc/Weak).
/// C: typedef struct linestruct { … } linestruct;
#[derive(Debug)]
pub struct LineNode {
    /// The text content of this line (without the newline terminator).
    pub data: String,
    /// 1-based line number within the file.
    pub lineno: isize,
    /// Owned forward link to the next line.
    pub next: Option<LinePtr>,
    /// Weak back-link to the previous line (avoids reference cycles).
    pub prev: Option<LineWeak>,
    /// Per-line coverage flags for each multiline color regex.
    #[cfg(feature = "color")]
    pub multidata: Vec<i16>,
    /// Whether the user has placed an anchor on this line.
    #[cfg(not(feature = "tiny"))]
    pub has_anchor: bool,
}

impl Default for LineNode {
    fn default() -> Self {
        LineNode {
            data: String::new(),
            lineno: 0,
            next: None,
            prev: None,
            #[cfg(feature = "color")]
            multidata: Vec::new(),
            #[cfg(not(feature = "tiny"))]
            has_anchor: false,
        }
    }
}

// --- undo structures (not NANO_TINY) ---

/// A group of lines that were indented/unindented together.
/// C: typedef struct groupstruct { … } groupstruct;
#[cfg(not(feature = "tiny"))]
#[derive(Debug)]
pub struct GroupStruct {
    /// The 1-based line number of the first line in the group.
    pub top_line: isize,
    /// The 1-based line number of the last line in the group.
    pub bottom_line: isize,
    /// The saved indentation strings, one per affected line.
    pub indentations: Vec<String>,
    /// The next group record, if any.
    pub next: Option<Box<GroupStruct>>,
}

#[cfg(not(feature = "tiny"))]
impl Default for GroupStruct {
    fn default() -> Self {
        GroupStruct {
            top_line: 0,
            bottom_line: 0,
            indentations: Vec::new(),
            next: None,
        }
    }
}

/// One item in the undo/redo history stack.
/// C: typedef struct undostruct { … } undostruct;
#[cfg(not(feature = "tiny"))]
#[derive(Debug)]
pub struct UndoStruct {
    /// The type of operation this record covers.
    pub r#type: UndoType,
    /// Extra flags for corner-case handling (WAS_BACKSPACE_AT_EOF, etc.).
    pub xflags: i32,
    /// Line number where the operation began or ended.
    pub head_lineno: isize,
    /// X position where the operation began or ended.
    pub head_x: usize,
    /// Saved string data needed to restore the affected line.
    pub strdata: Option<String>,
    /// File size before the action.
    pub wassize: usize,
    /// File size after the action.
    pub newsize: usize,
    /// Undo info for groups of lines (indent/unindent).
    pub grouping: Option<Box<GroupStruct>>,
    /// A copy of the cut buffer at the time of the action.
    pub cutbuffer: Option<LinePtr>,
    /// Line number of the current line (or context-dependent value).
    pub tail_lineno: isize,
    /// X position corresponding to `tail_lineno`.
    pub tail_x: usize,
    /// Link to the undo item for the preceding action.
    pub next: Option<Box<UndoStruct>>,
}

#[cfg(not(feature = "tiny"))]
impl Default for UndoStruct {
    fn default() -> Self {
        UndoStruct {
            r#type: UndoType::Other,
            xflags: 0,
            head_lineno: 0,
            head_x: 0,
            strdata: None,
            wassize: 0,
            newsize: 0,
            grouping: None,
            cutbuffer: None,
            tail_lineno: 0,
            tail_x: 0,
            next: None,
        }
    }
}

// --- position history (ENABLE_HISTORIES) ---

/// Saved cursor position for a previously visited file.
/// C: typedef struct positionstruct { … } positionstruct;
#[cfg(feature = "histories")]
#[derive(Debug)]
pub struct PositionStruct {
    /// The full path of the file.
    pub filename: String,
    /// The line number where the cursor was when the file was closed.
    pub linenumber: isize,
    /// The column number where the cursor was.
    pub columnnumber: isize,
    /// Anchor line numbers serialised to a string (e.g. `"3,17,42"`).
    pub anchors: Option<String>,
    /// Next entry in the positions register.
    pub next: Option<Box<PositionStruct>>,
}

#[cfg(feature = "histories")]
impl Default for PositionStruct {
    fn default() -> Self {
        PositionStruct {
            filename: String::new(),
            linenumber: 0,
            columnnumber: 0,
            anchors: None,
            next: None,
        }
    }
}

// --- file metadata ---

/// Cross-platform file stat information (replacing libc::stat).
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Time of last modification (seconds since epoch).
    pub st_mtime: i64,
    /// Device ID.
    pub st_dev: u64,
    /// Inode number.
    pub st_ino: u64,
    /// User ID (Unix only).
    #[cfg(unix)]
    pub st_uid: u32,
    /// Group ID (Unix only).
    #[cfg(unix)]
    pub st_gid: u32,
    /// File mode (permissions + type) (Unix only).
    #[cfg(unix)]
    pub st_mode: u32,
    /// Time of last access (Unix only).
    #[cfg(unix)]
    pub st_atime: i64,
    /// Nanoseconds of last access (Unix only).
    #[cfg(unix)]
    pub st_atime_nsec: i64,
    /// Nanoseconds of last modification (Unix only).
    #[cfg(unix)]
    pub st_mtime_nsec: i64,
}

// --- open file (buffer) ---

/// All the state associated with one open file / buffer.
/// C: typedef struct openfilestruct { … } openfilestruct;
#[derive(Debug)]
pub struct OpenFileStruct {
    /// The file's name (may be empty for a new unsaved buffer).
    pub filename: String,
    /// The first line of the buffer.
    pub filetop: Option<LinePtr>,
    /// The last line of the buffer.
    pub filebot: Option<LinePtr>,
    /// The line currently at the top of the edit window.
    pub edittop: Option<LinePtr>,
    /// The currently active line (where the cursor is).
    pub current: Option<LinePtr>,
    /// Total number of characters in the buffer.
    pub totsize: usize,
    /// Starting display column of the top line (non-zero only in softwrap mode).
    pub firstcolumn: usize,
    /// The cursor's byte offset within `current`.
    pub current_x: usize,
    /// The preferred display column for the cursor (used by vertical movement).
    pub placewewant: usize,
    /// The column from which the edit window is drawn when panning.
    pub brink: usize,
    /// The row within the edit window that the cursor occupies.
    pub cursor_row: isize,
    /// File metadata from the last open or save (used for change detection).
    pub statinfo: Option<FileStat>,
    /// The line used to prepend overflow text during hard-wrapping.
    #[cfg(feature = "wrapping")]
    pub spillage_line: Option<LinePtr>,
    /// The line where the mark anchor is set; `None` if no mark.
    #[cfg(not(feature = "tiny"))]
    pub mark: Option<LinePtr>,
    /// The byte offset of the mark within `mark`.
    #[cfg(not(feature = "tiny"))]
    pub mark_x: usize,
    /// Whether the marked region was created by holding Shift.
    #[cfg(not(feature = "tiny"))]
    pub softmark: bool,
    /// The line-ending format of this file.
    #[cfg(not(feature = "tiny"))]
    pub fmt: FormatType,
    /// Path of the lockfile we created for this buffer (if any).
    #[cfg(not(feature = "tiny"))]
    pub lock_filename: Option<String>,
    /// The top of the undo list for this buffer.
    #[cfg(not(feature = "tiny"))]
    pub undotop: Option<Box<UndoStruct>>,
    /// The current (next available) undo level.
    #[cfg(not(feature = "tiny"))]
    pub current_undo: *mut UndoStruct,
    /// The undo item at which the buffer was last saved.
    #[cfg(not(feature = "tiny"))]
    pub last_saved: *mut UndoStruct,
    /// The type of the last action performed by the user.
    #[cfg(not(feature = "tiny"))]
    pub last_action: UndoType,
    /// Whether this buffer has unsaved changes.
    pub modified: bool,
    /// The syntax definition that applies to this file (if any).
    #[cfg(feature = "color")]
    pub syntax: Option<*mut SyntaxType>,
    /// An ALERT-level message that occurred when the file was opened.
    #[cfg(feature = "multibuffer")]
    pub errormessage: Option<String>,
    /// Creation order; the oldest surviving buffer plays the role of C's
    /// `startfile` for [n/total] buffer numbering.  (The circular list
    /// itself is AppState.openfile + AppState.buffer_ring.)
    #[cfg(feature = "multibuffer")]
    pub seq: usize,
}

impl Default for OpenFileStruct {
    fn default() -> Self {
        OpenFileStruct {
            filename: String::new(),
            filetop: None,
            filebot: None,
            edittop: None,
            current: None,
            totsize: 0,
            firstcolumn: 0,
            current_x: 0,
            placewewant: 0,
            brink: 0,
            cursor_row: 0,
            statinfo: None,
            #[cfg(feature = "wrapping")]
            spillage_line: None,
            #[cfg(not(feature = "tiny"))]
            mark: None,
            #[cfg(not(feature = "tiny"))]
            mark_x: 0,
            #[cfg(not(feature = "tiny"))]
            softmark: false,
            #[cfg(not(feature = "tiny"))]
            fmt: FormatType::Unspecified,
            #[cfg(not(feature = "tiny"))]
            lock_filename: None,
            #[cfg(not(feature = "tiny"))]
            undotop: None,
            #[cfg(not(feature = "tiny"))]
            current_undo: std::ptr::null_mut(),
            #[cfg(not(feature = "tiny"))]
            last_saved: std::ptr::null_mut(),
            #[cfg(not(feature = "tiny"))]
            last_action: UndoType::Other,
            modified: false,
            #[cfg(feature = "color")]
            syntax: None,
            #[cfg(feature = "multibuffer")]
            errormessage: None,
            #[cfg(feature = "multibuffer")]
            seq: 0,
        }
    }
}

// --- nanorc option descriptor ---

/// One option entry from the nanorc parser.
/// C: typedef struct rcoption { … } rcoption;
#[cfg(feature = "nanorc")]
#[derive(Debug, Clone)]
pub struct RcOption {
    /// The name of the rcfile option (e.g. `"autoindent"`).
    pub name: &'static str,
    /// The flag constant associated with this option (0 if it has none).
    pub flag: i64,
}

#[cfg(feature = "nanorc")]
impl Default for RcOption {
    fn default() -> Self {
        RcOption { name: "", flag: 0 }
    }
}

// --- key binding ---

/// Function pointer type for editor commands.
pub type FuncPtr = fn();

/// One keystroke binding in nano's shortcut system.
/// C: typedef struct keystruct { … } keystruct;
#[derive(Debug, Clone)]
pub struct KeyStruct {
    /// Human-readable description of the keystroke, e.g. `"^C"` or `"M-R"`.
    pub keystr: &'static str,
    /// The integer keycode (together with the meta flag) identifying this stroke.
    pub keycode: i32,
    /// Bitmask of the menus in which this binding is active.
    pub menus: i32,
    /// The function to invoke when this key is pressed.
    pub func: Option<FuncPtr>,
    /// If this is a toggle, which flag it toggles.
    #[cfg(not(feature = "tiny"))]
    pub toggle: i32,
    /// Sequence number of this toggle (keeps toggles in display order).
    #[cfg(not(feature = "tiny"))]
    pub ordinal: i32,
    /// The string of keycodes to which this shortcut expands (string binds).
    #[cfg(feature = "nanorc")]
    pub expansion: Option<String>,
    /// Next keystruct in the binding list.
    pub next: Option<Box<KeyStruct>>,
}

impl Default for KeyStruct {
    fn default() -> Self {
        KeyStruct {
            keystr: "",
            keycode: 0,
            menus: 0,
            func: None,
            #[cfg(not(feature = "tiny"))]
            toggle: 0,
            #[cfg(not(feature = "tiny"))]
            ordinal: 0,
            #[cfg(feature = "nanorc")]
            expansion: None,
            next: None,
        }
    }
}

/// One entry in the list of all bindable editor functions.
/// C: typedef struct funcstruct { … } funcstruct;
#[derive(Debug, Clone)]
pub struct FuncStruct {
    /// The function itself.
    pub func: Option<FuncPtr>,
    /// The short label shown in the help bar, e.g. `"Where Is"`.
    pub tag: &'static str,
    /// The longer description shown in the help viewer.
    #[cfg(feature = "help")]
    pub phrase: &'static str,
    /// Whether to add a blank line after this entry in the help viewer.
    #[cfg(feature = "help")]
    pub blank_after: bool,
    /// Bitmask of the menus where this function is applicable.
    pub menus: i32,
    /// Next funcstruct in the global list.
    pub next: Option<Box<FuncStruct>>,
}

impl Default for FuncStruct {
    fn default() -> Self {
        FuncStruct {
            func: None,
            tag: "",
            #[cfg(feature = "help")]
            phrase: "",
            #[cfg(feature = "help")]
            blank_after: false,
            menus: 0,
            next: None,
        }
    }
}

// --- word-completion ---

/// One candidate word in the completion list.
/// C: typedef struct completionstruct { … } completionstruct;
#[cfg(feature = "wordcomp")]
#[derive(Debug)]
pub struct CompletionStruct {
    /// The candidate completion string.
    pub word: String,
    /// Next candidate.
    pub next: Option<Box<CompletionStruct>>,
}

#[cfg(feature = "wordcomp")]
impl Default for CompletionStruct {
    fn default() -> Self {
        CompletionStruct { word: String::new(), next: None }
    }
}
