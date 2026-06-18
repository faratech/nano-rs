#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/global.c + extern declarations from src/prototypes.h
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2026 Benno Schulenberg

use std::cell::UnsafeCell;
use crate::definitions::*;

// ---------------------------------------------------------------------------
// Re-entrant global cell — mimics C global variable semantics.
// Unlike RefCell, this does NOT panic on nested borrow/borrow_mut calls.
// This is safe because nano is single-threaded: one thread, one AppState.
// ---------------------------------------------------------------------------
pub struct NanoCell(UnsafeCell<AppState>);
// SAFETY: nano is strictly single-threaded; no concurrent access ever occurs.
unsafe impl Sync for NanoCell {}
impl NanoCell {
    const fn new(val: AppState) -> Self { Self(UnsafeCell::new(val)) }
    #[inline] pub fn borrow(&self)     -> &AppState     { unsafe { &*self.0.get() } }
    #[inline] pub fn borrow_mut(&self) -> &mut AppState { unsafe { &mut *self.0.get() } }
}

// ---------------------------------------------------------------------------
// ncurses key constants (not in definitions.rs — needed by shortcut_init)
// ---------------------------------------------------------------------------

pub const KEY_ENTER:     i32 = 0x157; // ncurses KEY_ENTER (343)
pub const KEY_BACKSPACE: i32 = 0x107; // ncurses KEY_BACKSPACE (263)
pub const KEY_DC:        i32 = 0x14A; // ncurses KEY_DC (delete char) (330)
pub const KEY_IC:        i32 = 0x14B; // ncurses KEY_IC (insert) (331)
pub const KEY_UP:        i32 = 0x103; // ncurses KEY_UP (259)
pub const KEY_DOWN:      i32 = 0x102; // ncurses KEY_DOWN (258)
pub const KEY_LEFT:      i32 = 0x104; // ncurses KEY_LEFT (260)
pub const KEY_RIGHT:     i32 = 0x105; // ncurses KEY_RIGHT (261)
pub const KEY_HOME:      i32 = 0x106; // ncurses KEY_HOME (262)
pub const KEY_END:       i32 = 0x166; // ncurses KEY_END (358)
pub const KEY_PPAGE:     i32 = 0x153; // ncurses KEY_PPAGE (339)
pub const KEY_NPAGE:     i32 = 0x152; // ncurses KEY_NPAGE (338)
pub const KEY_F0:        i32 = 0x108; // ncurses KEY_F0 (264) — KEY_F(n) = KEY_F0 + n
pub const KEY_CANCEL:    i32 = 0x155; // ncurses KEY_CANCEL (341)
pub const KEY_SIC:       i32 = 0x15F; // ncurses KEY_SIC (351) — Shift-Insert
pub const KEY_FRESH:     i32 = 0x4FE as i32;

/// Equivalent to the C macro KEY_F(n).
#[inline(always)]
pub fn key_f(n: i32) -> i32 {
    KEY_F0 + n
}

/// Curses attribute A_REVERSE (for hilite_attribute default).
pub const A_REVERSE: i32 = 0x0004_0000;

// ---------------------------------------------------------------------------
// Flags macros ported from definitions.h (operate on AppState.flags)
// ---------------------------------------------------------------------------

/// Return the index into flags[] that holds the given flag bit.
#[inline(always)]
pub fn flag_index(flag: u32) -> usize {
    (flag / 32) as usize
}

/// Return the bitmask for the given flag within its flags[] element.
#[inline(always)]
pub fn flag_mask(flag: u32) -> u32 {
    1u32 << (flag % 32)
}

/// Test whether a flag is set (reads from AppState via STATE).
/// C: ISSET(flag)
#[macro_export]
macro_rules! ISSET {
    ($flag:expr) => {
        $crate::global::with_state(|s| {
            (s.flags[$crate::global::flag_index($flag)] & $crate::global::flag_mask($flag)) != 0
        })
    };
}

/// Set a flag in AppState.flags.
/// C: SET(flag)
#[macro_export]
macro_rules! SET {
    ($flag:expr) => {
        $crate::global::with_state_mut(|s| {
            s.flags[$crate::global::flag_index($flag)] |= $crate::global::flag_mask($flag);
        })
    };
}

/// Unset a flag in AppState.flags.
/// C: UNSET(flag)
#[macro_export]
macro_rules! UNSET {
    ($flag:expr) => {
        $crate::global::with_state_mut(|s| {
            s.flags[$crate::global::flag_index($flag)] &= !$crate::global::flag_mask($flag);
        })
    };
}

/// Toggle a flag in AppState.flags.
/// C: TOGGLE(flag)
#[macro_export]
macro_rules! TOGGLE {
    ($flag:expr) => {
        $crate::global::with_state_mut(|s| {
            s.flags[$crate::global::flag_index($flag)] ^= $crate::global::flag_mask($flag);
        })
    };
}

// ---------------------------------------------------------------------------
// NanoWindow — replaces the three WINDOW* pointers from ncurses
// ---------------------------------------------------------------------------

/// Replacement for ncurses WINDOW* — stores position/size of a sub-window.
/// C: WINDOW *topwin, *midwin, *footwin
#[derive(Debug, Clone, Default)]
pub struct NanoWindow {
    pub rows: u16,
    pub cols: u16,
    pub y:    u16,
    pub x:    u16,
}

impl NanoWindow {
    /// Const equivalent of `Default` — all fields zero. Lets `AppState::new()`
    /// stay `const fn` so the thread-local `STATE` can use const-init.
    pub const fn new() -> Self { NanoWindow { rows: 0, cols: 0, y: 0, x: 0 } }
}

// ---------------------------------------------------------------------------
// AppState — all global variables from global.c / prototypes.h
// ---------------------------------------------------------------------------

pub struct AppState {
    /// Backing storage for every line node (all buffers, cutbuffer, undo
    /// snapshots, completion, histories).  LinePtr/LineWeak index into this.
    pub lines: crate::definitions::LineArena,
    // --- Signal flags (volatile sig_atomic_t in C) ---
    /// Set to true whenever SIGWINCH occurs (not NANO_TINY only).
    #[cfg(not(feature = "tiny"))]
    pub the_window_resized: bool,
    /// Same as above, used by the file browser.
    #[cfg(not(feature = "tiny"))]
    pub resized_for_browser: bool,

    // --- Terminal / locale ---
    /// Whether we're running on a Linux console (a VT).
    pub on_a_vt: bool,
    /// Whether we're in a UTF-8 locale.
    pub using_utf8: bool,
    /// Whether any Sh-M-<letter> combo has been bound.
    pub shifted_metas: bool,

    // --- Current keystroke state ---
    /// Whether the current keystroke is a Meta key.
    pub meta_key: bool,
    /// Whether Shift was held together with a movement key.
    pub shift_held: bool,
    /// Whether to ignore modifier keys while running a macro or string bind.
    pub mute_modifiers: bool,

    // --- Editor lifecycle ---
    /// True once all options and files have been read.
    pub we_are_running: bool,
    /// Whether more than one buffer is or has been open.
    pub more_than_one: bool,
    /// Whether to show the number of lines when the minibar is used.
    pub report_size: bool,

    // --- Tool execution ---
    /// Whether a tool has been run at the Execute-Command prompt.
    pub ran_a_tool: bool,
    /// What was typed at the Execute prompt before invoking a tool.
    #[cfg(not(feature = "tiny"))]
    pub foretext: Option<String>,

    // --- Exit status ---
    /// The status value that nano returns upon exit.
    pub final_status: i32,

    // --- Help viewer ---
    /// Whether we are in the help viewer.
    pub inhelp: bool,
    /// Title of the current help text (None when not in help).
    pub title: Option<String>,

    // --- Screen refresh ---
    /// Whether a command mangled enough of the buffer that we should repaint.
    pub refresh_needed: bool,
    /// Whether to scroll all lines sideways (pan mode).
    pub united_sidescroll: bool,
    /// Whether an update of the edit window should center the cursor.
    pub focusing: bool,

    // --- Display ---
    /// Whether a 0x0A byte should be shown as ^@ instead of ^J.
    pub as_an_at: bool,

    // --- Interrupt ---
    /// Whether Ctrl+C was pressed (when a keyboard interrupt is enabled).
    pub control_C_was_pressed: bool,

    // --- Status-bar messages ---
    /// Messages of type HUSH should not overwrite type MILD nor ALERT.
    pub lastmessage: MessageType,

    // --- Word completion ---
    /// The line where the last completion was found, if any.
    pub pletion_line: Option<LinePtr>,

    // --- Marking ---
    /// Whether indenting/commenting should include the last line of the marked region.
    pub also_the_last: bool,

    // --- Prompt/search state ---
    /// The answer string used by the status-bar prompt.
    pub answer: String,

    /// The last string we searched for.
    pub last_search: String,
    /// Whether the last search found something.
    pub didfind: i32,

    /// The current browser directory when trying to do tab completion.
    pub present_path: Option<String>,

    // --- Feature flags array ---
    /// Our flags array, containing the states of all global options.
    /// C: unsigned flags[4]
    pub flags: [u32; 4],

    // --- Special key codes for modified arrow keys ---
    pub controlleft: i32,
    pub controlright: i32,
    pub controlup: i32,
    pub controldown: i32,
    pub controlhome: i32,
    pub controlend: i32,
    #[cfg(not(feature = "tiny"))]
    pub controldelete: i32,
    #[cfg(not(feature = "tiny"))]
    pub controlshiftdelete: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftup: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftdown: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontrolleft: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontrolright: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontrolup: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontroldown: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontrolhome: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftcontrolend: i32,
    #[cfg(not(feature = "tiny"))]
    pub altleft: i32,
    #[cfg(not(feature = "tiny"))]
    pub altright: i32,
    #[cfg(not(feature = "tiny"))]
    pub altup: i32,
    #[cfg(not(feature = "tiny"))]
    pub altdown: i32,
    #[cfg(not(feature = "tiny"))]
    pub althome: i32,
    #[cfg(not(feature = "tiny"))]
    pub altend: i32,
    #[cfg(not(feature = "tiny"))]
    pub altpageup: i32,
    #[cfg(not(feature = "tiny"))]
    pub altpagedown: i32,
    #[cfg(not(feature = "tiny"))]
    pub altinsert: i32,
    #[cfg(not(feature = "tiny"))]
    pub altdelete: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftaltleft: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftaltright: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftaltup: i32,
    #[cfg(not(feature = "tiny"))]
    pub shiftaltdown: i32,
    pub mousefocusin: i32,
    pub mousefocusout: i32,

    // --- Wrap/fill settings ---
    /// The relative column where we will wrap lines.
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    pub fill: isize,
    /// The actual column where we will wrap lines, based on fill.
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    pub wrap_at: usize,

    // --- Windows (replacing WINDOW* from ncurses) ---
    /// Top portion of the screen (title bar).
    pub topwin: NanoWindow,
    /// Middle portion of the screen (edit window).
    pub midwin: NanoWindow,
    /// Bottom portion of the screen (status bar / shortcuts).
    pub footwin: NanoWindow,
    /// How many rows does the edit window take up?
    pub editwinrows: i32,
    /// The number of usable columns in the edit window: COLS - margin.
    pub editwincols: i32,
    /// The amount of space reserved at the left for line numbers.
    pub margin: i32,
    /// Becomes 1 when the indicator "scroll bar" must be shown.
    pub sidebar: i32,
    #[cfg(not(feature = "tiny"))]
    /// An array of characters depicting the scrollbar.
    pub bardata: Vec<i32>,
    #[cfg(not(feature = "tiny"))]
    /// The column at which a vertical bar will be drawn.
    pub stripe_column: isize,
    #[cfg(not(feature = "tiny"))]
    /// 0=center, 1=push to top, 2=push to bottom.
    pub cycling_aim: i32,

    // --- Cut buffer ---
    /// The buffer where we store cut text.
    pub cutbuffer: Option<LinePtr>,
    /// The last line in the cutbuffer.
    pub cutbottom: Option<LinePtr>,
    /// Whether to add to the cutbuffer instead of clearing it first.
    pub keep_cutbuffer: bool,

    // --- Open file buffers ---
    /// The current buffer (C: openfile).
    pub openfile: Option<Box<OpenFileStruct>>,
    /// The other open buffers, in circular order: the front is the buffer
    /// "after" the current one, the back is the buffer "before" it.
    /// Together with `openfile` this models C's circular openfilestruct list.
    #[cfg(feature = "multibuffer")]
    pub buffer_ring: std::collections::VecDeque<Box<OpenFileStruct>>,
    /// Monotonic creation counter; the oldest surviving buffer plays the
    /// role of C's `startfile` for buffer numbering.
    #[cfg(feature = "multibuffer")]
    pub buffer_seq_counter: usize,

    // --- Bracket matching / whitespace display ---
    #[cfg(not(feature = "tiny"))]
    /// The opening and closing brackets for bracket searches.
    pub matchbrackets: Option<String>,
    #[cfg(not(feature = "tiny"))]
    /// The characters used when visibly showing tabs and spaces.
    pub whitespace: Option<String>,
    #[cfg(not(feature = "tiny"))]
    /// The byte lengths of the whitespace display characters.
    pub whitelen: [i32; 2],

    // --- Justify ---
    /// String tags (always present for tag declarations)
    pub exit_tag: &'static str,
    pub close_tag: &'static str,

    #[cfg(feature = "justify")]
    /// The closing punctuation that can end sentences.
    pub punct: Option<String>,
    #[cfg(feature = "justify")]
    /// The closing brackets that can follow closing punctuation.
    pub brackets: Option<String>,
    #[cfg(feature = "justify")]
    /// The quoting string.
    pub quotestr: Option<String>,
    #[cfg(feature = "justify")]
    /// The compiled regular expression from the quoting string.
    pub quotereg: Option<regex_lite::Regex>,

    // --- Word characters ---
    /// Nonalphanumeric characters that also form words.
    pub word_chars: Option<String>,

    // --- Tab size ---
    /// The width of a tab in spaces. -1 until set in main().
    pub tabsize: isize,

    // --- Backup / operating directories ---
    #[cfg(not(feature = "tiny"))]
    /// The directory where backup files are stored.
    pub backup_dir: Option<String>,
    #[cfg(feature = "operatingdir")]
    /// The path to the confining "operating" directory.
    pub operating_dir: Option<String>,

    // --- Spell checker ---
    #[cfg(feature = "speller")]
    /// The command to use for the alternate spell checker.
    pub alt_speller: Option<String>,

    // --- Syntax highlighting ---
    #[cfg(feature = "color")]
    /// The global list of color syntaxes.
    pub syntaxes: Option<Box<SyntaxType>>,
    #[cfg(feature = "color")]
    /// The color syntax name specified on the command line.
    pub syntaxstr: Option<String>,
    #[cfg(feature = "color")]
    /// Whether the colors for the current syntax have been initialized.
    pub have_palette: bool,
    #[cfg(feature = "color")]
    /// Becomes true when NO_COLOR is set in the environment.
    pub rescind_colors: bool,
    #[cfg(feature = "color")]
    /// Whether the multiline-coloring situation has changed.
    pub perturbed: bool,
    #[cfg(feature = "color")]
    /// Whether the multidata should be recalculated.
    pub recook: bool,

    // --- Menu / keybinding state ---
    /// The currently active menu.
    pub currmenu: u32,
    /// The flat list of all shortcut bindings (Vec for O(1) append).
    pub sclist: Vec<KeyStruct>,
    /// The flat list of all bindable functions.
    pub allfuncs: Vec<FuncStruct>,
    /// Index into allfuncs of the Exit/Close item.
    pub exitfunc: Option<usize>,
    /// Internal counter for add_to_sclist toggles.
    pub tailsc_toggle_counter: i32,
    /// The toggle value from the last-added sclist entry (for ordinal tracking).
    pub tailsc_last_toggle: i32,

    // --- Search/replace/execute history ---
    /// The current item in the list of strings that were searched for.
    pub search_history: Option<LinePtr>,
    /// The current item in the list of replace strings.
    pub replace_history: Option<LinePtr>,
    /// The current item in the list of commands run with ^T.
    pub execute_history: Option<LinePtr>,
    #[cfg(feature = "histories")]
    /// The oldest item in the list of search strings.
    pub searchtop: Option<LinePtr>,
    #[cfg(feature = "histories")]
    /// The newest (empty sentinel) item in the search history.
    pub searchbot: Option<LinePtr>,
    #[cfg(feature = "histories")]
    pub replacetop: Option<LinePtr>,
    #[cfg(feature = "histories")]
    pub replacebot: Option<LinePtr>,
    #[cfg(feature = "histories")]
    pub executetop: Option<LinePtr>,
    #[cfg(feature = "histories")]
    pub executebot: Option<LinePtr>,

    // --- History items (Vec<String> representation used by history.rs) ---
    /// Search history strings, oldest-first.
    pub search_history_items: Vec<String>,
    /// Current position in the search history (= len means "at bottom").
    pub search_history_pos: usize,
    /// Replace history strings, oldest-first.
    pub replace_history_items: Vec<String>,
    /// Current position in the replace history.
    pub replace_history_pos: usize,
    /// Execute (^T) history strings, oldest-first.
    pub execute_history_items: Vec<String>,
    /// Current position in the execute history.
    pub execute_history_pos: usize,

    // --- Regex search state ---
    /// The compiled regular expression to use in searches.
    pub search_regexp: Option<regex_lite::Regex>,
    /// The match positions for parenthetical subexpressions (up to 10).
    /// Each entry is (start, end) byte offsets.
    pub regmatches: [(usize, usize); 10],

    // --- Highlighting / color pairs ---
    /// The attribute we use to highlight something. C: A_REVERSE.
    pub hilite_attribute: i32,
    #[cfg(feature = "color")]
    /// The color combinations for interface elements given in the rcfile.
    pub color_combo: [Option<Box<ColorType>>; NUMBER_OF_ELEMENTS],
    /// The processed color pairs for the interface elements.
    pub interface_color_pair: [i32; NUMBER_OF_ELEMENTS],
    /// The decoded (fg, bg) ncurses color indices for each interface pair,
    /// indexed by the 1-based pair index encoded in interface_color_pair's low
    /// bits.  THE_DEFAULT (-1) means "use the terminal default".
    pub interface_color_rgb: [(i16, i16); NUMBER_OF_ELEMENTS + 1],

    // --- Paths ---
    /// The user's home directory.
    pub homedir: Option<String>,
    /// The directory for nano's history files.
    pub statedir: Option<String>,
    #[cfg(any(feature = "nanorc", feature = "histories"))]
    /// An error message about nanorc/history files.
    pub startup_problem: Option<String>,
    #[cfg(feature = "nanorc")]
    /// The argument of the --rcfile option.
    pub custom_nanorc: Option<String>,
    #[cfg(feature = "nanorc")]
    /// The name of a function between braces in a string bind.
    pub commandname: Option<String>,
    #[cfg(feature = "nanorc")]
    /// The function that commandname resolves to (index into sclist), if any.
    pub planted_shortcut: Option<usize>,

    // --- Spotlight ---
    /// Whether any text is spotlighted.
    pub spotlighted: bool,
    /// Where the spotlighted text starts (column).
    pub light_from_col: usize,
    /// Where the spotlighted text ends (column).
    pub light_to_col: usize,
}

// ---------------------------------------------------------------------------
// Default impl for AppState — matches C global.c initializations
// ---------------------------------------------------------------------------

impl AppState {
    /// Const constructor for the initial global state. Being `const fn` lets the
    /// thread-local `STATE` use const-init, which removes the per-access lazy-init
    /// guard branch from every `state()`/`state_mut()` call (and thus from every
    /// arena incref/decref) — no `Once`/initialized check in the hot path.
    pub const fn new() -> Self {
        AppState {
            lines: crate::definitions::LineArena::new(),
            #[cfg(not(feature = "tiny"))]
            the_window_resized: false,
            #[cfg(not(feature = "tiny"))]
            resized_for_browser: false,

            on_a_vt: false,
            using_utf8: false,
            shifted_metas: false,

            meta_key: false,
            shift_held: false,
            mute_modifiers: false,

            we_are_running: false,
            more_than_one: false,
            report_size: true,   // C: bool report_size = TRUE

            ran_a_tool: false,
            #[cfg(not(feature = "tiny"))]
            foretext: None,

            final_status: 0,

            inhelp: false,
            title: None,

            refresh_needed: false,
            united_sidescroll: true,  // C: bool united_sidescroll = TRUE
            focusing: true,           // C: bool focusing = TRUE

            as_an_at: true,           // C: bool as_an_at = TRUE

            control_C_was_pressed: false,

            lastmessage: MessageType::Vacuum,

            pletion_line: None,

            also_the_last: false,

            answer: String::new(),    // C: char *answer = NULL (empty)
            last_search: String::new(),
            didfind: 0,

            present_path: None,

            flags: [0u32; 4],

            controlleft: 0,
            controlright: 0,
            controlup: 0,
            controldown: 0,
            controlhome: 0,
            controlend: 0,
            #[cfg(not(feature = "tiny"))]
            controldelete: 0,
            #[cfg(not(feature = "tiny"))]
            controlshiftdelete: 0,
            #[cfg(not(feature = "tiny"))]
            shiftup: 0,
            #[cfg(not(feature = "tiny"))]
            shiftdown: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontrolleft: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontrolright: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontrolup: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontroldown: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontrolhome: 0,
            #[cfg(not(feature = "tiny"))]
            shiftcontrolend: 0,
            #[cfg(not(feature = "tiny"))]
            altleft: 0,
            #[cfg(not(feature = "tiny"))]
            altright: 0,
            #[cfg(not(feature = "tiny"))]
            altup: 0,
            #[cfg(not(feature = "tiny"))]
            altdown: 0,
            #[cfg(not(feature = "tiny"))]
            althome: 0,
            #[cfg(not(feature = "tiny"))]
            altend: 0,
            #[cfg(not(feature = "tiny"))]
            altpageup: 0,
            #[cfg(not(feature = "tiny"))]
            altpagedown: 0,
            #[cfg(not(feature = "tiny"))]
            altinsert: 0,
            #[cfg(not(feature = "tiny"))]
            altdelete: 0,
            #[cfg(not(feature = "tiny"))]
            shiftaltleft: 0,
            #[cfg(not(feature = "tiny"))]
            shiftaltright: 0,
            #[cfg(not(feature = "tiny"))]
            shiftaltup: 0,
            #[cfg(not(feature = "tiny"))]
            shiftaltdown: 0,
            mousefocusin: 0,
            mousefocusout: 0,

            #[cfg(any(feature = "wrapping", feature = "justify"))]
            fill: -(COLUMNS_FROM_EOL as isize), // C: ssize_t fill = -COLUMNS_FROM_EOL
            #[cfg(any(feature = "wrapping", feature = "justify"))]
            wrap_at: 0,

            topwin: NanoWindow::new(),
            midwin: NanoWindow::new(),
            footwin: NanoWindow::new(),
            editwinrows: 0,
            editwincols: -1,  // C: int editwincols = -1
            margin: 0,
            sidebar: 0,
            #[cfg(not(feature = "tiny"))]
            bardata: Vec::new(),
            #[cfg(not(feature = "tiny"))]
            stripe_column: 0,
            #[cfg(not(feature = "tiny"))]
            cycling_aim: 0,

            cutbuffer: None,
            cutbottom: None,
            keep_cutbuffer: false,

            openfile: None,
            #[cfg(feature = "multibuffer")]
            buffer_ring: std::collections::VecDeque::new(),
            #[cfg(feature = "multibuffer")]
            buffer_seq_counter: 0,

            #[cfg(not(feature = "tiny"))]
            matchbrackets: None,
            #[cfg(not(feature = "tiny"))]
            whitespace: None,
            #[cfg(not(feature = "tiny"))]
            whitelen: [0i32; 2],

            exit_tag: "Exit",
            close_tag: "Close",

            #[cfg(feature = "justify")]
            punct: None,
            #[cfg(feature = "justify")]
            brackets: None,
            #[cfg(feature = "justify")]
            quotestr: None,
            #[cfg(feature = "justify")]
            quotereg: None,

            word_chars: None,

            tabsize: -1,   // C: ssize_t tabsize = -1

            #[cfg(not(feature = "tiny"))]
            backup_dir: None,
            #[cfg(feature = "operatingdir")]
            operating_dir: None,

            #[cfg(feature = "speller")]
            alt_speller: None,

            #[cfg(feature = "color")]
            syntaxes: None,
            #[cfg(feature = "color")]
            syntaxstr: None,
            #[cfg(feature = "color")]
            have_palette: false,
            #[cfg(feature = "color")]
            rescind_colors: false,
            #[cfg(feature = "color")]
            perturbed: false,
            #[cfg(feature = "color")]
            recook: false,

            currmenu: MMOST,  // C: int currmenu = MMOST
            sclist: Vec::new(),
            allfuncs: Vec::new(),
            exitfunc: None,
            tailsc_toggle_counter: 0,
            tailsc_last_toggle: 0,

            search_history: None,
            replace_history: None,
            execute_history: None,
            #[cfg(feature = "histories")]
            searchtop: None,
            #[cfg(feature = "histories")]
            searchbot: None,
            #[cfg(feature = "histories")]
            replacetop: None,
            #[cfg(feature = "histories")]
            replacebot: None,
            #[cfg(feature = "histories")]
            executetop: None,
            #[cfg(feature = "histories")]
            executebot: None,

            search_history_items: Vec::new(),
            search_history_pos: 0,
            replace_history_items: Vec::new(),
            replace_history_pos: 0,
            execute_history_items: Vec::new(),
            execute_history_pos: 0,

            search_regexp: None,
            regmatches: [(0, 0); 10],

            hilite_attribute: A_REVERSE,  // C: int hilite_attribute = A_REVERSE
            #[cfg(feature = "color")]
            color_combo: {
                // Initialize all NUMBER_OF_ELEMENTS entries to None
                const NONE: Option<Box<ColorType>> = None;
                [NONE; NUMBER_OF_ELEMENTS]
            },
            interface_color_pair: [0i32; NUMBER_OF_ELEMENTS],
            interface_color_rgb: [(-1i16, -1i16); NUMBER_OF_ELEMENTS + 1],

            homedir: None,
            statedir: None,
            #[cfg(any(feature = "nanorc", feature = "histories"))]
            startup_problem: None,
            #[cfg(feature = "nanorc")]
            custom_nanorc: None,
            #[cfg(feature = "nanorc")]
            commandname: None,
            #[cfg(feature = "nanorc")]
            planted_shortcut: None,

            spotlighted: false,
            light_from_col: 0,
            light_to_col: 0,
        }
    }
}

impl Default for AppState {
    #[inline]
    fn default() -> Self { Self::new() }
}

// ---------------------------------------------------------------------------
// Convenience methods on AppState (used by history.rs, utils.rs, etc.)
// ---------------------------------------------------------------------------

impl AppState {
    /// Test whether a feature flag is set.
    /// C: ISSET(flag)
    #[inline]
    pub fn flag_isset(&self, flag: u32) -> bool {
        (self.flags[flag_index(flag)] & flag_mask(flag)) != 0
    }

    /// Return the `brink` (horizontal scroll offset) of the current open file.
    /// Used by utils::get_page_start.
    #[inline]
    pub fn openfile_brink(&self) -> usize {
        self.openfile.as_ref().map(|f| f.brink).unwrap_or(0)
    }

    /// Return the current cursor x position in the open file.
    #[inline]
    pub fn current_x(&self) -> usize {
        self.openfile.as_ref().map(|f| f.current_x).unwrap_or(0)
    }

    /// Return a clone of the current line's text data.
    pub fn current_line_data(&self) -> String {
        self.openfile.as_ref()
            .and_then(|f| f.current.as_ref())
            .map(|lp| lp.borrow().data.clone())
            .unwrap_or_default()
    }

    /// Append a new empty magic line to the end of the current buffer.
    /// C: new_magicline()
    pub fn append_magicline(&mut self) {
        // Create a new empty line and append it to filebot.
        let lineno = self.openfile.as_ref()
            .and_then(|f| f.filebot.as_ref())
            .map(|lb| self.lines.node(lb.idx).lineno + 1)
            .unwrap_or(1);
        let new_line = self.lines.alloc(LineNode {
            data: String::new(),
            lineno,
            next: None,
            prev: None,
            #[cfg(feature = "color")]
            multidata: Vec::new(),
            has_anchor: false,
        });
        if let Some(ref mut of) = self.openfile {
            if let Some(ref bot) = of.filebot.clone() {
                // Link the new line as successor of filebot.
                let weak_bot = LinePtr::downgrade(bot);
                new_line.borrow_mut().prev = Some(weak_bot);
                bot.borrow_mut().next = Some(new_line.clone());
            }
            of.filebot = Some(new_line);
        }
    }

    /// Remove the magic line at the end of the buffer if it is empty
    /// and is not the only line.
    /// C: remove_magicline()
    pub fn remove_magicline_if_empty(&mut self) {
        if let Some(ref mut of) = self.openfile {
            // If filebot is empty and there is a previous line, unlink it.
            let should_remove = of.filebot.as_ref().map(|lb| {
                lb.borrow().data.is_empty() && lb.borrow().prev.is_some()
            }).unwrap_or(false);
            if should_remove {
                if let Some(bot) = of.filebot.take() {
                    if let Some(prev_weak) = bot.borrow().prev.clone() {
                        if let Some(prev) = prev_weak.upgrade() {
                            prev.borrow_mut().next = None;
                            of.filebot = Some(prev);
                        }
                    }
                }
            }
        }
    }

    /// Return true when the mark is before or at the cursor position.
    /// C: mark_is_before_cursor()
    pub fn mark_is_before_cursor(&self) -> bool {
        if let Some(ref of) = self.openfile {
            if let Some(ref mark) = of.mark {
                if let Some(ref current) = of.current {
                    let mark_lineno = mark.borrow().lineno;
                    let cur_lineno = current.borrow().lineno;
                    if mark_lineno < cur_lineno {
                        return true;
                    }
                    if mark_lineno == cur_lineno {
                        return of.mark_x <= of.current_x;
                    }
                    return false;
                }
            }
        }
        false
    }

    /// Return the start and end coordinates of the marked region as
    /// (top_lineno, top_x, bot_lineno, bot_x).
    /// C: get_region()
    pub fn get_region_coords(&self) -> (usize, usize, usize, usize) {
        if let Some(ref of) = self.openfile {
            if let (Some(mark), Some(current)) = (&of.mark, &of.current) {
                let mark_lineno = mark.borrow().lineno as usize;
                let cur_lineno = current.borrow().lineno as usize;
                let mark_x = of.mark_x;
                let cur_x = of.current_x;
                if self.mark_is_before_cursor() {
                    return (mark_lineno, mark_x, cur_lineno, cur_x);
                } else {
                    return (cur_lineno, cur_x, mark_lineno, mark_x);
                }
            }
        }
        (0, 0, 0, 0)
    }

    /// Return the line numbers of the top and bottom of the range to operate on.
    /// C: get_range()
    pub fn get_range_linenos(&mut self) -> (usize, usize) {
        if let Some(ref of) = self.openfile {
            if of.mark.is_some() {
                let (top_lineno, _, bot_lineno, bot_x) = self.get_region_coords();
                // Exclude the last line if the cursor is at its start.
                let bot = if bot_x == 0 && bot_lineno > top_lineno {
                    bot_lineno - 1
                } else {
                    bot_lineno
                };
                return (top_lineno, bot);
            }
            if let Some(ref cur) = of.current {
                let lineno = cur.borrow().lineno as usize;
                return (lineno, lineno);
            }
        }
        (0, 0)
    }
}

// ---------------------------------------------------------------------------
// Thread-local global state
// ---------------------------------------------------------------------------

thread_local! {
    // const-init: with a const initializer the thread_local! macro drops the
    // per-access lazy-initialization guard that a runtime initializer requires.
    pub static STATE: NanoCell = const { NanoCell::new(AppState::new()) };
}

/// Direct read access to the global `AppState`.
///
/// SAFETY: nano edits strictly single-threaded.  This returns a `&'static
/// AppState` derived from the thread-local `STATE`'s `UnsafeCell` — a deliberate
/// lifetime extension that is valid for the main thread's lifetime.  The returned
/// reference must NEVER be moved into the background update thread (installer.rs),
/// which has its own `STATE` and never touches `AppState`.  Overlapping
/// `state()` / `state_mut()` views follow the same aliasing contract as the
/// original `with_state` closures (C-style global access), NOT Rust's `&mut`
/// uniqueness rule.
#[inline(always)]
pub fn state() -> &'static AppState {
    STATE.with(|s| unsafe { &*(s.borrow() as *const AppState) })
}

/// Direct mutable access to the global `AppState`.  See `state()` for the safety
/// contract; treat overlapping mutable views like overlapping C global writes.
#[inline(always)]
#[allow(clippy::mut_from_ref)]
pub fn state_mut() -> &'static mut AppState {
    STATE.with(|s| unsafe { &mut *(s.borrow_mut() as *mut AppState) })
}

/// Read access to AppState — thin shim over `state()`, kept so the migration to
/// direct accessors can stay incremental.  Re-entrant safe.
#[inline(always)]
pub fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&AppState) -> R,
{
    f(state())
}

/// Write access to AppState — thin shim over `state_mut()`.  Re-entrant safe.
#[inline(always)]
pub fn with_state_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut AppState) -> R,
{
    f(state_mut())
}

// ---------------------------------------------------------------------------
// Empty stub functions from global.c (toggles and cancels, no body in C)
// These are the ACTUAL functions defined in global.c lines 308-335.
// The real implementations are just {;} in C — they are command targets.
// ---------------------------------------------------------------------------

// These C "void" markers are empty by design — they are sentinel command
// targets, identified throughout the keybinding system by FUNCTION-POINTER
// IDENTITY (see first_sc_for / shown_entries_for / the search_init toggles).
// In C, distinct empty functions have distinct addresses; in Rust, two functions
// with identical bodies may be merged by identical-code-folding, which would make
// their pointers compare EQUAL and corrupt that dispatch.  Each body therefore
// carries a unique discriminant (black_box(line!())) so it cannot be folded.
macro_rules! void_marker {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        pub fn $name() { std::hint::black_box(line!()); }
    };
}

void_marker!(/* C: void case_sens_void(void) {;} */ case_sens_void);
void_marker!(/* C: void regexp_void(void) {;} */ regexp_void);
void_marker!(/* C: void backwards_void(void) {;} */ backwards_void);
void_marker!(#[cfg(feature = "histories")] get_older_item);
void_marker!(#[cfg(feature = "histories")] get_newer_item);
void_marker!(/* C: void flip_replace(void) {;} */ flip_replace);
void_marker!(#[cfg(feature = "browser")] to_files);
void_marker!(#[cfg(feature = "browser")] goto_dir);
void_marker!(#[cfg(not(feature = "tiny"))] do_nothing);
void_marker!(#[cfg(not(feature = "tiny"))] do_toggle);
void_marker!(#[cfg(not(feature = "tiny"))] dos_format);
void_marker!(#[cfg(not(feature = "tiny"))] append_it);
void_marker!(#[cfg(not(feature = "tiny"))] prepend_it);
void_marker!(#[cfg(not(feature = "tiny"))] back_it_up);
void_marker!(#[cfg(not(feature = "tiny"))] flip_execute);
void_marker!(#[cfg(not(feature = "tiny"))] flip_pipe);
void_marker!(#[cfg(not(feature = "tiny"))] flip_convert);
void_marker!(#[cfg(feature = "multibuffer")] flip_newbuffer);
void_marker!(/* C: void discard_buffer(void) {;} */ discard_buffer);
void_marker!(/* C: void do_cancel(void) {;} */ do_cancel);

// ---------------------------------------------------------------------------
// Forward declarations / stubs for functions in other modules that
// shortcut_init must reference. These will be superseded by the real
// implementations when those modules are ported.
// ---------------------------------------------------------------------------

// From help.c
pub fn do_help() { crate::help::do_help() }

// From move.c
pub fn do_page_up()    { crate::move_::do_page_up() }
pub fn do_page_down()  { crate::move_::do_page_down() }
pub fn to_first_line() { crate::move_::to_first_line() }
pub fn to_last_line()  { crate::move_::to_last_line() }
pub fn do_up()         { crate::move_::do_up() }
pub fn do_down()       { crate::move_::do_down() }
pub fn do_left()       { crate::move_::do_left() }
pub fn do_right()      { crate::move_::do_right() }
pub fn do_home()       { crate::move_::do_home() }
pub fn do_end()        { crate::move_::do_end() }
pub fn to_prev_word()  { crate::move_::to_prev_word() }
pub fn to_next_word()  { crate::move_::to_next_word() }
pub fn to_prev_block() { crate::move_::to_prev_block() }
pub fn to_next_block() { crate::move_::to_next_block() }
pub fn do_scroll_up()    { crate::move_::do_scroll_up() }
pub fn do_scroll_down()  { crate::move_::do_scroll_down() }
#[cfg(not(feature = "tiny"))]
pub fn do_scroll_left()  { crate::move_::do_scroll_left() }
#[cfg(not(feature = "tiny"))]
pub fn do_scroll_right() { crate::move_::do_scroll_right() }
#[cfg(not(feature = "tiny"))]
pub fn to_top_row()    { crate::move_::to_top_row() }
#[cfg(not(feature = "tiny"))]
pub fn to_bottom_row() { crate::move_::to_bottom_row() }
#[cfg(not(feature = "tiny"))]
pub fn do_cycle()      { crate::move_::do_cycle() }

/* C: void show_curses_version(void) — global.c; reports the terminal
 * backend version (ncurses in C, crossterm in this port). */
#[cfg(feature = "extra")]
pub fn show_curses_version() {
    crate::winio::statusline(
        MessageType::Notice,
        concat!("nano-rs ", env!("CARGO_PKG_VERSION"), ", using crossterm"),
    );
}
#[cfg(not(feature = "tiny"))]
pub fn do_center()     { crate::move_::do_center() }
#[cfg(feature = "justify")]
pub fn to_para_begin() { crate::move_::to_para_begin() }
#[cfg(feature = "justify")]
pub fn to_para_end()   { crate::move_::to_para_end() }
#[cfg(feature = "multibuffer")]
pub fn switch_to_prev_buffer() { crate::files::switch_to_prev_buffer() }
#[cfg(feature = "multibuffer")]
pub fn switch_to_next_buffer() { crate::files::switch_to_next_buffer() }

// From cut.c
pub fn cut_text()  { crate::cut::cut_text() }
pub fn paste_text() { crate::cut::paste_text() }
pub fn copy_text() { crate::cut::copy_text() }
#[cfg(not(feature = "tiny"))]
pub fn cut_till_eof() { crate::cut::cut_till_eof() }
#[cfg(not(feature = "tiny"))]
pub fn zap_text()  { crate::cut::zap_text() }

// From search.c
pub fn do_search_forward()  { crate::search::do_search_forward() }
pub fn do_search_backward() { crate::search::do_search_backward() }
pub fn do_findprevious()    { crate::search::do_findprevious() }
pub fn do_findnext()        { crate::search::do_findnext() }
pub fn do_replace()         { crate::search::do_replace() }
pub fn do_gotolinecolumn()  { crate::search::do_gotolinecolumn() }
#[cfg(not(feature = "tiny"))]
pub fn do_find_bracket()    { crate::search::do_find_bracket() }
#[cfg(not(feature = "tiny"))]
pub fn put_or_lift_anchor() { crate::search::put_or_lift_anchor() }
#[cfg(not(feature = "tiny"))]
pub fn to_prev_anchor()     { crate::search::to_prev_anchor() }
#[cfg(not(feature = "tiny"))]
pub fn to_next_anchor()     { crate::search::to_next_anchor() }

// From text.c / cut.c / winio.c
pub fn do_tab()            { crate::text::do_tab() }
pub fn do_enter()          { crate::text::do_enter() }
pub fn do_backspace()      { crate::cut::do_backspace() }
pub fn do_delete()         { crate::cut::do_delete() }
pub fn do_undo()           { crate::text::do_undo() }
pub fn do_redo()           { crate::text::do_redo() }
pub fn do_verbatim_input() { crate::text::do_verbatim_input() }
#[cfg(not(feature = "tiny"))]
pub fn do_mark()           { crate::text::do_mark() }
#[cfg(not(feature = "tiny"))]
pub fn do_indent()         { crate::text::do_indent() }
#[cfg(not(feature = "tiny"))]
pub fn do_unindent()       { crate::text::do_unindent() }
#[cfg(not(feature = "tiny"))]
pub fn chop_previous_word() { crate::cut::chop_previous_word() }
#[cfg(not(feature = "tiny"))]
pub fn chop_next_word()    { crate::cut::chop_next_word() }
#[cfg(not(feature = "tiny"))]
pub fn record_macro()      { crate::winio::record_macro() }
#[cfg(not(feature = "tiny"))]
pub fn run_macro()         { crate::winio::run_macro() }
#[cfg(not(feature = "tiny"))]
pub fn count_lines_words_and_characters() { crate::text::count_lines_words_and_characters() }
#[cfg(feature = "justify")]
pub fn do_justify()        { crate::text::do_justify() }
#[cfg(feature = "justify")]
pub fn do_full_justify()   { crate::text::do_full_justify() }
#[cfg(feature = "speller")]
pub fn do_spell()          { crate::text::do_spell() }
#[cfg(feature = "linter")]
pub fn do_linter()         { crate::text::do_linter() }
#[cfg(feature = "formatter")]
pub fn do_formatter()      { crate::text::do_formatter() }
#[cfg(feature = "comment")]
pub fn do_comment()        { crate::text::do_comment() }
#[cfg(feature = "wordcomp")]
pub fn complete_a_word()   { crate::text::complete_a_word() }

// From files.c
pub fn do_writeout()     { crate::files::do_writeout() }
pub fn do_insertfile()   { crate::files::do_insertfile() }
pub fn do_savefile()     { crate::files::do_savefile() }
#[cfg(not(feature = "tiny"))]
pub fn do_execute()      { crate::files::do_execute() }
#[cfg(feature = "browser")]
pub fn to_first_file()   { crate::browser::to_first_file() }
#[cfg(feature = "browser")]
pub fn to_last_file()    { crate::browser::to_last_file() }

// From winio.c
pub fn full_refresh()        { crate::winio::full_refresh() }
pub fn report_cursor_position() { crate::winio::report_cursor_position() }
#[cfg(not(feature = "tiny"))]
pub fn suggest_ctrlT_ctrlZ() { crate::nano::suggest_ctrlT_ctrlZ() }
#[cfg(not(feature = "tiny"))]
pub fn suck_up_input_and_paste_it() { crate::nano::suck_up_input_and_paste_it() }

// From nano.c
pub fn do_exit()    { crate::nano::do_exit() }
#[cfg(not(feature = "tiny"))]
pub fn do_suspend() { crate::nano::do_suspend() }
#[cfg(feature = "tiny")]
pub fn do_suspend() {}
pub fn changes_something(f: FuncPtr) -> bool { crate::nano::changes_something(f) }

// Tiny-only toggle helper
#[cfg(all(feature = "tiny", feature = "linenumbers"))]
/* C: void toggle_numbers(void) { TOGGLE(LINE_NUMBERS); } */
pub fn toggle_numbers() {
    with_state_mut(|s| {
        s.flags[flag_index(LINE_NUMBERS)] ^= flag_mask(LINE_NUMBERS);
    });
}

// ---------------------------------------------------------------------------
// add_to_funcs — append a function descriptor to allfuncs
// ---------------------------------------------------------------------------
/* C: void add_to_funcs(void (*function)(void), int menus, const char *tag,
                         const char *phrase, bool blank_after) */
pub fn add_to_funcs(
    function: FuncPtr,
    menus: u32,
    tag: &'static str,
    #[cfg(feature = "help")] phrase: &'static str,
    #[cfg(feature = "help")] blank_after: bool,
    #[cfg(not(feature = "help"))] _phrase: &'static str,
    #[cfg(not(feature = "help"))] _blank_after: bool,
) {
    let entry = FuncStruct {
        func: Some(function),
        tag,
        #[cfg(feature = "help")]
        phrase,
        #[cfg(feature = "help")]
        blank_after,
        menus: menus as i32,
        next: None,
    };
    with_state_mut(|s| s.allfuncs.push(entry));
}

// ---------------------------------------------------------------------------
// keycode_from_string — parse a keystring into an integer keycode
// ---------------------------------------------------------------------------
/* C: int keycode_from_string(const char *keystring) */
pub fn keycode_from_string(keystring: &str) -> i32 {
    let bytes = keystring.as_bytes();
    if bytes.is_empty() {
        return -1;
    }

    if bytes[0] == b'^' {
        if keystring.len() == 2 {
            let ch = bytes[1];
            if ch == b'/' || ch == b'-' {
                return 31;
            }
            if ch <= b'_' {
                return (ch as i32) - 64;
            }
            if ch == b'`' {
                return 0;
            }
            return -1;
        } else if keystring.eq_ignore_ascii_case("^Space") {
            return 0;
        } else {
            return -1;
        }
    }

    if bytes[0] == b'M' {
        if bytes.len() >= 2 && bytes[1] == b'-' && keystring.len() == 3 {
            let ch = bytes[2];
            if ch.is_ascii_uppercase() {
                return (ch | 0x20) as i32;
            } else {
                return ch as i32;
            }
        }
        if keystring.eq_ignore_ascii_case("M-Space") {
            return b' ' as i32;
        } else if keystring.eq_ignore_ascii_case("M-Left") {
            return ALT_LEFT as i32;
        } else if keystring.eq_ignore_ascii_case("M-Right") {
            return ALT_RIGHT as i32;
        } else if keystring.eq_ignore_ascii_case("M-Up") {
            return ALT_UP as i32;
        } else if keystring.eq_ignore_ascii_case("M-Down") {
            return ALT_DOWN as i32;
        } else if keystring.eq_ignore_ascii_case("M-Ins") {
            return ALT_INSERT as i32;
        } else if keystring.eq_ignore_ascii_case("M-Del") {
            return ALT_DELETE as i32;
        } else {
            return -1;
        }
    }

    // #ifdef ENABLE_NANORC — Sh-M-<letter>
    #[cfg(feature = "nanorc")]
    {
        // Compare on bytes (like C's strncasecmp); slicing keystring[..5] as a &str
        // would panic when byte 5 is not a char boundary for a 6-byte multibyte key.
        if bytes.len() == 6
            && bytes[..5].eq_ignore_ascii_case(b"Sh-M-")
        {
            let ch = bytes[5];
            let lower = ch | 0x20;
            if lower >= b'a' && lower <= b'z' {
                with_state_mut(|s| s.shifted_metas = true);
                return (ch & 0x5F) as i32;
            }
        }
    }

    if bytes[0] == b'F' {
        // atoi-style: parse only the leading run of digits, so "F12" works and a
        // trailing remainder (as C's atoi tolerates) does not reject the key.
        let digits: String = keystring[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(fn_num) = digits.parse::<i32>() {
            if fn_num >= 1 && fn_num <= 24 {
                return KEY_F0 + fn_num;
            }
        }
        return -1;
    }

    if keystring.eq_ignore_ascii_case("Ins") {
        return KEY_IC;
    }
    if keystring.eq_ignore_ascii_case("Del") {
        return KEY_DC;
    }

    -1
}

// ---------------------------------------------------------------------------
// add_to_sclist — append a key binding to sclist
// ---------------------------------------------------------------------------
/* C: void add_to_sclist(int menus, const char *scstring, const int keycode,
                          void (*function)(void), int toggle) */
pub fn add_to_sclist(
    menus: u32,
    scstring: &'static str,
    keycode: i32,
    function: FuncPtr,
    toggle: i32,
) {
    with_state_mut(|s| {
        #[cfg(not(feature = "tiny"))]
        let ordinal = if toggle != 0 {
            // When not the same toggle as the previous one, increment the counter.
            if s.tailsc_last_toggle != toggle {
                s.tailsc_toggle_counter += 1;
            }
            s.tailsc_last_toggle = toggle;
            s.tailsc_toggle_counter
        } else {
            0
        };

        let resolved_keycode = if keycode != 0 {
            keycode
        } else {
            keycode_from_string(scstring)
        };

        let entry = KeyStruct {
            keystr: scstring,
            keycode: resolved_keycode,
            menus: menus as i32,
            func: Some(function),
            #[cfg(not(feature = "tiny"))]
            toggle,
            #[cfg(not(feature = "tiny"))]
            ordinal,
            #[cfg(feature = "nanorc")]
            expansion: None,
            next: None,
        };
        s.sclist.push(entry);
    });
}

// ---------------------------------------------------------------------------
// first_sc_for — find the first shortcut in the given menu matching a function
// ---------------------------------------------------------------------------
/* C: const keystruct *first_sc_for(int menu, void (*function)(void)) */
/// Returns the keystr of the first matching shortcut, or "" if none.
pub fn first_sc_for(menu: u32, function: FuncPtr) -> Option<(i32, &'static str)> {
    with_state(|s| {
        for sc in &s.sclist {
            if (sc.menus as u32 & menu) != 0
                && sc.func == Some(function)
                && !sc.keystr.is_empty()
            {
                return Some((sc.keycode, sc.keystr));
            }
        }
        None
    })
}

// ---------------------------------------------------------------------------
// shown_entries_for — count how many function entries apply to the given menu
// ---------------------------------------------------------------------------
/* C: size_t shown_entries_for(int menu) */
pub fn shown_entries_for(menu: u32) -> usize {
    with_state(|s| {
        // C uses the live COLS in this formula (global.c:476), not a fixed 80.
        let cols: usize = crate::winio::get_cols();
        let maximum = ((cols + 40) / 20) * 2;
        let mut count = 0usize;
        let mut found_all = true;
        for (_i, item) in s.allfuncs.iter().enumerate() {
            if count >= maximum {
                found_all = false;
                break;
            }
            if (item.menus as u32) & menu != 0 {
                count += 1;
            }
        }
        // When --saveonexit is not used, widen the grid of the WriteOut menu.
        if menu == MWRITEFILE && found_all {
            // If discard_buffer is not in sclist for this menu, decrement.
            let has_discard = s.sclist.iter().any(|sc| {
                (sc.menus as u32 & menu) != 0 && sc.func == Some(discard_buffer as FuncPtr)
            });
            if !has_discard && count > 0 {
                count -= 1;
            }
        }
        count
    })
}

// ---------------------------------------------------------------------------
// get_shortcut — find first shortcut in current menu matching the keycode
// ---------------------------------------------------------------------------
/* C: const keystruct *get_shortcut(const int keycode) */
/// Returns Some(FuncPtr) if a shortcut matches, or None.
pub fn get_shortcut(keycode: i32) -> Option<FuncPtr> {
    with_state(|s| {
        // Plain characters and upper control codes cannot be shortcuts.
        if !s.meta_key && keycode >= 0x20 && keycode <= 0xFF {
            return None;
        }
        // Lower control codes with Meta cannot be shortcuts either.
        if s.meta_key && keycode < 0x20 {
            return None;
        }

        #[cfg(feature = "nanorc")]
        if keycode == PLANTED_A_COMMAND as i32 {
            if let Some(idx) = s.planted_shortcut {
                return s.sclist.get(idx).and_then(|sc| sc.func);
            }
            return None;
        }

        let currmenu = s.currmenu;
        for sc in &s.sclist {
            if (sc.menus as u32 & currmenu) != 0 && keycode == sc.keycode {
                return sc.func;
            }
        }
        None
    })
}

// ---------------------------------------------------------------------------
// func_from_key — return the function bound to the given keycode
// ---------------------------------------------------------------------------
/* C: functionptrtype func_from_key(const int keycode) */
pub fn func_from_key(keycode: i32) -> Option<FuncPtr> {
    get_shortcut(keycode)
}

// ---------------------------------------------------------------------------
// interpret — used by browser/help viewer for plain-char shortcuts
// ---------------------------------------------------------------------------
/* C: functionptrtype interpret(const int keycode) */
#[cfg(any(feature = "browser", feature = "help"))]
pub fn interpret(keycode: i32) -> Option<FuncPtr> {
    let meta = with_state(|s| s.meta_key);
    if !meta && keycode < 0x7F {
        if keycode == b'N' as i32 {
            return Some(do_findprevious as FuncPtr);
        }
        if keycode == b'n' as i32 {
            return Some(do_findnext as FuncPtr);
        }
        match (keycode as u8 as char).to_ascii_lowercase() {
            'b' | '-' => return Some(do_page_up as FuncPtr),
            ' '       => return Some(do_page_down as FuncPtr),
            'w' | '/' => return Some(do_search_forward as FuncPtr),
            #[cfg(feature = "browser")]
            'g'       => return Some(goto_dir as FuncPtr),
            '?'       => return Some(do_help as FuncPtr),
            's'       => return Some(do_enter as FuncPtr),
            'e' | 'q' | 'x' => return Some(do_exit as FuncPtr),
            _ => {}
        }
    }
    func_from_key(keycode)
}

// ---------------------------------------------------------------------------
// epithet_of_flag — return text description of a toggle flag
// ---------------------------------------------------------------------------
/* C: const char *epithet_of_flag(int flag) */
#[cfg(not(feature = "tiny"))]
pub fn epithet_of_flag(flag: u32) -> &'static str {
    match flag {
        f if f == ZERO              => "Hidden interface",
        f if f == NO_HELP           => "Help mode",
        f if f == CONSTANT_SHOW     => "Constant cursor position display",
        f if f == SOFTWRAP          => "Soft wrapping of overlong lines",
        f if f == LINE_NUMBERS      => "Line numbering",
        f if f == WHITESPACE_DISPLAY => "Whitespace display",
        f if f == NO_SYNTAX         => "Color syntax highlighting",
        f if f == SMART_HOME        => "Smart home key",
        f if f == AUTOINDENT        => "Auto indent",
        f if f == CUT_FROM_CURSOR   => "Cut to end",
        f if f == BREAK_LONG_LINES  => "Hard wrapping of overlong lines",
        f if f == TABS_TO_SPACES    => "Conversion of typed tabs to spaces",
        f if f == USE_MOUSE         => "Mouse support",
        _                           => "Ehm...",
    }
}

// ---------------------------------------------------------------------------
// shortcut_init — build the allfuncs and sclist tables
// ---------------------------------------------------------------------------
/* C: void shortcut_init(void) */
pub fn shortcut_init() {
    // These local tag strings parallel the C N_("...") calls.
    // TRANSLATORS strings are kept as-is for the port.

    // Clear any previous state.
    with_state_mut(|s| {
        s.allfuncs.clear();
        s.sclist.clear();
        s.exitfunc = None;
        s.tailsc_toggle_counter = 0;
        s.tailsc_last_toggle = 0;
    });

    // ---- Help descriptions ----
    // #ifdef ENABLE_HELP  — only used if help feature is enabled
    // We declare them as &'static str regardless; they're only wired in
    // add_to_funcs under the feature gate.
    let cancel_gist = "Cancel the current function";
    let help_gist = "Display this help text";
    let exit_gist = "Close the current buffer / Exit from nano";
    let writeout_gist = "Write the current buffer (or the marked region) to disk";
    let readfile_gist = "Insert another file into current buffer (or into new buffer)";
    let whereis_gist = "Search forward for a string or a regular expression";
    let wherewas_gist = "Search backward for a string or a regular expression";
    let cut_gist = "Cut current line (or marked region) and store it in cutbuffer";
    let copy_gist = "Copy current line (or marked region) and store it in cutbuffer";
    let paste_gist = "Paste the contents of cutbuffer at current cursor position";
    let cursorpos_gist = "Display the position of the cursor";
    #[cfg(feature = "speller")]
    let spell_gist = "Invoke the spell checker, if available";
    let replace_gist = "Replace a string or a regular expression";
    let gotoline_gist = "Go to line and column number";
    #[cfg(not(feature = "tiny"))]
    let bracket_gist = "Go to the matching bracket";
    #[cfg(not(feature = "tiny"))]
    let mark_gist = "Mark text starting from the cursor position";
    #[cfg(not(feature = "tiny"))]
    let zap_gist = "Throw away the current line (or marked region)";
    #[cfg(not(feature = "tiny"))]
    let indent_gist = "Indent the current line (or marked lines)";
    #[cfg(not(feature = "tiny"))]
    let unindent_gist = "Unindent the current line (or marked lines)";
    #[cfg(not(feature = "tiny"))]
    let undo_gist = "Undo the last operation";
    #[cfg(not(feature = "tiny"))]
    let redo_gist = "Redo the last undone operation";
    let back_gist = "Go back one character";
    let forward_gist = "Go forward one character";
    let prevword_gist = "Go back one word";
    let nextword_gist = "Go forward one word";
    let prevline_gist = "Go to previous line";
    let nextline_gist = "Go to next line";
    let home_gist = "Go to beginning of current line";
    let end_gist = "Go to end of current line";
    let prevblock_gist = "Go to previous block of text";
    let nextblock_gist = "Go to next block of text";
    #[cfg(feature = "justify")]
    let parabegin_gist = "Go to beginning of paragraph; then of previous paragraph";
    #[cfg(feature = "justify")]
    let paraend_gist = "Go just beyond end of paragraph; then of next paragraph";
    #[cfg(not(feature = "tiny"))]
    let toprow_gist = "Go to first row in the viewport";
    #[cfg(not(feature = "tiny"))]
    let bottomrow_gist = "Go to last row in the viewport";
    #[cfg(not(feature = "tiny"))]
    let center_gist = "Center the line where the cursor is";
    #[cfg(not(feature = "tiny"))]
    let cycle_gist = "Push the cursor line to the center, then top, then bottom";
    let prevpage_gist = "Go one screenful up";
    let nextpage_gist = "Go one screenful down";
    let firstline_gist = "Go to the first line of the file";
    let lastline_gist = "Go to the last line of the file";
    #[cfg(not(feature = "tiny"))]
    let scrollleft_gist = "Scroll the viewport a tabsize to the left";
    #[cfg(not(feature = "tiny"))]
    let scrollright_gist = "Scroll the viewport a tabsize to the right";
    #[cfg(any(not(feature = "tiny"), feature = "help"))]
    let scrollup_gist = "Scroll up one line without moving the cursor textually";
    #[cfg(any(not(feature = "tiny"), feature = "help"))]
    let scrolldown_gist = "Scroll down one line without moving the cursor textually";
    #[cfg(feature = "multibuffer")]
    let prevfile_gist = "Switch to the previous file buffer";
    #[cfg(feature = "multibuffer")]
    let nextfile_gist = "Switch to the next file buffer";
    let verbatim_gist = "Insert the next keystroke verbatim";
    let tab_gist = "Insert a tab at the cursor position (or indent marked lines)";
    let enter_gist = "Insert a newline at the cursor position";
    let delete_gist = "Delete the character under the cursor";
    let backspace_gist = "Delete the character to the left of the cursor";
    #[cfg(not(feature = "tiny"))]
    let chopwordleft_gist = "Delete backward from cursor to word start";
    #[cfg(not(feature = "tiny"))]
    let chopwordright_gist = "Delete forward from cursor to next word start";
    #[cfg(not(feature = "tiny"))]
    let cuttilleof_gist = "Cut from the cursor position to the end of the file";
    #[cfg(feature = "justify")]
    let justify_gist = "Justify the current paragraph";
    #[cfg(feature = "justify")]
    let fulljustify_gist = "Justify the entire file";
    #[cfg(not(feature = "tiny"))]
    let wordcount_gist = "Count the number of lines, words, and characters";
    #[cfg(not(feature = "tiny"))]
    let suspend_gist = "Suspend the editor (return to the shell)";
    let refresh_gist = "Refresh (redraw) the current screen";
    #[cfg(feature = "wordcomp")]
    let completion_gist = "Try and complete the current word";
    #[cfg(feature = "comment")]
    let comment_gist = "Comment/uncomment the current line (or marked lines)";
    let savefile_gist = "Save file without prompting";
    let findprev_gist = "Search next occurrence backward";
    let findnext_gist = "Search next occurrence forward";
    #[cfg(not(feature = "tiny"))]
    let recordmacro_gist = "Start/stop recording a macro";
    #[cfg(not(feature = "tiny"))]
    let runmacro_gist = "Run the last recorded macro";
    #[cfg(not(feature = "tiny"))]
    let anchor_gist = "Place or remove an anchor at the current line";
    #[cfg(not(feature = "tiny"))]
    let prevanchor_gist = "Jump backward to the nearest anchor";
    #[cfg(not(feature = "tiny"))]
    let nextanchor_gist = "Jump forward to the nearest anchor";
    let case_gist = "Toggle the case sensitivity of the search";
    let reverse_gist = "Reverse the direction of the search";
    let regexp_gist_str = "Toggle the use of regular expressions";
    #[cfg(feature = "histories")]
    let older_gist = "Recall the previous search/replace string";
    #[cfg(feature = "histories")]
    let newer_gist = "Recall the next search/replace string";
    #[cfg(not(feature = "tiny"))]
    let dos_gist = "Toggle the use of DOS format";
    #[cfg(not(feature = "tiny"))]
    let append_gist = "Toggle appending";
    #[cfg(not(feature = "tiny"))]
    let prepend_gist = "Toggle prepending";
    #[cfg(not(feature = "tiny"))]
    let backup_gist = "Toggle backing up of the original file";
    #[cfg(not(feature = "tiny"))]
    let execute_gist = "Execute a function or an external command";
    #[cfg(not(feature = "tiny"))]
    let pipe_gist = "Pipe the current buffer (or marked region) to the command";
    #[cfg(all(not(feature = "tiny"), feature = "histories"))]
    let older_command_gist = "Recall the previous command";
    #[cfg(all(not(feature = "tiny"), feature = "histories"))]
    let newer_command_gist = "Recall the next command";
    #[cfg(not(feature = "tiny"))]
    let convert_gist = "Do not convert from DOS format";
    #[cfg(feature = "multibuffer")]
    let newbuffer_gist = "Toggle the use of a new buffer";
    let discardbuffer_gist = "Close buffer without saving it";
    #[cfg(feature = "browser")]
    let tofiles_gist = "Go to file browser";
    #[cfg(feature = "browser")]
    let exitbrowser_gist = "Exit from the file browser";
    #[cfg(feature = "browser")]
    let firstfile_gist = "Go to the first file in the list";
    #[cfg(feature = "browser")]
    let lastfile_gist = "Go to the last file in the list";
    #[cfg(feature = "browser")]
    let backfile_gist = "Go to the previous file in the list";
    #[cfg(feature = "browser")]
    let forwardfile_gist = "Go to the next file in the list";
    #[cfg(all(feature = "browser", not(feature = "tiny")))]
    let browserlefthand_gist = "Go to lefthand column";
    #[cfg(all(feature = "browser", not(feature = "tiny")))]
    let browserrighthand_gist = "Go to righthand column";
    #[cfg(all(feature = "browser", not(feature = "tiny")))]
    let browsertoprow_gist = "Go to first row in this column";
    #[cfg(all(feature = "browser", not(feature = "tiny")))]
    let browserbottomrow_gist = "Go to last row in this column";
    #[cfg(feature = "browser")]
    let browserwhereis_gist = "Search forward for a string";
    #[cfg(feature = "browser")]
    let browserwherewas_gist = "Search backward for a string";
    #[cfg(feature = "browser")]
    let browserrefresh_gist = "Refresh the file list";
    #[cfg(feature = "browser")]
    let gotodir_gist = "Go to directory";
    #[cfg(feature = "linter")]
    let lint_gist = "Invoke the linter, if available";
    #[cfg(feature = "linter")]
    let prevlint_gist = "Go to previous linter msg";
    #[cfg(feature = "linter")]
    let nextlint_gist = "Go to next linter msg";
    #[cfg(feature = "formatter")]
    let formatter_gist = "Invoke a program to format/arrange/manipulate the buffer";

    // Determine help_key.  C: help_key = (bsp_string && *bsp_string != 0x08) ?
    // "^H" : "^N" — i.e. "^H" whenever the terminal's Backspace is not ^H, which
    // is the case for essentially every modern terminal (Backspace is 0x7F).
    // crossterm exposes no terminfo "kb" capability and reports the physical
    // Backspace key as KeyCode::Backspace (bound below via "Bsp"), so default to
    // "^H" — the value C produces on modern terminals.  help_key is added before
    // the (now shadowed) ^H->backspace entry, and get_shortcut returns the first
    // match, so ^H -> Help while the real Backspace key still deletes; this also
    // frees ^N for do_down (the previous "^N" default collided with it).
    let help_key: &'static str = "^H";

    const BLANKAFTER: bool = true; // C: #define BLANKAFTER TRUE
    const TOGETHER:   bool = false;

    // ---- Start populating menus with functions ----

    #[cfg(feature = "help")]
    {
        // add_to_funcs(do_help, (MMOST | MBROWSER) & ~MFINDINHELP, "Help", help_gist, TOGETHER);
        add_to_funcs(do_help as FuncPtr, (MMOST | MBROWSER) & !MFINDINHELP,
            "Help", help_gist, TOGETHER);
    }

    add_to_funcs(do_cancel as FuncPtr, (MMOST & !MMAIN) | MYESNO,
        "Cancel", cancel_gist, BLANKAFTER);

    add_to_funcs(do_exit as FuncPtr, MMAIN,
        "Exit", exit_gist, TOGETHER);
    // Remember the index for Exit, to be able to replace it with Close.
    with_state_mut(|s| {
        if !s.allfuncs.is_empty() {
            s.exitfunc = Some(s.allfuncs.len() - 1);
        }
    });

    #[cfg(feature = "browser")]
    {
        add_to_funcs(do_exit as FuncPtr, MBROWSER,
            "Close", exitbrowser_gist, TOGETHER);
    }

    #[cfg(not(feature = "help"))]
    {
        add_to_funcs(full_refresh as FuncPtr, MMAIN | MREPLACE, "Refresh", "x", false);
        #[cfg(not(feature = "tiny"))]
        add_to_funcs(full_refresh as FuncPtr, MINSERTFILE | MEXECUTE, "Refresh", "x", false);
    }

    add_to_funcs(do_writeout as FuncPtr, MMAIN,
        "Write Out", writeout_gist, TOGETHER);

    // In restricted mode, replace Insert with Justify when possible;
    // otherwise, show Insert anyway to keep the help items paired.
    let is_restricted = with_state(|s| {
        (s.flags[flag_index(RESTRICTED)] & flag_mask(RESTRICTED)) != 0
    });
    #[cfg(feature = "justify")]
    {
        if !is_restricted {
            add_to_funcs(do_insertfile as FuncPtr, MMAIN,
                "Read File", readfile_gist, BLANKAFTER);
        } else {
            add_to_funcs(do_justify as FuncPtr, MMAIN,
                "Justify", justify_gist, BLANKAFTER);
        }
    }
    #[cfg(not(feature = "justify"))]
    {
        add_to_funcs(do_insertfile as FuncPtr, MMAIN,
            "Read File", readfile_gist, BLANKAFTER);
    }

    #[cfg(feature = "help")]
    {
        // The help viewer doesn't have a help text, so phrase/blank are irrelevant.
        add_to_funcs(full_refresh as FuncPtr, MHELP, "Refresh", "x", false);
        add_to_funcs(do_exit as FuncPtr, MHELP, "Close", "x", false);
    }

    add_to_funcs(do_search_forward as FuncPtr, MMAIN | MHELP,
        "Where Is", whereis_gist, TOGETHER);

    add_to_funcs(do_replace as FuncPtr, MMAIN,
        "Replace", replace_gist, TOGETHER);

    #[cfg(feature = "tiny")]
    {
        add_to_funcs(do_search_backward as FuncPtr, MHELP,
            "Where Was", wherewas_gist, TOGETHER);
        add_to_funcs(do_findprevious as FuncPtr, MMAIN | MHELP,
            "Previous", findprev_gist, TOGETHER);
        add_to_funcs(do_findnext as FuncPtr, MMAIN | MHELP,
            "Next", findnext_gist, BLANKAFTER);
    }

    add_to_funcs(cut_text as FuncPtr, MMAIN,
        "Cut", cut_gist, TOGETHER);

    add_to_funcs(paste_text as FuncPtr, MMAIN,
        "Paste", paste_gist, BLANKAFTER);

    let is_view_mode = with_state(|s| {
        (s.flags[flag_index(VIEW_MODE)] & flag_mask(VIEW_MODE)) != 0
    });

    if !is_restricted {
        #[cfg(not(feature = "tiny"))]
        {
            add_to_funcs(do_execute as FuncPtr, MMAIN,
                "Execute", execute_gist, TOGETHER);
        }
        #[cfg(feature = "justify")]
        {
            add_to_funcs(do_justify as FuncPtr, MMAIN,
                "Justify", justify_gist, BLANKAFTER);
        }
    }

    add_to_funcs(report_cursor_position as FuncPtr, MMAIN,
        "Location", cursorpos_gist, TOGETHER);

    #[cfg(any(feature = "tiny", feature = "justify"))]
    {
        add_to_funcs(do_gotolinecolumn as FuncPtr, MMAIN,
            "Go To Line", gotoline_gist, BLANKAFTER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(do_undo as FuncPtr, MMAIN,
            "Undo", undo_gist, TOGETHER);
        add_to_funcs(do_redo as FuncPtr, MMAIN,
            "Redo", redo_gist, BLANKAFTER);

        add_to_funcs(do_mark as FuncPtr, MMAIN,
            "Set Mark", mark_gist, TOGETHER);
        add_to_funcs(copy_text as FuncPtr, MMAIN,
            "Copy", copy_gist, BLANKAFTER);
    }

    add_to_funcs(case_sens_void as FuncPtr, MWHEREIS | MREPLACE,
        "Case sensitive", case_gist, TOGETHER);
    add_to_funcs(regexp_void as FuncPtr, MWHEREIS | MREPLACE,
        "Reg.expression", regexp_gist_str, TOGETHER);
    add_to_funcs(backwards_void as FuncPtr, MWHEREIS | MREPLACE,
        "Backwards", reverse_gist, BLANKAFTER);

    add_to_funcs(flip_replace as FuncPtr, MWHEREIS,
        "Replace", replace_gist, BLANKAFTER);
    add_to_funcs(flip_replace as FuncPtr, MREPLACE,
        "No Replace", whereis_gist, BLANKAFTER);

    #[cfg(feature = "histories")]
    {
        add_to_funcs(get_older_item as FuncPtr, MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE,
            "Older", older_gist, TOGETHER);
        add_to_funcs(get_newer_item as FuncPtr, MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE,
            "Newer", newer_gist, BLANKAFTER);
        #[cfg(not(feature = "tiny"))]
        {
            add_to_funcs(get_older_item as FuncPtr, MEXECUTE,
                "Older", older_command_gist, TOGETHER);
            add_to_funcs(get_newer_item as FuncPtr, MEXECUTE,
                "Newer", newer_command_gist, BLANKAFTER);
        }
    }

    #[cfg(feature = "browser")]
    {
        add_to_funcs(goto_dir as FuncPtr, MBROWSER,
            "Go To Dir", gotodir_gist, TOGETHER);
        #[cfg(feature = "help")]
        {
            add_to_funcs(full_refresh as FuncPtr, MBROWSER,
                "Refresh", browserrefresh_gist, BLANKAFTER);
        }
        add_to_funcs(do_search_forward as FuncPtr, MBROWSER,
            "Where Is", browserwhereis_gist, TOGETHER);
        add_to_funcs(do_search_backward as FuncPtr, MBROWSER,
            "Where Was", browserwherewas_gist, TOGETHER);
        add_to_funcs(do_findprevious as FuncPtr, MBROWSER,
            "Previous", findprev_gist, TOGETHER);
        add_to_funcs(do_findnext as FuncPtr, MBROWSER,
            "Next", findnext_gist, BLANKAFTER);
    }

    #[cfg(feature = "tiny")]
    {
        add_to_funcs(to_prev_word as FuncPtr, MMAIN,
            "Prev Word", prevword_gist, TOGETHER);
        add_to_funcs(to_next_word as FuncPtr, MMAIN,
            "Next Word", nextword_gist, BLANKAFTER);
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(do_find_bracket as FuncPtr, MMAIN,
            "To Bracket", bracket_gist, BLANKAFTER);
        add_to_funcs(do_search_backward as FuncPtr, MMAIN | MHELP,
            "Where Was", wherewas_gist, TOGETHER);
        add_to_funcs(do_findprevious as FuncPtr, MMAIN | MHELP,
            "Previous", findprev_gist, TOGETHER);
        add_to_funcs(do_findnext as FuncPtr, MMAIN | MHELP,
            "Next", findnext_gist, BLANKAFTER);
    }

    add_to_funcs(do_left as FuncPtr, MMAIN,
        "Back", back_gist, TOGETHER);
    add_to_funcs(do_right as FuncPtr, MMAIN,
        "Forward", forward_gist, TOGETHER);
    #[cfg(feature = "browser")]
    {
        add_to_funcs(do_left as FuncPtr, MBROWSER,
            "Back", backfile_gist, TOGETHER);
        add_to_funcs(do_right as FuncPtr, MBROWSER,
            "Forward", forwardfile_gist, TOGETHER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(to_prev_word as FuncPtr, MMAIN,
            "Prev Word", prevword_gist, TOGETHER);
        add_to_funcs(to_next_word as FuncPtr, MMAIN,
            "Next Word", nextword_gist, TOGETHER);
    }
    add_to_funcs(do_home as FuncPtr, MMAIN,
        "Home", home_gist, TOGETHER);
    add_to_funcs(do_end as FuncPtr, MMAIN,
        "End", end_gist, TOGETHER);
    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(do_scroll_left as FuncPtr, MMAIN,
            "Scroll Left", scrollleft_gist, TOGETHER);
        add_to_funcs(do_scroll_right as FuncPtr, MMAIN,
            "Scroll Right", scrollright_gist, BLANKAFTER);
    }

    add_to_funcs(do_up as FuncPtr, MMAIN | MBROWSER | MHELP,
        "Prev Line", prevline_gist, TOGETHER);
    add_to_funcs(do_down as FuncPtr, MMAIN | MBROWSER | MHELP,
        "Next Line", nextline_gist, TOGETHER);
    #[cfg(any(not(feature = "tiny"), feature = "help"))]
    {
        add_to_funcs(do_scroll_up as FuncPtr, MMAIN,
            "Scroll Up", scrollup_gist, TOGETHER);
        add_to_funcs(do_scroll_down as FuncPtr, MMAIN,
            "Scroll Down", scrolldown_gist, BLANKAFTER);
    }

    add_to_funcs(to_prev_block as FuncPtr, MMAIN,
        "Prev Block", prevblock_gist, TOGETHER);
    add_to_funcs(to_next_block as FuncPtr, MMAIN,
        "Next Block", nextblock_gist, TOGETHER);
    #[cfg(feature = "justify")]
    {
        add_to_funcs(to_para_begin as FuncPtr, MMAIN | MGOTOLINE,
            "Start of Paragraph", parabegin_gist, TOGETHER);
        add_to_funcs(to_para_end as FuncPtr, MMAIN | MGOTOLINE,
            "End of Paragraph", paraend_gist, BLANKAFTER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(to_top_row as FuncPtr, MMAIN,
            "Top Row", toprow_gist, TOGETHER);
        add_to_funcs(to_bottom_row as FuncPtr, MMAIN,
            "Bottom Row", bottomrow_gist, BLANKAFTER);
    }

    add_to_funcs(do_page_up as FuncPtr, MMAIN | MHELP,
        "Prev Page", prevpage_gist, TOGETHER);
    add_to_funcs(do_page_down as FuncPtr, MMAIN | MHELP,
        "Next Page", nextpage_gist, TOGETHER);

    add_to_funcs(to_first_line as FuncPtr, MMAIN | MHELP | MGOTOLINE,
        "First Line", firstline_gist, TOGETHER);
    add_to_funcs(to_last_line as FuncPtr, MMAIN | MHELP | MGOTOLINE,
        "Last Line", lastline_gist, BLANKAFTER);

    #[cfg(feature = "multibuffer")]
    {
        add_to_funcs(switch_to_prev_buffer as FuncPtr, MMAIN,
            "Prev File", prevfile_gist, TOGETHER);
        add_to_funcs(switch_to_next_buffer as FuncPtr, MMAIN,
            "Next File", nextfile_gist, BLANKAFTER);
    }

    #[cfg(all(not(feature = "tiny"), not(feature = "justify")))]
    {
        add_to_funcs(do_gotolinecolumn as FuncPtr, MMAIN,
            "Go To Line", gotoline_gist, BLANKAFTER);
    }

    add_to_funcs(do_tab as FuncPtr, MMAIN,
        "Tab", tab_gist, TOGETHER);
    add_to_funcs(do_enter as FuncPtr, MMAIN,
        "Enter", enter_gist, BLANKAFTER);

    add_to_funcs(do_backspace as FuncPtr, MMAIN,
        "Backspace", backspace_gist, TOGETHER);
    add_to_funcs(do_delete as FuncPtr, MMAIN,
        "Delete", delete_gist, BLANKAFTER);

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(chop_previous_word as FuncPtr, MMAIN,
            "Chop Left", chopwordleft_gist, TOGETHER);
        add_to_funcs(chop_next_word as FuncPtr, MMAIN,
            "Chop Right", chopwordright_gist, TOGETHER);
        add_to_funcs(cut_till_eof as FuncPtr, MMAIN,
            "Cut Till End", cuttilleof_gist, BLANKAFTER);
    }

    #[cfg(feature = "justify")]
    {
        add_to_funcs(do_full_justify as FuncPtr, MMAIN,
            "Full Justify", fulljustify_gist, TOGETHER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(count_lines_words_and_characters as FuncPtr, MMAIN,
            "Word Count", wordcount_gist, TOGETHER);
    }
    #[cfg(feature = "tiny")]
    {
        add_to_funcs(copy_text as FuncPtr, MMAIN,
            "Copy", copy_gist, BLANKAFTER);
    }

    add_to_funcs(do_verbatim_input as FuncPtr, MMAIN,
        "Verbatim", verbatim_gist, BLANKAFTER);

    #[cfg(feature = "tiny")]
    {
        add_to_funcs(do_search_backward as FuncPtr, MMAIN,
            "Where Was", wherewas_gist, BLANKAFTER);
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(do_indent as FuncPtr, MMAIN,
            "Indent", indent_gist, TOGETHER);
        add_to_funcs(do_unindent as FuncPtr, MMAIN,
            "Unindent", unindent_gist, BLANKAFTER);
    }
    #[cfg(feature = "comment")]
    {
        add_to_funcs(do_comment as FuncPtr, MMAIN,
            "Comment Lines", comment_gist, TOGETHER);
    }
    #[cfg(feature = "wordcomp")]
    {
        add_to_funcs(complete_a_word as FuncPtr, MMAIN,
            "Complete", completion_gist, BLANKAFTER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(record_macro as FuncPtr, MMAIN,
            "Record", recordmacro_gist, TOGETHER);
        add_to_funcs(run_macro as FuncPtr, MMAIN,
            "Run Macro", runmacro_gist, BLANKAFTER);

        add_to_funcs(zap_text as FuncPtr, MMAIN,
            "Zap", zap_gist, BLANKAFTER);

        add_to_funcs(put_or_lift_anchor as FuncPtr, MMAIN,
            "Anchor", anchor_gist, TOGETHER);
        add_to_funcs(to_prev_anchor as FuncPtr, MMAIN,
            "Up to anchor", prevanchor_gist, TOGETHER);
        add_to_funcs(to_next_anchor as FuncPtr, MMAIN,
            "Down to anchor", nextanchor_gist, BLANKAFTER);

        #[cfg(feature = "speller")]
        {
            add_to_funcs(do_spell as FuncPtr, MMAIN,
                "Spell Check", spell_gist, TOGETHER);
        }
        #[cfg(feature = "linter")]
        {
            add_to_funcs(do_linter as FuncPtr, MMAIN,
                "Linter", lint_gist, TOGETHER);
        }
        #[cfg(feature = "formatter")]
        {
            add_to_funcs(do_formatter as FuncPtr, MMAIN,
                "Formatter", formatter_gist, BLANKAFTER);
        }
        // Although not allowed in restricted mode, keep execution rebindable.
        if is_restricted {
            add_to_funcs(do_execute as FuncPtr, MMAIN,
                "Execute", execute_gist, TOGETHER);
        }

        add_to_funcs(do_suspend as FuncPtr, MMAIN,
            "Suspend", suspend_gist, TOGETHER);
    } // !NANO_TINY

    #[cfg(feature = "help")]
    {
        add_to_funcs(full_refresh as FuncPtr, MMAIN,
            "Refresh", refresh_gist, BLANKAFTER);
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(do_center as FuncPtr, MMAIN,
            "Center", center_gist, TOGETHER);
        add_to_funcs(do_cycle as FuncPtr, MMAIN,
            "Cycle", cycle_gist, BLANKAFTER);
    }

    add_to_funcs(do_savefile as FuncPtr, MMAIN,
        "Save", savefile_gist, BLANKAFTER);

    #[cfg(feature = "multibuffer")]
    {
        if !is_restricted && !is_view_mode {
            add_to_funcs(flip_newbuffer as FuncPtr, MINSERTFILE | MEXECUTE,
                "New Buffer", newbuffer_gist, TOGETHER);
        }
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(flip_pipe as FuncPtr, MEXECUTE,
            "Pipe Text", pipe_gist, BLANKAFTER);
    }
    #[cfg(feature = "speller")]
    {
        add_to_funcs(do_spell as FuncPtr, MEXECUTE,
            "Spell Check", spell_gist, TOGETHER);
    }
    #[cfg(feature = "linter")]
    {
        add_to_funcs(do_linter as FuncPtr, MEXECUTE,
            "Linter", lint_gist, BLANKAFTER);
    }
    #[cfg(feature = "justify")]
    {
        add_to_funcs(do_full_justify as FuncPtr, MEXECUTE,
            "Full Justify", fulljustify_gist, TOGETHER);
    }
    #[cfg(feature = "formatter")]
    {
        add_to_funcs(do_formatter as FuncPtr, MEXECUTE,
            "Formatter", formatter_gist, BLANKAFTER);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_funcs(dos_format as FuncPtr, MWRITEFILE,
            "DOS Format", dos_gist, TOGETHER);

        if !is_restricted {
            add_to_funcs(back_it_up as FuncPtr, MWRITEFILE,
                "Backup File", backup_gist, TOGETHER);
            add_to_funcs(append_it as FuncPtr, MWRITEFILE,
                "Append", append_gist, TOGETHER);
            add_to_funcs(prepend_it as FuncPtr, MWRITEFILE,
                "Prepend", prepend_gist, BLANKAFTER);
        }

        add_to_funcs(flip_convert as FuncPtr, MINSERTFILE,
            "No Conversion", convert_gist, BLANKAFTER);

        if !is_restricted && !is_view_mode {
            add_to_funcs(flip_execute as FuncPtr, MINSERTFILE,
                "Execute Command", execute_gist, BLANKAFTER);
        }

        add_to_funcs(cut_till_eof as FuncPtr, MEXECUTE,
            "Cut Till End", cuttilleof_gist, BLANKAFTER);

        add_to_funcs(do_suspend as FuncPtr, MEXECUTE,
            "Suspend", suspend_gist, BLANKAFTER);
    } // !NANO_TINY

    add_to_funcs(discard_buffer as FuncPtr, MWRITEFILE,
        "Discard buffer", discardbuffer_gist, BLANKAFTER);

    #[cfg(feature = "browser")]
    {
        if !is_restricted {
            add_to_funcs(to_files as FuncPtr, MWRITEFILE | MINSERTFILE,
                "Browse", tofiles_gist, BLANKAFTER);
        }
        add_to_funcs(do_page_up as FuncPtr, MBROWSER,
            "Prev Page", prevpage_gist, TOGETHER);
        add_to_funcs(do_page_down as FuncPtr, MBROWSER,
            "Next Page", nextpage_gist, TOGETHER);

        add_to_funcs(to_first_file as FuncPtr, MBROWSER | MWHEREISFILE,
            "First File", firstfile_gist, TOGETHER);
        add_to_funcs(to_last_file as FuncPtr, MBROWSER | MWHEREISFILE,
            "Last File", lastfile_gist, BLANKAFTER);

        #[cfg(not(feature = "tiny"))]
        {
            add_to_funcs(to_prev_word as FuncPtr, MBROWSER,
                "Left Column", browserlefthand_gist, TOGETHER);
            add_to_funcs(to_next_word as FuncPtr, MBROWSER,
                "Right Column", browserrighthand_gist, TOGETHER);
            add_to_funcs(to_prev_block as FuncPtr, MBROWSER,
                "Top Row", browsertoprow_gist, TOGETHER);
            add_to_funcs(to_next_block as FuncPtr, MBROWSER,
                "Bottom Row", browserbottomrow_gist, BLANKAFTER);
        }
    }

    #[cfg(feature = "linter")]
    {
        add_to_funcs(do_page_up as FuncPtr, MLINTER,
            "Previous Linter message", prevlint_gist, TOGETHER);
        add_to_funcs(do_page_down as FuncPtr, MLINTER,
            "Next Linter message", nextlint_gist, TOGETHER);
    }

    // ---- Link key combos to functions ----

    // On Linux console, use "^-" instead of "^/" for goto-line.
    // C: #ifdef __linux__
    //    #define SLASH_OR_DASH  (on_a_vt) ? "^-" : "^/"
    //    #else
    //    #define SLASH_OR_DASH  "^/"
    let on_a_vt_now = with_state(|s| s.on_a_vt);
    #[cfg(target_os = "linux")]
    let _slash_or_dash: &'static str = if on_a_vt_now { "^-" } else { "^/" };
    #[cfg(not(target_os = "linux"))]
    let slash_or_dash: &'static str = "^/";

    add_to_sclist(MMOST | MBROWSER, "^M", '\r' as i32, do_enter as FuncPtr, 0);
    add_to_sclist(MMOST | MBROWSER, "Enter", KEY_ENTER, do_enter as FuncPtr, 0);
    add_to_sclist(MMOST, "^I", '\t' as i32, do_tab as FuncPtr, 0);
    add_to_sclist(MMOST, "Tab", '\t' as i32, do_tab as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "^B", 0, do_search_backward as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "^F", 0, do_search_forward as FuncPtr, 0);

    let modern = with_state(|s| {
        (s.flags[flag_index(MODERN_BINDINGS)] & flag_mask(MODERN_BINDINGS)) != 0
    });

    if modern {
        add_to_sclist((MMOST | MBROWSER) & !MFINDINHELP, help_key, 0, do_help as FuncPtr, 0);
        add_to_sclist(MHELP, help_key, 0, do_exit as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^Q", 0, do_exit as FuncPtr, 0);
        add_to_sclist(MMAIN, "^S", 0, do_savefile as FuncPtr, 0);
        add_to_sclist(MMAIN, "^W", 0, do_writeout as FuncPtr, 0);
        add_to_sclist(MMAIN, "^O", 0, do_insertfile as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^D", 0, do_findprevious as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^G", 0, do_findnext as FuncPtr, 0);
        add_to_sclist(MMAIN, "^R", 0, do_replace as FuncPtr, 0);
        add_to_sclist(MMAIN, "^T", 0, do_gotolinecolumn as FuncPtr, 0);
        add_to_sclist(MMAIN, "^P", 0, report_cursor_position as FuncPtr, 0);
        #[cfg(not(feature = "tiny"))]
        {
            add_to_sclist(MMAIN, "^Z", 0, do_undo as FuncPtr, 0);
            add_to_sclist(MMAIN, "^Y", 0, do_redo as FuncPtr, 0);
            add_to_sclist(MMAIN, "^A", 0, do_mark as FuncPtr, 0);
        }
        add_to_sclist(MMAIN, "^X", 0, cut_text as FuncPtr, 0);
        add_to_sclist(MMAIN, "^C", 0, copy_text as FuncPtr, 0);
        add_to_sclist(MMAIN, "^V", 0, paste_text as FuncPtr, 0);
    } else {
        add_to_sclist((MMOST | MBROWSER) & !MFINDINHELP, "^G", 0, do_help as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^X", 0, do_exit as FuncPtr, 0);
        let preserve = with_state(|s| {
            (s.flags[flag_index(PRESERVE)] & flag_mask(PRESERVE)) != 0
        });
        if !preserve {
            add_to_sclist(MMAIN, "^S", 0, do_savefile as FuncPtr, 0);
        }
        add_to_sclist(MMAIN, "^O", 0, do_writeout as FuncPtr, 0);
        add_to_sclist(MMAIN, "^R", 0, do_insertfile as FuncPtr, 0);
        let preserve2 = with_state(|s| {
            (s.flags[flag_index(PRESERVE)] & flag_mask(PRESERVE)) != 0
        });
        if !preserve2 {
            add_to_sclist(MMAIN | MBROWSER | MHELP, "^Q", 0, do_search_backward as FuncPtr, 0);
        }
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^W", 0, do_search_forward as FuncPtr, 0);
        add_to_sclist(MMOST, "^A", 0, do_home as FuncPtr, 0);
        add_to_sclist(MMOST, "^E", 0, do_end as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^P", 0, do_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "^N", 0, do_down as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "^Y", 0, do_page_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "^V", 0, do_page_down as FuncPtr, 0);
        add_to_sclist(MMAIN, "^C", 0, report_cursor_position as FuncPtr, 0);
        add_to_sclist(MMOST, "^H", '\x08' as i32, do_backspace as FuncPtr, 0);
        add_to_sclist(MMOST, "^D", 0, do_delete as FuncPtr, 0);
    }

    add_to_sclist(MMOST, "Bsp", KEY_BACKSPACE, do_backspace as FuncPtr, 0);
    add_to_sclist(MMOST, "Sh-Del", SHIFT_DELETE as i32, do_backspace as FuncPtr, 0);
    add_to_sclist(MMOST, "Del", KEY_DC, do_delete as FuncPtr, 0);
    add_to_sclist(MMAIN, "Ins", KEY_IC, do_insertfile as FuncPtr, 0);
    add_to_sclist(MMAIN, "^\\", 0, do_replace as FuncPtr, 0);
    add_to_sclist(MMAIN, "M-R", 0, do_replace as FuncPtr, 0);
    add_to_sclist(MMOST, "^K", 0, cut_text as FuncPtr, 0);

    #[cfg(feature = "tiny")]
    {
        add_to_sclist(MMAIN, "M-6", 0, copy_text as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-^", 0, copy_text as FuncPtr, 0);
        add_to_sclist(MMAIN, "^U", 0, paste_text as FuncPtr, 0);
        #[cfg(feature = "speller")]
        {
            let execute_key = if modern { "^E" } else { "^T" };
            add_to_sclist(MMAIN, execute_key, 0, do_spell as FuncPtr, 0);
        }
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist(MMOST, "M-6", 0, copy_text as FuncPtr, 0);
        add_to_sclist(MMOST, "M-^", 0, copy_text as FuncPtr, 0);
        add_to_sclist(MMOST, "^U", 0, paste_text as FuncPtr, 0);
        let execute_key = if modern { "^E" } else { "^T" };
        add_to_sclist(MMAIN, execute_key, 0, do_execute as FuncPtr, 0);
        #[cfg(feature = "speller")]
        {
            let preserve3 = with_state(|s| {
                (s.flags[flag_index(PRESERVE)] & flag_mask(PRESERVE)) != 0
            });
            if !preserve3 {
                add_to_sclist(MEXECUTE, "^S", 0, do_spell as FuncPtr, 0);
            }
            add_to_sclist(MEXECUTE, "^T", 0, do_spell as FuncPtr, 0);
        }
    }

    #[cfg(feature = "justify")]
    {
        add_to_sclist(MMAIN, "^J", '\n' as i32, do_justify as FuncPtr, 0);
    }
    #[cfg(feature = "linter")]
    {
        add_to_sclist(MEXECUTE, "^Y", 0, do_linter as FuncPtr, 0);
    }
    #[cfg(feature = "formatter")]
    {
        add_to_sclist(MEXECUTE, "^O", 0, do_formatter as FuncPtr, 0);
    }

    // slash_or_dash is not 'static, so we need a trick.
    // In C, SLASH_OR_DASH is a macro that expands to a string literal.
    // We store the resolved value in the sclist entry.
    // Use 'static strings based on compile-time platform + runtime on_a_vt.
    #[cfg(target_os = "linux")]
    {
        if on_a_vt_now {
            add_to_sclist(MMAIN, "^-", 0, do_gotolinecolumn as FuncPtr, 0);
        } else {
            add_to_sclist(MMAIN, "^/", 0, do_gotolinecolumn as FuncPtr, 0);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        add_to_sclist(MMAIN, "^/", 0, do_gotolinecolumn as FuncPtr, 0);
    }
    add_to_sclist(MMAIN, "M-G", 0, do_gotolinecolumn as FuncPtr, 0);
    add_to_sclist(MMAIN, "^_", 0, do_gotolinecolumn as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "PgUp", KEY_PPAGE, do_page_up as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "PgDn", KEY_NPAGE, do_page_down as FuncPtr, 0);
    add_to_sclist(MBROWSER | MHELP, "Bsp", KEY_BACKSPACE, do_page_up as FuncPtr, 0);
    add_to_sclist(MBROWSER | MHELP, "Sh-Del", SHIFT_DELETE as i32, do_page_up as FuncPtr, 0);
    add_to_sclist(MBROWSER | MHELP, "Space", 0x20, do_page_down as FuncPtr, 0);
    add_to_sclist(MMAIN | MHELP, "M-\\", 0, to_first_line as FuncPtr, 0);
    add_to_sclist(MMAIN | MHELP, "^Home", CONTROL_HOME as i32, to_first_line as FuncPtr, 0);
    add_to_sclist(MMAIN | MHELP, "M-/", 0, to_last_line as FuncPtr, 0);
    add_to_sclist(MMAIN | MHELP, "^End", CONTROL_END as i32, to_last_line as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "M-B", 0, do_findprevious as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "M-F", 0, do_findnext as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "M-W", 0, do_findnext as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "M-Q", 0, do_findprevious as FuncPtr, 0);

    #[cfg(feature = "tiny")]
    {
        #[cfg(feature = "linenumbers")]
        {
            add_to_sclist(MMAIN, "M-N", 0, toggle_numbers as FuncPtr, 0);
        }
        #[cfg(not(feature = "linenumbers"))]
        {
            add_to_sclist(MMAIN, "M-N", 0, to_next_word as FuncPtr, 0);
        }
        add_to_sclist(MMAIN, "M-D", 0, to_prev_word as FuncPtr, 0);
    }
    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist(MMAIN, "M-]", 0, do_find_bracket as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-A", 0, do_mark as FuncPtr, 0);
        add_to_sclist(MMAIN, "^6", 0, do_mark as FuncPtr, 0);
        add_to_sclist(MMAIN, "^^", 0, do_mark as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-}", 0, do_indent as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-{", 0, do_unindent as FuncPtr, 0);
        add_to_sclist(MMAIN, "Sh-Tab", SHIFT_TAB as i32, do_unindent as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-:", 0, record_macro as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-;", 0, run_macro as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-U", 0, do_undo as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-E", 0, do_redo as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-Bsp", CONTROL_SHIFT_DELETE as i32, chop_previous_word as FuncPtr, 0);
        add_to_sclist(MMAIN, "Sh-^Del", CONTROL_SHIFT_DELETE as i32, chop_previous_word as FuncPtr, 0);
        add_to_sclist(MMAIN, "^Del", CONTROL_DELETE as i32, chop_next_word as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-Del", ALT_DELETE as i32, zap_text as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-Ins", ALT_INSERT as i32, put_or_lift_anchor as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-Home", ALT_HOME as i32, to_top_row as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-End", ALT_END as i32, to_bottom_row as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-PgUp", ALT_PAGEUP as i32, to_prev_anchor as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-PgDn", ALT_PAGEDOWN as i32, to_next_anchor as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-\"", 0, put_or_lift_anchor as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-'", 0, to_next_anchor as FuncPtr, 0);
    }

    #[cfg(feature = "wordcomp")]
    {
        add_to_sclist(MMAIN, "^]", 0, complete_a_word as FuncPtr, 0);
    }
    #[cfg(feature = "comment")]
    {
        add_to_sclist(MMAIN, "M-3", 0, do_comment as FuncPtr, 0);
    }

    add_to_sclist(MMOST & !MMAIN, "^B", 0, do_left as FuncPtr, 0);
    add_to_sclist(MMOST & !MMAIN, "^F", 0, do_right as FuncPtr, 0);

    // UTF-8 arrow symbols vs plain arrow key names
    let use_utf8 = with_state(|s| s.using_utf8);
    #[cfg(feature = "utf8")]
    if use_utf8 {
        // U+25C2 BLACK LEFT-POINTING SMALL TRIANGLE  = "\u{25c2}" = "\xE2\x97\x82"
        // U+25B8 BLACK RIGHT-POINTING SMALL TRIANGLE = "\u{25b8}" = "\xE2\x96\xb8"
        add_to_sclist(MMOST | MBROWSER | MHELP, "◂", KEY_LEFT, do_left as FuncPtr, 0);
        add_to_sclist(MMOST | MBROWSER | MHELP, "▸", KEY_RIGHT, do_right as FuncPtr, 0);
        add_to_sclist(MSOME, "^◂", CONTROL_LEFT as i32, to_prev_word as FuncPtr, 0);
        add_to_sclist(MSOME, "^▸", CONTROL_RIGHT as i32, to_next_word as FuncPtr, 0);
        #[cfg(all(feature = "multibuffer", not(feature = "tiny")))]
        {
            if !on_a_vt_now {
                add_to_sclist(MMAIN, "M-◂", ALT_LEFT as i32, switch_to_prev_buffer as FuncPtr, 0);
                add_to_sclist(MMAIN, "M-▸", ALT_RIGHT as i32, switch_to_next_buffer as FuncPtr, 0);
            }
        }
    } else {
        add_to_sclist(MMOST | MBROWSER | MHELP, "Left", KEY_LEFT, do_left as FuncPtr, 0);
        add_to_sclist(MMOST | MBROWSER | MHELP, "Right", KEY_RIGHT, do_right as FuncPtr, 0);
        add_to_sclist(MSOME, "^Left", CONTROL_LEFT as i32, to_prev_word as FuncPtr, 0);
        add_to_sclist(MSOME, "^Right", CONTROL_RIGHT as i32, to_next_word as FuncPtr, 0);
        #[cfg(all(feature = "multibuffer", not(feature = "tiny")))]
        {
            if !on_a_vt_now {
                add_to_sclist(MMAIN, "M-Left", ALT_LEFT as i32, switch_to_prev_buffer as FuncPtr, 0);
                add_to_sclist(MMAIN, "M-Right", ALT_RIGHT as i32, switch_to_next_buffer as FuncPtr, 0);
            }
        }
    }
    #[cfg(not(feature = "utf8"))]
    {
        add_to_sclist(MMOST | MBROWSER | MHELP, "Left", KEY_LEFT, do_left as FuncPtr, 0);
        add_to_sclist(MMOST | MBROWSER | MHELP, "Right", KEY_RIGHT, do_right as FuncPtr, 0);
        add_to_sclist(MSOME, "^Left", CONTROL_LEFT as i32, to_prev_word as FuncPtr, 0);
        add_to_sclist(MSOME, "^Right", CONTROL_RIGHT as i32, to_next_word as FuncPtr, 0);
        #[cfg(all(feature = "multibuffer", not(feature = "tiny")))]
        {
            if !on_a_vt_now {
                add_to_sclist(MMAIN, "M-Left", ALT_LEFT as i32, switch_to_prev_buffer as FuncPtr, 0);
                add_to_sclist(MMAIN, "M-Right", ALT_RIGHT as i32, switch_to_next_buffer as FuncPtr, 0);
            }
        }
    }

    add_to_sclist(MMOST, "M-Space", 0, to_prev_word as FuncPtr, 0);
    add_to_sclist(MMOST, "^Space", 0, to_next_word as FuncPtr, 0);
    add_to_sclist(MMOST, "Home", KEY_HOME, do_home as FuncPtr, 0);
    add_to_sclist(MMOST, "End", KEY_END, do_end as FuncPtr, 0);

    #[cfg(feature = "utf8")]
    if use_utf8 {
        // U+25B4 BLACK UP-POINTING SMALL TRIANGLE   = "\u{25b4}" = "\xE2\x96\xb4"
        // U+25BE BLACK DOWN-POINTING SMALL TRIANGLE = "\u{25be}" = "\xE2\x96\xbe"
        add_to_sclist(MMAIN | MBROWSER | MHELP, "▴", KEY_UP, do_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "▾", KEY_DOWN, do_down as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^▴", CONTROL_UP as i32, to_prev_block as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^▾", CONTROL_DOWN as i32, to_next_block as FuncPtr, 0);
    } else {
        add_to_sclist(MMAIN | MBROWSER | MHELP, "Up", KEY_UP, do_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "Down", KEY_DOWN, do_down as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^Up", CONTROL_UP as i32, to_prev_block as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^Down", CONTROL_DOWN as i32, to_next_block as FuncPtr, 0);
    }
    #[cfg(not(feature = "utf8"))]
    {
        add_to_sclist(MMAIN | MBROWSER | MHELP, "Up", KEY_UP, do_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MHELP, "Down", KEY_DOWN, do_down as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^Up", CONTROL_UP as i32, to_prev_block as FuncPtr, 0);
        add_to_sclist(MMAIN | MBROWSER | MLINTER, "^Down", CONTROL_DOWN as i32, to_next_block as FuncPtr, 0);
    }

    add_to_sclist(MMAIN, "M-7", 0, to_prev_block as FuncPtr, 0);
    add_to_sclist(MMAIN, "M-8", 0, to_next_block as FuncPtr, 0);
    #[cfg(feature = "justify")]
    {
        add_to_sclist(MMAIN, "M-(", 0, to_para_begin as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-9", 0, to_para_begin as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-)", 0, to_para_end as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-0", 0, to_para_end as FuncPtr, 0);
    }

    #[cfg(not(feature = "tiny"))]
    {
        #[cfg(feature = "utf8")]
        if use_utf8 {
            add_to_sclist(MMAIN | MHELP, "M-▴", ALT_UP as i32, do_scroll_up as FuncPtr, 0);
            add_to_sclist(MMAIN | MHELP, "M-▾", ALT_DOWN as i32, do_scroll_down as FuncPtr, 0);
        } else {
            add_to_sclist(MMAIN | MHELP, "M-Up", ALT_UP as i32, do_scroll_up as FuncPtr, 0);
            add_to_sclist(MMAIN | MHELP, "M-Down", ALT_DOWN as i32, do_scroll_down as FuncPtr, 0);
        }
        #[cfg(not(feature = "utf8"))]
        {
            add_to_sclist(MMAIN | MHELP, "M-Up", ALT_UP as i32, do_scroll_up as FuncPtr, 0);
            add_to_sclist(MMAIN | MHELP, "M-Down", ALT_DOWN as i32, do_scroll_down as FuncPtr, 0);
        }
    }

    #[cfg(any(not(feature = "tiny"), feature = "help"))]
    {
        add_to_sclist(MMAIN | MHELP, "M--", 0, do_scroll_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MHELP, "M-_", 0, do_scroll_up as FuncPtr, 0);
        add_to_sclist(MMAIN | MHELP, "M-+", 0, do_scroll_down as FuncPtr, 0);
        add_to_sclist(MMAIN | MHELP, "M-=", 0, do_scroll_down as FuncPtr, 0);
    }

    #[cfg(feature = "multibuffer")]
    {
        add_to_sclist(MMAIN, "M-,", 0, switch_to_prev_buffer as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-.", 0, switch_to_next_buffer as FuncPtr, 0);
    }

    add_to_sclist(MMOST, "M-V", 0, do_verbatim_input as FuncPtr, 0);

    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist(MMAIN, "M-T", 0, cut_till_eof as FuncPtr, 0);
        add_to_sclist(MEXECUTE, "^V", 0, cut_till_eof as FuncPtr, 0);
        add_to_sclist(MEXECUTE, "^Z", 0, do_suspend as FuncPtr, 0);
        add_to_sclist(MMAIN, "^Z", 0, suggest_ctrlT_ctrlZ as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-D", 0, count_lines_words_and_characters as FuncPtr, 0);
    }
    #[cfg(feature = "tiny")]
    {
        add_to_sclist(MMAIN, "M-H", 0, do_help as FuncPtr, 0);
    }

    #[cfg(feature = "justify")]
    {
        add_to_sclist(MMAIN, "M-J", 0, do_full_justify as FuncPtr, 0);
        add_to_sclist(MEXECUTE, "^J", 0, do_full_justify as FuncPtr, 0);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist(MMAIN, "M-<", 0, do_scroll_left as FuncPtr, 0);
        add_to_sclist(MMAIN, "M->", 0, do_scroll_right as FuncPtr, 0);
        add_to_sclist(MMAIN, "^L", 0, do_center as FuncPtr, 0);
        add_to_sclist(MMAIN, "M-%", 0, do_cycle as FuncPtr, 0);
        add_to_sclist((MMOST | MBROWSER | MHELP | MYESNO) & !MMAIN, "^L", 0, full_refresh as FuncPtr, 0);
    }

    // C: #if defined(ENABLE_EXTRA) && defined(NCURSES_VERSION_PATCH)
    #[cfg(feature = "extra")]
    add_to_sclist(MMAIN, "M-&", 0, show_curses_version as FuncPtr, 0);
    #[cfg(feature = "tiny")]
    {
        add_to_sclist(MMOST | MBROWSER | MHELP | MYESNO, "^L", 0, full_refresh as FuncPtr, 0);
    }

    // Toggles (not NANO_TINY)
    #[cfg(not(feature = "tiny"))]
    {
        // Group of "Appearance" toggles.
        add_to_sclist(MMAIN, "M-Z", 0, do_toggle as FuncPtr, ZERO as i32);
        add_to_sclist((MMOST | MBROWSER | MYESNO) & !MFINDINHELP, "M-X", 0, do_toggle as FuncPtr, NO_HELP as i32);
        add_to_sclist(MMAIN, "M-C", 0, do_toggle as FuncPtr, CONSTANT_SHOW as i32);
        add_to_sclist(MMAIN, "M-S", 0, do_toggle as FuncPtr, SOFTWRAP as i32);
        add_to_sclist(MMAIN, "M-$", 0, do_toggle as FuncPtr, SOFTWRAP as i32);
        #[cfg(feature = "linenumbers")]
        {
            add_to_sclist(MMAIN, "M-N", 0, do_toggle as FuncPtr, LINE_NUMBERS as i32);
            add_to_sclist(MMAIN, "M-#", 0, do_toggle as FuncPtr, LINE_NUMBERS as i32);
        }
        add_to_sclist(MMAIN, "M-P", 0, do_toggle as FuncPtr, WHITESPACE_DISPLAY as i32);
        #[cfg(feature = "color")]
        {
            add_to_sclist(MMAIN, "M-Y", 0, do_toggle as FuncPtr, NO_SYNTAX as i32);
        }

        // Group of "Behavior" toggles.
        add_to_sclist(MMAIN, "M-H", 0, do_toggle as FuncPtr, SMART_HOME as i32);
        add_to_sclist(MMAIN, "M-I", 0, do_toggle as FuncPtr, AUTOINDENT as i32);
        add_to_sclist(MMAIN, "M-K", 0, do_toggle as FuncPtr, CUT_FROM_CURSOR as i32);
        #[cfg(feature = "wrapping")]
        {
            add_to_sclist(MMAIN, "M-L", 0, do_toggle as FuncPtr, BREAK_LONG_LINES as i32);
        }
        add_to_sclist(MMAIN, "M-O", 0, do_toggle as FuncPtr, TABS_TO_SPACES as i32);
        #[cfg(feature = "mouse")]
        {
            add_to_sclist(MMAIN, "M-M", 0, do_toggle as FuncPtr, USE_MOUSE as i32);
        }
    } // !NANO_TINY

    add_to_sclist((MMOST & !MMAIN) | MYESNO, "^C", 0, do_cancel as FuncPtr, 0);

    add_to_sclist(MWHEREIS | MREPLACE, "M-C", 0, case_sens_void as FuncPtr, 0);
    add_to_sclist(MWHEREIS | MREPLACE, "M-R", 0, regexp_void as FuncPtr, 0);
    add_to_sclist(MWHEREIS | MREPLACE, "M-B", 0, backwards_void as FuncPtr, 0);
    add_to_sclist(MWHEREIS | MREPLACE, "^R", 0, flip_replace as FuncPtr, 0);

    #[cfg(feature = "histories")]
    {
        add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "^P", 0, get_older_item as FuncPtr, 0);
        add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "^N", 0, get_newer_item as FuncPtr, 0);
        #[cfg(feature = "utf8")]
        if use_utf8 {
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "▴", KEY_UP, get_older_item as FuncPtr, 0);
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "▾", KEY_DOWN, get_newer_item as FuncPtr, 0);
        } else {
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "Up", KEY_UP, get_older_item as FuncPtr, 0);
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "Down", KEY_DOWN, get_newer_item as FuncPtr, 0);
        }
        #[cfg(not(feature = "utf8"))]
        {
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "Up", KEY_UP, get_older_item as FuncPtr, 0);
            add_to_sclist(MWHEREIS|MREPLACE|MREPLACEWITH|MWHEREISFILE|MFINDINHELP|MEXECUTE, "Down", KEY_DOWN, get_newer_item as FuncPtr, 0);
        }
    }

    #[cfg(feature = "justify")]
    {
        add_to_sclist(MGOTOLINE, "^W", 0, to_para_begin as FuncPtr, 0);
        add_to_sclist(MGOTOLINE, "^O", 0, to_para_end as FuncPtr, 0);
    }
    add_to_sclist(MGOTOLINE | MWHEREIS | MFINDINHELP, "^Y", 0, to_first_line as FuncPtr, 0);
    add_to_sclist(MGOTOLINE | MWHEREIS | MFINDINHELP, "^V", 0, to_last_line as FuncPtr, 0);

    #[cfg(feature = "browser")]
    {
        add_to_sclist(MWHEREISFILE, "^Y", 0, to_first_file as FuncPtr, 0);
        add_to_sclist(MWHEREISFILE, "^V", 0, to_last_file as FuncPtr, 0);
        add_to_sclist(MBROWSER | MWHEREISFILE, "M-\\", 0, to_first_file as FuncPtr, 0);
        add_to_sclist(MBROWSER | MWHEREISFILE, "M-/", 0, to_last_file as FuncPtr, 0);
        add_to_sclist(MBROWSER, "Home", KEY_HOME, to_first_file as FuncPtr, 0);
        add_to_sclist(MBROWSER, "End", KEY_END, to_last_file as FuncPtr, 0);
        add_to_sclist(MBROWSER, "^Home", CONTROL_HOME as i32, to_first_file as FuncPtr, 0);
        add_to_sclist(MBROWSER, "^End", CONTROL_END as i32, to_last_file as FuncPtr, 0);
        #[cfg(target_os = "linux")]
        {
            if on_a_vt_now {
                add_to_sclist(MBROWSER, "^-", 0, goto_dir as FuncPtr, 0);
            } else {
                add_to_sclist(MBROWSER, "^/", 0, goto_dir as FuncPtr, 0);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            add_to_sclist(MBROWSER, "^/", 0, goto_dir as FuncPtr, 0);
        }
        add_to_sclist(MBROWSER, "M-G", 0, goto_dir as FuncPtr, 0);
        add_to_sclist(MBROWSER, "^_", 0, goto_dir as FuncPtr, 0);
    }

    let preserve4 = with_state(|s| {
        (s.flags[flag_index(PRESERVE)] & flag_mask(PRESERVE)) != 0
    });
    if !preserve4 {
        add_to_sclist(MWRITEFILE, "^Q", 0, discard_buffer as FuncPtr, 0);
    }

    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist(MWRITEFILE, "M-D", 0, dos_format as FuncPtr, 0);
        if !is_restricted && !is_view_mode {
            add_to_sclist(MWRITEFILE, "M-B", 0, back_it_up as FuncPtr, 0);
            add_to_sclist(MWRITEFILE, "M-A", 0, append_it as FuncPtr, 0);
            add_to_sclist(MWRITEFILE, "M-P", 0, prepend_it as FuncPtr, 0);
            add_to_sclist(MINSERTFILE | MEXECUTE, "^X", 0, flip_execute as FuncPtr, 0);
        }
        add_to_sclist(MINSERTFILE, "M-N", 0, flip_convert as FuncPtr, 0);
    }

    #[cfg(feature = "multibuffer")]
    {
        if !is_restricted && !is_view_mode {
            add_to_sclist(MINSERTFILE | MEXECUTE, "M-F", 0, flip_newbuffer as FuncPtr, 0);
            #[cfg(not(feature = "tiny"))]
            {
                add_to_sclist(MEXECUTE, "M-\\", 0, flip_pipe as FuncPtr, 0);
            }
        }
    }

    add_to_sclist(MBROWSER | MHELP, "^C", 0, do_exit as FuncPtr, 0);

    #[cfg(feature = "browser")]
    {
        if !is_restricted {
            add_to_sclist(MWRITEFILE | MINSERTFILE, "^T", 0, to_files as FuncPtr, 0);
        }
        add_to_sclist(MBROWSER, "^T", 0, do_exit as FuncPtr, 0);
    }

    #[cfg(feature = "help")]
    {
        add_to_sclist(MHELP, "^G", 0, do_exit as FuncPtr, 0);
        add_to_sclist(MHELP, "F1", key_f(1), do_exit as FuncPtr, 0);
        add_to_sclist(MHELP, "Home", KEY_HOME, to_first_line as FuncPtr, 0);
        add_to_sclist(MHELP, "End", KEY_END, to_last_line as FuncPtr, 0);
    }

    #[cfg(feature = "linter")]
    {
        add_to_sclist(MLINTER, "^X", 0, do_cancel as FuncPtr, 0);
    }

    add_to_sclist(MMOST & !MFINDINHELP, "F1", key_f(1), do_help as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "F2", key_f(2), do_exit as FuncPtr, 0);
    add_to_sclist(MMAIN, "F3", key_f(3), do_writeout as FuncPtr, 0);
    #[cfg(feature = "justify")]
    {
        add_to_sclist(MMAIN, "F4", key_f(4), do_justify as FuncPtr, 0);
    }
    add_to_sclist(MMAIN, "F5", key_f(5), do_insertfile as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP, "F6", key_f(6), do_search_forward as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "F7", key_f(7), do_page_up as FuncPtr, 0);
    add_to_sclist(MMAIN | MBROWSER | MHELP | MLINTER, "F8", key_f(8), do_page_down as FuncPtr, 0);
    add_to_sclist(MMOST, "F9", key_f(9), cut_text as FuncPtr, 0);
    add_to_sclist(MMOST, "F10", key_f(10), paste_text as FuncPtr, 0);
    add_to_sclist(MMAIN, "F11", key_f(11), report_cursor_position as FuncPtr, 0);
    #[cfg(feature = "speller")]
    {
        add_to_sclist(MMAIN, "F12", key_f(12), do_spell as FuncPtr, 0);
    }

    // #if defined(ENABLE_EXTRA) && defined(NCURSES_VERSION_PATCH)
    // show_curses_version binding omitted — no "extra" feature in Cargo.toml

    #[cfg(not(feature = "tiny"))]
    {
        add_to_sclist((MMOST & !MMAIN) | MYESNO, "", KEY_CANCEL, do_cancel as FuncPtr, 0);
        add_to_sclist(MMAIN, "", KEY_CENTER as i32, do_center as FuncPtr, 0);
        add_to_sclist(MMAIN, "", KEY_SIC, do_insertfile as FuncPtr, 0);
        add_to_sclist(MMAIN, "", START_OF_PASTE as i32, suck_up_input_and_paste_it as FuncPtr, 0);
        add_to_sclist(MMOST, "", START_OF_PASTE as i32, do_nothing as FuncPtr, 0);
        add_to_sclist(MMOST, "", END_OF_PASTE as i32, do_nothing as FuncPtr, 0);
    }
    #[cfg(feature = "tiny")]
    {
        add_to_sclist(MMOST | MBROWSER | MHELP | MYESNO, "", KEY_FRESH, full_refresh as FuncPtr, 0);
    }
}
// End of shortcut_init
