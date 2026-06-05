#![allow(unused, non_snake_case, dead_code, non_camel_case_types)]
// Port of src/prompt.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2016, 2018, 2020-2022, 2025 Benno Schulenberg
//
// GNU nano is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crate::definitions::*;
use crate::global::{STATE, with_state, with_state_mut};
use crate::winio;
use crate::history;

// Bring the exported macros into scope.
use crate::{ISSET, TOGGLE, SET, UNSET};

/// Minimal tr! macro for this module (no-op translation).
macro_rules! tr {
    ($s:literal) => { $s };
    ($fmt:literal, $($arg:tt)*) => { format!($fmt, $($arg)*) };
}

// ---------------------------------------------------------------------------
// Module-level statics (C file-scope statics → thread_local!)
// ---------------------------------------------------------------------------

thread_local! {
    /// The prompt string used for status-bar questions.
    /// C: static char *prompt = NULL;
    static PROMPT: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());

    /// The cursor position in answer.
    /// C: static size_t typing_x = HIGHEST_POSITIVE;
    static TYPING_X: std::cell::RefCell<usize> =
        std::cell::RefCell::new(HIGHEST_POSITIVE);

    /// Input buffer for absorb_character (C: static char *puddle).
    static PUDDLE: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::with_capacity(8));
}

/// Helper: read TYPING_X.
fn get_typing_x() -> usize {
    TYPING_X.with(|tx| *tx.borrow())
}

/// Helper: write TYPING_X.
fn set_typing_x(x: usize) {
    TYPING_X.with(|tx| *tx.borrow_mut() = x);
}

/// Helper: read PROMPT as a String clone.
fn get_prompt() -> String {
    PROMPT.with(|p| p.borrow().clone())
}

/// Helper: set PROMPT.
fn set_prompt(s: String) {
    PROMPT.with(|p| *p.borrow_mut() = s);
}

// ---------------------------------------------------------------------------
// Cursor movement in the answer string
// ---------------------------------------------------------------------------

/* C: void do_statusbar_home(void) */
/// Move to the beginning of the answer.
pub fn do_statusbar_home() {
    set_typing_x(0);
}

/* C: void do_statusbar_end(void) */
/// Move to the end of the answer.
pub fn do_statusbar_end() {
    let len = with_state(|s| s.answer.len());
    set_typing_x(len);
}

/* C: void do_statusbar_prev_word(void) — #ifndef NANO_TINY */
/// Move to the previous word in the answer.
#[cfg(not(feature = "tiny"))]
pub fn do_statusbar_prev_word() {
    let answer = with_state(|s| s.answer.clone());
    let mut typing_x = get_typing_x();
    let mut seen_a_word = false;
    let mut step_forward = false;

    /* Move backward until we pass over the start of a word. */
    while typing_x != 0 {
        typing_x = step_left(&answer, typing_x);

        if is_word_char(&answer[typing_x..], false) {
            seen_a_word = true;
        } else if is_zerowidth(&answer[typing_x..]) {
            /* skip zero-width characters */
        } else if seen_a_word {
            /* This is space now: we've overshot the start of the word. */
            step_forward = true;
            break;
        }
    }

    if step_forward {
        /* Move one character forward again to sit on the start of the word. */
        typing_x = step_right(&answer, typing_x);
    }

    set_typing_x(typing_x);
}

/* C: void do_statusbar_next_word(void) — #ifndef NANO_TINY */
/// Move to the next word in the answer.
#[cfg(not(feature = "tiny"))]
pub fn do_statusbar_next_word() {
    let answer = with_state(|s| s.answer.clone());
    let mut typing_x = get_typing_x();

    let seen_space_init = !is_word_char(&answer[typing_x..], false);
    let mut seen_space = seen_space_init;
    let mut seen_word = !seen_space;
    let after_ends = ISSET!(AFTER_ENDS);

    /* Move forward until we reach either the end or the start of a word,
     * depending on whether the AFTER_ENDS flag is set or not. */
    while typing_x < answer.len() {
        typing_x = step_right(&answer, typing_x);

        if after_ends {
            /* If this is a word character, continue; else it's a separator,
             * and if we've already seen a word, then it's a word end. */
            if is_word_char(&answer[typing_x..], false) {
                seen_word = true;
            } else if is_zerowidth(&answer[typing_x..]) {
                /* skip zero-width characters */
            } else if seen_word {
                break;
            }
        } else {
            /* If this is not a word character, then it's a separator; else
             * if we've already seen a separator, then it's a word start. */
            if is_zerowidth(&answer[typing_x..]) {
                /* skip zero-width characters */
            } else if !is_word_char(&answer[typing_x..], false) {
                seen_space = true;
            } else if seen_space {
                break;
            }
        }
    }

    set_typing_x(typing_x);
}

/* C: void do_statusbar_left(void) */
/// Move left one character in the answer.
pub fn do_statusbar_left() {
    let answer = with_state(|s| s.answer.clone());
    let mut typing_x = get_typing_x();

    if typing_x > 0 {
        typing_x = step_left(&answer, typing_x);
        #[cfg(feature = "utf8")]
        while typing_x > 0 && is_zerowidth(&answer[typing_x..]) {
            typing_x = step_left(&answer, typing_x);
        }
    }

    set_typing_x(typing_x);
}

/* C: void do_statusbar_right(void) */
/// Move right one character in the answer.
pub fn do_statusbar_right() {
    let answer = with_state(|s| s.answer.clone());
    let mut typing_x = get_typing_x();

    if typing_x < answer.len() {
        typing_x = step_right(&answer, typing_x);
        #[cfg(feature = "utf8")]
        while typing_x < answer.len() && is_zerowidth(&answer[typing_x..]) {
            typing_x = step_right(&answer, typing_x);
        }
    }

    set_typing_x(typing_x);
}

// ---------------------------------------------------------------------------
// Deletion in the answer string
// ---------------------------------------------------------------------------

/* C: void do_statusbar_backspace(void) */
/// Backspace over one character in the answer.
pub fn do_statusbar_backspace() {
    let mut typing_x = get_typing_x();
    if typing_x > 0 {
        let was_x = typing_x;
        let answer = with_state(|s| s.answer.clone());
        typing_x = step_left(&answer, typing_x);
        with_state_mut(|s| {
            s.answer.drain(typing_x..was_x);
        });
        set_typing_x(typing_x);
    }
}

/* C: void do_statusbar_delete(void) */
/// Delete one character in the answer.
pub fn do_statusbar_delete() {
    let typing_x = get_typing_x();
    let answer = with_state(|s| s.answer.clone());

    if typing_x < answer.len() {
        let charlen = char_length(&answer[typing_x..]);
        with_state_mut(|s| {
            s.answer.drain(typing_x..typing_x + charlen);
        });
        #[cfg(feature = "utf8")]
        {
            let answer2 = with_state(|s| s.answer.clone());
            if typing_x < answer2.len() && is_zerowidth(&answer2[typing_x..]) {
                do_statusbar_delete();
            }
        }
    }
}

/* C: void lop_the_answer(void) */
/// Zap the part of the answer after the cursor, or the whole answer.
pub fn lop_the_answer() {
    let typing_x = get_typing_x();
    let is_at_end = with_state(|s| {
        s.answer.as_bytes().get(typing_x) == Some(&b'\0') || typing_x >= s.answer.len()
    });

    if is_at_end {
        set_typing_x(0);
    }

    with_state_mut(|s| {
        s.answer.truncate(get_typing_x());
    });
}

// ---------------------------------------------------------------------------
// Copy / paste within the answer (#ifndef NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void copy_the_answer(void) — #ifndef NANO_TINY */
/// Copy the current answer (if any) into the cutbuffer.
#[cfg(not(feature = "tiny"))]
pub fn copy_the_answer() {
    let answer = with_state(|s| s.answer.clone());
    if !answer.is_empty() {
        // free_lines(cutbuffer)  →  drop old cutbuffer
        // cutbuffer = make_new_node(NULL);  cutbuffer->data = copy_of(answer)
        use std::rc::Rc;
        use std::cell::RefCell;
        let new_node = Rc::new(RefCell::new(LineNode {
            data: answer,
            lineno: 0,
            next: None,
            prev: None,
            #[cfg(feature = "color")]
            multidata: Vec::new(),
            #[cfg(not(feature = "tiny"))]
            has_anchor: false,
        }));
        with_state_mut(|s| {
            s.cutbuffer = Some(new_node.clone());
            s.cutbottom = Some(new_node);
        });
        set_typing_x(0);
    }
}

/* C: void paste_into_answer(void) — #ifndef NANO_TINY */
/// Paste the first line of the cutbuffer into the current answer.
#[cfg(not(feature = "tiny"))]
pub fn paste_into_answer() {
    let paste_data: Option<String> = with_state(|s| {
        s.cutbuffer.as_ref().map(|cb| cb.borrow().data.clone())
    });

    if let Some(data) = paste_data {
        let pastelen = data.len();
        let typing_x = get_typing_x();

        with_state_mut(|s| {
            s.answer.insert_str(typing_x, &data);
        });

        set_typing_x(typing_x + pastelen);
    }
}

// ---------------------------------------------------------------------------
// Mouse click in prompt bar (#ifdef ENABLE_MOUSE)
// ---------------------------------------------------------------------------

/* C: int process_prompt_click(void) — #ifdef ENABLE_MOUSE */
/// Handle a mouse click in the prompt bar or the help lines.
/// Returns the same value as get_mouseinput: 0 = handled, 1 = shortcut consumed, -1 = error.
#[cfg(feature = "mouse")]
pub fn process_prompt_click() -> i32 {
    let mut click_row: i32 = 0;
    let mut click_col: i32 = 0;
    let retval = crate::winio::get_mouseinput(&mut click_row, &mut click_col);

    /* When the click is in the prompt bar, position the cursor. */
    if retval == 0 {
        // wmouse_trafo equivalent: check if click is in footwin row 0
        let (foot_y, foot_cols) = with_state(|s| (s.footwin.y as i32, s.footwin.cols as i32));
        if click_row == 0 {
            let prompt_str = get_prompt();
            let start_col = breadth(&prompt_str) + 2;
            let answer = with_state(|s| s.answer.clone());
            let typing_x = get_typing_x();

            if click_col >= start_col as i32 {
                let page_start = get_statusbar_page_start(
                    start_col,
                    start_col + wideness(&answer, typing_x),
                );
                let new_x = actual_x(
                    &answer,
                    page_start + (click_col as usize) - start_col,
                );
                set_typing_x(new_x);
            } else {
                set_typing_x(0);
            }
        }
    }

    retval
}

// ---------------------------------------------------------------------------
// Inserting characters into the answer
// ---------------------------------------------------------------------------

/* C: void inject_into_answer(char *burst, size_t count) */
/// Insert the given short burst of bytes into the answer.
pub fn inject_into_answer(burst: &[u8]) {
    let count = burst.len();
    if count == 0 {
        return;
    }

    // First encode any embedded NUL byte as 0x0A.
    let mut owned: Vec<u8> = burst.to_vec();
    for b in &mut owned {
        if *b == b'\0' {
            *b = b'\n';
        }
    }

    let typing_x = get_typing_x();
    let burst_str = String::from_utf8_lossy(&owned).into_owned();

    with_state_mut(|s| {
        s.answer.insert_str(typing_x, &burst_str);
    });

    set_typing_x(typing_x + burst_str.len());
}

/* C: void do_statusbar_verbatim_input(void) */
/// Get a verbatim keystroke and insert it into the answer.
pub fn do_statusbar_verbatim_input() {
    let mut count: usize = 1;
    let bytes = crate::winio::get_verbatim_kbinput(&mut count);

    if count > 0 && count < 999 {
        inject_into_answer(bytes.as_bytes());
    } else if count == 0 {
        crate::winio::beep();
    }
}

/* C: void absorb_character(int input, functionptrtype function) */
/// Add the given input to the input buffer when it's a normal byte,
/// and inject the gathered bytes into the answer when ready.
pub fn absorb_character(input: i32, function: Option<FuncPtr>) {
    let meta_key = with_state(|s| s.meta_key);
    let currmenu = with_state(|s| s.currmenu);
    let openfile_filename_empty = with_state(|s| {
        s.openfile.as_ref().map(|f| f.filename.is_empty()).unwrap_or(true)
    });
    let restricted = ISSET!(RESTRICTED);

    /* If not a command, discard anything that is not a normal character byte.
     * Apart from that, only accept input when not in restricted mode, or when
     * not at the "Write File" prompt, or when there is no filename yet. */
    if function.is_none() {
        if (input < 0x20 && input != b'\t' as i32) || meta_key || input > 0xFF {
            crate::winio::beep();
        } else if !restricted || currmenu != MWRITEFILE || openfile_filename_empty {
            PUDDLE.with(|p| p.borrow_mut().push(input as u8));
        }
    }

    /* If there are gathered bytes and we have a command or no other key codes
     * are waiting, it's time to insert these bytes into the answer. */
    let depth = PUDDLE.with(|p| p.borrow().len());
    if depth > 0 && (function.is_some() || crate::winio::waiting_keycodes() == 0) {
        let bytes: Vec<u8> = PUDDLE.with(|p| p.borrow().clone());
        inject_into_answer(&bytes);
        PUDDLE.with(|p| p.borrow_mut().clear());
    }
}

// ---------------------------------------------------------------------------
// Editing shortcut dispatcher
// ---------------------------------------------------------------------------

/* C: bool handle_editing(functionptrtype function) */
/// Handle any editing shortcut, and return true when handled.
pub fn handle_editing(function: FuncPtr) -> bool {
    use crate::global::*;

    if function == do_left as FuncPtr {
        do_statusbar_left();
    } else if function == do_right as FuncPtr {
        do_statusbar_right();
    } else if {
        #[cfg(not(feature = "tiny"))]
        { function == to_prev_word as FuncPtr }
        #[cfg(feature = "tiny")]
        { false }
    } {
        #[cfg(not(feature = "tiny"))]
        do_statusbar_prev_word();
    } else if {
        #[cfg(not(feature = "tiny"))]
        { function == to_next_word as FuncPtr }
        #[cfg(feature = "tiny")]
        { false }
    } {
        #[cfg(not(feature = "tiny"))]
        do_statusbar_next_word();
    } else if function == do_home as FuncPtr {
        do_statusbar_home();
    } else if function == do_end as FuncPtr {
        do_statusbar_end();
    } else if {
        /* When in restricted mode at the "Write File" prompt and the
         * filename isn't blank, disallow any input and deletion. */
        let restricted = ISSET!(RESTRICTED);
        let at_writefile = with_state(|s| s.currmenu) == MWRITEFILE;
        let filename_nonempty = with_state(|s| {
            s.openfile.as_ref().map(|f| !f.filename.is_empty()).unwrap_or(false)
        });
        restricted && at_writefile && filename_nonempty
    } && (function == do_verbatim_input as FuncPtr
        || function == do_delete as FuncPtr
        || function == do_backspace as FuncPtr
        || function == cut_text as FuncPtr
        || function == paste_text as FuncPtr)
    {
        // disallowed — do nothing
    } else if function == do_verbatim_input as FuncPtr {
        do_statusbar_verbatim_input();
    } else if function == do_delete as FuncPtr {
        do_statusbar_delete();
    } else if function == do_backspace as FuncPtr {
        do_statusbar_backspace();
    } else if function == cut_text as FuncPtr {
        lop_the_answer();
    } else if {
        #[cfg(not(feature = "tiny"))]
        { function == copy_text as FuncPtr }
        #[cfg(feature = "tiny")]
        { false }
    } {
        #[cfg(not(feature = "tiny"))]
        copy_the_answer();
    } else if {
        #[cfg(not(feature = "tiny"))]
        { function == paste_text as FuncPtr }
        #[cfg(feature = "tiny")]
        { false }
    } {
        #[cfg(not(feature = "tiny"))]
        {
            let has_cutbuffer = with_state(|s| s.cutbuffer.is_some());
            if has_cutbuffer {
                paste_into_answer();
            }
        }
    } else {
        return false;
    }

    /* Don't handle any handled function again. */
    true
}

// ---------------------------------------------------------------------------
// Prompt bar page-start calculation
// ---------------------------------------------------------------------------

/* C: size_t get_statusbar_page_start(size_t base, size_t column) */
/// Return the column number of the first character of the answer that is
/// displayed in the status bar when the cursor is at the given column,
/// with the available room for the answer starting at base.
/// Note that (0 <= column - get_statusbar_page_start(column) < COLS).
pub fn get_statusbar_page_start(base: usize, column: usize) -> usize {
    let cols = crate::winio::get_cols();

    if cols == 0 {
        return 0;
    }
    if column == base || column < cols.saturating_sub(1) {
        0
    } else if cols > base + 2 {
        column - base - 1 - (column - base - 1) % (cols - base - 2)
    } else {
        column.saturating_sub(2)
    }
}

/* C: void put_cursor_at_end_of_answer(void) */
/// Reinitialize the cursor position in the answer.
pub fn put_cursor_at_end_of_answer() {
    set_typing_x(HIGHEST_POSITIVE);
}

// ---------------------------------------------------------------------------
// Drawing the prompt bar
// ---------------------------------------------------------------------------

/* C: void draw_the_promptbar(void) */
/// Redraw the prompt bar and place the cursor at the right spot.
pub fn draw_the_promptbar() {
    let prompt_str = get_prompt();
    let answer = with_state(|s| s.answer.clone());
    let typing_x = get_typing_x();

    let base = breadth(&prompt_str) + 2;
    let column = base + wideness(&answer, typing_x);

    let the_page = get_statusbar_page_start(base, column);
    let end_page = get_statusbar_page_start(
        base,
        base + breadth(&answer).saturating_sub(1),
    );

    let cols = crate::winio::get_cols();

    // Color the prompt bar over its full width.
    let prompt_bar_pair = with_state(|s| s.interface_color_pair[PROMPT_BAR]);
    crate::winio::footwin_wattron(prompt_bar_pair);
    // mvwprintw(footwin, 0, 0, "%*s", COLS, " ") — fill with spaces
    crate::winio::footwin_mvwprintw_spaces(0, 0, cols);

    crate::winio::footwin_mvwaddstr(0, 0, &prompt_str);
    crate::winio::footwin_waddch(b':' as char);
    crate::winio::footwin_waddch(if the_page == 0 { ' ' } else { '<' });

    let expanded = display_string(&answer, the_page, cols.saturating_sub(base), false, true);
    crate::winio::footwin_waddstr(&expanded);

    if the_page < end_page && base + breadth(&answer) - the_page > cols {
        crate::winio::footwin_mvwaddch(0, cols.saturating_sub(1) as i32, '>');
    }

    crate::winio::footwin_wattroff(prompt_bar_pair);

    // Place the cursor at the right spot.
    crate::winio::footwin_wmove(0, (column - the_page) as i32);

    crate::winio::footwin_wnoutrefresh();
}

// ---------------------------------------------------------------------------
// Pipe-symbol toggle (#ifndef NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void add_or_remove_pipe_symbol_from_answer(void) — #ifndef NANO_TINY */
/// Remove or add the pipe character at the answer's head.
#[cfg(not(feature = "tiny"))]
pub fn add_or_remove_pipe_symbol_from_answer() {
    let typing_x = get_typing_x();
    let starts_with_pipe = with_state(|s| s.answer.starts_with('|'));

    if starts_with_pipe {
        with_state_mut(|s| {
            s.answer.remove(0);
        });
        if typing_x > 0 {
            set_typing_x(typing_x - 1);
        }
    } else {
        with_state_mut(|s| {
            s.answer.insert(0, '|');
        });
        set_typing_x(typing_x + 1);
    }
}

// ---------------------------------------------------------------------------
// Core prompt input loop
// ---------------------------------------------------------------------------

/* C: functionptrtype acquire_an_answer(int *actual, bool *listed,
 *     linestruct **history_list, void (*refresh_func)(void)) */
/// Get a string of input at the status-bar prompt.
/// Returns the function that terminated the loop, plus the last keystroke.
fn acquire_an_answer(
    history_kind: Option<crate::history::HistoryKind>,
    refresh_func: Option<fn()>,
    listed: &mut bool,
) -> (Option<FuncPtr>, i32) {
    #[cfg(all(feature = "histories", feature = "tabcomp"))]
    let mut previous_was_tab = false;
    #[cfg(all(feature = "histories", feature = "tabcomp"))]
    let mut fragment_length: usize = 0;

    #[cfg(feature = "histories")]
    let mut stored_string: Option<String> = None;

    #[cfg(not(feature = "tiny"))]
    let mut bracketed_paste = false;

    // Make sure typing_x is within bounds.
    {
        let answer_len = with_state(|s| s.answer.len());
        if get_typing_x() > answer_len {
            set_typing_x(answer_len);
        }
    }

    let mut input: i32 = 0;
    let mut function: Option<FuncPtr> = None;

    loop {
        draw_the_promptbar();

        /* Read in one keystroke. */
        input = crate::winio::get_kbinput(VISIBLE);

        #[cfg(not(feature = "tiny"))]
        {
            /* If the window size changed, go reformat the prompt string. */
            if input == THE_WINDOW_RESIZED as i32 {
                // Cleanup stored_string
                #[cfg(feature = "histories")]
                {
                    // stored_string dropped automatically
                }
                return (None, THE_WINDOW_RESIZED as i32);
            }
            if input == START_OF_PASTE as i32 || input == END_OF_PASTE as i32 {
                bracketed_paste = input == START_OF_PASTE as i32;
            }
        }

        #[cfg(feature = "mouse")]
        {
            /* For a click on a shortcut, read in the resulting keycode. */
            if input == crate::winio::KEY_MOUSE_CODE {
                if process_prompt_click() == 1 {
                    input = crate::winio::get_kbinput(BLIND);
                }
                if input == crate::winio::KEY_MOUSE_CODE {
                    continue;
                }
            }
        }

        /* Check for a shortcut in the current list. */
        function = crate::global::get_shortcut(input);

        #[cfg(not(feature = "tiny"))]
        {
            /* Tabs in an external paste are not commands. */
            if input == b'\t' as i32 && bracketed_paste {
                function = None;
            }
        }

        /* When it's a normal character, add it to the answer. */
        let func_copy = function;
        absorb_character(input, func_copy);

        #[cfg(not(feature = "tiny"))]
        {
            /* Ignore any commands inside an external paste. */
            if bracketed_paste {
                if let Some(f) = function {
                    if f != crate::global::do_nothing as FuncPtr {
                        crate::winio::beep();
                    }
                }
                continue;
            }
        }

        // Check for cancel or enter
        let is_cancel = function.map_or(false, |f| f == crate::global::do_cancel as FuncPtr);
        let is_enter  = function.map_or(false, |f| f == crate::global::do_enter  as FuncPtr);

        if is_cancel || is_enter {
            break;
        }

        #[cfg(feature = "tabcomp")]
        {
            let is_tab = function.map_or(false, |f| f == crate::global::do_tab as FuncPtr);
            if is_tab {
                #[cfg(feature = "histories")]
                {
                    if let Some(kind) = history_kind {
                        if !previous_was_tab {
                            fragment_length = with_state(|s| s.answer.len());
                        }

                        if fragment_length > 0 {
                            let new_answer = crate::history::get_history_completion(
                                kind,
                                &with_state(|s| s.answer.clone()),
                                fragment_length,
                            );
                            let new_len = new_answer.len();
                            with_state_mut(|s| s.answer = new_answer);
                            set_typing_x(new_len);
                        }
                    } else {
                        do_tab_complete(refresh_func, listed);
                    }
                }
                #[cfg(not(feature = "histories"))]
                {
                    do_tab_complete(refresh_func, listed);
                }

                #[cfg(all(feature = "histories", feature = "tabcomp"))]
                {
                    previous_was_tab = true;
                }
                continue;
            } else {
                #[cfg(all(feature = "histories", feature = "tabcomp"))]
                {
                    previous_was_tab = false;
                }
            }
        }

        #[cfg(feature = "histories")]
        {
            let is_older = function.map_or(false, |f| f == crate::global::get_older_item as FuncPtr);
            let is_newer = function.map_or(false, |f| f == crate::global::get_newer_item as FuncPtr);

            if is_older && history_kind.is_some() {
                let kind = history_kind.unwrap();

                /* If this is the first step into history, start at the bottom. */
                if stored_string.is_none() {
                    crate::history::reset_history_pointer_for(kind);
                }

                /* When moving up from the bottom, remember the current answer. */
                let at_bottom = is_history_at_bottom(kind);
                if at_bottom {
                    stored_string = Some(with_state(|s| s.answer.clone()));
                }

                /* If there is an older item, move to it and copy its string. */
                if let Some(older) = get_older_history_item(kind) {
                    let len = older.len();
                    with_state_mut(|s| s.answer = older);
                    set_typing_x(len);
                }
            } else if is_newer && history_kind.is_some() {
                let kind = history_kind.unwrap();

                /* If there is a newer item, move to it and copy its string. */
                if let Some(newer) = get_newer_history_item(kind) {
                    let len = newer.len();
                    with_state_mut(|s| s.answer = newer);
                    set_typing_x(len);
                }

                /* When at the bottom of the history list, restore the old answer. */
                if is_history_at_bottom(kind) {
                    if let Some(ref stored) = stored_string {
                        if with_state(|s| s.answer.is_empty()) {
                            let s2 = stored.clone();
                            let len = s2.len();
                            with_state_mut(|s| s.answer = s2);
                            set_typing_x(len);
                        }
                    }
                }
            } else {
                // fall through to other checks
                history_handle_other(function, refresh_func);
            }
        }

        #[cfg(not(feature = "histories"))]
        {
            history_handle_other(function, refresh_func);
        }

        #[cfg(all(feature = "histories", feature = "tabcomp"))]
        {
            previous_was_tab = function.map_or(false, |f| f == crate::global::do_tab as FuncPtr);
        }
    }

    #[cfg(not(feature = "tiny"))]
    {
        /* When an external command was run, clear a possibly stashed answer. */
        let at_execute = with_state(|s| s.currmenu) == MEXECUTE;
        let is_enter = function.map_or(false, |f| f == crate::global::do_enter as FuncPtr);
        if at_execute && is_enter {
            // *foretext = '\0'
            with_state_mut(|s| {
                if let Some(ref mut ft) = s.foretext {
                    ft.clear();
                }
            });
        }
    }

    #[cfg(feature = "histories")]
    {
        /* If the history pointer was moved, point it at the bottom again. */
        if stored_string.is_some() {
            if let Some(kind) = history_kind {
                crate::history::reset_history_pointer_for(kind);
            }
        }
    }

    (function, input)
}

/// Handle do_help / full_refresh / do_toggle / do_nothing / implant and
/// non-editing shortcuts.  Factored out so it can be called from both the
/// histories branch and the no-histories branch.
fn history_handle_other(function: Option<FuncPtr>, refresh_func: Option<fn()>) {
    let is_help      = function.map_or(false, |f| f == crate::global::do_help         as FuncPtr);
    let is_refresh   = function.map_or(false, |f| f == crate::global::full_refresh     as FuncPtr);

    if is_help || is_refresh {
        if let Some(f) = function {
            f();
        }
        return;
    }

    #[cfg(not(feature = "tiny"))]
    {
        let is_toggle = function.map_or(false, |f| f == crate::global::do_toggle as FuncPtr);
        if is_toggle {
            // Check if the shortcut's toggle is NO_HELP.
            let toggle_is_no_help = with_state(|s| {
                let currmenu = s.currmenu;
                // Find the shortcut with do_toggle that matches
                s.sclist.iter().any(|sc| {
                    sc.func == Some(crate::global::do_toggle as FuncPtr)
                        && (sc.menus as u32 & currmenu) != 0
                        && sc.toggle == NO_HELP as i32
                })
            });

            if toggle_is_no_help {
                TOGGLE!(NO_HELP);
                crate::nano::window_init();
                with_state_mut(|s| s.focusing = false);
                if let Some(rf) = refresh_func {
                    rf();
                }
                crate::winio::bottombars(with_state(|s| s.currmenu));
                return;
            }
        }

        let is_nothing = function.map_or(false, |f| f == crate::global::do_nothing as FuncPtr);
        if is_nothing {
            return;
        }
    }

    #[cfg(feature = "nanorc")]
    {
        // implant check: if function == implant, call implant(shortcut->expansion)
        // We skip this for now since implant has a special C signature.
        // TODO: wire up implant properly when rcfile.rs is ported.
    }

    // Generic shortcut: run it if not view-mode or if it doesn't change content.
    if let Some(f) = function {
        let is_editing = handle_editing(f);
        if !is_editing {
            let view_mode = ISSET!(VIEW_MODE);
            if !view_mode || !changes_something(f) {
                #[cfg(not(feature = "tiny"))]
                {
                    /* When invoking a tool at the Execute prompt, stash an "answer". */
                    let at_execute = with_state(|s| s.currmenu) == MEXECUTE;
                    if at_execute {
                        let ans = with_state(|s| s.answer.clone());
                        with_state_mut(|s| {
                            s.foretext = Some(ans);
                        });
                    }
                }
                f();
            } else {
                crate::winio::beep();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// History helpers (Vec-based, since we ported history.rs that way)
// ---------------------------------------------------------------------------

#[cfg(feature = "histories")]
fn is_history_at_bottom(kind: crate::history::HistoryKind) -> bool {
    with_state(|s| {
        match kind {
            crate::history::HistoryKind::Search  =>
                s.search_history_pos  >= s.search_history_items.len(),
            crate::history::HistoryKind::Replace =>
                s.replace_history_pos >= s.replace_history_items.len(),
            crate::history::HistoryKind::Execute =>
                s.execute_history_pos >= s.execute_history_items.len(),
        }
    })
}

/// Move the history pointer one step older (upward).
/// Returns the string at the new position, or None if already at the top.
#[cfg(feature = "histories")]
fn get_older_history_item(kind: crate::history::HistoryKind) -> Option<String> {
    with_state_mut(|s| {
        let (items, pos) = match kind {
            crate::history::HistoryKind::Search  =>
                (&s.search_history_items,  &mut s.search_history_pos  as *mut usize),
            crate::history::HistoryKind::Replace =>
                (&s.replace_history_items, &mut s.replace_history_pos as *mut usize),
            crate::history::HistoryKind::Execute =>
                (&s.execute_history_items, &mut s.execute_history_pos as *mut usize),
        };
        // Safety: we hold a mutable borrow; the pointer is valid for the duration.
        let pos_ref: &mut usize = unsafe { &mut *pos };
        if *pos_ref > 0 {
            *pos_ref -= 1;
            Some(items[*pos_ref].clone())
        } else {
            None
        }
    })
}

/// Move the history pointer one step newer (downward).
/// Returns the string at the new position, or None if already at the bottom.
#[cfg(feature = "histories")]
fn get_newer_history_item(kind: crate::history::HistoryKind) -> Option<String> {
    with_state_mut(|s| {
        let (items_len, pos) = match kind {
            crate::history::HistoryKind::Search  =>
                (s.search_history_items.len(),  &mut s.search_history_pos  as *mut usize),
            crate::history::HistoryKind::Replace =>
                (s.replace_history_items.len(), &mut s.replace_history_pos as *mut usize),
            crate::history::HistoryKind::Execute =>
                (s.execute_history_items.len(), &mut s.execute_history_pos as *mut usize),
        };
        let pos_ref: &mut usize = unsafe { &mut *pos };
        if *pos_ref < items_len {
            *pos_ref += 1;
        }
        if *pos_ref < items_len {
            let items = match kind {
                crate::history::HistoryKind::Search  => &s.search_history_items,
                crate::history::HistoryKind::Replace => &s.replace_history_items,
                crate::history::HistoryKind::Execute => &s.execute_history_items,
            };
            Some(items[*pos_ref].clone())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Tab completion helper
// ---------------------------------------------------------------------------

/// Handle tab completion for filenames in the prompt.
/// C: answer = input_tab(answer, &typing_x, refresh_func, listed)
#[cfg(feature = "tabcomp")]
fn do_tab_complete(refresh_func: Option<fn()>, listed: &mut bool) {
    let currmenu = with_state(|s| s.currmenu);
    let restricted = ISSET!(RESTRICTED);

    /* Allow tab completion of filenames, but not in restricted mode. */
    if (currmenu & (MINSERTFILE | MWRITEFILE | MGOTODIR)) != 0 && !restricted {
        let answer = with_state(|s| s.answer.clone());
        let mut tx = get_typing_x();
        let new_answer = crate::files::input_tab(&answer, &mut tx, refresh_func.unwrap_or(|| {}), listed);
        let new_len = new_answer.len();
        with_state_mut(|s| s.answer = new_answer);
        set_typing_x(tx);
    }
}

// ---------------------------------------------------------------------------
// Public do_prompt
// ---------------------------------------------------------------------------

/* C: int do_prompt(int menu, const char *provided,
 *     linestruct **history_list, void (*refresh_func)(void),
 *     const char *msg, ...) */
/// Ask a question on the status bar.
///
/// Returns:
///  *  0  — text was entered
///  * -1  — cancelled
///  * -2  — blank string entered (Enter with empty answer)
///  * keycode — a valid shortcut key was pressed
///
/// The `provided` parameter is the default answer for when simply Enter is typed.
/// `history_kind` selects which history list to use (None = no history navigation).
/// `refresh_func` is called when needed to repaint the edit window.
/// `msg` is the printf-style prompt format string (pre-formatted by the caller here,
/// since Rust doesn't have varargs; pass the final string directly).
pub fn do_prompt(
    menu: u32,
    provided: Option<&str>,
    history_kind: Option<crate::history::HistoryKind>,
    refresh_func: Option<fn()>,
    msg: &str,
) -> i32 {
    let mut listed = false;

    /* Save a possible current status-bar x position and prompt. */
    let was_typing_x = get_typing_x();
    let saved_prompt = get_prompt();

    crate::winio::bottombars(menu);

    /* Set the answer to the provided default, if any. */
    {
        let provided_str = provided.unwrap_or("");
        let cur_answer = with_state(|s| s.answer.clone());
        if cur_answer != provided_str {
            with_state_mut(|s| s.answer = provided_str.to_string());
        }
    }

    let cols = crate::winio::get_cols();
    let maxcharlen = MAXCHARLEN;

    // redo_theprompt label → we use a loop for the resize-retry.
    let result;
    loop {
        /* Build the prompt string truncated to fit on screen. */
        let mut prompt_buf = msg.to_string();
        /* Reserve five columns for colon plus angles plus answer, ":<aa>". */
        let max_prompt_bytes = if cols < 5 { 0 } else { cols - 5 };
        let trunc_x = actual_x(&prompt_buf, max_prompt_bytes);
        prompt_buf.truncate(trunc_x);
        set_prompt(prompt_buf);

        with_state_mut(|s| s.lastmessage = MessageType::Vacuum);

        let (function, retval_raw) = acquire_an_answer(history_kind, refresh_func, &mut listed);

        #[cfg(not(feature = "tiny"))]
        {
            if retval_raw == THE_WINDOW_RESIZED as i32 {
                // Redo the prompt after resize.
                continue;
            }
        }

        /* Restore a possible previous prompt and maybe the typing position. */
        set_prompt(saved_prompt.clone());

        let restore_tx = function.map_or(false, |f| {
            f == crate::global::do_cancel    as FuncPtr
            || f == crate::global::do_enter  as FuncPtr
            || f == crate::global::to_first_line as FuncPtr
            || f == crate::global::to_last_line  as FuncPtr
            || {
                #[cfg(feature = "browser")]
                {
                    f == crate::global::to_first_file as FuncPtr
                    || f == crate::global::to_last_file  as FuncPtr
                }
                #[cfg(not(feature = "browser"))]
                { false }
            }
        });

        if restore_tx {
            set_typing_x(was_typing_x);
        }

        /* Set the proper return value for Cancel and Enter. */
        let retval = if function.map_or(false, |f| f == crate::global::do_cancel as FuncPtr) {
            -1
        } else if function.map_or(false, |f| f == crate::global::do_enter as FuncPtr) {
            if with_state(|s| s.answer.is_empty()) { -2 } else { 0 }
        } else {
            retval_raw
        };

        let last_vacuum = with_state(|s| s.lastmessage == MessageType::Vacuum);
        if last_vacuum {
            crate::winio::wipe_statusbar();
        }

        #[cfg(feature = "tabcomp")]
        {
            /* If possible filename completions are still listed, clear them off. */
            if listed {
                if let Some(rf) = refresh_func {
                    rf();
                }
            }
        }

        result = retval;
        break;
    }

    result
}

// ---------------------------------------------------------------------------
// ask_user — Yes/No/All/Cancel prompt
// ---------------------------------------------------------------------------

const UNDECIDED: i32 = -2;

/* C: int ask_user(bool withall, const char *question) */
/// Ask a simple Yes/No (and optionally All) question on the status bar
/// and return the choice — either YES or NO or ALL or CANCEL.
pub fn ask_user(withall: bool, question: &str) -> i32 {
    let mut choice = UNDECIDED;
    let mut width: usize = 16;

    /* TRANSLATORS: For the next three strings, specify the starting letters
     * of the translations for "Yes"/"No"/"All".  The first letter of each of
     * these strings MUST be a single-byte letter; others may be multi-byte. */
    // C: const char *yesstr = _("Yy");
    let yesstr = tr!("Yy");
    // C: const char *nostr  = _("Nn");
    let nostr  = tr!("Nn");
    // C: const char *allstr = _("Aa");
    let allstr = tr!("Aa");

    while choice == UNDECIDED {
        let kbinput: i32;

        // Draw shortcut keys when help lines are shown.
        if !ISSET!(NO_HELP) {
            let cols = crate::winio::get_cols();
            if cols < 32 {
                width = cols / 2;
            }

            /* Clear the shortcut list from the bottom of the screen. */
            crate::winio::blank_bottombars();

            /* Now show the ones for "Yes", "No", "Cancel" and maybe "All". */
            let yes_key = format!(" {}", yesstr.chars().next().unwrap_or('Y'));
            crate::winio::footwin_wmove(1, 0);
            crate::winio::post_one_key(&yes_key, tr!("Yes"), width as i32);

            let no_key = format!(" {}", nostr.chars().next().unwrap_or('N'));
            crate::winio::footwin_wmove(2, 0);
            crate::winio::post_one_key(&no_key, tr!("No"), width as i32);

            if withall {
                let all_key = format!(" {}", allstr.chars().next().unwrap_or('A'));
                crate::winio::footwin_wmove(1, width as i32);
                crate::winio::post_one_key(&all_key, tr!("All"), width as i32);
            }

            // Find the cancel shortcut.
            let cancel_keystr: &'static str =
                crate::global::first_sc_for(MYESNO, crate::global::do_cancel as FuncPtr)
                    .map(|(_, ks)| ks)
                    .unwrap_or("^C");
            crate::winio::footwin_wmove(2, width as i32);
            crate::winio::post_one_key(cancel_keystr, tr!("Cancel"), width as i32);
        }

        /* Color the prompt bar over its full width and display the question. */
        let prompt_bar_pair = with_state(|s| s.interface_color_pair[PROMPT_BAR]);
        let cols = crate::winio::get_cols();
        crate::winio::footwin_wattron(prompt_bar_pair);
        crate::winio::footwin_mvwprintw_spaces(0, 0, cols);
        let qlen = actual_x(question, cols.saturating_sub(1));
        crate::winio::footwin_mvwaddnstr(0, 0, question, qlen);
        crate::winio::footwin_wattroff(prompt_bar_pair);
        crate::winio::footwin_wnoutrefresh();

        with_state_mut(|s| s.currmenu = MYESNO);

        /* When not replacing, show the cursor while waiting for a key. */
        kbinput = crate::winio::get_kbinput(!withall);

        #[cfg(not(feature = "tiny"))]
        {
            if kbinput == THE_WINDOW_RESIZED as i32 {
                continue;
            }

            /* Accept first character of an external paste and ignore the rest. */
            if kbinput == START_OF_PASTE as i32 {
                let first = crate::winio::get_kbinput(BLIND);
                loop {
                    let k = crate::winio::get_kbinput(BLIND);
                    if k == END_OF_PASTE as i32 {
                        break;
                    }
                }
                // Use `first` as the actual input (matches C behaviour).
                // But in C the variable `kbinput` is replaced with the char after START_OF_PASTE.
                // We need to reassign — but kbinput is not mut here in the outer scope.
                // We handle this by falling through with `first` as the letter check.
                let ch = first as u8 as char;
                if yesstr.contains(ch) {
                    choice = YES;
                } else if nostr.contains(ch) {
                    choice = NO;
                } else if withall && allstr.contains(ch) {
                    choice = ALL;
                }
                // Either we set choice or it stays UNDECIDED; loop continues.
                continue;
            }
        }

        // NLS letter check: match against yes/no/all strings.
        {
            let letter = char::from_u32(kbinput as u32).unwrap_or('\0');
            if yesstr.contains(letter) {
                choice = YES;
            } else if nostr.contains(letter) {
                choice = NO;
            } else if withall && allstr.contains(letter) {
                choice = ALL;
            }
        }

        if choice != UNDECIDED {
            break;
        }

        let func = crate::global::get_shortcut(kbinput);

        if func.map_or(false, |f| f == crate::global::do_cancel as FuncPtr) {
            choice = CANCEL;
        } else if func.map_or(false, |f| f == crate::global::full_refresh as FuncPtr) {
            crate::global::full_refresh();
        } else if {
            #[cfg(not(feature = "tiny"))]
            { func.map_or(false, |f| f == crate::global::do_toggle as FuncPtr) }
            #[cfg(feature = "tiny")]
            { false }
        } {
            #[cfg(not(feature = "tiny"))]
            {
                // Check if it's the NO_HELP toggle.
                let toggle_is_no_help = with_state(|s| {
                    let currmenu = s.currmenu;
                    s.sclist.iter().any(|sc| {
                        sc.func == Some(crate::global::do_toggle as FuncPtr)
                            && (sc.menus as u32 & currmenu) != 0
                            && sc.toggle == NO_HELP as i32
                    })
                });
                if toggle_is_no_help {
                    TOGGLE!(NO_HELP);
                    crate::nano::window_init();
                    crate::winio::titlebar(None);
                    with_state_mut(|s| s.focusing = false);
                    crate::winio::edit_refresh();
                    with_state_mut(|s| s.focusing = true);
                }
            }
        }
        /* Interpret ^N as "No", to allow exiting in anger, and ^Q or ^X too. */
        else if kbinput == b'\x0E' as i32
            || (kbinput == b'\x11' as i32 && !ISSET!(MODERN_BINDINGS))
            || (kbinput == b'\x18' as i32 &&  ISSET!(MODERN_BINDINGS))
        {
            choice = NO;
            if kbinput != b'\x0E' as i32 {
                // ^X^Q makes nano exit with an error.
                with_state_mut(|s| s.final_status = 2);
            }
        }
        /* Also, interpret ^Y as "Yes", and ^A as "All". */
        else if kbinput == b'\x19' as i32 {
            choice = YES;
        } else if kbinput == b'\x01' as i32 && withall {
            choice = ALL;
        } else if {
            #[cfg(feature = "mouse")]
            { kbinput == crate::winio::KEY_MOUSE_CODE }
            #[cfg(not(feature = "mouse"))]
            { false }
        } {
            #[cfg(feature = "mouse")]
            {
                let mut mouse_y: i32 = 0;
                let mut mouse_x: i32 = 0;
                if crate::winio::get_mouseinput(&mut mouse_y, &mut mouse_x) == 0 {
                    // Check if click is in footwin, y > 0.
                    let in_footwin = true; // wmouse_trafo equivalent
                    if in_footwin && mouse_x < (width * 2) as i32 && mouse_y > 0 {
                        let x = mouse_x / width as i32;
                        let y = mouse_y - 1;

                        /* x == 0 means Yes or No, y == 0 means Yes or All. */
                        choice = -2 * x * y + x - y + 1;

                        if choice == ALL && !withall {
                            choice = UNDECIDED;
                        }
                    }
                }
            }
        } else {
            crate::winio::beep();
        }
    }

    choice
}

// ---------------------------------------------------------------------------
// Stub wrappers for functions referenced but defined in other modules
// ---------------------------------------------------------------------------
// These forward to the char/utils modules once they are ported.
// For now they call the stubs so the module compiles stand-alone.

/// C: size_t breadth(const char *s) — display width of s.
/// Forwarded to utils::breadth.
#[inline]
fn breadth(s: &str) -> usize {
    crate::utils::breadth(s)
}

/// C: size_t wideness(const char *s, size_t pos) — display width up to pos bytes.
#[inline]
fn wideness(s: &str, pos: usize) -> usize {
    crate::utils::wideness(s, pos)
}

/// C: size_t actual_x(const char *str, size_t column) — byte offset for column.
#[inline]
fn actual_x(s: &str, column: usize) -> usize {
    crate::utils::actual_x(s, column)
}

/// C: size_t step_left(const char *s, size_t pos) — byte offset one char left.
#[inline]
fn step_left(s: &str, pos: usize) -> usize {
    crate::chars::step_left(s, pos)
}

/// C: size_t step_right(const char *s, size_t pos) — byte offset one char right.
#[inline]
fn step_right(s: &str, pos: usize) -> usize {
    crate::chars::step_right(s, pos)
}

/// C: size_t char_length(const char *s) — byte length of the char at s.
#[inline]
fn char_length(s: &str) -> usize {
    crate::chars::char_length(s)
}

/// C: bool is_word_char(const char *s, bool allow_punct).
#[inline]
fn is_word_char(s: &str, allow_punct: bool) -> bool {
    crate::chars::is_word_char(s, allow_punct)
}

/// C: bool is_zerowidth(const char *s) — true for zero-width Unicode chars.
/// Always defined; when utf8 is disabled it always returns false.
#[inline]
fn is_zerowidth(s: &str) -> bool {
    #[cfg(feature = "utf8")]
    { crate::chars::is_zerowidth(s) }
    #[cfg(not(feature = "utf8"))]
    { false }
}

/// C: char *display_string(…) — produce a printable version of the answer
/// for the given page offset and width.
/// In nano, display_string() is defined in winio.c, so we call crate::winio::display_string.
#[inline]
fn display_string(s: &str, start_col: usize, span: usize, isdata: bool, isprompt: bool) -> String {
    crate::winio::display_string(s, start_col, span, isdata, isprompt)
}

/// C: bool changes_something(functionptrtype f) — true if f modifies the buffer.
#[inline]
fn changes_something(f: FuncPtr) -> bool {
    crate::nano::changes_something(f)
}
