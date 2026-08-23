#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]
// Port of src/search.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2015-2022, 2025 Benno Schulenberg

use crate::definitions::*;
use crate::global::{state, state_mut, with_state, with_state_mut};
use crate::{ISSET, SET, TOGGLE, UNSET};
use regex::bytes::RegexBuilder;

// ---------------------------------------------------------------------------
// Module-level state (static variables in C)
// ---------------------------------------------------------------------------

// C: static bool came_full_circle = FALSE;
// C: static bool have_compiled_regexp = FALSE;
// Stored in thread_local to match the C static storage duration.
thread_local! {
    static CAME_FULL_CIRCLE: std::cell::Cell<bool> = std::cell::Cell::new(false);
    static HAVE_COMPILED_REGEXP: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

fn get_came_full_circle() -> bool {
    CAME_FULL_CIRCLE.with(|c| c.get())
}

fn set_came_full_circle(v: bool) {
    CAME_FULL_CIRCLE.with(|c| c.set(v));
}

fn get_have_compiled_regexp() -> bool {
    HAVE_COMPILED_REGEXP.with(|c| c.get())
}

fn set_have_compiled_regexp(v: bool) {
    HAVE_COMPILED_REGEXP.with(|c| c.set(v));
}

// ---------------------------------------------------------------------------
// External function stubs (implemented in other modules)
// ---------------------------------------------------------------------------

unsafe extern "Rust" {}

// These are stubs for functions defined in other modules that search.c calls.
// In a fully ported codebase these will resolve to the real implementations.

#[inline]
fn statusbar(msg: &str) {
    crate::winio::statusbar(msg)
}

#[inline]
fn statusline(mtype: MessageType, msg: &str) {
    crate::winio::statusline(mtype, msg)
}

#[inline]
fn wipe_statusbar() {
    crate::winio::wipe_statusbar()
}

#[inline]
fn edit_refresh() {
    crate::winio::edit_refresh()
}

#[inline]
fn edit_redraw(was_current: &LinePtr, mode: UpdateType) {
    crate::winio::edit_redraw(was_current, mode)
}

/// C: size_t xplustabs(void) — winio.c — column of the cursor in the current line.
#[inline]
fn xplustabs() -> usize {
    crate::utils::xplustabs()
}

/// C: size_t wideness(const char *text, size_t maxlen) — chars.c.
#[inline]
fn wideness<T: AsRef<[u8]> + ?Sized>(data: &T, x: usize) -> usize {
    crate::utils::wideness(data, x)
}

/// C: size_t breadth(const char *text) — chars.c — display width of text.
#[inline]
fn breadth<T: AsRef<[u8]> + ?Sized>(data: &T) -> usize {
    crate::utils::breadth(data)
}

/// C: char *display_string(…) — winio.c.
#[inline]
fn display_string<T: AsRef<[u8]> + ?Sized>(
    s: &T,
    column: usize,
    span: usize,
    isdata: bool,
    isprompt: bool,
) -> String {
    crate::winio::display_string(s, column, span, isdata, isprompt)
}

/// C: size_t actual_x(const char *text, size_t column) — chars.c.
#[inline]
fn actual_x<T: AsRef<[u8]> + ?Sized>(data: &T, column: usize) -> usize {
    crate::utils::actual_x(data, column)
}

/// C: size_t char_length(const char *s) — chars.c.
#[inline]
fn char_length<T: AsRef<[u8]> + ?Sized>(s: &T) -> usize {
    crate::chars::char_length(s)
}

/// C: size_t step_left(const char *buf, size_t pos) — chars.c.
#[inline]
fn step_left<T: AsRef<[u8]> + ?Sized>(data: &T, x: usize) -> usize {
    crate::chars::step_left(data, x)
}

/// C: size_t step_right(const char *buf, size_t pos) — chars.c.
#[inline]
fn step_right<T: AsRef<[u8]> + ?Sized>(data: &T, x: usize) -> usize {
    crate::chars::step_right(data, x)
}

/// C: size_t mbstrlen(const char *s) — chars.c.
#[inline]
fn mbstrlen<T: AsRef<[u8]> + ?Sized>(s: &T) -> usize {
    crate::chars::mbstrlen(s)
}

/// C: size_t get_page_start(size_t column) — utils.c.
#[inline]
fn get_page_start(col: usize) -> usize {
    crate::utils::get_page_start(col)
}

/// C: print_view_warning() — nano.c.
#[inline]
fn print_view_warning() {
    crate::nano::print_view_warning()
}

fn do_prompt(
    menu: u32,
    initial: &str,
    history_kind: Option<crate::history::HistoryKind>,
    refresh_fn: fn(),
    prompt: &str,
) -> i32 {
    crate::prompt::do_prompt(menu, Some(initial), history_kind, Some(refresh_fn), prompt)
}

fn ask_user(yesorallorno: bool, question: &str) -> i32 {
    crate::prompt::ask_user(yesorallorno, question)
}

#[inline]
fn set_modified() {
    crate::files::set_modified()
}

fn parse_line_column(input: &str, line: &mut isize, col: &mut isize) -> bool {
    // Mirror C utils.c:parse_line_column — return TRUE only when the REQUIRED parts
    // actually parse.  In particular, with "line,col" BOTH must parse, so an invalid
    // column is not silently accepted (the tuple-returning utils helper cannot
    // distinguish "no column" from "invalid column", hence the inline logic here).
    let s = input.trim_start_matches(' ');
    match s.find(|c| c == ',' || c == '.' || c == ':') {
        None => match crate::utils::parse_num(s) {
            Some(ln) => {
                *line = ln;
                true
            }
            None => false,
        },
        Some(pos) => {
            let col_ok = match crate::utils::parse_num(&s[pos + 1..]) {
                Some(cn) => {
                    *col = cn;
                    true
                }
                None => false,
            };
            if pos == 0 {
                // Separator at the very start: only the column is given.
                col_ok
            } else {
                // Both the line part and the column part must parse.
                let line_ok = match crate::utils::parse_num(&s[..pos]) {
                    Some(ln) => {
                        *line = ln;
                        true
                    }
                    None => false,
                };
                line_ok && col_ok
            }
        }
    }
}

#[inline]
fn adjust_viewport(mode: UpdateType) {
    crate::winio::adjust_viewport(mode)
}

/// C: linestruct *line_from_number(ssize_t number) — utils.c.
#[inline]
fn line_from_number(n: isize) -> Option<LinePtr> {
    crate::utils::line_from_number(n)
}

fn go_forward_chunks(rows: i32, line: &mut Option<LinePtr>, leftedge: &mut usize) -> i32 {
    match line {
        Some(lp) => crate::winio::go_forward_chunks(rows, lp, leftedge),
        None => rows,
    }
}

#[inline]
fn leftedge_for(col: usize, line: &LinePtr) -> usize {
    crate::winio::leftedge_for(col, &line.borrow().data)
}

#[inline]
fn update_line(line: &LinePtr, x: usize) {
    crate::winio::update_line(line, x);
}

/// C: bool is_separate_word(size_t position, size_t length, const char *text) — chars.c.
#[cfg(feature = "speller")]
#[inline]
fn is_separate_word<T: AsRef<[u8]> + ?Sized>(position: usize, length: usize, text: &T) -> bool {
    crate::utils::is_separate_word(position, length, text)
}

#[cfg(feature = "histories")]
fn update_history(history: &Option<LinePtr>, answer: &str, prune: bool) {
    use crate::history::HistoryKind;
    // Determine which history list by comparing the pointer with the global search/replace histories.
    let kind = with_state(|s| {
        let is_replace = s
            .replace_history
            .as_ref()
            .zip(history.as_ref())
            .map(|(r, h)| LinePtr::ptr_eq(r, h))
            .unwrap_or(false);
        if is_replace {
            HistoryKind::Replace
        } else {
            HistoryKind::Search
        }
    });
    crate::history::update_history(kind, answer, prune);
}

#[cfg(feature = "color")]
#[inline]
fn check_the_multis(line: &LinePtr) {
    crate::color::check_the_multis(line)
}

#[cfg(not(feature = "tiny"))]
#[inline]
fn add_undo(kind: UndoType, msg: Option<&str>) {
    crate::text::add_undo(kind, msg)
}

#[cfg(not(feature = "tiny"))]
fn mark_is_before_cursor() -> bool {
    state().mark_is_before_cursor()
}

// ASCII case-insensitive substring search — zero allocation. Used as the fast
// path for the common (ASCII) case-insensitive plain search, replacing a
// per-line `to_lowercase()` heap allocation. Because ASCII lowercasing is
// byte-length-preserving, the returned byte offsets are identical to those from
// `haystack.to_lowercase().find(&needle.to_lowercase())`. Callers must ensure
// both slices are ASCII and the needle is non-empty.
fn ascii_ci_find(hay: &[u8], ndl: &[u8]) -> Option<usize> {
    let n = ndl.len();
    if n == 0 {
        return Some(0);
    }
    if n > hay.len() {
        return None;
    }
    let first = ndl[0].to_ascii_lowercase();
    let last = hay.len() - n;
    let mut i = 0;
    while i <= last {
        if hay[i].to_ascii_lowercase() == first && hay[i..i + n].eq_ignore_ascii_case(ndl) {
            return Some(i);
        }
        i += 1;
    }
    None
}

// Reverse of `ascii_ci_find`: the LAST match position in `hay` (largest start
// offset). Equivalent to the to_lowercase() backward loop, which keeps the last
// match found while scanning forward by one character at a time.
fn ascii_ci_rfind(hay: &[u8], ndl: &[u8]) -> Option<usize> {
    let n = ndl.len();
    if n == 0 || n > hay.len() {
        return None;
    }
    let first = ndl[0].to_ascii_lowercase();
    let mut i = hay.len() - n;
    loop {
        if hay[i].to_ascii_lowercase() == first && hay[i..i + n].eq_ignore_ascii_case(ndl) {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// The three search flags that are invariant for the whole duration of one
/// findnextstr() call (they are only ever written in search_init / do_search_*,
/// before findnextstr runs). Read once and passed by value so the per-line hot
/// loop and strstrwrapper stop re-entering the thread-local STATE to re-read
/// them on every line — matching C, which reads BSS globals at zero cost.
#[derive(Clone, Copy)]
struct SearchFlags {
    use_regexp: bool,
    backwards: bool,
    case_sensitive: bool,
}

impl SearchFlags {
    #[inline]
    fn current() -> Self {
        SearchFlags {
            use_regexp: ISSET!(USE_REGEXP),
            backwards: ISSET!(BACKWARDS_SEARCH),
            case_sensitive: ISSET!(CASE_SENSITIVE),
        }
    }
}

#[cfg(test)]
fn unicode_ci_match_end_at(haystack: &str, start: usize, folded_needle: &str) -> Option<usize> {
    if folded_needle.is_empty() {
        return Some(start);
    }

    let mut folded = String::new();
    for (relative, ch) in haystack[start..].char_indices() {
        folded.extend(ch.to_lowercase());
        if folded.len() >= folded_needle.len() {
            return (folded == folded_needle).then_some(start + relative + ch.len_utf8());
        }
        if !folded_needle.starts_with(&folded) {
            return None;
        }
    }

    None
}

#[cfg(test)]
fn unicode_ci_find(haystack: &str, needle: &str) -> Option<usize> {
    let folded_needle = needle.to_lowercase();
    if folded_needle.is_empty() {
        return Some(0);
    }

    for (start, _) in haystack.char_indices() {
        if unicode_ci_match_end_at(haystack, start, &folded_needle).is_some() {
            return Some(start);
        }
    }

    None
}

#[cfg(test)]
fn unicode_ci_rfind(haystack: &str, needle: &str) -> Option<usize> {
    unicode_ci_rfind_at_or_before(haystack, needle, haystack.len())
}

#[cfg(test)]
fn unicode_ci_rfind_at_or_before(haystack: &str, needle: &str, ceiling: usize) -> Option<usize> {
    let folded_needle = needle.to_lowercase();
    if folded_needle.is_empty() {
        return None;
    }

    let mut last_match = None;
    for (start, _) in haystack.char_indices() {
        if start > ceiling {
            break;
        }
        if unicode_ci_match_end_at(haystack, start, &folded_needle).is_some() {
            last_match = Some(start);
        }
    }

    last_match
}

// ---------------------------------------------------------------------------
// strstrwrapper — search for needle in haystack starting from pos
// ---------------------------------------------------------------------------
// C: const char *strstrwrapper(const char *data, const char *needle, const char *from)
// Returns the byte offset of the match within `data`, or None.
// (Not NANO_TINY-gated: findnextstr — always compiled — calls it, matching C.)
fn strstrwrapper<D: AsRef<[u8]> + ?Sized, N: AsRef<[u8]> + ?Sized>(
    data: &D,
    needle: &N,
    from_offset: usize,
    _lowered_needle: Option<&str>,
    flags: SearchFlags,
) -> Option<usize> {
    let data = data.as_ref();
    let needle = needle.as_ref();
    if from_offset > data.len() {
        return None;
    }

    let use_regexp = flags.use_regexp;
    let backwards = flags.backwards;

    if use_regexp {
        // Regex search.
        let result = with_state(|s| {
            if let Some(ref re) = s.search_regexp {
                if backwards {
                    // Find the last match whose start is at or before the
                    // ceiling.  Restart one character after each match start
                    // so overlapping matches remain visible.
                    let mut last_match: Option<(usize, usize)> = None;
                    let mut next_rung = 0;
                    while let Some(m) = re.find_at(data, next_rung) {
                        if m.start() > from_offset {
                            break;
                        }
                        last_match = Some((m.start(), m.end()));
                        if m.start() == from_offset || m.start() == data.len() {
                            break;
                        }
                        next_rung = step_right(&data, m.start());
                    }
                    // Store regmatches for the found match.
                    if let Some((start, _end)) = last_match {
                        return Some(start);
                    }
                    None
                } else {
                    // Search forward from from_offset.
                    if let Some(m) = re.find_at(data, from_offset) {
                        // Update regmatches in STATE.
                        Some(m.start())
                    } else {
                        None
                    }
                }
            } else {
                None
            }
        });

        // Update regmatches if we found something. Compute the ten capture spans
        // against a read borrow of the compiled regex, then store them in one
        // assignment — avoids cloning the whole compiled regex (which the old code
        // did on every match just to dodge the &mut-while-reading borrow).
        if let Some(match_start) = result {
            let new_matches: Option<[(usize, usize); 10]> = with_state(|s| {
                let re = s.search_regexp.as_ref()?;
                let caps = re.captures_at(data, match_start)?;
                if caps.get(0)?.start() != match_start {
                    return None;
                }
                let mut rm = [(0usize, 0usize); 10];
                for i in 0..10 {
                    if let Some(m) = caps.get(i) {
                        rm[i] = (m.start(), m.end());
                    }
                }
                Some(rm)
            });
            if let Some(rm) = new_matches {
                with_state_mut(|s| s.regmatches = rm);
            }
        }

        result
    } else {
        // Plain string search.
        let case_sensitive = flags.case_sensitive;

        if backwards {
            // GNU's case-sensitive literal search is deliberately bytewise,
            // even when that means a match starts inside valid UTF-8.
            if case_sensitive {
                if needle.is_empty() {
                    Some(from_offset.min(data.len()))
                } else {
                    let last_start = from_offset.min(data.len().saturating_sub(needle.len()));
                    data[..last_start.saturating_add(needle.len())]
                        .windows(needle.len())
                        .rposition(|part| part == needle)
                        .filter(|&at| at <= from_offset)
                }
            } else if !needle.is_empty() && data.is_ascii() && needle.is_ascii() {
                // Common case: ASCII, case-insensitive. Scan in place with no
                // allocation. ASCII case-folding is byte-length-preserving, so this
                // yields the exact same last-match offset as the to_lowercase() path.
                let search_end = from_offset.saturating_add(needle.len()).min(data.len());
                ascii_ci_rfind(&data[..search_end], needle).filter(|&start| start <= from_offset)
            } else {
                crate::chars::mbrevstrcasestr(&data, &needle, from_offset)
            }
        } else {
            // Find first occurrence at or after from_offset.
            let search_slice = &data[from_offset..];
            let found = if case_sensitive {
                if needle.is_empty() {
                    Some(0)
                } else {
                    search_slice
                        .windows(needle.len())
                        .position(|part| part == needle)
                }
            } else if !needle.is_empty() && search_slice.is_ascii() && needle.is_ascii() {
                // Common case: ASCII, case-insensitive — zero-alloc in-place scan.
                ascii_ci_find(search_slice, needle)
            } else {
                crate::chars::mbstrcasestr(&search_slice, &needle)
            };
            found.map(|pos| from_offset + pos)
        }
    }
}

// ---------------------------------------------------------------------------
// regexp_init — compile the search regular expression
// ---------------------------------------------------------------------------
/* C: bool regexp_init(const char *regexp) */
pub fn regexp_init(regexp: &str) -> bool {
    let case_sensitive = ISSET!(CASE_SENSITIVE);

    let result = RegexBuilder::new(regexp)
        .case_insensitive(!case_sensitive)
        .unicode(crate::chars::using_utf8())
        .dot_matches_new_line(true)
        .build();

    match result {
        Ok(re) => {
            with_state_mut(|s| {
                s.search_regexp = Some(re);
            });
            set_have_compiled_regexp(true);
            true
        }
        Err(e) => {
            let msg = format!("Bad regex \"{}\": {}", regexp, e);
            statusline(MessageType::Ahem, &msg);
            false
        }
    }
}

// ---------------------------------------------------------------------------
// tidy_up_after_search — free compiled regexp and schedule refresh
// ---------------------------------------------------------------------------
/* C: void tidy_up_after_search(void) */
pub fn tidy_up_after_search() {
    if get_have_compiled_regexp() {
        with_state_mut(|s| {
            s.search_regexp = None;
        });
        set_have_compiled_regexp(false);
    }

    #[cfg(not(feature = "tiny"))]
    {
        let has_mark = with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some());
        if has_mark {
            state_mut().refresh_needed = true;
        }
    }

    #[cfg(feature = "color")]
    {
        with_state_mut(|s| {
            s.recook |= s.perturbed;
        });
    }
}

// ---------------------------------------------------------------------------
// search_init — prompt the user and initiate search or replace
// ---------------------------------------------------------------------------
/* C: void search_init(bool replacing, bool retain_answer) */
pub fn search_init(replacing: bool, retain_answer: bool) {
    // Build the default string (last searched text, truncated).
    let thedefault: String = {
        let last = state().last_search.clone();
        if !last.is_empty() {
            // C truncates against the full terminal width (COLS / 3), not
            // the edit-window width; with margins active the port's
            // editwincols cut the hint far too early.
            let cols = crate::winio::get_cols().max(1);
            let disp = display_string(&last, 0, cols / 3, false, false);
            let is_long = breadth(&last) > cols / 3;
            format!(" [{}{}]", disp, if is_long { "..." } else { "" })
        } else {
            String::new()
        }
    };

    let mut replacing = replacing;
    let mut retain_answer = retain_answer;

    loop {
        // Build the prompt string components.
        let case_sensitive_str = if ISSET!(CASE_SENSITIVE) {
            " [Case sensitive]"
        } else {
            ""
        };
        let regexp_str = if ISSET!(USE_REGEXP) {
            " [Reg.exp.]"
        } else {
            ""
        };
        let backwards_str = if ISSET!(BACKWARDS_SEARCH) {
            " [Backwards]"
        } else {
            ""
        };
        let replace_str = if replacing {
            #[cfg(not(feature = "tiny"))]
            {
                let in_sel =
                    with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some());
                if in_sel {
                    " (to replace) in selection"
                } else {
                    " (to replace)"
                }
            }
            #[cfg(feature = "tiny")]
            " (to replace)"
        } else {
            ""
        };

        let prompt = format!(
            "Search{}{}{}{}{}",
            case_sensitive_str, regexp_str, backwards_str, replace_str, thedefault
        );

        let menu = {
            let inhelp = state().inhelp;
            if inhelp {
                MFINDINHELP
            } else if replacing {
                MREPLACE
            } else {
                MWHEREIS
            }
        };

        let initial = if retain_answer {
            state().answer.clone()
        } else {
            String::new()
        };

        let response = do_prompt(
            menu,
            &initial,
            Some(crate::history::HistoryKind::Search),
            crate::winio::edit_refresh,
            &prompt,
        );

        let last_search_empty = state().last_search.is_empty();

        // If the search was cancelled, or we have a blank answer and
        // nothing was searched for yet during this session, get out.
        if response == -1 || (response == -2 && last_search_empty) {
            statusbar("Cancelled");
            break;
        }

        // If Enter was pressed, prepare to do a replace or a search.
        if response == 0 || response == -2 {
            let answer = state().answer.clone();
            if !answer.is_empty() {
                with_state_mut(|s| {
                    s.last_search = answer.clone();
                });
                #[cfg(feature = "histories")]
                {
                    let hist = state().search_history.clone();
                    update_history(&hist, &answer, PRUNE_DUPLICATE);
                }
            }

            if ISSET!(USE_REGEXP) {
                let ls = state().last_search.clone();
                if !regexp_init(&ls) {
                    break;
                }
            }

            if replacing {
                ask_for_and_do_replacements();
            } else {
                go_looking();
            }

            break;
        }

        retain_answer = true;

        let function = crate::global::func_from_key(response);

        // If we're here, one of the five toggles was pressed, or
        // a shortcut was executed.
        if function == Some(crate::global::case_sens_void as FuncPtr) {
            TOGGLE!(CASE_SENSITIVE);
        } else if function == Some(crate::global::backwards_void as FuncPtr) {
            TOGGLE!(BACKWARDS_SEARCH);
        } else if function == Some(crate::global::regexp_void as FuncPtr) {
            TOGGLE!(USE_REGEXP);
        } else if function == Some(crate::global::flip_replace as FuncPtr) {
            if ISSET!(VIEW_MODE) {
                print_view_warning();
                crate::winio::napms(600);
            } else {
                replacing = !replacing;
            }
        } else if function == Some(crate::global::flip_goto as FuncPtr) {
            // Switch to the Go-To-Line prompt, handing over the typed text.
            let answer = state().answer.clone();
            ask_for_line_and_column(&answer);
            break;
        } else {
            break;
        }
    }

    let inhelp = state().inhelp;
    if !inhelp {
        tidy_up_after_search();
    }
}

// ---------------------------------------------------------------------------
// findnextstr — search for needle starting at openfile->current/current_x
// ---------------------------------------------------------------------------

/// Consume currently queued input while a long search is running and report
/// whether it contains the menu's Cancel shortcut.  A zero-timeout poll keeps
/// this function from ever blocking the search.
fn search_cancel_requested() -> bool {
    loop {
        if crate::winio::waiting_keycodes() == 0 {
            if !crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                return false;
            }
            crate::winio::read_keys_from();
            if crate::winio::waiting_keycodes() == 0 {
                continue;
            }
        }

        let mut input = crate::winio::get_input(None);
        if input == ESC_CODE as i32 {
            if crate::winio::waiting_keycodes() == 0 {
                state_mut().meta_key = false;
                continue;
            }
            input = crate::winio::get_input(None);
            state_mut().meta_key = true;
        } else {
            state_mut().meta_key = false;
        }

        let cancelled = crate::global::func_from_key(input)
            .is_some_and(|f| f == crate::global::do_cancel as FuncPtr);
        state_mut().meta_key = false;

        if cancelled {
            while crate::winio::waiting_keycodes() > 0 {
                let _ = crate::winio::get_input(None);
            }
            return true;
        }
    }
}

/* C: int findnextstr(const char *needle, bool whole_word_only, int modus,
size_t *match_len, bool skipone,
const linestruct *begin, size_t begin_x) */
// Returns: 1=found, 0=not found, -2=cancelled
pub fn findnextstr(
    needle: &str,
    whole_word_only: bool,
    modus: i32,
    match_len: &mut usize,
    skipone: bool,
    begin: Option<&LinePtr>,
    begin_x: usize,
) -> i32 {
    // The length of a match (recomputed for regex).
    let mut found_len = needle.len();
    // When > 0, show "Searching..." message.
    let mut feedback: i32 = 0;

    // Set came_full_circle to false when starting a new search.
    if begin.is_none() {
        set_came_full_circle(false);
    }

    // Collect what we need from state.
    let (current_line_ptr, start_x) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (lp, x)
    });

    let mut line: Option<LinePtr> = current_line_ptr;
    let mut from_offset: usize = start_x;
    let mut skipone = skipone;

    // Record the begin line number for full-circle detection.
    let begin_lineno: isize = begin.map(|b| b.borrow().lineno).unwrap_or(-1);

    // Record the time for "Searching..." feedback. The monotonic clock is
    // sampled only once every CLOCK_CHECK_INTERVAL scanned lines: Instant::now()
    // is a clock_gettime, which on some platforms (notably WSL2/ARM64) traps to a
    // real syscall instead of the vDSO and — when called once per line — utterly
    // dominated full-buffer search CPU (~89% of samples in a profile; the actual
    // matching was ~6%). The check only drives the cosmetic "Searching..." status
    // and a (stubbed) cancel poll, so coarser sampling is behavior-equivalent for
    // search results. C affords a per-line time(NULL) because that hits the cheap
    // vDSO path.
    let mut lastkbcheck = std::time::Instant::now();
    let mut lines_since_clock: u32 = 0;
    const CLOCK_CHECK_INTERVAL: u32 = 1024;

    // Read the loop-invariant search flags once (they can't change mid-search),
    // so the per-line loop and strstrwrapper never re-enter the thread-local
    // STATE to re-read them. came_full_circle is NOT hoisted: it is mutated
    // mid-loop on wrap-around, so it stays read per-iteration.
    let flags = SearchFlags::current();

    // Pre-lower the needle once per search for the case-insensitive plain-search
    // fallback path (non-ASCII lines), instead of re-lowering it on every line
    // scanned. The ASCII fast path inside strstrwrapper needs no lowered needle.
    let lowered_needle: Option<String> = if !flags.use_regexp && !flags.case_sensitive {
        Some(needle.to_lowercase())
    } else {
        None
    };
    let lowered_needle = lowered_needle.as_deref();

    loop {
        let current_line = match line {
            Some(ref l) => l.clone(),
            None => {
                // Shouldn't happen but be safe.
                return 0;
            }
        };

        let backwards = flags.backwards;

        // Scan this line for the needle against a borrow of its data. The common
        // case is "no match", so borrowing avoids cloning the whole line String on
        // every line walked (the dominant per-line cost of a full-buffer search).
        let found_offset: Option<usize> = {
            let guard = current_line.borrow();
            let ld: &[u8] = guard.data.as_bytes();
            if skipone {
                skipone = false;
                if backwards && from_offset != 0 {
                    let new_from = step_left(ld, from_offset);
                    strstrwrapper(ld, needle, new_from, lowered_needle, flags).filter(|&pos| {
                        if backwards {
                            pos <= new_from
                        } else {
                            pos >= new_from
                        }
                    })
                } else if !backwards && from_offset < ld.len() {
                    let new_from = from_offset + char_length(&ld[from_offset..]);
                    strstrwrapper(ld, needle, new_from, lowered_needle, flags)
                } else {
                    None
                }
            } else {
                strstrwrapper(ld, needle, from_offset, lowered_needle, flags)
            }
        };

        // Filter for backwards: strstrwrapper with backwards returns last match
        // before from_offset; make sure it actually found one within range.
        let found_offset = found_offset.filter(|_| {
            // strstrwrapper already handles backwards, just pass through.
            true
        });

        if let Some(found_x) = found_offset {
            // A match was found (rare relative to lines scanned) — now materialize
            // the line data so the found-handling below is unchanged.
            let line_data = current_line.borrow().data.clone();

            // When doing regex search, compute the length of the match.
            if flags.use_regexp {
                let (rm_so, rm_eo) = state().regmatches[0];
                found_len = rm_eo.saturating_sub(rm_so);
            }

            #[cfg(feature = "speller")]
            if whole_word_only && !is_separate_word(found_x, found_len, &line_data) {
                // Continue looking in the rest of the line.
                from_offset = found_x + char_length(&line_data[found_x..]);
                continue;
            }

            // When not on the magic line, the match is valid.
            let is_magic_line = {
                let has_next = current_line.borrow().next.is_none();
                let data_empty = line_data.is_empty();
                has_next && data_empty
            };

            if !is_magic_line {
                // Ensure the found occurrence is not beyond the starting x — once the
                // search has wrapped around (came_full_circle), a match at/after the
                // original position must not be accepted (C search.c:311-315).
                // begin_x and found_x are both byte offsets into the line.
                if get_came_full_circle()
                    && ((!backwards
                        && (found_x > begin_x || (modus == REPLACING && found_x == begin_x)))
                        || (backwards && found_x < begin_x))
                {
                    return 0;
                }

                // Found it. Update state.
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current = Some(current_line.clone());
                        of.current_x = found_x;
                    }
                });

                *match_len = found_len;

                // Spotlight the match if just finding.
                #[cfg(not(feature = "tiny"))]
                if modus == JUSTFIND {
                    let no_mark = with_state(|s| {
                        s.openfile
                            .as_ref()
                            .map(|f| f.mark.is_none() || f.softmark)
                            .unwrap_or(true)
                    });
                    if no_mark {
                        let from_col = xplustabs();
                        let to_col = wideness(&line_data, found_x + found_len);
                        with_state_mut(|s| {
                            s.spotlighted = true;
                            s.light_from_col = from_col;
                            s.light_to_col = to_col;

                            let editwincols = s.editwincols as usize;
                            if s.united_sidescroll && to_col < editwincols.saturating_sub(CUSHION) {
                                if let Some(ref mut of) = s.openfile {
                                    of.brink = 0;
                                }
                            } else if s.united_sidescroll {
                                let page = get_page_start(to_col);
                                if let Some(ref mut of) = s.openfile {
                                    of.brink = page;
                                }
                            }

                            s.refresh_needed = true;
                        });
                    }
                }

                if feedback > 0 {
                    wipe_statusbar();
                }

                return 1;
            }
        }

        // Check for window resize.
        if crate::winio::consume_resize_request(None) {
            statusbar("Searching...");
            feedback = 1;
        }

        // If we're back at the beginning, there is no needle.
        if get_came_full_circle() {
            return 0;
        }

        // Move to the previous or next line.
        let backwards = flags.backwards;
        let next_line: Option<LinePtr> = if backwards {
            current_line
                .borrow()
                .prev
                .as_ref()
                .and_then(|w| w.upgrade())
        } else {
            current_line.borrow().next.clone()
        };

        if next_line.is_none() {
            // Reached the start or end of buffer — wrap around.
            if whole_word_only || modus == INREGION {
                return 0;
            }

            let wrap_target: Option<LinePtr> = with_state(|s| {
                s.openfile.as_ref().and_then(|f| {
                    if backwards {
                        f.filebot.clone()
                    } else {
                        f.filetop.clone()
                    }
                })
            });

            if modus == JUSTFIND {
                statusline(MessageType::Remark, "Search Wrapped");
                feedback = -2;
            }

            line = wrap_target;
        } else {
            line = next_line;
        }

        // Check if we've reached the original starting line.
        if let Some(ref l) = line {
            if l.borrow().lineno == begin_lineno {
                set_came_full_circle(true);
            }
        }

        // Set the starting position to the start or end of the new line.
        let backwards = flags.backwards;
        from_offset = if backwards {
            line.as_ref().map(|l| l.borrow().data.len()).unwrap_or(0)
        } else {
            0
        };

        // Periodically check for cancel keystroke / show "Searching...". Sample
        // the (syscall-expensive) monotonic clock only every Nth scanned line.
        lines_since_clock += 1;
        if lines_since_clock >= CLOCK_CHECK_INTERVAL {
            lines_since_clock = 0;
            if lastkbcheck.elapsed().as_secs() > 0 {
                lastkbcheck = std::time::Instant::now();

                if search_cancel_requested() {
                    crate::winio::consume_resize_request(None);
                    statusbar("Cancelled");
                    return -2;
                }

                feedback += 1;
                if feedback > 0 {
                    statusbar("Searching...");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// do_search_forward — ask for a string and search forward
// ---------------------------------------------------------------------------
/* C: void do_search_forward(void) */
pub fn do_search_forward() {
    UNSET!(BACKWARDS_SEARCH);
    search_init(false, false);
}

// ---------------------------------------------------------------------------
// do_search_backward — ask for a string and search backwards
// ---------------------------------------------------------------------------
/* C: void do_search_backward(void) */
pub fn do_search_backward() {
    SET!(BACKWARDS_SEARCH);
    search_init(false, false);
}

// ---------------------------------------------------------------------------
// do_research — search for the last string without prompting
// ---------------------------------------------------------------------------
/* C: void do_research(void) */
pub fn do_research() {
    #[cfg(feature = "histories")]
    {
        let (last_empty, has_prev) = with_state(|s| {
            let empty = s.last_search.is_empty();
            let prev = s
                .searchbot
                .as_ref()
                .and_then(|b| b.borrow().prev.as_ref().and_then(|w| w.upgrade()))
                .is_some();
            (empty, prev)
        });

        if last_empty && has_prev {
            let prev_data = with_state(|s| {
                s.searchbot
                    .as_ref()
                    .and_then(|b| b.borrow().prev.as_ref().and_then(|w| w.upgrade()))
                    .map(|p| p.borrow().data.clone())
            });
            if let Some(data) = prev_data {
                if let Some(text) = data.as_utf8() {
                    state_mut().last_search = text.to_owned();
                }
            }
        }
    }

    let last_empty = state().last_search.is_empty();
    if last_empty {
        statusline(MessageType::Ahem, "No current search pattern");
        return;
    }

    if ISSET!(USE_REGEXP) {
        let ls = state().last_search.clone();
        if !regexp_init(&ls) {
            return;
        }
    }

    // Use the search-menu key bindings to allow cancelling.
    state_mut().currmenu = MWHEREIS;

    let lines = state().editwinrows;
    if lines > 1 {
        wipe_statusbar();
    }

    go_looking();

    let inhelp = state().inhelp;
    if !inhelp {
        tidy_up_after_search();
    }
}

// ---------------------------------------------------------------------------
// do_findprevious — search backward for the next occurrence
// ---------------------------------------------------------------------------
/* C: void do_findprevious(void) */
pub fn do_findprevious() {
    SET!(BACKWARDS_SEARCH);
    do_research();
}

// ---------------------------------------------------------------------------
// do_findnext — search forward for the next occurrence
// ---------------------------------------------------------------------------
/* C: void do_findnext(void) */
pub fn do_findnext() {
    UNSET!(BACKWARDS_SEARCH);
    do_research();
}

// ---------------------------------------------------------------------------
// not_found_msg — report on the status bar that a string was not found
// ---------------------------------------------------------------------------
/* C: void not_found_msg(const char *str) */
pub fn not_found_msg(s: &str) {
    // C uses the full terminal COLS (not editwincols) and truncates with
    // actual_x(disp, wideness(disp, COLS/2)) — not breadth().min(COLS/2).
    let cols = crate::winio::get_cols().max(1);
    let disp = display_string(s, 0, (cols / 2) + 1, false, false);
    let numchars = actual_x(&disp, wideness(&disp, cols / 2));
    let truncated = &disp[..numchars];
    let ellipsis = if numchars < disp.len() { "..." } else { "" };
    statusline(
        MessageType::Ahem,
        &format!("\"{}{}\" not found", truncated, ellipsis),
    );
}

// ---------------------------------------------------------------------------
// go_looking — search for last_search; report when found only once
// ---------------------------------------------------------------------------
/* C: void go_looking(void) */
pub fn go_looking() {
    let (was_current, was_x) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (lp, x)
    });

    set_came_full_circle(false);

    let (begin_ptr, begin_x) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (lp, x)
    });

    let needle = state().last_search.clone();

    let mut match_len: usize = 0;
    let result = findnextstr(
        &needle,
        false,
        JUSTFIND,
        &mut match_len,
        true,
        begin_ptr.as_ref(),
        begin_x,
    );

    state_mut().didfind = result;

    // If found and we're back at exact same spot, this is the only occurrence.
    let (now_current, now_x) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (lp, x)
    });

    let same_spot = was_current
        .as_ref()
        .zip(now_current.as_ref())
        .map(|(w, n)| LinePtr::ptr_eq(w, n))
        .unwrap_or(false)
        && was_x == now_x;

    if result == 1 && same_spot {
        statusline(MessageType::Remark, "This is the only occurrence");
    } else if result == 0 {
        not_found_msg(&needle);
    }

    if let Some(ref was) = was_current {
        edit_redraw(was, UpdateType::Centering);
    }
}

// ---------------------------------------------------------------------------
// replace_regexp — apply regex replacement with back-references
// ---------------------------------------------------------------------------
/* C: int replace_regexp(char *string, bool create) */
// In Rust: always returns the replacement string (create=true) or just the size.
// Return replacement bytes so captured malformed input is never decoded.
pub fn replace_regexp_str() -> LineData {
    let answer = state().answer.clone();
    let mut result = LineData::empty();
    let chars: Vec<char> = answer.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            let num = (next as i32) - ('0' as i32);
            if num >= 1 && num <= 9 {
                // Check if this subgroup exists in search_regexp.
                let nsub = with_state(|s| {
                    s.search_regexp
                        .as_ref()
                        .map(|re| re.captures_len())
                        .unwrap_or(0)
                });
                if (num as usize) < nsub {
                    let (rm_so, rm_eo) = state().regmatches[num as usize];
                    let current_data = with_state(|s| {
                        s.openfile
                            .as_ref()
                            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
                            .unwrap_or_default()
                    });
                    if rm_so < current_data.len() && rm_eo <= current_data.len() {
                        result.extend_bytes(&current_data[rm_so..rm_eo]);
                    }
                    i += 2;
                    continue;
                }
            }
        }
        result.push(chars[i]);
        i += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// replace_line — return a copy of current line with one needle replaced
// ---------------------------------------------------------------------------
/* C: char *replace_line(const char *needle) */
pub fn replace_line(needle: &str) -> LineData {
    let (current_data, current_x) = with_state(|s| {
        let data = s
            .openfile
            .as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default();
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (data, x)
    });

    let use_regexp = ISSET!(USE_REGEXP);

    let match_len: usize;
    let replacement: LineData;

    if use_regexp {
        let (rm_so, rm_eo) = state().regmatches[0];
        match_len = rm_eo.saturating_sub(rm_so);
        replacement = replace_regexp_str();
    } else {
        match_len = needle.len();
        replacement = LineData::from_utf8(&state().answer);
    }

    let head = &current_data[..current_x];
    let tail = if current_x + match_len <= current_data.len() {
        &current_data[current_x + match_len..]
    } else {
        &[]
    };
    let mut altered = LineData::from_internal(head.to_vec());
    altered.extend_bytes(replacement.as_bytes());
    altered.extend_bytes(tail);
    altered
}

// ---------------------------------------------------------------------------
// do_replace_loop — step through occurrences and prompt for replacement
// ---------------------------------------------------------------------------
/* C: ssize_t do_replace_loop(const char *needle, bool whole_word_only,
const linestruct *real_current, size_t *real_current_x) */
// Returns: -1 if needle not found, -2 if aborted, else number of replacements.
pub fn do_replace_loop(
    needle: &str,
    whole_word_only: bool,
    real_current: Option<&LinePtr>,
    real_current_x: &mut usize,
) -> isize {
    let backwards = ISSET!(BACKWARDS_SEARCH);
    let mut skipone = backwards;
    let mut replaceall = false;
    let mut modus = REPLACING;
    let mut numreplaced: isize = -1;

    #[cfg(not(feature = "tiny"))]
    let was_mark: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.clone()));

    #[cfg(not(feature = "tiny"))]
    let right_side_up = was_mark.is_some() && mark_is_before_cursor();

    #[cfg(not(feature = "tiny"))]
    let mut top: Option<LinePtr> = None;
    #[cfg(not(feature = "tiny"))]
    let mut top_x: usize = 0;
    #[cfg(not(feature = "tiny"))]
    let mut bot: Option<LinePtr> = None;
    #[cfg(not(feature = "tiny"))]
    let mut bot_x: usize = 0;

    // If the mark is on, frame the region, and turn the mark off.
    #[cfg(not(feature = "tiny"))]
    if was_mark.is_some() {
        let (t_ln, t_x, b_ln, b_x) = crate::utils::get_region();
        top = crate::utils::line_from_number(t_ln as isize);
        bot = crate::utils::line_from_number(b_ln as isize);
        top_x = t_x;
        bot_x = b_x;

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.mark = None;
            }
        });
        modus = INREGION;

        // Start either at the top or the bottom of the marked region.
        let (start_line, start_x) = if !backwards {
            (top.clone(), top_x)
        } else {
            (bot.clone(), bot_x)
        };
        if start_line.is_some() {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = start_line.clone();
                    of.current_x = start_x;
                }
            });
        }
    }

    set_came_full_circle(false);

    loop {
        let mut match_len: usize = 0;
        let _real_lp = real_current.cloned();
        let rx = *real_current_x;

        let result = findnextstr(
            needle,
            whole_word_only,
            modus,
            &mut match_len,
            skipone,
            real_current,
            rx,
        );

        if result < 1 {
            if result < 0 {
                numreplaced = -2; // Cancelled.
            }
            break;
        }

        // An occurrence outside of the marked region means we're done.
        #[cfg(not(feature = "tiny"))]
        if was_mark.is_some() {
            let outside = with_state(|s| {
                let f = s.openfile.as_ref();
                let cur = f.and_then(|f| f.current.clone());
                let cx = f.map(|f| f.current_x).unwrap_or(0);
                match (&cur, &top, &bot) {
                    (Some(cur), Some(top), Some(bot)) => {
                        let cl = cur.borrow().lineno;
                        cl > bot.borrow().lineno
                            || cl < top.borrow().lineno
                            || (LinePtr::ptr_eq(cur, bot) && cx + match_len > bot_x)
                            || (LinePtr::ptr_eq(cur, top) && cx < top_x)
                    }
                    _ => false,
                }
            });
            if outside {
                break;
            }
        }

        // Indicate that we found the search string.
        if numreplaced == -1 {
            numreplaced = 0;
        }

        let mut choice = NO;

        if !replaceall {
            let (found_x, found_data) = with_state(|s| {
                let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
                let data = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
                    .unwrap_or_default();
                (x, data)
            });

            let from_col = xplustabs();
            let to_col = wideness(&found_data, found_x + match_len);

            with_state_mut(|s| {
                s.spotlighted = true;
                s.light_from_col = from_col;
                s.light_to_col = to_col;

                #[cfg(not(feature = "tiny"))]
                {
                    let editwincols = s.editwincols as usize;
                    if s.united_sidescroll && to_col < editwincols.saturating_sub(CUSHION) {
                        if let Some(ref mut of) = s.openfile {
                            of.brink = 0;
                        }
                    } else if s.united_sidescroll {
                        let page = get_page_start(to_col);
                        if let Some(ref mut of) = s.openfile {
                            of.brink = page;
                        }
                    }
                }
            });

            edit_refresh();

            choice = ask_user(YESORALLORNO, "Replace this instance?");

            state_mut().spotlighted = false;

            if choice == CANCEL {
                break;
            }

            replaceall = choice == ALL;

            // When "No" or moving backwards, first move one more char before continuing.
            skipone = choice == NO || ISSET!(BACKWARDS_SEARCH);
        }

        if choice == YES || replaceall {
            let altered = replace_line(needle);

            let (old_len, new_len, _current_x) = with_state(|s| {
                let old = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.len()))
                    .unwrap_or(0);
                let cx = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
                (old, altered.len(), cx)
            });

            let length_change = new_len as isize - old_len as isize;

            #[cfg(not(feature = "tiny"))]
            add_undo(UndoType::Replace, None);

            #[cfg(not(feature = "tiny"))]
            {
                // If the mark was on and it was located after the cursor,
                // then adjust its x position for any text length changes.
                if was_mark.is_some() && !right_side_up {
                    let adjusted_mark_x = with_state_mut(|s| {
                        let Some(of) = s.openfile.as_mut() else {
                            return None;
                        };
                        let cur = of.current.clone();
                        let cx = of.current_x;
                        if let (Some(cur), Some(wm)) = (cur, was_mark.as_ref()) {
                            if LinePtr::ptr_eq(&cur, wm) && of.mark_x > cx {
                                if of.mark_x < cx + match_len {
                                    of.mark_x = cx;
                                } else {
                                    of.mark_x = (of.mark_x as isize + length_change) as usize;
                                }
                                return Some(of.mark_x);
                            }
                        }
                        None
                    });
                    if let Some(mx) = adjusted_mark_x {
                        bot_x = mx;
                    }
                }

                // If the mark was not on or it was before the cursor, then
                // adjust the cursor's x position for any text length changes.
                if was_mark.is_none() || right_side_up {
                    let (is_real, cx_lt_rx) = with_state(|s| {
                        let is_real = real_current
                            .map(|rc| {
                                s.openfile
                                    .as_ref()
                                    .and_then(|f| f.current.as_ref())
                                    .map(|cur| LinePtr::ptr_eq(cur, rc))
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false);
                        let cx = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
                        (is_real, cx < *real_current_x)
                    });
                    if is_real && cx_lt_rx {
                        let cx =
                            with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                        if *real_current_x < cx + match_len {
                            *real_current_x = cx + match_len;
                        }
                        let new_rx = (*real_current_x as isize + length_change) as usize;
                        *real_current_x = new_rx;
                        bot_x = *real_current_x;
                    }
                }
            }

            #[cfg(feature = "tiny")]
            {
                let (is_real, cx_lt_rx) = with_state(|s| {
                    let is_real = real_current
                        .map(|rc| {
                            s.openfile
                                .as_ref()
                                .and_then(|f| f.current.as_ref())
                                .map(|cur| LinePtr::ptr_eq(cur, rc))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    let cx = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
                    (is_real, cx < *real_current_x)
                });
                if is_real && cx_lt_rx {
                    let cx = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                    if *real_current_x < cx + match_len {
                        *real_current_x = cx + match_len;
                    }
                    let new_rx = (*real_current_x as isize + length_change) as usize;
                    *real_current_x = new_rx;
                }
            }

            // Don't find the same zero-length or BOL match again.
            if match_len == 0 || (needle.starts_with('^') && ISSET!(USE_REGEXP)) {
                skipone = true;
            }

            // When moving forward, advance cursor past the replacement text.
            if !ISSET!(BACKWARDS_SEARCH) {
                let new_x = with_state(|s| {
                    s.openfile
                        .as_ref()
                        .map(|f| {
                            (f.current_x as isize + match_len as isize + length_change) as usize
                        })
                        .unwrap_or(0)
                });
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current_x = new_x;
                    }
                });
            }

            // Update file size and replace the line data.
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    let old_char_count = of
                        .current
                        .as_ref()
                        .map(|l| mbstrlen(&l.borrow().data))
                        .unwrap_or(0);
                    let new_char_count = mbstrlen(&altered);
                    of.totsize = (of.totsize as isize + new_char_count as isize
                        - old_char_count as isize) as usize;
                    if let Some(ref cur) = of.current {
                        cur.borrow_mut().data = altered.clone();
                    }
                }
            });

            #[cfg(feature = "color")]
            {
                let cur = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
                if let Some(ref l) = cur {
                    check_the_multis(l);
                }
                state_mut().refresh_needed = false;
            }

            set_modified();
            state_mut().as_an_at = true;
            numreplaced += 1;
        }
    }

    if numreplaced == -1 {
        not_found_msg(needle);
    }

    // Restore the mark (C: openfile->mark = was_mark).
    #[cfg(not(feature = "tiny"))]
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.mark = was_mark.clone();
        }
    });

    numreplaced
}

// ---------------------------------------------------------------------------
// do_replace — replace a string
// ---------------------------------------------------------------------------
/* C: void do_replace(void) */
pub fn do_replace() {
    if ISSET!(VIEW_MODE) {
        print_view_warning();
    } else {
        UNSET!(BACKWARDS_SEARCH);
        search_init(true, false);
    }
}

// ---------------------------------------------------------------------------
// ask_for_and_do_replacements — ask what to replace with, then do it
// ---------------------------------------------------------------------------
/* C: void ask_for_and_do_replacements(void) */
pub fn ask_for_and_do_replacements() {
    let was_edittop: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.edittop.clone()));
    let was_firstcolumn: usize =
        with_state(|s| s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0));
    let beginline: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
    let mut begin_x: usize = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));

    let replacee = state().last_search.clone();

    // Prompt for replacement string.
    let response = do_prompt(
        MREPLACEWITH,
        "",
        Some(crate::history::HistoryKind::Replace),
        crate::winio::edit_refresh,
        "Replace with",
    );

    // Restore the search string (it may have changed at the prompt).
    state_mut().last_search = replacee.clone();

    #[cfg(feature = "histories")]
    if response == 0 {
        let answer = state().answer.clone();
        let hist = state().replace_history.clone();
        update_history(&hist, &answer, PRUNE_DUPLICATE);
    }

    if response == -1 {
        statusbar("Cancelled");
        return;
    } else if response > 0 {
        return;
    }

    let needle = state().last_search.clone();
    let numreplaced = do_replace_loop(&needle, false, beginline.as_ref(), &mut begin_x);

    // Restore where we were.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.edittop = was_edittop.clone();
            of.firstcolumn = was_firstcolumn;
            if let Some(bl) = beginline.clone() {
                of.current = Some(bl);
            }
            of.current_x = begin_x;
        }
        s.refresh_needed = true;
    });

    if numreplaced >= 0 {
        let msg = if numreplaced == 1 {
            format!("Replaced {} occurrence", numreplaced)
        } else {
            format!("Replaced {} occurrences", numreplaced)
        };
        statusline(MessageType::Remark, &msg);
    }
}

// ---------------------------------------------------------------------------
// goto_line_posx — go to specified line and x position
// ---------------------------------------------------------------------------
/* C: void goto_line_posx(ssize_t linenumber, size_t pos_x) */
#[cfg(any(
    not(feature = "tiny"),
    feature = "speller",
    feature = "linter",
    feature = "formatter"
))]
pub fn goto_line_posx(linenumber: isize, pos_x: usize) {
    #[cfg(feature = "color")]
    {
        let needs_recook = with_state(|s| {
            let editwinrows = s.editwinrows;
            let edittop_lineno = s
                .openfile
                .as_ref()
                .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let current_lineno = s
                .openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let softwrap = s.flag_isset(SOFTWRAP);
            linenumber > edittop_lineno + editwinrows as isize
                || (softwrap && linenumber > current_lineno)
        });
        if needs_recook {
            with_state_mut(|s| {
                s.recook |= s.perturbed;
            });
        }
    }

    let filebot_lineno = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
            .unwrap_or(0)
    });

    if linenumber < filebot_lineno {
        let target = line_from_number(linenumber);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = target;
            }
        });
    } else {
        let filebot = with_state(|s| s.openfile.as_ref().and_then(|f| f.filebot.clone()));
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = filebot;
            }
        });
    }

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current_x = pos_x;
        }
    });
    let pww = crate::utils::xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = pww;
        }
        s.refresh_needed = true;
    });
}

#[cfg(all(
    feature = "tiny",
    not(any(feature = "speller", feature = "linter", feature = "formatter"))
))]
pub fn goto_line_posx(_linenumber: isize, _pos_x: usize) {}

// ---------------------------------------------------------------------------
// do_gotolinecolumn — implement Go To Line menu
// ---------------------------------------------------------------------------
/* C: void do_gotolinecolumn(void) */
pub fn do_gotolinecolumn() {
    ask_for_line_and_column("");
}

// ---------------------------------------------------------------------------
// ask_for_line_and_column — ask for line/column and jump there
// ---------------------------------------------------------------------------
/* C: void ask_for_line_and_column(char *provided) */
pub fn ask_for_line_and_column(provided: &str) {
    let (cur_line, cur_col) = with_state(|s| {
        let line = s
            .openfile
            .as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
            .unwrap_or(1);
        let col = s
            .openfile
            .as_ref()
            .map(|f| f.placewewant as isize + 1)
            .unwrap_or(1);
        (line, col)
    });

    let mut line = cur_line;
    let mut column = cur_col;

    let response = do_prompt(
        MGOTOLINE,
        provided,
        None,
        crate::winio::edit_refresh,
        "Enter line number, column number",
    );

    // When switching to Search, retain what the user typed so far.
    if crate::global::func_from_key(response) == Some(crate::global::flip_goto as FuncPtr) {
        UNSET!(BACKWARDS_SEARCH);
        search_init(false, true);
        return;
    }

    if response < 0 {
        statusbar("Cancelled");
        return;
    } else if response > 0 {
        return;
    }

    let answer = state().answer.clone();

    // A ++ or -- before the number signifies a relative jump.
    let doublesign = if answer.starts_with("++") || answer.starts_with("--") {
        1usize
    } else {
        0
    };

    let input = if doublesign > 0 {
        &answer[doublesign..]
    } else {
        &answer[..]
    };

    if !parse_line_column(input, &mut line, &mut column) {
        statusline(MessageType::Ahem, "Invalid line or column number");
        return;
    }

    if doublesign > 0 {
        let cur_lineno = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(1)
        });
        line += cur_lineno;
    }
    if doublesign > 0 && line < 1 {
        line = 1;
    }

    goto_line_and_column(line, column, false);

    let mode = if answer.starts_with(',') {
        UpdateType::Stationary
    } else {
        UpdateType::Centering
    };
    adjust_viewport(mode);
    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// goto_line_and_column — go to the specified line and column (1-based)
// ---------------------------------------------------------------------------
/* C: void goto_line_and_column(ssize_t line, ssize_t column, bool hugfloor) */
pub fn goto_line_and_column(mut line: isize, mut column: isize, hugfloor: bool) {
    let filebot_lineno = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
            .unwrap_or(1)
    });

    // Negative line means: from the end of file.
    if line < 0 {
        line = filebot_lineno + line + 1;
    } else if line == 0 {
        line = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(1)
        });
    }
    if line < 1 {
        line = 1;
    }

    #[cfg(feature = "color")]
    {
        let needs_recook = with_state(|s| {
            let editwinrows = s.editwinrows;
            let edittop_lineno = s
                .openfile
                .as_ref()
                .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let current_lineno = s
                .openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let softwrap = s.flag_isset(SOFTWRAP);
            line > edittop_lineno + editwinrows as isize || (softwrap && line > current_lineno)
        });
        if needs_recook {
            with_state_mut(|s| {
                s.recook |= s.perturbed;
            });
        }
    }

    // Iterate to the requested line.
    {
        let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
        let mut current = filetop;
        let mut remaining = line - 1;
        while remaining > 0 {
            let next = current.as_ref().and_then(|l| l.borrow().next.clone());
            let is_bot = with_state(|s| {
                let bot = s.openfile.as_ref().and_then(|f| f.filebot.clone());
                current
                    .as_ref()
                    .zip(bot.as_ref())
                    .map(|(c, b)| LinePtr::ptr_eq(c, b))
                    .unwrap_or(false)
            });
            if is_bot {
                break;
            }
            current = next;
            remaining -= 1;
        }
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = current;
            }
        });
    }

    // Negative column means: from the end of the line.
    let current_data = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default()
    });
    let line_breadth = breadth(&current_data) as isize;

    if column < 0 {
        column = line_breadth + column + 2;
    } else if column == 0 {
        column = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.placewewant as isize + 1)
                .unwrap_or(1)
        });
    }
    if column < 1 {
        column = 1;
    }

    let col_zero = (column - 1) as usize;
    let x = actual_x(&current_data, col_zero);

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current_x = x;
            of.placewewant = col_zero;
        }
    });

    #[cfg(not(feature = "tiny"))]
    {
        let (softwrap, editwincols, placewewant, line_breadth_u) = with_state(|s| {
            let sw = s.flag_isset(SOFTWRAP);
            let ec = s.editwincols as usize;
            let pw = s.openfile.as_ref().map(|f| f.placewewant).unwrap_or(0);
            let lb = breadth(&current_data);
            (sw, ec, pw, lb)
        });
        let adjusted = softwrap_placewewant(placewewant, line_breadth_u, editwincols);
        if softwrap && adjusted != placewewant {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.placewewant = adjusted;
                }
            });
        }
    }

    if !hugfloor {
        return;
    }

    // Position the viewport so the target is near the bottom if close to EOF.
    let rows_from_tail: i32;

    #[cfg(not(feature = "tiny"))]
    {
        let softwrap = state().flag_isset(SOFTWRAP);
        if softwrap {
            let cur = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
            let editwinrows = state().editwinrows;
            let mut currentline = cur;
            let mut leftedge = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| {
                        f.current.as_ref().map(|l| {
                            // C anchors on xplustabs(): the actual display
                            // column of current_x, which is clamped when a
                            // wide character straddles the target column --
                            // placewewant can point mid-character.
                            leftedge_for(xplustabs(), l)
                        })
                    })
                    .unwrap_or(0)
            });
            rows_from_tail = (editwinrows / 2)
                - go_forward_chunks(editwinrows / 2, &mut currentline, &mut leftedge);
        } else {
            let (cur_lineno, bot_lineno) = with_state(|s| {
                let cur = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                let bot = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                (cur, bot)
            });
            rows_from_tail = (bot_lineno - cur_lineno) as i32;
        }
    }

    #[cfg(feature = "tiny")]
    {
        let (cur_lineno, bot_lineno) = with_state(|s| {
            let cur = s
                .openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let bot = s
                .openfile
                .as_ref()
                .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            (cur, bot)
        });
        rows_from_tail = (bot_lineno - cur_lineno) as i32;
    }

    let editwinrows = state().editwinrows;
    let jumpy = ISSET!(JUMPY_SCROLLING);

    if rows_from_tail < editwinrows / 2 && !jumpy {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.cursor_row = (editwinrows - 1 - rows_from_tail) as isize;
            }
        });
        adjust_viewport(UpdateType::Stationary);
    } else {
        adjust_viewport(UpdateType::Centering);
    }
}

/// Keep a requested softwrap column on the line's final screen chunk.  Both
/// inputs are display columns, not byte or character counts.
fn softwrap_placewewant(requested: usize, line_breadth: usize, editwincols: usize) -> usize {
    if editwincols != 0 && requested / editwincols > line_breadth / editwincols {
        line_breadth
    } else {
        requested
    }
}

// ---------------------------------------------------------------------------
// find_a_bracket — search for any of the two characters in bracket_pair
// ---------------------------------------------------------------------------
/* C: bool find_a_bracket(bool reverse, const char *bracket_pair) */
#[cfg(not(feature = "tiny"))]
pub fn find_a_bracket(reverse: bool, bracket_pair: &str) -> bool {
    let Some(mut line) = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone())) else {
        return false;
    };
    let current_x: usize = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));

    let found_x: usize;

    if reverse {
        // First step away from the current bracket.
        let mut pointer_offset: usize;
        if current_x == 0 {
            let prev = {
                let b = line.borrow();
                b.prev.as_ref().and_then(|w| w.upgrade())
            };
            match prev {
                None => return false,
                Some(p) => {
                    line = p;
                }
            }
            pointer_offset = line.borrow().data.len();
        } else {
            pointer_offset = {
                let b = line.borrow();
                step_left(&b.data, current_x)
            };
        }

        // Now seek for any of the two brackets we are interested in.
        loop {
            let hit = {
                let b = line.borrow();
                crate::chars::mbrevstrpbrk(&b.data, bracket_pair, pointer_offset)
            };
            if let Some(x) = hit {
                found_x = x;
                break;
            }
            let prev = {
                let b = line.borrow();
                b.prev.as_ref().and_then(|w| w.upgrade())
            };
            match prev {
                None => return false,
                Some(p) => {
                    line = p;
                }
            }
            pointer_offset = line.borrow().data.len();
        }
    } else {
        // Forward search.
        let mut pointer_offset = {
            let b = line.borrow();
            step_right(&b.data, current_x)
        };

        loop {
            let hit = {
                let b = line.borrow();
                let from = pointer_offset.min(b.data.len());
                crate::chars::mbstrpbrk(&b.data[from..], bracket_pair).map(|x| from + x)
            };
            if let Some(x) = hit {
                found_x = x;
                break;
            }
            let next = line.borrow().next.clone();
            match next {
                None => return false,
                Some(n) => {
                    line = n;
                }
            }
            pointer_offset = 0;
        }
    }

    // Set the current position to the found bracket.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = Some(line.clone());
            of.current_x = found_x;
        }
    });
    true
}

// ---------------------------------------------------------------------------
// do_find_bracket — search for a match to the bracket at cursor
// ---------------------------------------------------------------------------
/* C: void do_find_bracket(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_find_bracket() {
    let was_current: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
    let was_x: usize = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));

    let matchbrackets: String = with_state(|s| s.matchbrackets.clone().unwrap_or_default());

    // Find the current character in matchbrackets.
    let current_data: LineData = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default()
    });
    let current_x = was_x;

    // Get the character at current_x.
    let ch_str = &current_data[current_x..];
    let ch_result = crate::chars::mbstrchr(&matchbrackets, ch_str);

    if ch_result.is_none() {
        statusline(MessageType::Ahem, "Not a bracket");
        return;
    }

    let ch_offset = ch_result.unwrap();

    // Find the halfway point in matchbrackets.
    let charcount = mbstrlen(&matchbrackets) / 2;
    let mut halfway = 0usize;
    for _ in 0..charcount {
        halfway += char_length(&matchbrackets[halfway..]);
    }

    // Determine search direction.
    let reverse = ch_offset >= halfway;

    // Step to find the complementary bracket.
    let ch_char = matchbrackets[ch_offset..].chars().next().unwrap_or('?');
    let _ch_len = ch_char.len_utf8();

    // Find wanted_ch by stepping charcount positions.
    let mut wanted_offset = ch_offset;
    let mut remaining = charcount;
    while remaining > 0 {
        if reverse {
            if wanted_offset == 0 {
                break;
            }
            wanted_offset = step_left(&matchbrackets, wanted_offset);
        } else {
            wanted_offset += char_length(&matchbrackets[wanted_offset..]);
        }
        remaining -= 1;
    }

    let wanted_ch = &matchbrackets[wanted_offset..];
    let wanted_char = wanted_ch.chars().next().unwrap_or('?');
    let _wanted_ch_len = wanted_char.len_utf8();

    // Build the bracket pair string.
    let bracket_pair = format!("{}{}", ch_char, wanted_char);

    let mut balance: usize = 1;

    loop {
        if !find_a_bracket(reverse, &bracket_pair) {
            statusline(MessageType::Ahem, "No matching bracket");
            // Restore the cursor position.
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = was_current.clone();
                    of.current_x = was_x;
                }
            });
            return;
        }

        // Check whether the found character is the same bracket or the other.
        let found_data = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
                .unwrap_or_default()
        });
        let found_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));

        let found_ch = crate::chars::mbtowide(&found_data[found_x..])
            .map(|pair| pair.0)
            .unwrap_or('?');

        if found_ch == ch_char {
            balance += 1;
        } else {
            if balance == 0 {
                balance = 0; // avoid underflow
            } else {
                balance -= 1;
            }
        }

        if balance == 0 {
            if let Some(ref was) = was_current {
                edit_redraw(was, UpdateType::Flowing);
            }
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// put_or_lift_anchor — place or remove anchor at current line
// ---------------------------------------------------------------------------
/* C: void put_or_lift_anchor(void) */
#[cfg(not(feature = "tiny"))]
pub fn put_or_lift_anchor() {
    let (current_lp, current_x, is_filetop, has_anchor) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        let is_top = s
            .openfile
            .as_ref()
            .map(|f| {
                f.current
                    .as_ref()
                    .zip(f.filetop.as_ref())
                    .map(|(c, t)| LinePtr::ptr_eq(c, t))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let anchor = lp.as_ref().map(|l| l.borrow().has_anchor).unwrap_or(false);
        (lp, x, is_top, anchor)
    });

    // Toggle the anchor.
    if let Some(ref l) = current_lp {
        l.borrow_mut().has_anchor = !has_anchor;
    }
    let new_anchor = !has_anchor;

    if is_filetop {
        state_mut().refresh_needed = true;
    } else if let Some(ref l) = current_lp {
        update_line(l, current_x);
    }

    let (line_numbers, minibar, zero) = with_state(|s| {
        (
            s.flag_isset(LINE_NUMBERS),
            s.flag_isset(MINIBAR),
            s.flag_isset(ZERO),
        )
    });

    if !line_numbers && (!minibar || zero) {
        if new_anchor {
            statusline(MessageType::Remark, "Placed anchor");
        } else {
            statusline(MessageType::Hush, "Removed anchor");
        }
    }
}

// ---------------------------------------------------------------------------
// go_to_and_confirm — make the given line the current line or report anchored
// ---------------------------------------------------------------------------
/* C: void go_to_and_confirm(linestruct *line) */
#[cfg(not(feature = "tiny"))]
pub fn go_to_and_confirm(target: &LinePtr) {
    let was_current: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));

    let is_current = was_current
        .as_ref()
        .map(|c| LinePtr::ptr_eq(c, target))
        .unwrap_or(false);

    if !is_current {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(target.clone());
                of.current_x = 0;
            }
        });

        #[cfg(feature = "color")]
        {
            let needs_recook = with_state(|s| {
                let editwinrows = s.editwinrows;
                let edittop_lineno = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                let _cur_lineno = s
                    .openfile
                    .as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                let softwrap = s.flag_isset(SOFTWRAP);
                let was_lineno = was_current.as_ref().map(|w| w.borrow().lineno).unwrap_or(0);
                target.borrow().lineno > edittop_lineno + editwinrows as isize
                    || (softwrap && target.borrow().lineno > was_lineno)
            });
            if needs_recook {
                with_state_mut(|s| {
                    s.recook |= s.perturbed;
                });
            }
        }

        if let Some(ref was) = was_current {
            edit_redraw(was, UpdateType::Centering);
        }

        let line_numbers = state().flag_isset(LINE_NUMBERS);
        if !line_numbers {
            statusbar("Jumped to anchor");
        }
    } else {
        // We are already on this line.
        let has_anchor = target.borrow().has_anchor;
        if has_anchor {
            statusline(MessageType::Remark, "This is the only anchor");
        } else {
            statusline(MessageType::Ahem, "There are no anchors");
        }
    }
}

// ---------------------------------------------------------------------------
// to_prev_anchor — jump to the first anchor before the current line
// ---------------------------------------------------------------------------
/* C: void to_prev_anchor(void) */
#[cfg(not(feature = "tiny"))]
pub fn to_prev_anchor() {
    let current: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));

    let current_lp = match current {
        Some(ref l) => l.clone(),
        None => return,
    };

    let mut line = current_lp.clone();

    loop {
        // Move to previous or wrap to filebot.
        let prev = {
            let borrowed = line.borrow();
            borrowed.prev.as_ref().and_then(|w| w.upgrade())
        };
        line = match prev {
            Some(p) => p,
            None => {
                // Wrap to filebot.
                match with_state(|s| s.openfile.as_ref().and_then(|f| f.filebot.clone())) {
                    Some(b) => b,
                    None => return,
                }
            }
        };

        if line.borrow().has_anchor {
            break;
        }

        // If we've wrapped all the way back to current, stop.
        if LinePtr::ptr_eq(&line, &current_lp) {
            break;
        }
    }

    go_to_and_confirm(&line);
}

// ---------------------------------------------------------------------------
// to_next_anchor — jump to the first anchor after the current line
// ---------------------------------------------------------------------------
/* C: void to_next_anchor(void) */
#[cfg(not(feature = "tiny"))]
pub fn to_next_anchor() {
    let current: Option<LinePtr> =
        with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));

    let current_lp = match current {
        Some(ref l) => l.clone(),
        None => return,
    };

    let mut line = current_lp.clone();

    loop {
        // Move to next or wrap to filetop.
        let next = line.borrow().next.clone();
        line = match next {
            Some(n) => n,
            None => {
                // Wrap to filetop.
                match with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone())) {
                    Some(t) => t,
                    None => return,
                }
            }
        };

        if line.borrow().has_anchor {
            break;
        }

        // If we've wrapped all the way back to current, stop.
        if LinePtr::ptr_eq(&line, &current_lp) {
            break;
        }
    }

    go_to_and_confirm(&line);
}

// ---------------------------------------------------------------------------
// Tests — differential parity gate for the ASCII case-insensitive scan helpers.
// These are pure functions (no STATE), so they can be exercised in isolation.
// They guarantee ascii_ci_find/ascii_ci_rfind stay byte-identical to the
// to_ascii_lowercase() reference they replace on the hot path.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod ci_scan_tests {
    use super::{
        SearchFlags, ascii_ci_find, ascii_ci_rfind, regexp_init, replace_line,
        search_cancel_requested, softwrap_placewewant, strstrwrapper, unicode_ci_find,
        unicode_ci_rfind,
    };
    use crate::definitions::{LineData, OpenFileStruct, USE_REGEXP};
    use crate::global::{state, state_mut};

    /// Reference forward search: first match of the lowercased needle.
    fn ref_find(hay: &str, ndl: &str) -> Option<usize> {
        hay.to_ascii_lowercase().find(&ndl.to_ascii_lowercase())
    }
    /// Reference backward search: last match (largest start offset).
    fn ref_rfind(hay: &str, ndl: &str) -> Option<usize> {
        let h = hay.to_ascii_lowercase();
        let n = ndl.to_ascii_lowercase();
        if n.is_empty() || n.len() > h.len() {
            return None;
        }
        let hb = h.as_bytes();
        let nb = n.as_bytes();
        (0..=h.len() - n.len())
            .rev()
            .find(|&i| &hb[i..i + n.len()] == nb)
    }

    #[test]
    fn edge_cases() {
        // Empty needle: documented helper contract.
        assert_eq!(ascii_ci_find(b"abc", b""), Some(0));
        assert_eq!(ascii_ci_rfind(b"abc", b""), None);
        // Needle longer than haystack.
        assert_eq!(ascii_ci_find(b"ab", b"abc"), None);
        assert_eq!(ascii_ci_rfind(b"ab", b"abc"), None);
        // Empty haystack.
        assert_eq!(ascii_ci_find(b"", b"a"), None);
        assert_eq!(ascii_ci_rfind(b"", b"a"), None);
        // Whole-string match.
        assert_eq!(ascii_ci_find(b"AbC", b"abc"), Some(0));
        assert_eq!(ascii_ci_rfind(b"AbC", b"abc"), Some(0));
        // Overlap: last match must be the largest start offset.
        assert_eq!(ascii_ci_find(b"aAaA", b"aa"), Some(0));
        assert_eq!(ascii_ci_rfind(b"aAaA", b"aa"), Some(2));
    }

    #[test]
    fn unicode_case_insensitive_offsets_are_original_byte_offsets() {
        assert_eq!(unicode_ci_find("İx", "x"), Some(2));
        assert_eq!(unicode_ci_rfind("İx", "x"), Some(2));
        assert_eq!(unicode_ci_find("preİpost", "post"), Some(5));
        assert_eq!(unicode_ci_find("İ", "i\u{307}"), Some(0));
    }

    #[test]
    fn backward_regex_captures_selected_match() {
        assert!(regexp_init("([a-z])([0-9])"));
        let flags = SearchFlags {
            use_regexp: true,
            backwards: true,
            case_sensitive: true,
        };

        let data = "a1 b2";
        assert_eq!(
            strstrwrapper(data, "unused", data.len(), None, flags),
            Some(3)
        );
        let matches = state().regmatches;
        assert_eq!(matches[0], (3, 5));
        assert_eq!(matches[1], (3, 4));
        assert_eq!(matches[2], (4, 5));

        state_mut().search_regexp = None;
    }

    #[test]
    fn backward_plain_match_may_extend_beyond_ceiling() {
        let sensitive = SearchFlags {
            use_regexp: false,
            backwards: true,
            case_sensitive: true,
        };
        let insensitive = SearchFlags {
            case_sensitive: false,
            ..sensitive
        };

        assert_eq!(strstrwrapper("abba", "bb", 2, None, sensitive), Some(1));
        assert_eq!(
            strstrwrapper("aBBa", "bb", 2, Some("bb"), insensitive),
            Some(1)
        );
        assert_eq!(strstrwrapper("abb", "b", 1, None, sensitive), Some(1));
    }

    #[test]
    fn backward_regex_enumerates_overlaps_and_preserves_captures() {
        assert!(regexp_init("(aba)"));
        let flags = SearchFlags {
            use_regexp: true,
            backwards: true,
            case_sensitive: true,
        };

        assert_eq!(strstrwrapper("ababa", "unused", 5, None, flags), Some(2));
        assert_eq!(state().regmatches[0], (2, 5));
        assert_eq!(state().regmatches[1], (2, 5));

        assert!(regexp_init("bb"));
        assert_eq!(strstrwrapper("abba", "unused", 2, None, flags), Some(1));
        assert_eq!(state().regmatches[0], (1, 3));

        state_mut().search_regexp = None;
    }

    #[test]
    fn regex_replacement_backreference_preserves_malformed_bytes() {
        let line = crate::nano::make_new_node(None);
        line.borrow_mut().data = LineData::from_internal(vec![b'a', 0xFF, b'b']);

        let mut buffer = Box::new(OpenFileStruct::default());
        buffer.filetop = Some(line.clone());
        buffer.filebot = Some(line.clone());
        buffer.edittop = Some(line.clone());
        buffer.current = Some(line);
        buffer.current_x = 1;
        state_mut().openfile = Some(buffer);
        state_mut().answer = r"<\1>".to_string();
        state_mut().regmatches[0] = (1, 2);
        state_mut().regmatches[1] = (1, 2);

        assert!(regexp_init("(.)"));
        crate::SET!(USE_REGEXP);
        assert_eq!(
            replace_line("unused").as_bytes(),
            &[b'a', b'<', 0xFF, b'>', b'b']
        );

        crate::UNSET!(USE_REGEXP);
        state_mut().search_regexp = None;
        state_mut().openfile = None;
    }

    #[test]
    fn ctrl_t_maps_to_the_flip_in_both_prompt_menus() {
        // Issue #68: nano's ^T toggle between Search and Go-To-Line was
        // missing entirely.
        crate::global::shortcut_init();
        let saved_menu = state().currmenu;

        state_mut().currmenu = crate::definitions::MWHEREIS;
        assert_eq!(
            crate::global::func_from_key(20),
            Some(crate::global::flip_goto as crate::definitions::FuncPtr),
            "^T must map to flip_goto from the Search prompt"
        );

        state_mut().currmenu = crate::definitions::MGOTOLINE;
        assert_eq!(
            crate::global::func_from_key(20),
            Some(crate::global::flip_goto as crate::definitions::FuncPtr),
            "^T must map to flip_goto from the Go-To-Line prompt"
        );

        state_mut().currmenu = saved_menu;
    }

    #[test]
    fn queued_cancel_is_detected_without_blocking() {
        crate::global::shortcut_init();
        state_mut().currmenu = crate::definitions::MWHEREIS;
        crate::winio::put_back(3);

        assert!(search_cancel_requested());
        assert_eq!(crate::winio::waiting_keycodes(), 0);
    }

    #[test]
    fn softwrap_limit_uses_display_breadth() {
        let old_tabsize = state().tabsize;
        state_mut().tabsize = 8;
        let tab_breadth = crate::utils::breadth("\t");
        assert!(tab_breadth > "\t".chars().count());
        assert_eq!(softwrap_placewewant(16, tab_breadth, 8), tab_breadth);
        assert_eq!(softwrap_placewewant(7, tab_breadth, 8), 7);
        state_mut().tabsize = old_tabsize;
    }

    #[test]
    fn differential_random_ascii() {
        // Small alphabet maximizes case/overlap collisions and includes
        // non-alphabetic first bytes (where lower==upper).
        let alpha = b"aAbB1 .zZ";
        let mut s: u64 = 0x1234_5678_9abc_def1;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..500_000u64 {
            let hlen = (next() % 12) as usize;
            let nlen = 1 + (next() % 4) as usize; // non-empty: the usage contract
            let mut hb = Vec::with_capacity(hlen);
            for _ in 0..hlen {
                hb.push(alpha[(next() as usize) % alpha.len()]);
            }
            let mut nb = Vec::with_capacity(nlen);
            for _ in 0..nlen {
                nb.push(alpha[(next() as usize) % alpha.len()]);
            }
            let hay = std::str::from_utf8(&hb).unwrap();
            let ndl = std::str::from_utf8(&nb).unwrap();
            assert_eq!(
                ascii_ci_find(&hb, &nb),
                ref_find(hay, ndl),
                "find hay={hay:?} ndl={ndl:?}"
            );
            assert_eq!(
                ascii_ci_rfind(&hb, &nb),
                ref_rfind(hay, ndl),
                "rfind hay={hay:?} ndl={ndl:?}"
            );
        }
    }
}
