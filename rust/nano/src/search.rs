#![allow(unused, non_snake_case, dead_code, non_camel_case_types)]
// Port of src/search.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2015-2022, 2025 Benno Schulenberg

use crate::definitions::*;
use crate::global::{STATE, with_state, with_state_mut};
use crate::{ISSET, SET, UNSET, TOGGLE};
use regex::{Regex, RegexBuilder};

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

extern "Rust" {}

// These are stubs for functions defined in other modules that search.c calls.
// In a fully ported codebase these will resolve to the real implementations.

#[inline] fn statusbar(msg: &str) { crate::winio::statusbar(msg) }

#[inline] fn statusline(mtype: MessageType, msg: &str) { crate::winio::statusline(mtype, msg) }

#[inline] fn wipe_statusbar() { crate::winio::wipe_statusbar() }

#[inline] fn edit_refresh() { crate::winio::edit_refresh() }

#[inline] fn edit_redraw(was_current: &LinePtr, mode: UpdateType) {
    crate::winio::edit_redraw(was_current.borrow().lineno, mode)
}

#[inline] fn regenerate_screen() { crate::nano::regenerate_screen() }

fn xplustabs() -> usize {
    // C: xplustabs() — winio.c
    with_state(|s| s.current_x())
}

fn wideness(data: &str, x: usize) -> usize {
    // C: wideness(data, x) — chars.c
    // Simplified: return x (assumes single-width chars)
    x
}

fn breadth(data: &str) -> usize {
    // C: breadth(data) — chars.c — visual width
    data.chars().count()
}

fn display_string(s: &str, _from: usize, max_cols: usize, _tabs: bool, _isdata: bool) -> String {
    // C: display_string() — winio.c
    s.chars().take(max_cols).collect()
}

fn actual_x(data: &str, cols: usize) -> usize {
    // C: actual_x(data, cols) — chars.c
    // Simplified: return min(cols, data.len())
    cols.min(data.len())
}

fn char_length(s: &str) -> usize {
    // C: char_length(s) — chars.c
    // Returns the byte length of the first char in s.
    s.chars().next().map(|c| c.len_utf8()).unwrap_or(0)
}

fn step_left(data: &str, x: usize) -> usize {
    // C: step_left(data, x) — chars.c
    // Step one character to the left (find the previous character boundary).
    if x == 0 {
        return 0;
    }
    let bytes = data.as_bytes();
    let mut pos = x - 1;
    // Walk back to find a UTF-8 character boundary.
    while pos > 0 && (bytes[pos] & 0xC0) == 0x80 {
        pos -= 1;
    }
    pos
}

fn step_right(data: &str, x: usize) -> usize {
    // C: step_right(data, x) — chars.c
    // Step one character to the right.
    if x >= data.len() {
        return data.len();
    }
    x + char_length(&data[x..])
}

fn mbstrlen(s: &str) -> usize {
    // C: mbstrlen(s) — chars.c — number of multibyte characters
    s.chars().count()
}

fn get_page_start(col: usize) -> usize {
    // C: get_page_start(col) — utils.c
    with_state(|s| {
        let cols = s.editwincols as usize;
        if col == 0 || col < cols {
            0
        } else {
            col - (col % (cols - 2))
        }
    })
}

fn print_view_warning() {
    // C: print_view_warning() — nano.c
    statusline(MessageType::Ahem, "File is unmodifiable");
}

fn napms(_ms: i32) {
    // C: napms(ms) — ncurses
}

fn do_prompt(
    _menu: u32,
    _initial: &str,
    _history: Option<&LinePtr>,
    _refresh_fn: fn(),
    _prompt: &str,
) -> i32 {
    // C: do_prompt() — prompt.c
    -1
}

fn ask_user(_yesorallorno: bool, _question: &str) -> i32 {
    // C: ask_user() — winio.c / prompt.c
    CANCEL
}

#[inline] fn set_modified() { crate::files::set_modified() }

fn parse_line_column(_input: &str, _line: &mut isize, _col: &mut isize) -> bool {
    // C: parse_line_column() — utils.c
    false
}

#[inline] fn adjust_viewport(mode: UpdateType) { crate::winio::adjust_viewport(mode) }

fn line_from_number(n: isize) -> Option<LinePtr> {
    // C: line_from_number() — utils.c — find line by number
    with_state(|s| {
        if let Some(ref of) = s.openfile {
            let mut line = of.filetop.clone();
            while let Some(l) = line {
                if l.borrow().lineno == n {
                    return Some(l);
                }
                let next = l.borrow().next.clone();
                line = next;
            }
        }
        None
    })
}

fn go_forward_chunks(rows: i32, line: &mut Option<LinePtr>, leftedge: &mut usize) -> i32 {
    let mut lineno: isize = line.as_ref().map(|l| l.borrow().lineno).unwrap_or(0);
    let result = crate::winio::go_forward_chunks(rows, &mut lineno, leftedge);
    result
}

#[inline] fn leftedge_for(col: usize, line: &LinePtr) -> usize {
    crate::winio::leftedge_for(col, &line.borrow().data)
}

fn update_line(line: &LinePtr, x: usize) {
    let (lineno, data) = { let b = line.borrow(); (b.lineno, b.data.clone()) };
    crate::winio::update_line(lineno, &data,
        #[cfg(feature = "color")] &[],
        false, x);
}

fn mbstrchr<'a>(s: &'a str, needle_start: &str) -> Option<&'a str> {
    // C: mbstrchr(s, needle_start) — chars.c
    // Find needle_start (first char) in s.
    let needle_ch = needle_start.chars().next()?;
    let idx = s.find(needle_ch)?;
    Some(&s[idx..])
}

fn mbstrpbrk<'a>(s: &'a str, chars: &str) -> Option<&'a str> {
    // C: mbstrpbrk(s, chars) — chars.c
    // Find first occurrence of any char from `chars` in `s`.
    for (i, c) in s.char_indices() {
        if chars.contains(c) {
            return Some(&s[i..]);
        }
    }
    None
}

fn mbrevstrpbrk<'a>(data: &'a str, chars: &str, pointer: &str) -> Option<&'a str> {
    // C: mbrevstrpbrk(data, chars, pointer) — chars.c
    // Search backward from pointer in data for any char in chars.
    // pointer points inside data.
    let ptr_offset = pointer.as_ptr() as usize - data.as_ptr() as usize;
    let prefix = &data[..ptr_offset];
    // Iterate characters in prefix in reverse.
    let mut last_found: Option<usize> = None;
    for (i, c) in prefix.char_indices() {
        if chars.contains(c) {
            last_found = Some(i);
        }
    }
    last_found.map(|i| &data[i..])
}

#[cfg(feature = "speller")]
fn is_separate_word(_x: usize, _len: usize, _data: &str) -> bool {
    // C: is_separate_word() — chars.c
    false
}

#[cfg(feature = "histories")]
fn update_history(history: &Option<LinePtr>, answer: &str, prune: bool) {
    use crate::history::HistoryKind;
    // Determine which history list by comparing the pointer with the global search/replace histories.
    let kind = with_state(|s| {
        let is_replace = s.replace_history.as_ref().zip(history.as_ref())
            .map(|(r, h)| std::rc::Rc::ptr_eq(r, h))
            .unwrap_or(false);
        if is_replace { HistoryKind::Replace } else { HistoryKind::Search }
    });
    crate::history::update_history(kind, answer, prune);
}

#[cfg(feature = "color")]
#[inline] fn check_the_multis(line: &LinePtr) { crate::color::check_the_multis(line) }

#[cfg(not(feature = "tiny"))]
#[inline] fn add_undo(kind: UndoType, msg: Option<&str>) { crate::text::add_undo(kind, msg) }

#[cfg(not(feature = "tiny"))]
fn mark_is_before_cursor() -> bool {
    with_state(|s| s.mark_is_before_cursor())
}

#[cfg(not(feature = "tiny"))]
fn get_region(
    top: &mut Option<LinePtr>, top_x: &mut usize,
    bot: &mut Option<LinePtr>, bot_x: &mut usize,
) {
    // C: get_region() — search.c / cut.c
    with_state(|s| {
        let (tln, tx, bln, bx) = s.get_region_coords();
        if let Some(ref of) = s.openfile {
            // Find the lines by number — simplified.
            *top_x = tx;
            *bot_x = bx;
            // We'd need to walk the list; for now we leave the pointers as-is.
        }
    });
}

fn get_input(_win: Option<()>) -> i32 {
    // C: get_input(win) — winio.c
    -1 // ERR
}

// ---------------------------------------------------------------------------
// strstrwrapper — search for needle in haystack starting from pos
// ---------------------------------------------------------------------------
// C: const char *strstrwrapper(const char *data, const char *needle, const char *from)
// Returns the byte offset of the match within `data`, or None.
fn strstrwrapper(data: &str, needle: &str, from_offset: usize) -> Option<usize> {
    if from_offset > data.len() {
        return None;
    }

    let use_regexp = ISSET!(USE_REGEXP);
    let backwards = ISSET!(BACKWARDS_SEARCH);

    if use_regexp {
        // Regex search.
        let result = with_state(|s| {
            if let Some(ref re) = s.search_regexp {
                if backwards {
                    // Search from start up to from_offset for the LAST match.
                    let search_slice = &data[..from_offset];
                    let mut last_match: Option<(usize, usize)> = None;
                    for m in re.find_iter(search_slice) {
                        last_match = Some((m.start(), m.end()));
                    }
                    // Store regmatches for the found match.
                    if let Some((start, end)) = last_match {
                        return Some(start);
                    }
                    None
                } else {
                    // Search forward from from_offset.
                    let search_slice = &data[from_offset..];
                    if let Some(m) = re.find(search_slice) {
                        // Update regmatches in STATE.
                        Some(from_offset + m.start())
                    } else {
                        None
                    }
                }
            } else {
                None
            }
        });

        // Update regmatches if we found something.
        if let Some(match_start) = result {
            with_state_mut(|s| {
                if let Some(ref re) = s.search_regexp.clone() {
                    if backwards {
                        let search_slice = &data[..from_offset];
                        if let Some(caps) = re.captures(search_slice) {
                            for i in 0..10 {
                                s.regmatches[i] = if let Some(m) = caps.get(i) {
                                    (m.start(), m.end())
                                } else {
                                    (0, 0)
                                };
                            }
                        }
                    } else {
                        let search_slice = &data[from_offset..];
                        if let Some(caps) = re.captures(search_slice) {
                            for i in 0..10 {
                                s.regmatches[i] = if let Some(m) = caps.get(i) {
                                    (from_offset + m.start(), from_offset + m.end())
                                } else {
                                    (0, 0)
                                };
                            }
                        }
                    }
                }
            });
        }

        result
    } else {
        // Plain string search.
        let case_sensitive = ISSET!(CASE_SENSITIVE);

        if backwards {
            // Find the last occurrence at or before from_offset.
            let search_slice = &data[..from_offset];
            let mut last_match: Option<usize> = None;
            if case_sensitive {
                let mut start = 0;
                while let Some(pos) = search_slice[start..].find(needle) {
                    last_match = Some(start + pos);
                    start += pos + 1;
                    if start > search_slice.len() {
                        break;
                    }
                }
            } else {
                let lower_data = search_slice.to_lowercase();
                let lower_needle = needle.to_lowercase();
                let mut start = 0;
                while let Some(pos) = lower_data[start..].find(&lower_needle[..]) {
                    last_match = Some(start + pos);
                    start += pos + 1;
                    if start > lower_data.len() {
                        break;
                    }
                }
            }
            last_match
        } else {
            // Find first occurrence at or after from_offset.
            let search_slice = &data[from_offset..];
            let found = if case_sensitive {
                search_slice.find(needle)
            } else {
                let lower_data = search_slice.to_lowercase();
                let lower_needle = needle.to_lowercase();
                lower_data.find(&lower_needle[..])
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
        let has_mark = with_state(|s| {
            s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some()
        });
        if has_mark {
            with_state_mut(|s| s.refresh_needed = true);
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
        let last = with_state(|s| s.last_search.clone());
        if !last.is_empty() {
            let cols = with_state(|s| s.editwincols.max(1) as usize);
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
        let case_sensitive_str = if ISSET!(CASE_SENSITIVE) { " [Case sensitive]" } else { "" };
        let regexp_str = if ISSET!(USE_REGEXP) { " [Reg.exp.]" } else { "" };
        let backwards_str = if ISSET!(BACKWARDS_SEARCH) { " [Backwards]" } else { "" };
        let replace_str = if replacing {
            #[cfg(not(feature = "tiny"))]
            {
                let in_sel = with_state(|s| {
                    s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some()
                });
                if in_sel { " (to replace) in selection" } else { " (to replace)" }
            }
            #[cfg(feature = "tiny")]
            " (to replace)"
        } else {
            ""
        };

        let prompt = format!("Search{}{}{}{}{}", case_sensitive_str, regexp_str,
                              backwards_str, replace_str, thedefault);

        let menu = {
            let inhelp = with_state(|s| s.inhelp);
            if inhelp { MFINDINHELP } else if replacing { MREPLACE } else { MWHEREIS }
        };

        let initial = if retain_answer {
            with_state(|s| s.answer.clone())
        } else {
            String::new()
        };

        // We'd call do_prompt here; for now we use the stub that returns -1 (cancel).
        let search_hist = with_state(|s| s.search_history.clone());
        let response = {
            // Simplified: always cancelled in this stub environment.
            -1i32
        };

        let last_search_empty = with_state(|s| s.last_search.is_empty());

        // If the search was cancelled, or we have a blank answer and
        // nothing was searched for yet during this session, get out.
        if response == -1 || (response == -2 && last_search_empty) {
            statusbar("Cancelled");
            break;
        }

        // If Enter was pressed, prepare to do a replace or a search.
        if response == 0 || response == -2 {
            let answer = with_state(|s| s.answer.clone());
            if !answer.is_empty() {
                with_state_mut(|s| {
                    s.last_search = answer.clone();
                });
                #[cfg(feature = "histories")]
                {
                    let hist = with_state(|s| s.search_history.clone());
                    update_history(&hist, &answer, PRUNE_DUPLICATE);
                }
            }

            if ISSET!(USE_REGEXP) {
                let ls = with_state(|s| s.last_search.clone());
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

        // Handle toggle functions.
        // In the real implementation, func_from_key(response) is compared.
        // Here we rely on the stub returning -1 for all prompts, so we break.
        break;
    }

    let inhelp = with_state(|s| s.inhelp);
    if !inhelp {
        tidy_up_after_search();
    }
}

// ---------------------------------------------------------------------------
// findnextstr — search for needle starting at openfile->current/current_x
// ---------------------------------------------------------------------------
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

    // Record the time for "Searching..." feedback.
    let mut lastkbcheck = std::time::Instant::now();

    loop {
        let current_line = match line {
            Some(ref l) => l.clone(),
            None => {
                // Shouldn't happen but be safe.
                return 0;
            }
        };

        let line_data = current_line.borrow().data.clone();
        let backwards = ISSET!(BACKWARDS_SEARCH);

        let found_offset: Option<usize> = if skipone {
            skipone = false;
            if backwards && from_offset != 0 {
                let new_from = step_left(&line_data, from_offset);
                strstrwrapper(&line_data, needle, new_from)
                    .filter(|&pos| if backwards { pos <= new_from } else { pos >= new_from })
            } else if !backwards && from_offset < line_data.len() {
                let new_from = from_offset + char_length(&line_data[from_offset..]);
                strstrwrapper(&line_data, needle, new_from)
            } else {
                None
            }
        } else {
            strstrwrapper(&line_data, needle, from_offset)
        };

        // Filter for backwards: strstrwrapper with backwards returns last match
        // before from_offset; make sure it actually found one within range.
        let found_offset = found_offset.filter(|_| {
            // strstrwrapper already handles backwards, just pass through.
            true
        });

        if let Some(found_x) = found_offset {
            // When doing regex search, compute the length of the match.
            if ISSET!(USE_REGEXP) {
                let (rm_so, rm_eo) = with_state(|s| s.regmatches[0]);
                found_len = rm_eo.saturating_sub(rm_so);
            }

            #[cfg(feature = "speller")]
            if whole_word_only
                && !is_separate_word(found_x, found_len, &line_data)
            {
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
                        s.openfile.as_ref().map(|f| {
                            f.mark.is_none() || f.softmark
                        }).unwrap_or(true)
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
        #[cfg(not(feature = "tiny"))]
        {
            let resized = with_state(|s| s.the_window_resized);
            if resized {
                regenerate_screen();
                statusbar("Searching...");
                feedback = 1;
            }
        }

        // If we're back at the beginning, there is no needle.
        if get_came_full_circle() {
            return 0;
        }

        // Move to the previous or next line.
        let backwards = ISSET!(BACKWARDS_SEARCH);
        let next_line: Option<LinePtr> = if backwards {
            current_line.borrow().prev.as_ref()
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
        let backwards = ISSET!(BACKWARDS_SEARCH);
        from_offset = if backwards {
            line.as_ref().map(|l| l.borrow().data.len()).unwrap_or(0)
        } else {
            0
        };

        // Periodically check for cancel keystroke.
        if lastkbcheck.elapsed().as_secs() > 0 {
            lastkbcheck = std::time::Instant::now();

            // In the real implementation we'd call wgetch and check for cancel.
            // For now, just update the feedback counter.
            feedback += 1;
            if feedback > 0 {
                statusbar("Searching...");
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
            let prev = s.searchbot.as_ref()
                .and_then(|b| b.borrow().prev.as_ref().and_then(|w| w.upgrade()))
                .is_some();
            (empty, prev)
        });

        if last_empty && has_prev {
            let prev_data = with_state(|s| {
                s.searchbot.as_ref()
                    .and_then(|b| b.borrow().prev.as_ref()
                        .and_then(|w| w.upgrade()))
                    .map(|p| p.borrow().data.clone())
            });
            if let Some(data) = prev_data {
                with_state_mut(|s| s.last_search = data);
            }
        }
    }

    let last_empty = with_state(|s| s.last_search.is_empty());
    if last_empty {
        statusline(MessageType::Ahem, "No current search pattern");
        return;
    }

    if ISSET!(USE_REGEXP) {
        let ls = with_state(|s| s.last_search.clone());
        if !regexp_init(&ls) {
            return;
        }
    }

    // Use the search-menu key bindings to allow cancelling.
    with_state_mut(|s| s.currmenu = MWHEREIS);

    let lines = with_state(|s| s.editwinrows);
    if lines > 1 {
        wipe_statusbar();
    }

    go_looking();

    let inhelp = with_state(|s| s.inhelp);
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
    let cols = with_state(|st| st.editwincols.max(1) as usize);
    let disp = display_string(s, 0, (cols / 2) + 1, false, false);
    let numchars = actual_x(&disp, breadth(&disp).min(cols / 2));
    let truncated = &disp[..numchars];
    let ellipsis = if numchars < disp.len() { "..." } else { "" };
    statusline(MessageType::Ahem, &format!("\"{}{}\" not found", truncated, ellipsis));
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

    let needle = with_state(|s| s.last_search.clone());

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

    with_state_mut(|s| s.didfind = result);

    // If found and we're back at exact same spot, this is the only occurrence.
    let (now_current, now_x) = with_state(|s| {
        let lp = s.openfile.as_ref().and_then(|f| f.current.clone());
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (lp, x)
    });

    let same_spot = was_current.as_ref().zip(now_current.as_ref()).map(|(w, n)| {
        std::rc::Rc::ptr_eq(w, n)
    }).unwrap_or(false) && was_x == now_x;

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
// We return the replacement as a String.
pub fn replace_regexp_str() -> String {
    let answer = with_state(|s| s.answer.clone());
    let mut result = String::new();
    let chars: Vec<char> = answer.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            let num = (next as i32) - ('0' as i32);
            if num >= 1 && num <= 9 {
                // Check if this subgroup exists in search_regexp.
                let nsub = with_state(|s| {
                    s.search_regexp.as_ref().map(|re| re.captures_len()).unwrap_or(0)
                });
                if (num as usize) < nsub {
                    let (rm_so, rm_eo) = with_state(|s| s.regmatches[num as usize]);
                    let current_data = with_state(|s| {
                        s.openfile.as_ref()
                            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
                            .unwrap_or_default()
                    });
                    if rm_so < current_data.len() && rm_eo <= current_data.len() {
                        result.push_str(&current_data[rm_so..rm_eo]);
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
pub fn replace_line(needle: &str) -> String {
    let (current_data, current_x) = with_state(|s| {
        let data = s.openfile.as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default();
        let x = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
        (data, x)
    });

    let use_regexp = ISSET!(USE_REGEXP);

    let match_len: usize;
    let replacement: String;

    if use_regexp {
        let (rm_so, rm_eo) = with_state(|s| s.regmatches[0]);
        match_len = rm_eo.saturating_sub(rm_so);
        replacement = replace_regexp_str();
    } else {
        match_len = needle.len();
        replacement = with_state(|s| s.answer.clone());
    }

    let head = &current_data[..current_x];
    let tail = if current_x + match_len <= current_data.len() {
        &current_data[current_x + match_len..]
    } else {
        ""
    };

    format!("{}{}{}", head, replacement, tail)
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
    let was_mark: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.mark.clone())
    });

    #[cfg(not(feature = "tiny"))]
    let right_side_up = mark_is_before_cursor();

    #[cfg(not(feature = "tiny"))]
    {
        let has_mark = with_state(|s| {
            s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some()
        });
        if has_mark {
            // Get region and position to start/end of it.
            let backwards = backwards;
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = None;
                }
            });
            modus = INREGION;
            // Position at top or bottom of region.
            // (Simplified — in full implementation we'd use get_region.)
        }
    }

    set_came_full_circle(false);

    loop {
        let mut match_len: usize = 0;
        let real_lp = real_current.cloned();
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

        #[cfg(not(feature = "tiny"))]
        {
            // Check if we've gone outside the marked region.
            if let Some(ref was_m) = was_mark {
                // (Simplified — full implementation would check top/bot bounds.)
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
                let data = s.openfile.as_ref()
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

            with_state_mut(|s| s.spotlighted = false);

            if choice == CANCEL {
                break;
            }

            replaceall = choice == ALL;

            // When "No" or moving backwards, first move one more char before continuing.
            skipone = choice == NO || ISSET!(BACKWARDS_SEARCH);
        }

        if choice == YES || replaceall {
            let altered = replace_line(needle);

            let (old_len, new_len, current_x) = with_state(|s| {
                let old = s.openfile.as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.len()))
                    .unwrap_or(0);
                let cx = s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0);
                (old, altered.len(), cx)
            });

            let length_change = new_len as isize - old_len as isize;

            #[cfg(not(feature = "tiny"))]
            add_undo(UndoType::Replace, None);

            // Adjust real_current_x for length changes.
            #[cfg(not(feature = "tiny"))]
            {
                let (is_real, cx_lt_rx) = with_state(|s| {
                    let is_real = real_current.map(|rc| {
                        s.openfile.as_ref().and_then(|f| f.current.as_ref())
                            .map(|cur| std::rc::Rc::ptr_eq(cur, rc))
                            .unwrap_or(false)
                    }).unwrap_or(false);
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

            #[cfg(feature = "tiny")]
            {
                let (is_real, cx_lt_rx) = with_state(|s| {
                    let is_real = real_current.map(|rc| {
                        s.openfile.as_ref().and_then(|f| f.current.as_ref())
                            .map(|cur| std::rc::Rc::ptr_eq(cur, rc))
                            .unwrap_or(false)
                    }).unwrap_or(false);
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
                    s.openfile.as_ref().map(|f| {
                        (f.current_x as isize + match_len as isize + length_change) as usize
                    }).unwrap_or(0)
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
                    let old_char_count = of.current.as_ref()
                        .map(|l| l.borrow().data.chars().count())
                        .unwrap_or(0);
                    let new_char_count = altered.chars().count();
                    of.totsize = (of.totsize as isize
                        + new_char_count as isize
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
                with_state_mut(|s| s.refresh_needed = false);
            }

            set_modified();
            with_state_mut(|s| s.as_an_at = true);
            numreplaced += 1;
        }
    }

    if numreplaced == -1 {
        not_found_msg(needle);
    }

    #[cfg(not(feature = "tiny"))]
    {
        // Restore the mark.
        // (was_mark is captured above)
    }

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
    let was_edittop: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.edittop.clone())
    });
    let was_firstcolumn: usize = with_state(|s| {
        s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0)
    });
    let beginline: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });
    let mut begin_x: usize = with_state(|s| {
        s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0)
    });

    let replacee = with_state(|s| s.last_search.clone());

    // Prompt for replacement string.
    // (Stub: response will be -1 = cancelled.)
    let response = -1i32; // do_prompt(MREPLACEWITH, "", ...)

    // Restore the search string (it may have changed at the prompt).
    with_state_mut(|s| s.last_search = replacee.clone());

    #[cfg(feature = "histories")]
    if response == 0 {
        let answer = with_state(|s| s.answer.clone());
        let hist = with_state(|s| s.replace_history.clone());
        update_history(&hist, &answer, PRUNE_DUPLICATE);
    }

    if response == -1 {
        statusbar("Cancelled");
        return;
    } else if response > 0 {
        return;
    }

    let needle = with_state(|s| s.last_search.clone());
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
#[cfg(any(not(feature = "tiny"), feature = "speller", feature = "linter", feature = "formatter"))]
pub fn goto_line_posx(linenumber: isize, pos_x: usize) {
    #[cfg(feature = "color")]
    {
        let needs_recook = with_state(|s| {
            let editwinrows = s.editwinrows;
            let edittop_lineno = s.openfile.as_ref()
                .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let current_lineno = s.openfile.as_ref()
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
        s.openfile.as_ref()
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
            of.placewewant = pos_x; // simplified — should call xplustabs
        }
        s.refresh_needed = true;
    });
}

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
        let line = s.openfile.as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
            .unwrap_or(1);
        let col = s.openfile.as_ref()
            .map(|f| f.placewewant as isize + 1)
            .unwrap_or(1);
        (line, col)
    });

    let mut line = cur_line;
    let mut column = cur_col;

    // response = do_prompt(MGOTOLINE, provided, NULL, edit_refresh, ...)
    let response = -1i32; // stub

    if response < 0 {
        statusbar("Cancelled");
        return;
    } else if response > 0 {
        return;
    }

    let answer = with_state(|s| s.answer.clone());

    // A ++ or -- before the number signifies a relative jump.
    let doublesign = if (answer.starts_with("++") || answer.starts_with("--")) { 1usize } else { 0 };

    let input = if doublesign > 0 { &answer[doublesign..] } else { &answer[..] };

    if !parse_line_column(input, &mut line, &mut column) {
        statusline(MessageType::Ahem, "Invalid line or column number");
        return;
    }

    if doublesign > 0 {
        let cur_lineno = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(1)
        });
        line += cur_lineno;
    }
    if doublesign > 0 && line < 1 {
        line = 1;
    }

    goto_line_and_column(line, column, false);

    let mode = if answer.starts_with(',') { UpdateType::Stationary } else { UpdateType::Centering };
    adjust_viewport(mode);
    with_state_mut(|s| s.refresh_needed = true);
}

// ---------------------------------------------------------------------------
// goto_line_and_column — go to the specified line and column (1-based)
// ---------------------------------------------------------------------------
/* C: void goto_line_and_column(ssize_t line, ssize_t column, bool hugfloor) */
pub fn goto_line_and_column(mut line: isize, mut column: isize, hugfloor: bool) {
    let filebot_lineno = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
            .unwrap_or(1)
    });

    // Negative line means: from the end of file.
    if line < 0 {
        line = filebot_lineno + line + 1;
    } else if line == 0 {
        line = with_state(|s| {
            s.openfile.as_ref()
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
            let edittop_lineno = s.openfile.as_ref()
                .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let current_lineno = s.openfile.as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let softwrap = s.flag_isset(SOFTWRAP);
            line > edittop_lineno + editwinrows as isize
                || (softwrap && line > current_lineno)
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
                current.as_ref().zip(bot.as_ref()).map(|(c, b)| std::rc::Rc::ptr_eq(c, b)).unwrap_or(false)
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
        s.openfile.as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default()
    });
    let line_breadth = breadth(&current_data) as isize;

    if column < 0 {
        column = line_breadth + column + 2;
    } else if column == 0 {
        column = with_state(|s| {
            s.openfile.as_ref().map(|f| f.placewewant as isize + 1).unwrap_or(1)
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
            let lb = current_data.chars().count();
            (sw, ec, pw, lb)
        });
        if softwrap && placewewant / editwincols > line_breadth_u / editwincols {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.placewewant = line_breadth_u;
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
        let softwrap = with_state(|s| s.flag_isset(SOFTWRAP));
        if softwrap {
            let cur = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
            let editwinrows = with_state(|s| s.editwinrows);
            let mut currentline = cur;
            let mut leftedge = with_state(|s| {
                s.openfile.as_ref().and_then(|f| f.current.as_ref().map(|l| {
                    leftedge_for(s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0), l)
                })).unwrap_or(0)
            });
            rows_from_tail = (editwinrows / 2)
                - go_forward_chunks(editwinrows / 2, &mut currentline, &mut leftedge);
        } else {
            let (cur_lineno, bot_lineno) = with_state(|s| {
                let cur = s.openfile.as_ref()
                    .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                let bot = s.openfile.as_ref()
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
            let cur = s.openfile.as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            let bot = s.openfile.as_ref()
                .and_then(|f| f.filebot.as_ref().map(|l| l.borrow().lineno))
                .unwrap_or(0);
            (cur, bot)
        });
        rows_from_tail = (bot_lineno - cur_lineno) as i32;
    }

    let editwinrows = with_state(|s| s.editwinrows);
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

// ---------------------------------------------------------------------------
// find_a_bracket — search for any of the two characters in bracket_pair
// ---------------------------------------------------------------------------
/* C: bool find_a_bracket(bool reverse, const char *bracket_pair) */
#[cfg(not(feature = "tiny"))]
pub fn find_a_bracket(reverse: bool, bracket_pair: &str) -> bool {
    let current_lp: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });
    let current_x: usize = with_state(|s| {
        s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0)
    });

    let mut line = current_lp;

    if reverse {
        // First step away from the current bracket.
        let pointer_offset: usize;
        if current_x == 0 {
            // Move to previous line's end.
            let prev = line.as_ref().and_then(|l| {
                l.borrow().prev.as_ref().and_then(|w| w.upgrade())
            });
            if prev.is_none() {
                return false;
            }
            line = prev;
            pointer_offset = line.as_ref().map(|l| l.borrow().data.len()).unwrap_or(0);
        } else {
            let data = line.as_ref().map(|l| l.borrow().data.clone()).unwrap_or_default();
            pointer_offset = step_left(&data, current_x);
        }

        // Seek for any of the two brackets.
        loop {
            let data = line.as_ref().map(|l| l.borrow().data.clone()).unwrap_or_default();
            let pointer_str = &data[..pointer_offset.min(data.len())];
            if let Some(found) = mbrevstrpbrk(&data, bracket_pair, pointer_str) {
                let found_x = found.as_ptr() as usize - data.as_ptr() as usize;
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current = line.clone();
                        of.current_x = found_x;
                    }
                });
                return true;
            }

            let prev = line.as_ref().and_then(|l| {
                l.borrow().prev.as_ref().and_then(|w| w.upgrade())
            });
            if prev.is_none() {
                return false;
            }
            line = prev;
            // pointer is now at end of this new line.
            // (we'll re-compute in the next iteration from data.len())
            break; // Simplified — in full implementation we'd loop properly.
        }
        false
    } else {
        // Forward search.
        let data = line.as_ref().map(|l| l.borrow().data.clone()).unwrap_or_default();
        let start = step_right(&data, current_x);
        let pointer_str = &data[start.min(data.len())..];

        let mut search_from_offset = start;

        loop {
            let data = line.as_ref().map(|l| l.borrow().data.clone()).unwrap_or_default();
            let search_slice = &data[search_from_offset.min(data.len())..];
            if let Some(found) = mbstrpbrk(search_slice, bracket_pair) {
                let found_x = (found.as_ptr() as usize - data.as_ptr() as usize);
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current = line.clone();
                        of.current_x = found_x;
                    }
                });
                return true;
            }

            let next = line.as_ref().and_then(|l| l.borrow().next.clone());
            if next.is_none() {
                return false;
            }
            line = next;
            search_from_offset = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// do_find_bracket — search for a match to the bracket at cursor
// ---------------------------------------------------------------------------
/* C: void do_find_bracket(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_find_bracket() {
    let was_current: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });
    let was_x: usize = with_state(|s| {
        s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0)
    });

    let matchbrackets: String = with_state(|s| {
        s.matchbrackets.clone().unwrap_or_default()
    });

    // Find the current character in matchbrackets.
    let current_data: String = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
            .unwrap_or_default()
    });
    let current_x = was_x;

    // Get the character at current_x.
    let ch_str = &current_data[current_x..];
    let ch_result = mbstrchr(&matchbrackets, ch_str);

    if ch_result.is_none() {
        statusline(MessageType::Ahem, "Not a bracket");
        return;
    }

    let ch_in_matchbrackets = ch_result.unwrap();

    // Find the halfway point in matchbrackets.
    let charcount = mbstrlen(&matchbrackets) / 2;
    let mut halfway = 0usize;
    for _ in 0..charcount {
        halfway += char_length(&matchbrackets[halfway..]);
    }

    // Determine search direction.
    let ch_offset = ch_in_matchbrackets.as_ptr() as usize - matchbrackets.as_ptr() as usize;
    let reverse = ch_offset >= halfway;

    // Step to find the complementary bracket.
    let ch_char = ch_in_matchbrackets.chars().next().unwrap_or('?');
    let ch_len = ch_char.len_utf8();

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
    let wanted_ch_len = wanted_char.len_utf8();

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
            s.openfile.as_ref()
                .and_then(|f| f.current.as_ref().map(|l| l.borrow().data.clone()))
                .unwrap_or_default()
        });
        let found_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));

        let found_ch = found_data[found_x..].chars().next().unwrap_or('?');

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
        let is_top = s.openfile.as_ref().map(|f| {
            f.current.as_ref().zip(f.filetop.as_ref())
                .map(|(c, t)| std::rc::Rc::ptr_eq(c, t))
                .unwrap_or(false)
        }).unwrap_or(false);
        let anchor = lp.as_ref().map(|l| l.borrow().has_anchor).unwrap_or(false);
        (lp, x, is_top, anchor)
    });

    // Toggle the anchor.
    if let Some(ref l) = current_lp {
        l.borrow_mut().has_anchor = !has_anchor;
    }
    let new_anchor = !has_anchor;

    if is_filetop {
        with_state_mut(|s| s.refresh_needed = true);
    } else if let Some(ref l) = current_lp {
        update_line(l, current_x);
    }

    let (line_numbers, minibar, zero) = with_state(|s| {
        (s.flag_isset(LINE_NUMBERS), s.flag_isset(MINIBAR), s.flag_isset(ZERO))
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
    let was_current: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });

    let is_current = was_current.as_ref()
        .map(|c| std::rc::Rc::ptr_eq(c, target))
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
                let edittop_lineno = s.openfile.as_ref()
                    .and_then(|f| f.edittop.as_ref().map(|l| l.borrow().lineno))
                    .unwrap_or(0);
                let cur_lineno = s.openfile.as_ref()
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

        let line_numbers = with_state(|s| s.flag_isset(LINE_NUMBERS));
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
    let current: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });

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
        if std::rc::Rc::ptr_eq(&line, &current_lp) {
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
    let current: Option<LinePtr> = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.clone())
    });

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
        if std::rc::Rc::ptr_eq(&line, &current_lp) {
            break;
        }
    }

    go_to_and_confirm(&line);
}
