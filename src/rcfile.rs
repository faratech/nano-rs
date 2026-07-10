#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/rcfile.c from GNU nano.
// C original: Copyright (C) 2001-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014 Mike Frysinger
//             Copyright (C) 2019 Brand Huntsman
//             Copyright (C) 2014-2021, 2024 Benno Schulenberg

use crate::definitions::*;
use crate::global::{state, state_mut, with_state, with_state_mut};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::{ISSET, SET, UNSET};
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[cfg(feature = "color")]
use regex_lite::{Regex, RegexBuilder};

// ---------------------------------------------------------------------------
// Module-level statics (thread_local replacements for C file-scope statics)
// ---------------------------------------------------------------------------

// The current line number being parsed (C: static size_t lineno).
thread_local! {
    static LINENO: RefCell<usize> = RefCell::new(0);
    /// The path to the rcfile being parsed (C: static char *nanorc).
    static NANORC: RefCell<Option<String>> = RefCell::new(None);
    /// Whether we are allowed to add to the last syntax (C: static bool opensyntax).
    #[cfg(feature = "color")]
    static OPENSYNTAX: RefCell<bool> = RefCell::new(false);
    /// Whether a syntax definition contains any color commands (C: static bool seen_color_command).
    #[cfg(feature = "color")]
    static SEEN_COLOR_COMMAND: RefCell<bool> = RefCell::new(false);
    // NOTE: LIVE_SYNTAX is not used — we always work on STATE.syntaxes head directly.
    // C: static syntaxtype *live_syntax is replaced by STATE.syntaxes (head pointer).
    /// Errors gathered during parsing (C: static linestruct *errors_head/tail).
    static ERROR_LIST: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

// ---------------------------------------------------------------------------
// Helper: access the thread-locals
// ---------------------------------------------------------------------------

fn get_lineno() -> usize {
    LINENO.with(|l| *l.borrow())
}

fn set_lineno(n: usize) {
    LINENO.with(|l| *l.borrow_mut() = n);
}

fn get_nanorc() -> Option<String> {
    NANORC.with(|n| n.borrow().clone())
}

fn set_nanorc(s: Option<String>) {
    NANORC.with(|n| *n.borrow_mut() = s);
}

#[cfg(feature = "color")]
fn get_opensyntax() -> bool {
    OPENSYNTAX.with(|o| *o.borrow())
}

#[cfg(feature = "color")]
fn set_opensyntax(v: bool) {
    OPENSYNTAX.with(|o| *o.borrow_mut() = v);
}

#[cfg(feature = "color")]
fn get_seen_color_command() -> bool {
    SEEN_COLOR_COMMAND.with(|s| *s.borrow())
}

#[cfg(feature = "color")]
fn set_seen_color_command(v: bool) {
    SEEN_COLOR_COMMAND.with(|s| *s.borrow_mut() = v);
}

// ---------------------------------------------------------------------------
// Constants (from rcfile.c / nanorc.h)
// ---------------------------------------------------------------------------

pub const HOME_RC_NAME: &str = ".nanorc";
pub const RCFILE_NAME: &str = "nanorc";

// ncurses attribute bit constants (matching winio.rs local values)
pub const A_NORMAL: i32 = 0;
pub const A_BOLD:   i32 = 0x0200_0000;
pub const A_ITALIC: i32 = 0x0008_0000;

// Terminal color indices (matching ncurses COLOR_* constants)
pub const COLOR_BLACK:   i16 = 0;
pub const COLOR_RED:     i16 = 1;
pub const COLOR_GREEN:   i16 = 2;
pub const COLOR_YELLOW:  i16 = 3;
pub const COLOR_BLUE:    i16 = 4;
pub const COLOR_MAGENTA: i16 = 5;
pub const COLOR_CYAN:    i16 = 6;
pub const COLOR_WHITE:   i16 = 7;

/// Number of indexed colors the current terminal can actually display.
fn terminal_colors() -> i16 {
    if let Some(level) = supports_color::on_cached(supports_color::Stream::Stdout) {
        if level.has_256 || level.has_16m { 256 } else if level.has_basic { 8 } else { 0 }
    } else {
        // `supports-color` intentionally suppresses its answer for NO_COLOR,
        // but explicit nanorc colors override NO_COLOR in GNU nano.  Retain the
        // underlying indexed capability for that explicit-configuration case.
        let term = std::env::var("TERM").unwrap_or_default();
        if term == "dumb" { 0 } else if term.contains("256color") { 256 } else { 8 }
    }
}

/// SYSCONFDIR — where the system-wide nanorc lives.
const SYSCONFDIR: &str = "/etc";

// ---------------------------------------------------------------------------
// Option table (C: rcoption rcopts[])
// ---------------------------------------------------------------------------

/// C: typedef struct rcoption { const char *name; long flag; } rcoption;
/// flag==0 means the option takes an argument (not a pure flag bit).
struct RcOpt {
    name: &'static str,
    flag: u32,  // 0 = "takes an argument"
}

/// C: static const rcoption rcopts[]
static RCOPTS: &[RcOpt] = &[
    RcOpt { name: "boldtext",              flag: BOLD_TEXT },
    #[cfg(feature = "justify")]
    RcOpt { name: "brackets",              flag: 0 },
    #[cfg(feature = "wrapping")]
    RcOpt { name: "breaklonglines",        flag: BREAK_LONG_LINES },
    RcOpt { name: "casesensitive",         flag: CASE_SENSITIVE },
    RcOpt { name: "constantshow",          flag: CONSTANT_SHOW },
    // "fill" is ENABLED_WRAPORJUSTIFY — enabled when wrapping OR justify
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    RcOpt { name: "fill",                  flag: 0 },
    #[cfg(feature = "histories")]
    RcOpt { name: "historylog",            flag: HISTORYLOG },
    #[cfg(feature = "linenumbers")]
    RcOpt { name: "linenumbers",           flag: LINE_NUMBERS },
    #[cfg(feature = "libmagic")]
    RcOpt { name: "magic",                  flag: USE_MAGIC },
    #[cfg(feature = "mouse")]
    RcOpt { name: "mouse",                 flag: USE_MOUSE },
    #[cfg(feature = "multibuffer")]
    RcOpt { name: "multibuffer",           flag: MULTIBUFFER },
    RcOpt { name: "nohelp",               flag: NO_HELP },
    RcOpt { name: "nonewlines",           flag: NO_NEWLINES },
    #[cfg(feature = "wrapping")]
    RcOpt { name: "nowrap",               flag: NO_WRAP },
    #[cfg(feature = "operatingdir")]
    RcOpt { name: "operatingdir",         flag: 0 },
    #[cfg(feature = "histories")]
    RcOpt { name: "positionlog",          flag: POSITIONLOG },
    RcOpt { name: "preserve",             flag: PRESERVE },
    #[cfg(feature = "justify")]
    RcOpt { name: "punct",                flag: 0 },
    #[cfg(feature = "justify")]
    RcOpt { name: "quotestr",             flag: 0 },
    RcOpt { name: "quickblank",           flag: QUICK_BLANK },
    RcOpt { name: "rawsequences",         flag: RAW_SEQUENCES },
    RcOpt { name: "rebinddelete",         flag: REBIND_DELETE },
    RcOpt { name: "regexp",               flag: USE_REGEXP },
    RcOpt { name: "saveonexit",           flag: SAVE_ON_EXIT },
    #[cfg(feature = "speller")]
    RcOpt { name: "speller",              flag: 0 },
    // Non-tiny options:
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "afterends",            flag: AFTER_ENDS },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "allow_insecure_backup", flag: INSECURE_BACKUP },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "atblanks",             flag: AT_BLANKS },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "autoindent",           flag: AUTOINDENT },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "backup",               flag: MAKE_BACKUP },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "backupdir",            flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "bookstyle",            flag: BOOKSTYLE },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "colonparsing",         flag: COLON_PARSING },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "cutfromcursor",        flag: CUT_FROM_CURSOR },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "emptyline",            flag: EMPTY_LINE },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "guidestripe",          flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "indicator",            flag: INDICATOR },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "jumpyscrolling",       flag: JUMPY_SCROLLING },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "locking",              flag: LOCKING },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "matchbrackets",        flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "minibar",              flag: MINIBAR },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "noconvert",            flag: NO_CONVERT },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "showcursor",           flag: SHOW_CURSOR },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "smarthome",            flag: SMART_HOME },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "softwrap",             flag: SOFTWRAP },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "solosidescroll",       flag: SOLO_SIDESCROLL },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "stateflags",           flag: STATEFLAGS },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "tabsize",              flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "tabstospaces",         flag: TABS_TO_SPACES },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "trimblanks",           flag: TRIM_BLANKS },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "unix",                 flag: MAKE_IT_UNIX },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "whitespace",           flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "whitespacedisplay",    flag: WHITESPACE_DISPLAY },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "wordbounds",           flag: WORD_BOUNDS },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "wordchars",            flag: 0 },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "zap",                  flag: LET_THEM_ZAP },
    #[cfg(not(feature = "tiny"))]
    RcOpt { name: "zero",                 flag: ZERO },
    // Color interface options:
    #[cfg(feature = "color")]
    RcOpt { name: "titlecolor",           flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "numbercolor",          flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "stripecolor",          flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "scrollercolor",        flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "selectedcolor",        flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "spotlightcolor",       flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "minicolor",            flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "promptcolor",          flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "statuscolor",          flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "errorcolor",           flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "keycolor",             flag: 0 },
    #[cfg(feature = "color")]
    RcOpt { name: "functioncolor",        flag: 0 },
];

// ---------------------------------------------------------------------------
// Menu name / symbol tables
// ---------------------------------------------------------------------------

/* C: #define NUMBER_OF_MENUS  16 */
const NUMBER_OF_MENUS: usize = 16;

/* C: char *menunames[NUMBER_OF_MENUS] */
const MENUNAMES: [&str; NUMBER_OF_MENUS] = [
    "main", "search", "replace", "replacewith",
    "yesno", "gotoline", "writeout", "insert",
    "execute", "help", "spell", "linter",
    "browser", "whereisfile", "gotodir",
    "all",
];

/* C: int menusymbols[NUMBER_OF_MENUS] */
const MENUSYMBOLS: [u32; NUMBER_OF_MENUS] = [
    MMAIN, MWHEREIS, MREPLACE, MREPLACEWITH,
    MYESNO, MGOTOLINE, MWRITEFILE, MINSERTFILE,
    MEXECUTE, MHELP, MSPELL, MLINTER,
    MBROWSER, MWHEREISFILE, MGOTODIR,
    MMOST | MBROWSER | MHELP | MYESNO,
];

// ---------------------------------------------------------------------------
// Error reporting
// ---------------------------------------------------------------------------

/* C: void display_rcfile_errors(void) */
/// Print all gathered errors to stderr.
pub fn display_rcfile_errors() {
    ERROR_LIST.with(|el| {
        for msg in el.borrow().iter() {
            eprintln!("{}", msg);
        }
    });
}

/* C: void jot_error(const char *msg, ...) */
/// Record a parse error.  Uses the current LINENO/NANORC globals.
pub fn jot_error(msg: &str) {
    // If startup_problem not yet set, set it
    #[cfg(any(feature = "nanorc", feature = "histories"))]
    with_state_mut(|s| {
        if s.startup_problem.is_none() {
            #[cfg(feature = "nanorc")]
            {
                if let Some(ref rc) = NANORC.with(|n| n.borrow().clone()) {
                    s.startup_problem = Some(format!("Mistakes in '{}'", rc));
                } else {
                    s.startup_problem = Some("Problems with history file".to_string());
                }
            }
            #[cfg(all(not(feature = "nanorc"), feature = "histories"))]
            {
                s.startup_problem = Some("Problems with history file".to_string());
            }
        }
    });

    let lineno = get_lineno();
    let full_msg = if lineno > 0 {
        if let Some(ref rc) = get_nanorc() {
            format!("Error in {} on line {}: {}", rc, lineno, msg)
        } else {
            msg.to_string()
        }
    } else {
        msg.to_string()
    };

    ERROR_LIST.with(|el| {
        el.borrow_mut().push(full_msg);
    });
}

// ---------------------------------------------------------------------------
// die — fatal error (stub; mirrors C die())
// ---------------------------------------------------------------------------

/* C: void die(const char *msg, ...) */
fn die(msg: &str) -> ! {
    display_rcfile_errors();
    eprintln!("{}", msg);
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// String parsing helpers
// ---------------------------------------------------------------------------

/* C: char *parse_next_word(char *ptr)
 * Skip a non-blank token then advance past blanks.
 * Returns (token, rest_of_line). */
fn parse_next_word(s: &str) -> (&str, &str) {
    // Find end of current token
    let token_end = s.find(|c: char| c.is_ascii_whitespace()).unwrap_or(s.len());
    let token = &s[..token_end];
    // Skip blanks
    let rest = s[token_end..].trim_start_matches(|c: char| c == ' ' || c == '\t');
    (token, rest)
}

/* C: char *parse_argument(char *ptr)
 * Parse an argument, optionally enclosed in double quotes.
 * Returns None on unterminated quote error, else Some((arg, rest)). */
fn parse_argument_with_quote<'a>(ptr: &'a str) -> Option<(&'a str, &'a str, bool)> {
    if !ptr.starts_with('"') {
        let (token, rest) = parse_next_word(ptr);
        return Some((token, rest, false));
    }

    // Find the last '"' in the string
    let last_quote = ptr.rfind('"');
    match last_quote {
        None | Some(0) => {
            jot_error(&format!("Argument '{}' has an unterminated \"", ptr));
            None
        }
        Some(pos) => {
            let arg = &ptr[1..pos];
            let rest = ptr[pos + 1..].trim_start_matches(|c: char| c == ' ' || c == '\t');
            Some((arg, rest, true))
        }
    }
}

fn parse_argument<'a>(ptr: &'a str) -> Option<(&'a str, &'a str)> {
    parse_argument_with_quote(ptr).map(|(arg, rest, _)| (arg, rest))
}

#[cfg(feature = "color")]
/* C: char *parse_next_regex(char *ptr)
 * Advance over one regex string (delimited by the preceding '"').
 * Returns None on error, else Some((regex_str, rest)). */
fn parse_next_regex<'a>(ptr: &'a str) -> Option<(&'a str, &'a str)> {
    if ptr.is_empty() {
        jot_error("Regex strings must begin and end with a \" character");
        return None;
    }

    // Find closing quote, but it must be at end-of-string or followed by whitespace
    let mut end = None;
    let bytes = ptr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Check: followed by end or blank
            if i + 1 >= bytes.len() || (bytes[i + 1] == b' ' || bytes[i + 1] == b'\t') {
                end = Some(i);
                break;
            }
        }
        i += 1;
    }

    match end {
        None => {
            jot_error("Regex strings must begin and end with a \" character");
            None
        }
        Some(0) => {
            jot_error("Empty regex string");
            None
        }
        Some(pos) => {
            let regex_str = &ptr[..pos];
            let rest = ptr[pos + 1..].trim_start_matches(|c: char| c == ' ' || c == '\t');
            Some((regex_str, rest))
        }
    }
}

// ---------------------------------------------------------------------------
// strtosc — parse a function name into a KeyStruct
// ---------------------------------------------------------------------------

/* C: keystruct *strtosc(const char *input) */
#[cfg(feature = "nanorc")]
pub fn strtosc(input: &str) -> Option<KeyStruct> {
    use crate::global::*;

    let func: Option<FuncPtr> = match input {
        "cancel"    => Some(do_cancel as FuncPtr),
        #[cfg(feature = "help")]
        "help"      => Some(do_help as FuncPtr),
        "exit"      => Some(do_exit as FuncPtr),
        "discardbuffer" => Some(discard_buffer as FuncPtr),
        "writeout"  => Some(do_writeout as FuncPtr),
        "savefile"  => Some(do_savefile as FuncPtr),
        "insert"    => Some(do_insertfile as FuncPtr),
        "whereis"   => Some(do_search_forward as FuncPtr),
        "wherewas"  => Some(do_search_backward as FuncPtr),
        "findprevious" => Some(do_findprevious as FuncPtr),
        "findnext"  => Some(do_findnext as FuncPtr),
        "replace"   => Some(do_replace as FuncPtr),
        "cut"       => Some(cut_text as FuncPtr),
        "copy"      => Some(copy_text as FuncPtr),
        "paste"     => Some(paste_text as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "execute"   => Some(do_execute as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "cutrestoffile" => Some(cut_till_eof as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "zap"       => Some(zap_text as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "mark"      => Some(do_mark as FuncPtr),
        #[cfg(feature = "speller")]
        "tospell" | "speller" => Some(do_spell as FuncPtr),
        #[cfg(feature = "linter")]
        "linter"    => Some(do_linter as FuncPtr),
        #[cfg(feature = "formatter")]
        "formatter" => Some(do_formatter as FuncPtr),
        "location"  => Some(report_cursor_position as FuncPtr),
        "gotoline"  => Some(do_gotolinecolumn as FuncPtr),
        #[cfg(feature = "justify")]
        "justify"   => Some(do_justify as FuncPtr),
        #[cfg(feature = "justify")]
        "fulljustify" => Some(do_full_justify as FuncPtr),
        #[cfg(feature = "justify")]
        "beginpara" => Some(to_para_begin as FuncPtr),
        #[cfg(feature = "justify")]
        "endpara"   => Some(to_para_end as FuncPtr),
        #[cfg(feature = "comment")]
        "comment"   => Some(do_comment as FuncPtr),
        #[cfg(feature = "wordcomp")]
        "complete"  => Some(complete_a_word as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "indent"    => Some(do_indent as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "unindent"  => Some(do_unindent as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "chopwordleft"  => Some(chop_previous_word as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "chopwordright" => Some(chop_next_word as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "findbracket"   => Some(do_find_bracket as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "wordcount"     => Some(count_lines_words_and_characters as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "recordmacro"   => Some(record_macro as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "runmacro"      => Some(run_macro as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "anchor"        => Some(put_or_lift_anchor as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "prevanchor"    => Some(to_prev_anchor as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "nextanchor"    => Some(to_next_anchor as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "undo"      => Some(do_undo as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "redo"      => Some(do_redo as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "suspend"   => Some(do_suspend as FuncPtr),
        "left" | "back"    => Some(do_left as FuncPtr),
        "right" | "forward" => Some(do_right as FuncPtr),
        "up" | "prevline"  => Some(do_up as FuncPtr),
        "down" | "nextline" => Some(do_down as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "scrollleft"  => Some(do_scroll_left as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "scrollright" => Some(do_scroll_right as FuncPtr),
        #[cfg(any(not(feature = "tiny"), feature = "help"))]
        "scrollup"    => Some(do_scroll_up as FuncPtr),
        #[cfg(any(not(feature = "tiny"), feature = "help"))]
        "scrolldown"  => Some(do_scroll_down as FuncPtr),
        "prevword"  => Some(to_prev_word as FuncPtr),
        "nextword"  => Some(to_next_word as FuncPtr),
        "home"      => Some(do_home as FuncPtr),
        "end"       => Some(do_end as FuncPtr),
        "prevblock" => Some(to_prev_block as FuncPtr),
        "nextblock" => Some(to_next_block as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "toprow"    => Some(to_top_row as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "bottomrow" => Some(to_bottom_row as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "center"    => Some(do_center as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "cycle"     => Some(do_cycle as FuncPtr),
        "pageup" | "prevpage"   => Some(do_page_up as FuncPtr),
        "pagedown" | "nextpage" => Some(do_page_down as FuncPtr),
        "firstline" => Some(to_first_line as FuncPtr),
        "lastline"  => Some(to_last_line as FuncPtr),
        #[cfg(feature = "multibuffer")]
        "prevbuf"   => Some(switch_to_prev_buffer as FuncPtr),
        #[cfg(feature = "multibuffer")]
        "nextbuf"   => Some(switch_to_next_buffer as FuncPtr),
        "verbatim"  => Some(do_verbatim_input as FuncPtr),
        "tab"       => Some(do_tab as FuncPtr),
        "enter"     => Some(do_enter as FuncPtr),
        "delete"    => Some(do_delete as FuncPtr),
        "backspace" => Some(do_backspace as FuncPtr),
        "refresh"   => Some(full_refresh as FuncPtr),
        "casesens"  => Some(case_sens_void as FuncPtr),
        "regexp"    => Some(regexp_void as FuncPtr),
        "backwards" => Some(backwards_void as FuncPtr),
        "flipreplace" => Some(flip_replace as FuncPtr),
        #[cfg(feature = "histories")]
        "older"     => Some(get_older_item as FuncPtr),
        #[cfg(feature = "histories")]
        "newer"     => Some(get_newer_item as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "dosformat" => Some(dos_format as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "append"    => Some(append_it as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "prepend"   => Some(prepend_it as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "backup"    => Some(back_it_up as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "flipexecute" => Some(flip_execute as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "flippipe"  => Some(flip_pipe as FuncPtr),
        #[cfg(not(feature = "tiny"))]
        "flipconvert" => Some(flip_convert as FuncPtr),
        #[cfg(feature = "multibuffer")]
        "flipnewbuffer" => Some(flip_newbuffer as FuncPtr),
        #[cfg(feature = "browser")]
        "tofiles" | "browser" => Some(to_files as FuncPtr),
        #[cfg(feature = "browser")]
        "gotodir"   => Some(goto_dir as FuncPtr),
        #[cfg(feature = "browser")]
        "firstfile" => Some(to_first_file as FuncPtr),
        #[cfg(feature = "browser")]
        "lastfile"  => Some(to_last_file as FuncPtr),
        _ => None,
    };

    if let Some(f) = func {
        let mut s = KeyStruct::default();
        s.func = Some(f);
        return Some(s);
    }

    // Toggle functions (not tiny)
    #[cfg(not(feature = "tiny"))]
    {
        use crate::global::do_toggle;
        let toggle_flag: Option<u32> = match input {
            "nohelp"             => Some(NO_HELP),
            "zero"               => Some(ZERO),
            "constantshow"       => Some(CONSTANT_SHOW),
            "softwrap"           => Some(SOFTWRAP),
            #[cfg(feature = "linenumbers")]
            "linenumbers"        => Some(LINE_NUMBERS),
            "whitespacedisplay"  => Some(WHITESPACE_DISPLAY),
            #[cfg(feature = "color")]
            "nosyntax"           => Some(NO_SYNTAX),
            "smarthome"          => Some(SMART_HOME),
            "autoindent"         => Some(AUTOINDENT),
            "cutfromcursor"      => Some(CUT_FROM_CURSOR),
            #[cfg(feature = "wrapping")]
            "breaklonglines"     => Some(BREAK_LONG_LINES),
            "tabstospaces"       => Some(TABS_TO_SPACES),
            #[cfg(feature = "mouse")]
            "mouse"              => Some(USE_MOUSE),
            _ => None,
        };

        if let Some(flag) = toggle_flag {
            let mut s = KeyStruct::default();
            s.func = Some(do_toggle as FuncPtr);
            s.toggle = flag as i32;
            return Some(s);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// name_to_menu / menu_to_name
// ---------------------------------------------------------------------------

/* C: int name_to_menu(const char *name) */
pub fn name_to_menu(name: &str) -> u32 {
    for i in 0..NUMBER_OF_MENUS {
        if MENUNAMES[i] == name {
            return MENUSYMBOLS[i];
        }
    }
    0
}

/* C: char *menu_to_name(int menu) */
pub fn menu_to_name(menu: u32) -> &'static str {
    for i in 0..NUMBER_OF_MENUS {
        if MENUSYMBOLS[i] == menu {
            return MENUNAMES[i];
        }
    }
    "boooo"
}

// ---------------------------------------------------------------------------
// is_universal — true when function is present in almost all menus
// ---------------------------------------------------------------------------

/* C: bool is_universal(void (*func)(void)) */
#[cfg(feature = "nanorc")]
fn is_universal(func: FuncPtr) -> bool {
    use crate::global::*;
    func == do_left as FuncPtr
        || func == do_right as FuncPtr
        || func == do_home as FuncPtr
        || func == do_end as FuncPtr
        || {
            #[cfg(not(feature = "tiny"))]
            { func == to_prev_word as FuncPtr || func == to_next_word as FuncPtr }
            #[cfg(feature = "tiny")]
            { false }
        }
        || func == do_delete as FuncPtr
        || func == do_backspace as FuncPtr
        || func == cut_text as FuncPtr
        || func == paste_text as FuncPtr
        || func == do_tab as FuncPtr
        || func == do_enter as FuncPtr
        || func == do_verbatim_input as FuncPtr
}

// ---------------------------------------------------------------------------
// is_good_file — verify file is readable, not a dir/device
// ---------------------------------------------------------------------------

/* C: bool is_good_file(char *file) */
pub fn is_good_file(file: &str) -> bool {
    use std::fs;
    // Check readability
    if !Path::new(file).exists() {
        return false;
    }
    // Must be readable
    if File::open(file).is_err() {
        return false;
    }
    // Must not be directory or device
    if let Ok(meta) = fs::metadata(file) {
        if meta.is_dir() {
            jot_error(&format!("\"{}\" is a directory", file));
            return false;
        }
        // Block/char device check via file type bits
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let ft = meta.file_type();
            if ft.is_char_device() || ft.is_block_device() {
                jot_error(&format!("\"{}\" is a device file", file));
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Color parsing
// ---------------------------------------------------------------------------

#[cfg(feature = "color")]
/* C: short closest_index_color(short red, short green, short blue) */
fn closest_index_color(red: i16, green: i16, blue: i16) -> i16 {
    // Translation table, from 16 intended color levels to 6 available levels.
    static LEVEL: [i16; 16] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5];
    // Translation table, from 14 intended gray levels to 24 available levels.
    static GRAY: [i16; 14] = [1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 15, 18, 21, 23];

    if terminal_colors() != 256 {
        return THE_DEFAULT;
    } else if red == green && green == blue && red > 0 && red < 0xF {
        return 232 + GRAY[(red - 1) as usize];
    } else {
        return 36 * LEVEL[red as usize] + 6 * LEVEL[green as usize] + LEVEL[blue as usize] + 16;
    }
}

#[cfg(feature = "color")]
const COLORCOUNT: usize = 34;

#[cfg(feature = "color")]
static HUES: [&str; COLORCOUNT] = [
    "red", "green", "blue",
    "yellow", "cyan", "magenta",
    "white", "black", "normal",
    "pink", "purple", "mauve",
    "lagoon", "mint", "lime",
    "peach", "orange", "latte",
    "rosy", "beet", "plum",
    "sea", "sky", "slate",
    "teal", "sage", "brown",
    "ocher", "sand", "tawny",
    "brick", "crimson",
    "grey", "gray",
];

#[cfg(feature = "color")]
static INDICES: [i16; COLORCOUNT] = [
    COLOR_RED, COLOR_GREEN, COLOR_BLUE,
    COLOR_YELLOW, COLOR_CYAN, COLOR_MAGENTA,
    COLOR_WHITE, COLOR_BLACK, THE_DEFAULT,
    204, 163, 134, 38, 48, 148, 215, 208, 137,
    175, 127, 98, 32, 111, 66, 35, 107, 100,
    142, 186, 136, 166, 161,
    COLOR_BLACK + 8, COLOR_BLACK + 8,
];

#[cfg(feature = "color")]
/* C: short color_to_short(const char *colorname, bool *vivid, bool *thick) */
fn color_to_short(colorname: &str) -> (i16, bool, bool) {
    // Returns (color_index, vivid, thick)
    let (name, vivid, thick) = if colorname.starts_with("bright") && colorname.len() > 6 {
        (&colorname[6..], true, true)
    } else if colorname.starts_with("light") && colorname.len() > 5 {
        (&colorname[5..], true, false)
    } else {
        (colorname, false, false)
    };

    // Try #RGB hex color: '#' followed by EXACTLY three ASCII hex digits.  Using
    // chars() rather than byte length/indexing avoids panicking on a 4-byte input
    // like "#€" whose interior bytes are not on char boundaries.
    if let Some(hex) = name.strip_prefix('#') {
        let digits: Vec<char> = hex.chars().collect();
        if digits.len() == 3 && digits.iter().all(|c| c.is_ascii_hexdigit()) {
            if vivid {
                jot_error(&format!("Color '{}' takes no prefix", name));
                return (BAD_COLOR, vivid, thick);
            }
            let r = digits[0].to_digit(16).unwrap() as i16;
            let g = digits[1].to_digit(16).unwrap() as i16;
            let b = digits[2].to_digit(16).unwrap() as i16;
            return (closest_index_color(r, g, b), vivid, thick);
        }
    }

    // Look up named color
    for i in 0..COLORCOUNT {
        if HUES[i] == name {
            if i > 7 && vivid {
                jot_error(&format!("Color '{}' takes no prefix", name));
                return (BAD_COLOR, vivid, thick);
            } else if i > 8 && terminal_colors() < 255 {
                return (THE_DEFAULT, vivid, thick);
            } else {
                return (INDICES[i], vivid, thick);
            }
        }
    }

    jot_error(&format!("Color \"{}\" not understood", name));
    (BAD_COLOR, vivid, thick)
}

#[cfg(feature = "color")]
/* C: bool parse_combination(char *combotext, short *fg, short *bg, int *attributes) */
fn parse_combination(combotext: &str) -> Option<(i16, i16, i32)> {
    // Returns (fg, bg, attributes) or None on error.
    let mut attributes = A_NORMAL;
    let mut s = combotext;

    if s.starts_with("bold") {
        attributes |= A_BOLD;
        if !s[4..].starts_with(',') {
            jot_error("An attribute requires a subsequent comma");
            return None;
        }
        s = &s[5..];
    }

    if s.starts_with("italic") {
        attributes |= A_ITALIC;
        if !s[6..].starts_with(',') {
            jot_error("An attribute requires a subsequent comma");
            return None;
        }
        s = &s[7..];
    }

    let (fg_str, bg_str) = match s.find(',') {
        None => (s, None),
        Some(pos) => (&s[..pos], Some(&s[pos + 1..])),
    };

    let fg = if !fg_str.is_empty() {
        let (color, vivid, thick) = color_to_short(fg_str);
        if color == BAD_COLOR {
            return None;
        }
        let mut fg = color;
        if vivid && !thick && terminal_colors() > 8 {
            fg += 8;
        } else if vivid {
            attributes |= A_BOLD;
        }
        fg
    } else {
        THE_DEFAULT
    };

    let bg = if let Some(bg_name) = bg_str {
        let (color, vivid, _thick) = color_to_short(bg_name);
        if color == BAD_COLOR {
            return None;
        }
        let mut bg = color;
        if vivid && terminal_colors() > 8 {
            bg += 8;
        }
        bg
    } else {
        THE_DEFAULT
    };

    Some((fg, bg, attributes))
}

#[cfg(feature = "color")]
/* C: void set_interface_color(int element, char *combotext) */
fn set_interface_color(element: usize, combotext: &str) {
    if let Some((fg, bg, attributes)) = parse_combination(combotext) {
        with_state_mut(|s| {
            let trio = Box::new(ColorType {
                fg,
                bg,
                attributes,
                ..ColorType::default()
            });
            s.color_combo[element] = Some(trio);
        });
    }
}

// ---------------------------------------------------------------------------
// compile — compile a regex
// ---------------------------------------------------------------------------

/// Translate POSIX-ERE quirks into syntax the `regex` crate accepts.  nano's
/// syntax files (and POSIX `regcomp`) differ from the Rust regex engine in a few
/// ways that this bridges:
///   * a `]` that is the FIRST member of a bracket expression (`[]abc]`,
///     `[^]abc]`) is a literal `]`, and a bare `[` inside a class is a literal
///     `[` — the regex crate would instead close the class or start a nested one
///     (breaking `[^][]`);
///   * inside a bracket expression `\` is a LITERAL backslash, not an escape
///     (so `[^'\]` / `[^\]` keep their meaning instead of swallowing the class);
///   * an UNMATCHED `)` is a literal `)` in POSIX/GNU (the regex crate errors).
/// POSIX classes (`[:name:]`, `[.coll.]`, `[=equiv=]`) pass through unchanged.
#[cfg(feature = "color")]
fn posix_bracket_fixup(re: &str) -> String {
    let chars: Vec<char> = re.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(re.len() + 4);
    let mut i = 0;
    // Group-nesting depth, so an unmatched ')' can be escaped to a literal.
    let mut depth: i32 = 0;
    while i < n {
        let c = chars[i];
        if c == '\\' {
            // Copy an escape pair verbatim.
            out.push('\\');
            i += 1;
            if i < n {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if c == '[' {
            // Enter a bracket expression.
            out.push('[');
            i += 1;
            if i < n && chars[i] == '^' {
                out.push('^');
                i += 1;
            }
            // A ']' as the first member is a literal ']' in POSIX.
            if i < n && chars[i] == ']' {
                out.push_str("\\]");
                i += 1;
            }
            // Copy members until the real closing ']'.
            while i < n && chars[i] != ']' {
                if chars[i] == '['
                    && i + 1 < n
                    && (chars[i + 1] == ':' || chars[i + 1] == '.' || chars[i + 1] == '=')
                {
                    // POSIX class/collating/equivalence element: copy through to ":]" etc.
                    let kind = chars[i + 1];
                    out.push('[');
                    out.push(kind);
                    i += 2;
                    while i + 1 < n && !(chars[i] == kind && chars[i + 1] == ']') {
                        out.push(chars[i]);
                        i += 1;
                    }
                    if i + 1 < n {
                        out.push(kind);
                        out.push(']');
                        i += 2;
                    }
                    continue;
                }
                if chars[i] == '[' {
                    // A bare '[' inside a class is a literal '[' in POSIX.
                    out.push_str("\\[");
                    i += 1;
                    continue;
                }
                if chars[i] == '\\' {
                    // POSIX bracket expressions have NO escapes: '\' is a LITERAL
                    // backslash.  Emit it doubled so the regex crate reads a literal
                    // (and so it cannot "escape" a following ']' and swallow the
                    // rest of the pattern — e.g. c.nanorc's `[^'\]` and `[^\]`).
                    out.push_str("\\\\");
                    i += 1;
                    continue;
                }
                out.push(chars[i]);
                i += 1;
            }
            if i < n && chars[i] == ']' {
                out.push(']');
                i += 1;
            }
            continue;
        }
        if c == '(' {
            depth += 1;
            out.push('(');
            i += 1;
            continue;
        }
        if c == ')' {
            if depth > 0 {
                depth -= 1;
                out.push(')');
            } else {
                // Unmatched ')' is a literal in POSIX/GNU; escape it for the regex crate.
                out.push_str("\\)");
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/* C: bool compile(const char *expression, int rex_flags, regex_t **packed) */
#[cfg(feature = "color")]
fn compile(expression: &str, case_insensitive: bool) -> Option<Regex> {
    // nano's syntax files are POSIX ERE; bridge the bracket-expression differences
    // before handing the pattern to the (non-POSIX) `regex` crate.
    let pattern = posix_bracket_fixup(expression);
    let result = RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build();
    match result {
        Ok(r) => Some(r),
        Err(e) => {
            jot_error(&format!("Bad regex \"{}\": {}", expression, e));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Syntax management
// ---------------------------------------------------------------------------

#[cfg(feature = "color")]
/* C: void check_for_nonempty_syntax(void) */
pub fn check_for_nonempty_syntax() {
    if get_opensyntax() && !get_seen_color_command() {
        let saved_lineno = get_lineno();
        // Read from STATE.syntaxes head (the live syntax being built)
        // This is safe — we're not inside a with_state_mut borrow here
        let (syntax_lineno, syntax_name) = with_state(|s| {
            s.syntaxes.as_ref()
                .map(|sx| (sx.lineno, sx.name.clone()))
                .unwrap_or((0, String::new()))
        });
        set_lineno(syntax_lineno);
        jot_error(&format!("Syntax \"{}\" has no color commands", syntax_name));
        set_lineno(saved_lineno);
    }
    set_opensyntax(false);
}

#[cfg(not(feature = "color"))]
/* C: void check_for_nonempty_syntax(void) — no-op without color */
pub fn check_for_nonempty_syntax() {}

#[cfg(feature = "color")]
/* C: void grab_and_store(const char *kind, char *ptr, regexlisttype **storage) */
/// Parse quoted regexes from ptr, compile them, and return a linked list.
/// Returns None if there was a validation error; returns Some(head) on success
/// (head may still be None if all regexes were invalid but parseable).
fn grab_and_store_build(kind: &str, ptr: &str, for_default_syntax: bool) -> Option<Option<Box<RegexListType>>> {
    if !get_opensyntax() {
        jot_error(&format!("A '{}' command requires a preceding 'syntax' command", kind));
        return None;
    }

    if for_default_syntax && !ptr.is_empty() {
        jot_error(&format!("The \"default\" syntax does not accept '{}' regexes", kind));
        return None;
    }

    if ptr.is_empty() {
        jot_error(&format!("Missing regex string after '{}' command", kind));
        return None;
    }

    let mut head: Option<Box<RegexListType>> = None;
    let mut tail: *mut Option<Box<RegexListType>> = &mut head;

    let mut remaining = ptr.trim_start_matches(|c: char| c == ' ' || c == '\t');
    while !remaining.is_empty() {
        // Each regex string must start with '"'
        if !remaining.starts_with('"') {
            jot_error(&format!("Regex strings for '{}' must begin with a \" character", kind));
            return None;
        }
        remaining = &remaining[1..]; // skip opening '"'

        match parse_next_regex(remaining) {
            None => return None,
            Some((regex_str, rest)) => {
                remaining = rest;
                // compile happens OUTSIDE any STATE borrow
                if let Some(compiled) = compile(regex_str, false) {
                    let newthing = Box::new(RegexListType {
                        one_rgx: Some(compiled),
                        next: None,
                    });
                    unsafe {
                        *tail = Some(newthing);
                        if let Some(ref mut node) = *tail {
                            tail = &mut node.next;
                        }
                    }
                }
                if !remaining.is_empty() && !remaining.starts_with('"') {
                    jot_error(&format!("Unexpected text after '{}' regex", kind));
                    return None;
                }
            }
        }
    }

    Some(head)
}

#[cfg(feature = "color")]
/// Append compiled regex list to an existing storage chain.
fn append_regex_list(storage: &mut Option<Box<RegexListType>>, new_items: Option<Box<RegexListType>>) {
    if new_items.is_none() {
        return;
    }
    // Find the tail of storage
    let mut tail: *mut Option<Box<RegexListType>> = storage;
    unsafe {
        while let Some(ref mut node) = *tail {
            tail = &mut node.next;
        }
        *tail = new_items;
    }
}

#[cfg(feature = "color")]
/* C: void pick_up_name(const char *kind, char *ptr, char **storage) */
/// Parse a name argument (optionally quoted). Returns Some(name) on success, None on error.
/// MUST be called OUTSIDE any STATE borrow (it calls jot_error on failure).
fn pick_up_name(kind: &str, ptr: &str) -> Option<String> {
    if ptr.is_empty() {
        jot_error(&format!("Missing argument after '{}'", kind));
        return None;
    }

    if ptr.starts_with('"') {
        let inner = &ptr[1..];
        match inner.rfind('"') {
            None => {
                jot_error(&format!("Argument of '{}' lacks closing \"", kind));
                None
            }
            Some(pos) => Some(inner[..pos].to_string()),
        }
    } else {
        Some(ptr.to_string())
    }
}

#[cfg(feature = "color")]
/* C: void begin_new_syntax(char *ptr) */
fn begin_new_syntax(ptr: &str) {
    // Check that the syntax name is not empty
    if ptr.is_empty() || (ptr.starts_with('"') && (ptr.len() == 1 || ptr[1..].starts_with('"'))) {
        jot_error("Missing syntax name");
        return;
    }

    let (name_raw, rest) = parse_next_word(ptr);

    // Check for paired quotes
    let has_open_quote = name_raw.starts_with('"');
    let has_close_quote = name_raw.ends_with('"');
    if has_open_quote != has_close_quote {
        jot_error("Unpaired quote in syntax name");
        return;
    }

    // Strip quotes if present
    let nameptr = if has_open_quote && name_raw.len() >= 2 {
        &name_raw[1..name_raw.len() - 1]
    } else {
        name_raw
    };

    if nameptr == "none" {
        jot_error("The \"none\" syntax is reserved");
        return;
    }

    let nanorc_path = get_nanorc().unwrap_or_default();
    let current_lineno = get_lineno();

    // Initialize a new syntax struct
    let mut live = Box::new(SyntaxType {
        name: nameptr.to_string(),
        filename: nanorc_path,
        lineno: current_lineno,
        augmentations: None,
        extensions: None,
        headers: None,
        magics: None,
        linter: None,
        formatter: None,
        tabstring: None,
        #[cfg(feature = "comment")]
        comment: Some(GENERAL_COMMENT_CHARACTER.to_string()),
        color: None,
        multiscore: 0,
        next: None,
    });

    // Hook the new syntax in at the top of the STATE list
    with_state_mut(|s| {
        let old_head = s.syntaxes.take();
        live.next = old_head;
        s.syntaxes = Some(live);
    });

    // Point LIVE_SYNTAX at the head of STATE.syntaxes
    // We store a "detached" copy in LIVE_SYNTAX to work on, then keep it in STATE.
    // Actually: live_syntax in C is a pointer into the list. We simulate by always
    // working on STATE.syntaxes directly (it's the head).
    // We signal "open" so color commands can append.
    set_opensyntax(true);
    set_seen_color_command(false);

    // The default syntax should have no associated extensions
    if !rest.is_empty() {
        let syntax_name = with_state(|s| {
            s.syntaxes.as_ref().map(|sx| sx.name.clone()).unwrap_or_default()
        });
        if syntax_name == "default" {
            jot_error("The \"default\" syntax does not accept extensions");
            return;
        }
        // Build extension regex list OUTSIDE any STATE borrow (compile may call jot_error)
        if let Some(new_exts) = grab_and_store_build("extension", rest, false) {
            // Now atomically append into state
            with_state_mut(|s| {
                if let Some(ref mut sx) = s.syntaxes {
                    append_regex_list(&mut sx.extensions, new_exts);
                }
            });
        }
    }
}

/// Helper: build + store a regex list (outside any STATE borrow).
/// Calls grab_and_store_build then appends to storage.
#[cfg(feature = "color")]
#[allow(dead_code)] // parity: companion to grab_and_store_build for extendsyntax handling
fn grab_and_store_extensions(kind: &str, ptr: &str, storage: &mut Option<Box<RegexListType>>) {
    // is_default: not applicable in direct-storage mode; opensyntax check done in grab_and_store_build
    if let Some(new_items) = grab_and_store_build(kind, ptr, false) {
        append_regex_list(storage, new_items);
    }
}

#[cfg(feature = "color")]
/* C: void parse_rule(char *ptr, int rex_flags) */
fn parse_rule(ptr: &str, case_insensitive: bool) {
    if ptr.is_empty() {
        jot_error("Missing color name");
        return;
    }

    let (names, rest_after_names) = parse_next_word(ptr);

    let combo = match parse_combination(names) {
        None => return,
        Some(c) => c,
    };
    let (fg, bg, attributes) = combo;

    if rest_after_names.is_empty() {
        jot_error(&format!("Missing regex string after '{}' command", "color"));
        return;
    }

    let mut remaining = rest_after_names;

    while !remaining.is_empty() {
        let expectend = remaining.starts_with("start=");
        let regex_input = if expectend {
            &remaining[6..] // skip "start="
        } else {
            remaining
        };

        // Expect opening '"'
        if !regex_input.starts_with('"') {
            jot_error("Regex strings must begin and end with a \" character");
            return;
        }
        let regex_body = &regex_input[1..];

        let (start_str, after_start) = match parse_next_regex(regex_body) {
            None => return,
            Some(pair) => pair,
        };

        let start_rgx = match compile(start_str, case_insensitive) {
            None => return,
            Some(r) => r,
        };

        let end_rgx = if expectend {
            if !after_start.starts_with("end=") {
                jot_error("\"start=\" requires a corresponding \"end=\"");
                return;
            }
            let end_input = &after_start[4..];
            if !end_input.starts_with('"') {
                jot_error("Regex strings must begin and end with a \" character");
                return;
            }
            let end_body = &end_input[1..];
            let (end_str, after_end) = match parse_next_regex(end_body) {
                None => return,
                Some(pair) => pair,
            };
            remaining = after_end;
            match compile(end_str, case_insensitive) {
                None => return,
                Some(r) => Some(r),
            }
        } else {
            remaining = after_start;
            None
        };

        // Get the multiscore id before appending
        let multi_id = if end_rgx.is_some() {
            with_state(|s| s.syntaxes.as_ref().map(|sx| sx.multiscore).unwrap_or(0))
        } else {
            0
        };

        let newcolor = Box::new(ColorType {
            id: multi_id,
            fg,
            bg,
            attributes,
            start: Some(start_rgx),
            end: end_rgx.clone(),
            ..ColorType::default()
        });

        // Append to the head syntax's color list
        let is_multiline = newcolor.end.is_some();
        with_state_mut(|s| {
            if let Some(ref mut sx) = s.syntaxes {
                // Walk to end of color list and append
                let mut tail: &mut Option<Box<ColorType>> = &mut sx.color;
                while let Some(ref mut c) = *tail {
                    tail = &mut c.next;
                }
                *tail = Some(newcolor);
                if is_multiline {
                    sx.multiscore += 1;
                }
            }
        });

        // Mark that a color command was seen
        set_seen_color_command(true);

        // Loop back for another rule.  When `remaining` is empty the while-loop
        // exits naturally; when it is non-empty but not a quoted regex or "start="
        // (i.e. trailing garbage), the top of the loop reports the error — matching
        // C's loop-and-validate, instead of silently stopping here.
    }
}

// ---------------------------------------------------------------------------
// parse_syntax_commands
// ---------------------------------------------------------------------------

#[cfg(feature = "color")]
/* C: bool parse_syntax_commands(char *keyword, char *ptr) */
pub fn parse_syntax_commands(keyword: &str, ptr: &str) -> bool {
    match keyword {
        "color" => {
            parse_rule(ptr, false);
        }
        "icolor" => {
            parse_rule(ptr, true);
        }
        "comment" => {
            // pick_up_name may call jot_error — must be OUTSIDE with_state_mut
            #[cfg(feature = "comment")]
            {
                if let Some(val) = pick_up_name("comment", ptr) {
                    with_state_mut(|s| {
                        if let Some(ref mut sx) = s.syntaxes {
                            sx.comment = Some(val);
                        }
                    });
                }
            }
        }
        "tabgives" => {
            // pick_up_name may call jot_error — must be OUTSIDE with_state_mut
            if let Some(val) = pick_up_name("tabgives", ptr) {
                with_state_mut(|s| {
                    if let Some(ref mut sx) = s.syntaxes {
                        sx.tabstring = Some(val);
                    }
                });
            }
        }
        "linter" => {
            // pick_up_name may call jot_error — must be OUTSIDE with_state_mut
            if let Some(mut val) = pick_up_name("linter", ptr) {
                crate::chars::strip_leading_blanks_from(&mut val);
                with_state_mut(|s| {
                    if let Some(ref mut sx) = s.syntaxes {
                        sx.linter = Some(val);
                    }
                });
            }
        }
        "formatter" => {
            // pick_up_name may call jot_error — must be OUTSIDE with_state_mut
            if let Some(mut val) = pick_up_name("formatter", ptr) {
                crate::chars::strip_leading_blanks_from(&mut val);
                with_state_mut(|s| {
                    if let Some(ref mut sx) = s.syntaxes {
                        sx.formatter = Some(val);
                    }
                });
            }
        }
        _ => return false,
    }
    true
}

#[cfg(not(feature = "color"))]
pub fn parse_syntax_commands(_keyword: &str, _ptr: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// parse_binding
// ---------------------------------------------------------------------------

/* C: void parse_binding(char *ptr, bool dobind) */
#[cfg(feature = "nanorc")]
pub fn parse_binding(ptr: &str, dobind: bool) {
    use crate::global::*;

    check_for_nonempty_syntax();

    if ptr.is_empty() {
        jot_error("Missing key name");
        return;
    }

    let (keyptr, after_key) = parse_next_word(ptr);
    // Make a mutable copy to uppercase
    let mut keycopy: String = keyptr.to_string();

    // Uppercase the second char for '^' combos, or the first char otherwise
    let bytes = unsafe { keycopy.as_bytes_mut() };
    if bytes.len() >= 2 && bytes[0] == b'^' {
        if bytes[1] >= b'a' && bytes[1] <= b'z' {
            bytes[1] &= 0x5F;
        }
    } else if !bytes.is_empty() && bytes[0] >= b'a' && bytes[0] <= b'z' {
        bytes[0] &= 0x5F;
    }

    // Verify key name not too short
    let bytes = keycopy.as_bytes();
    if bytes.len() < 2 || (bytes[0] == b'M' && bytes.len() < 3) {
        jot_error(&format!("Key name {} is invalid", keycopy));
        return;
    }

    let keycode = keycode_from_string(&keycopy);
    if keycode < 0 {
        jot_error(&format!("Key name {} is invalid", keycopy));
        return;
    }

    let (funcptr_str, after_func, funcptr_was_quoted) = if dobind {
        let (f, a, was_quoted) = match parse_argument_with_quote(after_key) {
            None => return,
            Some((f, a, q)) => (f, a, q),
        };
        if f.is_empty() {
            jot_error("Must specify a function to bind the key to");
            return;
        }
        (f, a, was_quoted)
    } else {
        ("", after_key, false)
    };

    let (menuptr, _rest) = parse_next_word(after_func);

    if menuptr.is_empty() {
        jot_error("Must specify a menu (or \"all\") in which to bind/unbind the key");
        return;
    }

    let menu = name_to_menu(menuptr);
    if menu == 0 {
        jot_error(&format!("Unknown menu: {}", menuptr));
        return;
    }

    // Build the new shortcut for dobind
    let mut newsc: Option<KeyStruct> = None;
    if dobind {
        if funcptr_was_quoted {
            // String bind
            let mut sc = KeyStruct::default();
            sc.func = Some(implant_sentinel as FuncPtr);
            sc.expansion = Some(funcptr_str.to_string());
            newsc = Some(sc);
        } else {
            newsc = strtosc(funcptr_str);
            if newsc.is_none() {
                jot_error(&format!("Unknown function: {}", funcptr_str));
                return;
            }
        }
    }

    // Wipe the given shortcut from the given menu
    with_state_mut(|s| {
        for sc in s.sclist.iter_mut() {
            if (sc.menus as u32 & menu) != 0 && sc.keycode == keycode {
                sc.menus &= !(menu as i32);
            }
        }
    });

    // When unbinding, we are done
    if !dobind {
        return;
    }

    let mut sc = match newsc {
        None => return,
        Some(s) => s,
    };

    // Limit menu to where the function exists
    let effective_menu = if let Some(func) = sc.func {
        if is_universal(func) {
            menu & (MMOST | MBROWSER)
        } else {
            #[cfg(not(feature = "tiny"))]
            {
                if func == do_toggle as FuncPtr {
                    if sc.toggle == NO_HELP as i32 {
                        menu & ((MMOST | MBROWSER | MYESNO) & !MFINDINHELP)
                    } else {
                        menu & MMAIN
                    }
                } else if func == full_refresh as FuncPtr {
                    menu & (MMOST | MBROWSER | MHELP | MYESNO)
                } else if func == implant_sentinel as FuncPtr {
                    menu & (MMOST | MBROWSER | MHELP)
                } else {
                    // Tally up the menus where the function exists
                    let mask = with_state(|s| {
                        let mut m: u32 = 0;
                        for f in &s.allfuncs {
                            if f.func == Some(func) {
                                m |= f.menus as u32;
                            }
                        }
                        m
                    });
                    menu & mask
                }
            }
            #[cfg(feature = "tiny")]
            {
                if func == full_refresh as FuncPtr {
                    menu & (MMOST | MBROWSER | MHELP | MYESNO)
                } else if func == implant_sentinel as FuncPtr {
                    menu & (MMOST | MBROWSER | MHELP)
                } else {
                    let mask = with_state(|s| {
                        let mut m: u32 = 0;
                        for f in &s.allfuncs {
                            if f.func == Some(func) {
                                m |= f.menus as u32;
                            }
                        }
                        m
                    });
                    menu & mask
                }
            }
        }
    } else {
        0
    };

    if effective_menu == 0 {
        if !ISSET!(RESTRICTED) && !ISSET!(VIEW_MODE) {
            jot_error(&format!(
                "Function '{}' does not exist in menu '{}'",
                funcptr_str,
                menuptr
            ));
        }
        return;
    }

    // Disallow rebinding <Esc>
    if keycode == ESC_CODE as i32 {
        jot_error(&format!("Keystroke {} may not be rebound", keycopy));
        return;
    }

    sc.menus = effective_menu as i32;
    // Store key info — note keystr needs 'static lifetime; we leak the keycopy string.
    let leaked: &'static str = Box::leak(keycopy.into_boxed_str());
    sc.keystr = leaked;
    sc.keycode = keycode;

    // For toggles, find and copy the ordinal sequence number
    #[cfg(not(feature = "tiny"))]
    {
        if sc.func == Some(do_toggle as FuncPtr) {
            let toggle_val = sc.toggle;
            let ordinal = with_state(|s| {
                for existing in &s.sclist {
                    if existing.func == Some(do_toggle as FuncPtr)
                        && existing.toggle == toggle_val
                    {
                        return existing.ordinal;
                    }
                }
                0
            });
            sc.ordinal = ordinal;
        } else {
            sc.ordinal = 0;
        }
    }

    // Add the new shortcut at the START of the list (user binds override built-ins)
    with_state_mut(|s| {
        s.sclist.insert(0, sc);
    });
}

/// Sentinel function for string binds — replaces C's `implant` cast.
/// C: newsc->func = (functionptrtype)implant;
pub fn implant_sentinel() {
    // The expansion is stored in the KeyStruct; actual implanting happens in
    // get_shortcut / winio.rs when this function pointer is recognized.
}

// ---------------------------------------------------------------------------
// parse_includes — expand globs and include files
// ---------------------------------------------------------------------------

#[cfg(feature = "color")]
/* C: void parse_includes(char *ptr) */
fn parse_includes(ptr: &str) {
    check_for_nonempty_syntax();

    // parse_argument strips the outer quotes and returns the unquoted content
    let (pattern, _) = match parse_argument(ptr) {
        None => return,
        Some(p) => p,
    };

    if pattern.len() > PATH_MAX {
        jot_error("Path is too long");
        return;
    }

    // Expand leading tilde
    let expanded = crate::files::expand_leading_tilde(pattern);

    // Glob expansion
    let glob_results = expand_glob(&expanded);

    for file_path in &glob_results {
        parse_one_include(file_path, false);
    }
}

/// POSIX-like glob expansion.  Literal separators and leading dots must be
/// matched explicitly, which mirrors the `glob(3)` behavior nano relies on.
fn expand_glob(pattern: &str) -> Vec<String> {
    // No glob characters: return the literal path even when it does not exist,
    // mirroring glob()'s GLOB_NOCHECK, so parse_one_include reports the read error
    // (C passes a non-matching include through and diagnoses it).
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        return vec![pattern.to_string()];
    }

    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let mut results: Vec<String> = match glob::glob_with(pattern, options) {
        Ok(paths) => paths
            .filter_map(Result::ok)
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        Err(error) => {
            jot_error(&format!("Invalid include glob '{}': {}", pattern, error));
            Vec::new()
        }
    };
    results.sort();
    // GLOB_NOCHECK: when a wildcard pattern matches nothing, return the pattern
    // itself so parse_one_include emits the "Error reading" diagnostic, as C does.
    if results.is_empty() {
        results.push(pattern.to_string());
    }
    results
}

// ---------------------------------------------------------------------------
// parse_one_include
// ---------------------------------------------------------------------------

#[cfg(feature = "color")]
/* C: void parse_one_include(char *file, syntaxtype *syntax) */
pub fn parse_one_include(file: &str, full_parse: bool) {
    // Don't open directories, character files, or block files
    if Path::new(file).exists() && !is_good_file(file) {
        return;
    }

    let rcstream = match File::open(file) {
        Err(e) => {
            jot_error(&format!("Error reading {}: {}", file, e));
            return;
        }
        Ok(f) => f,
    };

    let saved_nanorc = get_nanorc();
    let saved_lineno = get_lineno();

    set_nanorc(Some(file.to_string()));
    set_lineno(0);

    if !full_parse {
        // First pass: parse only the prologue (syntax declarations)
        parse_rcfile(BufReader::new(rcstream), true, true);
    } else {
        // Full parse: parse the complete syntax
        parse_rcfile(BufReader::new(rcstream), true, false);

        // Apply any stored extendsyntax commands
        // (These were stored in augmentations on the syntax)
        let augments: Vec<(String, usize, String)> = with_state(|s| {
            s.syntaxes.as_ref().map(|sx| {
                let mut v = Vec::new();
                let mut aug = sx.augmentations.as_ref();
                while let Some(a) = aug {
                    v.push((a.filename.clone(), a.lineno as usize, a.data.clone()));
                    aug = a.next.as_ref();
                }
                v
            }).unwrap_or_default()
        });

        for (filename, lineno, data) in augments {
            set_nanorc(Some(filename));
            set_lineno(lineno);
            let (keyword, therest) = parse_next_word(&data);
            if !parse_syntax_commands(keyword, therest) {
                jot_error(&format!("Command \"{}\" not understood", keyword));
            }
        }

        // Mark syntax as loaded (clear filename)
        with_state_mut(|s| {
            if let Some(ref mut sx) = s.syntaxes {
                sx.filename = String::new();
            }
        });
    }

    set_nanorc(saved_nanorc);
    set_lineno(saved_lineno);
}

// ---------------------------------------------------------------------------
// check_vitals_mapped
// ---------------------------------------------------------------------------

/* C: static void check_vitals_mapped(void) */
fn check_vitals_mapped() {
    use crate::global::*;

    const VITALS: usize = 4;
    let vitals: [FuncPtr; VITALS] = [
        do_exit as FuncPtr,
        do_exit as FuncPtr,
        do_exit as FuncPtr,
        do_cancel as FuncPtr,
    ];
    let inmenus: [u32; VITALS] = [MMAIN, MBROWSER, MHELP, MYESNO];

    for v in 0..VITALS {
        let found_func = with_state(|s| {
            s.allfuncs.iter().any(|f| {
                f.func == Some(vitals[v]) && (f.menus as u32 & inmenus[v]) != 0
            })
        });
        if found_func {
            let bound = first_sc_for(inmenus[v], vitals[v]).is_some();
            if !bound {
                let tag = with_state(|s| {
                    s.allfuncs.iter()
                        .find(|f| f.func == Some(vitals[v]) && (f.menus as u32 & inmenus[v]) != 0)
                        .map(|f| f.tag)
                        .unwrap_or("(unknown)")
                });
                jot_error(&format!(
                    "No key is bound to function '{}' in menu '{}'. Exiting.",
                    tag, menu_to_name(inmenus[v])
                ));
                die("If needed, use nano with the -I option to adjust your nanorc settings.\n");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// parse_rcfile — main parser
// ---------------------------------------------------------------------------

/* C: void parse_rcfile(FILE *rcstream, bool just_syntax, bool intros_only) */
pub fn parse_rcfile<R: BufRead>(mut reader: R, just_syntax: bool, intros_only: bool) {
    let syntax_start_lineno = if just_syntax && !intros_only {
        #[cfg(feature = "color")]
        {
            with_state(|s| s.syntaxes.as_ref().map(|sx| sx.lineno).unwrap_or(0))
        }
        #[cfg(not(feature = "color"))]
        { 0usize }
    } else {
        0
    };

    let mut raw_bytes: Vec<u8> = Vec::new();
    loop {
        raw_bytes.clear();
        match reader.read_until(b'\n', &mut raw_bytes) {
            Ok(0) => break,      // end of file
            Ok(_) => {}
            Err(_) => break,     // genuine read error
        }

        LINENO.with(|l| *l.borrow_mut() += 1);

        // A single line that is not valid UTF-8 is reported and skipped — not fatal
        // to the rest of the file (C only rejects that argument and continues).
        let raw_line = match std::str::from_utf8(&raw_bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                jot_error("Argument is not a valid multibyte string");
                continue;
            }
        };

        let current_lineno = get_lineno();

        // If doing a full syntax parse, skip lines up to and including the 'syntax' command line
        #[cfg(feature = "color")]
        if just_syntax && !intros_only && current_lineno <= syntax_start_lineno {
            continue;
        }

        // Strip trailing CR/LF
        let line = raw_line.trim_end_matches('\n').trim_end_matches('\r');

        // Skip leading blanks
        let line = line.trim_start_matches(|c: char| c == ' ' || c == '\t');

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse the keyword
        let (keyword, rest_after_kw) = parse_next_word(line);

        #[cfg(feature = "color")]
        let mut drop_open = false;

        // Handle the various keywords
        #[cfg(feature = "color")]
        {
            // Handle extendsyntax first
            if !just_syntax && keyword == "extendsyntax" {
                check_for_nonempty_syntax();

                let (syntaxname, rest_after_sname) = parse_next_word(rest_after_kw);
                let (cmd_keyword, cmd_rest) = parse_next_word(rest_after_sname);

                // Find the target syntax
                let found = with_state(|s| {
                    let mut cur = s.syntaxes.as_ref();
                    while let Some(sx) = cur {
                        if sx.name == syntaxname {
                            return true;
                        }
                        cur = sx.next.as_ref();
                    }
                    false
                });

                if !found {
                    jot_error(&format!("Could not find syntax \"{}\" to extend", syntaxname));
                    continue;
                }

                // File-matching commands need to be processed immediately
                if cmd_keyword == "header" || cmd_keyword == "magic" {
                    if cmd_keyword == "header"
                        || (cmd_keyword == "magic" && cfg!(feature = "libmagic"))
                    {
                        // Build regex list OUTSIDE any STATE borrow (compile may call jot_error).
                        // For extendsyntax, C sets opensyntax = TRUE (paired with drop_open =
                        // TRUE, reset below) so grab_and_store does not reject the command with
                        // the spurious "requires a preceding 'syntax' command" error.
                        set_opensyntax(true);
                        let syntaxname_str = syntaxname.to_string();
                        let is_default = syntaxname == "default";
                        if let Some(new_items) = grab_and_store_build(cmd_keyword, cmd_rest, is_default) {
                            with_state_mut(|s| {
                                let mut cur = s.syntaxes.as_mut();
                                while let Some(sx) = cur {
                                    if sx.name == syntaxname_str {
                                        if cmd_keyword == "header" {
                                            append_regex_list(&mut sx.headers, new_items);
                                        } else {
                                            append_regex_list(&mut sx.magics, new_items);
                                        }
                                        break;
                                    }
                                    cur = sx.next.as_mut();
                                }
                            });
                        }
                    }
                    drop_open = true;
                } else {
                    // Store for later processing
                    let nanorc_path = get_nanorc().unwrap_or_default();
                    let new_aug = Box::new(AugmentStruct {
                        filename: nanorc_path,
                        lineno: current_lineno as isize,
                        data: format!("{} {}", cmd_keyword, cmd_rest),
                        next: None,
                    });
                    with_state_mut(|s| {
                        let mut cur = s.syntaxes.as_mut();
                        while let Some(sx) = cur {
                            if sx.name == syntaxname {
                                // Append to augmentations list
                                let mut tail = &mut sx.augmentations;
                                while let Some(a) = tail {
                                    tail = &mut a.next;
                                }
                                *tail = Some(new_aug);
                                break;
                            }
                            cur = sx.next.as_mut();
                        }
                    });
                    continue;
                }
            }
        }

        let mut set: i32 = 0;

        #[cfg(feature = "color")]
        {
            if keyword == "syntax" {
                if intros_only {
                    check_for_nonempty_syntax();
                    begin_new_syntax(rest_after_kw);
                } else {
                    break; // stop parsing
                }
            } else if keyword == "header" {
                if intros_only {
                    // Build regex list OUTSIDE STATE borrow, then insert
                    let is_default = with_state(|s| {
                        s.syntaxes.as_ref().map(|sx| sx.name == "default").unwrap_or(false)
                    });
                    if let Some(new_items) = grab_and_store_build("header", rest_after_kw, is_default) {
                        with_state_mut(|s| {
                            if let Some(ref mut sx) = s.syntaxes {
                                append_regex_list(&mut sx.headers, new_items);
                            }
                        });
                    }
                }
            } else if keyword == "magic" {
                #[cfg(feature = "libmagic")]
                if intros_only {
                    let is_default = with_state(|s| {
                        s.syntaxes.as_ref().map(|sx| sx.name == "default").unwrap_or(false)
                    });
                    if let Some(new_items) = grab_and_store_build("magic", rest_after_kw, is_default) {
                        with_state_mut(|s| {
                            if let Some(ref mut sx) = s.syntaxes {
                                append_regex_list(&mut sx.magics, new_items);
                            }
                        });
                    }
                }
                #[cfg(not(feature = "libmagic"))]
                let _ = rest_after_kw;
            } else if just_syntax
                && (keyword == "set"
                    || keyword == "unset"
                    || keyword == "bind"
                    || keyword == "unbind"
                    || keyword == "include"
                    || keyword == "extendsyntax")
            {
                if intros_only {
                    jot_error(&format!("Command \"{}\" not allowed in included file", keyword));
                } else {
                    break;
                }
            } else if intros_only
                && (keyword == "color"
                    || keyword == "icolor"
                    || keyword == "comment"
                    || keyword == "tabgives"
                    || keyword == "linter"
                    || keyword == "formatter")
            {
                if !get_opensyntax() {
                    jot_error(&format!(
                        "A '{}' command requires a preceding 'syntax' command",
                        keyword
                    ));
                }
                // icolor or color → mark seen
                if keyword == "color" || keyword == "icolor" {
                    set_seen_color_command(true);
                }
                // drop_open handling at the bottom
                if drop_open {
                    set_opensyntax(false);
                }
                continue;
            } else if parse_syntax_commands(keyword, rest_after_kw) {
                // handled
            } else if keyword == "include" {
                parse_includes(rest_after_kw);
            } else {
                // Fall through to set/unset/bind/unbind
                if keyword == "set" {
                    set = 1;
                } else if keyword == "unset" {
                    set = -1;
                } else if keyword == "bind" {
                    #[cfg(feature = "nanorc")]
                    parse_binding(rest_after_kw, true);
                } else if keyword == "unbind" {
                    #[cfg(feature = "nanorc")]
                    parse_binding(rest_after_kw, false);
                } else if intros_only {
                    jot_error(&format!("Command \"{}\" not understood", keyword));
                }
            }

            if drop_open {
                set_opensyntax(false);
            }
        }

        #[cfg(not(feature = "color"))]
        {
            if keyword == "set" {
                set = 1;
            } else if keyword == "unset" {
                set = -1;
            } else if keyword == "bind" {
                #[cfg(feature = "nanorc")]
                parse_binding(rest_after_kw, true);
            } else if keyword == "unbind" {
                #[cfg(feature = "nanorc")]
                parse_binding(rest_after_kw, false);
            } else if intros_only {
                jot_error(&format!("Command \"{}\" not understood", keyword));
            }
        }

        if set == 0 {
            continue;
        }

        check_for_nonempty_syntax();

        if rest_after_kw.is_empty() {
            jot_error("Missing option");
            continue;
        }

        let (option, rest_after_opt) = parse_next_word(rest_after_kw);

        // Find the option in the table
        let opt_entry = RCOPTS.iter().find(|o| o.name == option);

        if opt_entry.is_none() {
            jot_error(&format!("Unknown option: {}", option));
            continue;
        }
        let opt = opt_entry.unwrap();

        // If the option has a flag, set or unset it
        if opt.flag != 0 {
            if set == 1 {
                SET!(opt.flag);
            } else {
                UNSET!(opt.flag);
            }
            continue;
        }

        // Options that take arguments cannot be unset
        if set == -1 {
            jot_error(&format!("Cannot unset option \"{}\"", option));
            continue;
        }

        if rest_after_opt.is_empty() {
            jot_error(&format!("Option \"{}\" requires an argument", option));
            continue;
        }

        // Parse the argument — parse_argument strips outer quotes
        let (argument, _) = match parse_argument(rest_after_opt) {
            None => continue,
            Some(pair) => pair,
        };

        // Validate UTF-8 (when in a UTF-8 locale — always true in Rust)
        // Rust strings are always valid UTF-8, so no extra check needed.

        // Dispatch on option name
        #[cfg(feature = "color")]
        match option {
            "titlecolor"    => set_interface_color(TITLE_BAR, argument),
            "numbercolor"   => set_interface_color(LINE_NUMBER, argument),
            "stripecolor"   => set_interface_color(GUIDE_STRIPE, argument),
            "scrollercolor" => set_interface_color(SCROLL_BAR, argument),
            "selectedcolor" => set_interface_color(SELECTED_TEXT, argument),
            "spotlightcolor" => set_interface_color(SPOTLIGHTED, argument),
            "minicolor"     => set_interface_color(MINI_INFOBAR, argument),
            "promptcolor"   => set_interface_color(PROMPT_BAR, argument),
            "statuscolor"   => set_interface_color(STATUS_BAR, argument),
            "errorcolor"    => set_interface_color(ERROR_MESSAGE, argument),
            "keycolor"      => set_interface_color(KEY_COMBO, argument),
            "functioncolor" => set_interface_color(FUNCTION_TAG, argument),
            _ => { handle_non_color_option(option, argument); }
        }

        #[cfg(not(feature = "color"))]
        handle_non_color_option(option, argument);
    }

    if intros_only {
        check_for_nonempty_syntax();
    }

    set_lineno(0);
}

/// Handle all non-color set options.
fn handle_non_color_option(option: &str, argument: &str) {
    use crate::utils::parse_num;
    use crate::chars::{has_blank_char, mbstrlen, char_length};

    #[cfg(feature = "operatingdir")]
    if option == "operatingdir" {
        state_mut().operating_dir = Some(argument.to_string());
        return;
    }

    #[cfg(any(feature = "wrapping", feature = "justify"))]
    if option == "fill" {
        match parse_num(argument) {
            Some(n) => {
                state_mut().fill = n;
            }
            None => {
                jot_error(&format!("Requested fill size \"{}\" is invalid", argument));
                state_mut().fill = -(COLUMNS_FROM_EOL as isize);
            }
        }
        return;
    }

    #[cfg(not(feature = "tiny"))]
    if option == "matchbrackets" {
        if has_blank_char(argument) {
            jot_error("Non-blank characters required");
        } else if mbstrlen(argument) % 2 != 0 {
            jot_error("Even number of characters required");
        } else {
            state_mut().matchbrackets = Some(argument.to_string());
        }
        return;
    }

    #[cfg(not(feature = "tiny"))]
    if option == "whitespace" {
        if mbstrlen(argument) != 2 || crate::utils::breadth(argument) != 2 {
            jot_error("Two single-column characters required");
        } else {
            with_state_mut(|s| {
                s.whitespace = Some(argument.to_string());
                let wlen0 = char_length(argument);
                let wlen1 = char_length(&argument[wlen0..]);
                s.whitelen[0] = wlen0 as i32;
                s.whitelen[1] = wlen1 as i32;
            });
        }
        return;
    }

    #[cfg(feature = "justify")]
    {
        if option == "punct" {
            if has_blank_char(argument) {
                jot_error("Non-blank characters required");
            } else {
                state_mut().punct = Some(argument.to_string());
            }
            return;
        }
        if option == "brackets" {
            if has_blank_char(argument) {
                jot_error("Non-blank characters required");
            } else {
                state_mut().brackets = Some(argument.to_string());
            }
            return;
        }
        if option == "quotestr" {
            state_mut().quotestr = Some(argument.to_string());
            return;
        }
    }

    #[cfg(feature = "speller")]
    if option == "speller" {
        state_mut().alt_speller = Some(argument.to_string());
        return;
    }

    #[cfg(not(feature = "tiny"))]
    {
        if option == "backupdir" {
            state_mut().backup_dir = Some(argument.to_string());
            return;
        }
        if option == "wordchars" {
            state_mut().word_chars = Some(argument.to_string());
            return;
        }
        if option == "guidestripe" {
            match parse_num(argument) {
                Some(n) if n > 0 => {
                    state_mut().stripe_column = n;
                }
                _ => {
                    jot_error(&format!("Guide column \"{}\" is invalid", argument));
                    state_mut().stripe_column = 0;
                }
            }
            return;
        }
        if option == "tabsize" {
            match parse_num(argument) {
                Some(n) if n > 0 => {
                    state_mut().tabsize = n;
                }
                _ => {
                    jot_error(&format!("Requested tab size \"{}\" is invalid", argument));
                    state_mut().tabsize = -1;
                }
            }
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// parse_one_nanorc — open and parse one nanorc file
// ---------------------------------------------------------------------------

/* C: void parse_one_nanorc(void) */
pub fn parse_one_nanorc() {
    let path = match get_nanorc() {
        None => return,
        Some(p) => p,
    };

    match File::open(&path) {
        Ok(f) => {
            parse_rcfile(BufReader::new(f), false, true);
        }
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                jot_error(&format!("Error reading {}: {}", path, e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// have_nanorc — check if path/name is a readable file
// ---------------------------------------------------------------------------

/* C: bool have_nanorc(const char *path, const char *name) */
fn have_nanorc(path: Option<&str>, name: &str) -> bool {
    if let Some(dir) = path {
        let full = crate::utils::concatenate(dir, name);
        if is_good_file(&full) {
            set_nanorc(Some(full));
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// do_rcfiles — entry point
// ---------------------------------------------------------------------------

/* C: void do_rcfiles(void) */
pub fn do_rcfiles() {
    let custom = with_state(|s| {
        #[cfg(feature = "nanorc")]
        { s.custom_nanorc.clone() }
        #[cfg(not(feature = "nanorc"))]
        { None::<String> }
    });

    if let Some(ref custom_path) = custom {
        let full = crate::files::get_full_path(custom_path);
        match full {
            None => die("Specified rcfile does not exist\n"),
            Some(ref p) => {
                if !Path::new(p).exists() {
                    die("Specified rcfile does not exist\n");
                }
                set_nanorc(Some(p.clone()));
                if is_good_file(p) {
                    parse_one_nanorc();
                }
            }
        }
    } else {
        // System-wide nanorc
        if have_nanorc(Some(SYSCONFDIR), "/nanorc") {
            parse_one_nanorc();
        }

        crate::utils::get_homedir();

        let homedir = state().homedir.clone();
        let xdgconfdir = std::env::var("XDG_CONFIG_HOME").ok();

        // Try user nanorc in priority order
        let found = have_nanorc(homedir.as_deref(), &format!("/{}", HOME_RC_NAME))
            || have_nanorc(xdgconfdir.as_deref(), &format!("/nano/{}", RCFILE_NAME))
            || have_nanorc(homedir.as_deref(), &format!("/.config/nano/{}", RCFILE_NAME));

        if found {
            parse_one_nanorc();
        } else if homedir.is_none() && xdgconfdir.is_none() {
            jot_error("I can't find my home directory!  Wah!");
        }
    }

    check_vitals_mapped();

    set_nanorc(None);
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "color")]
    use super::expand_glob;

    #[cfg(feature = "nanorc")]
    fn reset_binding_test_state(menu: u32) {
        use crate::global::with_state_mut;

        with_state_mut(|state| {
            state.currmenu = menu;
            state.sclist.clear();
            state.commandname = None;
            state.planted_shortcut = None;
            state.startup_problem = None;
        });
        super::ERROR_LIST.with(|errors| errors.borrow_mut().clear());
        #[cfg(feature = "color")]
        super::set_opensyntax(false);
    }

    #[cfg(feature = "color")]
    #[test]
    fn include_globs_cover_components_classes_unicode_and_dot_rules() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["a", "b"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
            std::fs::write(root.path().join(directory).join("x.nanorc"), b"").unwrap();
        }
        std::fs::write(root.path().join("é.nanorc"), b"").unwrap();
        std::fs::write(root.path().join(".hidden.nanorc"), b"").unwrap();

        let nested = format!("{}/[ab]/*.nanorc", root.path().display());
        assert_eq!(expand_glob(&nested).len(), 2);

        let unicode = format!("{}/?.nanorc", root.path().display());
        assert_eq!(
            expand_glob(&unicode),
            vec![root.path().join("é.nanorc").to_string_lossy().into_owned()]
        );

        let visible = format!("{}/*.nanorc", root.path().display());
        assert!(!expand_glob(&visible).iter().any(|path| path.contains(".hidden")));
    }

    #[cfg(feature = "nanorc")]
    #[test]
    fn quoted_function_name_remains_a_literal_string_bind() {
        use crate::definitions::MWHEREIS;
        use crate::definitions::FuncPtr;
        use crate::global::state;

        reset_binding_test_state(MWHEREIS);
        super::parse_binding("M-X \"left\" search", true);

        let state = state();
        let binding = state.sclist.iter().find(|entry| entry.keystr == "M-X")
            .expect("quoted string binding");
        assert_eq!(binding.func, Some(super::implant_sentinel as FuncPtr));
        assert_eq!(binding.expansion.as_deref(), Some("left"));
    }

    #[cfg(feature = "nanorc")]
    #[test]
    fn invalid_string_bind_menu_reports_a_configuration_error() {
        use crate::definitions::MYESNO;

        reset_binding_test_state(MYESNO);
        super::parse_binding("M-X \"text\" yesno", true);

        let errors = super::ERROR_LIST.with(|items| items.borrow().clone());
        assert!(errors.iter().any(|message| {
            message.contains("does not exist in menu 'yesno'")
        }));
    }

    #[cfg(feature = "nanorc")]
    #[test]
    fn planted_function_names_resolve_via_nanorc_function_parser() {
        use crate::definitions::{FuncPtr, MWHEREIS, PLANTED_A_COMMAND};
        use crate::global;

        reset_binding_test_state(MWHEREIS);
        crate::winio::implant("{left}{}}");

        let command_code = crate::winio::get_input(None);
        assert_eq!(command_code, PLANTED_A_COMMAND as i32);
        assert_eq!(
            global::get_shortcut(command_code),
            Some(global::do_left as FuncPtr),
        );
        assert_eq!(crate::winio::get_input(None), b'}' as i32);

        let state = global::state();
        let planted = state.planted_shortcut.expect("transient planted shortcut");
        assert_eq!(state.sclist[planted].keycode, PLANTED_A_COMMAND as i32);
        assert_eq!(state.sclist[planted].menus as u32, MWHEREIS);
        drop(state);

        crate::winio::implant("{{}");
        assert_eq!(crate::winio::get_input(None), b'{' as i32);

        #[cfg(not(feature = "tiny"))]
        {
            use crate::definitions::NO_HELP;

            crate::winio::implant("{nohelp}");
            assert_eq!(crate::winio::get_input(None), PLANTED_A_COMMAND as i32);
            let state = global::state();
            let planted = state.planted_shortcut.expect("planted toggle shortcut");
            assert_eq!(state.sclist[planted].toggle as u32, NO_HELP);
        }
    }

    #[cfg(feature = "nanorc")]
    #[test]
    fn unknown_planted_function_returns_the_dedicated_error_code() {
        use crate::definitions::{MWHEREIS, NO_SUCH_FUNCTION};

        reset_binding_test_state(MWHEREIS);
        crate::winio::implant("{not_a_nanorc_function}");

        assert_eq!(crate::winio::get_input(None), NO_SUCH_FUNCTION as i32);
        assert_eq!(crate::global::state().commandname.as_deref(), Some("not_a_nanorc_function"));
    }
}
