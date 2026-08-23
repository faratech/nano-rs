#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]
// Port of src/text.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2015 Mark Majeres
//             Copyright (C) 2016 Mike Scalora
//             Copyright (C) 2016 Sumedh Pendurkar
//             Copyright (C) 2018 Marco Diego Aurélio Mesquita
//             Copyright (C) 2015-2022, 2024 Benno Schulenberg
//
// GNU nano is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#[allow(unused_imports)] // some of these are used only under feature gates
use crate::chars::{
    advance_over, char_length, is_blank_char, is_word_char, mbstrchr, mbstrlen, step_left,
    step_right, white_string,
};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::cut::{do_snip, expunge};
use crate::definitions::*;
use crate::files::set_modified;
use crate::global::{flag_index, flag_mask, state, state_mut, with_state, with_state_mut};
use crate::search::goto_line_posx;
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::utils::{
    actual_x, breadth, get_range, get_region, mark_is_before_cursor, measured_copy, new_magicline,
    remove_magicline, wideness, xplustabs,
};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::winio::{
    adjust_viewport, blank_bottombars, bottombars, edit_refresh, ensure_firstcolumn_is_aligned,
    full_refresh, place_the_cursor, statusbar, statusline, titlebar, window_init, wipe_statusbar,
};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::{ISSET, SET, UNSET, tr};
#[allow(unused_imports)] // some of these are used only under feature gates
#[allow(unused_imports)] // some of these are used only under feature gates
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Byte/editing-unit boundary helpers
//
// Nano stores byte offsets, but in a UTF-8 locale a valid scalar must remain
// atomic.  Malformed bytes are deliberately one editing unit each.  These
// helpers therefore use the same decoder as cursor motion instead of Rust's
// `str` boundaries (document data is not required to be UTF-8).
// ---------------------------------------------------------------------------

/// Return the largest editing-unit boundary that is not greater than `pos`.
#[inline]
fn safe_edit_boundary<T: AsRef<[u8]> + ?Sized>(data: &T, pos: usize) -> usize {
    let bytes = data.as_ref();
    let target = pos.min(bytes.len());
    if target == 0 {
        return 0;
    }

    // `step_left()` probes at most four bytes backward and validates while
    // walking forward.  This keeps typing at the end of a very long line O(1).
    let previous = step_left(bytes, target);
    let previous_end = (previous + char_length(&bytes[previous..])).min(bytes.len());
    if previous_end == target {
        target
    } else {
        previous
    }
}

/// Return the smallest editing-unit boundary that is not less than `pos`.
#[inline]
fn safe_edit_boundary_end<T: AsRef<[u8]> + ?Sized>(data: &T, pos: usize) -> usize {
    let bytes = data.as_ref();
    let target = pos.min(bytes.len());
    let start = safe_edit_boundary(bytes, target);
    if start == target {
        target
    } else {
        (start + char_length(&bytes[start..])).min(bytes.len())
    }
}

/// Return a stored byte range only when both ends are editing-unit boundaries.
#[inline]
fn checked_edit_range<T: AsRef<[u8]> + ?Sized>(
    data: &T,
    start: usize,
    len: usize,
) -> Option<std::ops::Range<usize>> {
    let bytes = data.as_ref();
    let end = start.checked_add(len)?;
    if end <= bytes.len()
        && safe_edit_boundary(bytes, start) == start
        && safe_edit_boundary(bytes, end) == end
    {
        Some(start..end)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Local helper stubs for functions not yet imported from other modules.
// These forward to the real implementations once all modules are wired.
// ---------------------------------------------------------------------------

/// C: make_new_node(prev) — create a new, empty LineNode (nano.c).
#[inline]
fn make_new_node(prev: Option<LinePtr>) -> LinePtr {
    crate::nano::make_new_node(prev.as_ref())
}

/// C: splice_node(node, newnode) — insert newnode after node (nano.c).
/// Also updates filebot when splicing at the end of the buffer.
#[inline]
fn splice_node(node: &LinePtr, newnode: LinePtr) {
    crate::nano::splice_node(node, newnode)
}

/// C: unlink_node(node) — remove a node from the list (nano.c).
/// Also updates filebot/edittop when the node is one of them.
#[inline]
fn unlink_node(node: &LinePtr) {
    crate::nano::unlink_node(node)
}

/// C: renumber_from(line) — reset line numbers starting from line.
#[inline]
fn renumber_from(start: &LinePtr) {
    crate::nano::renumber_from(start)
}

/// C: copy_buffer(src) — make a deep copy of the cut-buffer linked list.
fn copy_buffer(src: &LinePtr) -> LinePtr {
    crate::cut::copy_buffer(src)
}

/// C: copy_from_buffer(cutbuffer) — paste cutbuffer at cursor position.
fn copy_from_buffer(somebuffer: &LinePtr) {
    // Delegates to cut module implementation via ingraft_buffer.
    let copy = copy_buffer(somebuffer);
    ingraft_buffer(copy);
}

/// C: extract_segment — cut from (top, top_x) to (bot, bot_x) into cutbuffer.
fn extract_segment(top: LinePtr, top_x: usize, bot: LinePtr, bot_x: usize) {
    crate::cut::extract_segment(top, top_x, bot, bot_x);
}

/// C: ingraft_buffer(topline) — paste the given buffer at current position.
fn ingraft_buffer(topline: LinePtr) {
    crate::cut::ingraft_buffer(topline);
}

/// C: cut_marked_region() — cut the marked region into cutbuffer.
fn cut_marked_region() {
    // cut_marked_region is private in cut.rs; use do_snip with mark=true.
    do_snip(true, false, false);
}

/// C: free_lines(buf) — free the entire chain.
fn free_lines(buf: Option<LinePtr>) {
    // In Rust the Rc/RefCell chain is freed automatically when Rc counts drop.
    // However, we must break the linked list's internal Rc cycle by unlinking.
    if let Some(head) = buf {
        let mut cur = Some(head);
        while let Some(node) = cur {
            let next = node.borrow_mut().next.take();
            node.borrow_mut().prev = None;
            cur = next;
        }
    }
}

/// C: check_the_multis(line) — recalculate multiline color data for a line.
#[inline]
fn check_the_multis(line: &LinePtr) {
    #[cfg(feature = "color")]
    crate::color::check_the_multis(line);
    #[cfg(not(feature = "color"))]
    let _ = line;
}

/// C: do_para_begin(line) — advance line to start of its paragraph.
fn do_para_begin(line: LinePtr) -> LinePtr {
    #[cfg(feature = "justify")]
    return crate::move_::do_para_begin(line);
    #[cfg(not(feature = "justify"))]
    line
}

/// C: do_para_end(line) — advance line to end of its paragraph.
fn do_para_end(line: LinePtr) -> LinePtr {
    #[cfg(feature = "justify")]
    return crate::move_::do_para_end(line);
    #[cfg(not(feature = "justify"))]
    line
}

/// C: in_restricted_mode() — check whether nano is in restricted mode.
#[inline]
fn in_restricted_mode() -> bool {
    crate::nano::in_restricted_mode()
}

/// C: confirm_margin() — update line number margin.
#[cfg(feature = "linenumbers")]
#[inline]
fn confirm_margin() {
    crate::nano::confirm_margin()
}

/// C: block_sigwinch(block) — temporarily block/unblock SIGWINCH.
#[inline]
fn block_sigwinch(block: bool) {
    crate::nano::block_sigwinch(block)
}

/// C: terminal_init() — restore terminal settings after external program.
fn terminal_init() {
    crate::nano::terminal_init();
}

/// C: doupdate() — flush pending ncurses updates.
fn doupdate() {
    // Crossterm: flush stdout.

    crate::winio::flush_out();
}

/// C: beep() — ring the terminal bell.
fn beep() {
    crate::winio::beep();
}

/// C: napms(ms) — sleep for ms milliseconds.
#[inline]
fn napms(ms: u64) {
    crate::winio::napms(ms);
}

/// C: get_kbinput(win, visible) — read a keystroke.
fn get_kbinput(visible: bool) -> i32 {
    crate::winio::get_kbinput(visible)
}

/// C: put_cursor_at_end_of_answer() — position cursor at end of answer bar.
fn put_cursor_at_end_of_answer() {
    crate::prompt::put_cursor_at_end_of_answer();
}

/// C: regenerate_screen() — rebuild the screen after a resize.
#[cfg(not(feature = "tiny"))]
fn regenerate_screen() {
    // regenerate_screen is private in search.rs; stub with a full refresh.
    full_refresh();
}

/// C: wnoutrefresh(win) — schedule deferred refresh.
fn wnoutrefresh() {
    // No-op; use doupdate/full_refresh instead.
}

/// C: read_file(stream, fd, filename, undoable) — read file into buffer.
fn read_file<R: std::io::Read>(file: R, is_new_file: bool, filename: &str, undoable: bool) -> bool {
    crate::files::read_file_impl(file, is_new_file, filename, undoable)
}

/// C: write_file(name, stream, temporary, kind, notes) — write buffer to file.
fn write_file_to(name: &str, temporary: bool) -> bool {
    crate::files::write_file(name, None, temporary, KindOfWritingType::Overwrite, NONOTES)
}

/// C: write_region_to_file — write marked region to a temp file.
#[cfg(not(feature = "tiny"))]
fn write_region_to_file_to(name: &str, temporary: bool) -> bool {
    crate::files::write_region_to_file(name, None, temporary, KindOfWritingType::Overwrite)
}

/// C: write_it_out(exiting, withprompt) — interactively save file.
fn write_it_out(exiting: bool, withprompt: bool) -> i32 {
    crate::files::write_it_out(exiting, withprompt)
}

/// C: ask_user(withall, question) — ask YES/NO/CANCEL question on status bar.
fn ask_user(withall: bool, question: &str) -> i32 {
    crate::prompt::ask_user(withall, question)
}

/// C: findnextstr — search forward for a string.
fn findnextstr(
    needle: &str,
    whole_word_only: bool,
    modus: i32,
    _needle_len: Option<&mut usize>,
    skipone: bool,
    line_ptr: Option<&LinePtr>,
    column: usize,
) -> i32 {
    let mut dummy_len: usize = 0;
    crate::search::findnextstr(
        needle,
        whole_word_only,
        modus,
        &mut dummy_len,
        skipone,
        line_ptr,
        column,
    )
}

/// C: do_replace_loop — perform find+replace loop.
fn do_replace_loop(needle: &str, whole_word: bool, was_current: &LinePtr, was_x: &mut usize) {
    crate::search::do_replace_loop(needle, whole_word, Some(was_current), was_x);
}

#[cfg(feature = "justify")]
/// C: inpar(line) — return true if line is part of a paragraph.
fn inpar(line: &LinePtr) -> bool {
    // inpar is private in move_.rs; use our local inpar_fn.
    inpar_fn(line)
}

#[cfg(feature = "justify")]
/// C: begpar(line, depth) — return true if line is start of a paragraph.
fn begpar(line: &LinePtr, depth: i32) -> bool {
    // begpar is private in move_.rs; use our local begpar_fn.
    begpar_fn(line, depth)
}

/// Access the global cutbuffer.
fn get_cutbuffer() -> Option<LinePtr> {
    state().cutbuffer.clone()
}

fn line_chain_tail(head: &LinePtr) -> LinePtr {
    let mut tail = head.clone();
    loop {
        let next = tail.borrow().next.clone();
        match next {
            Some(node) => tail = node,
            None => return tail,
        }
    }
}

fn set_cutbuffer(buf: Option<LinePtr>) {
    let bottom = buf.as_ref().map(line_chain_tail);
    with_state_mut(|s| {
        s.cutbuffer = buf;
        s.cutbottom = bottom;
    });
}

// ---------------------------------------------------------------------------
// Section 1: Mark (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void do_mark(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_mark() {
    let has_mark = with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some());
    if !has_mark {
        // Set the mark at the current cursor position.
        let (cur, cur_x) = with_state(|s| {
            let f = s.openfile.as_ref().expect("an open buffer");
            (f.current.clone(), f.current_x)
        });
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.mark = cur;
                f.mark_x = cur_x;
                f.softmark = false;
            }
        });
        statusbar(tr!("Mark Set"));
    } else {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.mark = None;
            }
        });
        statusbar(tr!("Mark Unset"));
        state_mut().refresh_needed = true;
    }
}

// ---------------------------------------------------------------------------
// Section 2: Tab insertion
// ---------------------------------------------------------------------------

/* C: void do_tab(void) */
pub fn do_tab() {
    #[cfg(not(feature = "tiny"))]
    {
        // When a region is marked, indent it instead.
        let has_mark_other = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| {
                    f.mark.is_some()
                        && f.mark.as_ref().map(|m| m.as_ptr())
                            != f.current.as_ref().map(|c| c.as_ptr())
                })
                .unwrap_or(false)
        });
        if has_mark_other {
            do_indent();
            return;
        }
    }

    #[cfg(feature = "color")]
    {
        let tabstring = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.syntax.as_ref())
                .and_then(|syn_ptr| {
                    let syn = unsafe { &**syn_ptr };
                    syn.tabstring.clone()
                })
        });
        if let Some(ref ts) = tabstring {
            let ts_clone = ts.clone();
            let ts_len = ts_clone.len();
            inject(&ts_clone, ts_len);
            return;
        }
    }

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(TABS_TO_SPACES) {
            let tabsize = state().tabsize as usize;
            let col = xplustabs();
            let length = tabsize - (col % tabsize);
            let spaces: String = " ".repeat(length);
            inject(&spaces, length);
            return;
        }
    }

    inject("\t", 1);
}

// ---------------------------------------------------------------------------
// Section 3: Indentation (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void indent_a_line(linestruct *line, char *indentation) */
#[cfg(not(feature = "tiny"))]
pub fn indent_a_line<T: AsRef<[u8]> + ?Sized>(line: &LinePtr, indentation: &T) {
    let indentation = indentation.as_ref();
    let indent_len = indentation.len();
    if indent_len == 0 {
        return;
    }

    {
        let mut node = line.borrow_mut();
        let mut new_data = LineData::from_internal(indentation.to_vec());
        new_data.extend_bytes(node.data.as_bytes());
        node.data = new_data;
    }

    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.totsize += indent_len;
            // Compensate mark position if on this line.
            let mark_is_this = f.mark.as_ref().map(|m| m.as_ptr()) == Some(line.as_ptr());
            if mark_is_this && f.mark_x > 0 {
                f.mark_x += indent_len;
            }
            let cur_is_this = f.current.as_ref().map(|c| c.as_ptr()) == Some(line.as_ptr());
            if cur_is_this && f.current_x > 0 {
                f.current_x += indent_len;
                f.placewewant = xplustabs();
            }
        }
    });
}

/* C: void do_indent(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_indent() {
    let (top_lineno, bot_lineno) = get_range();

    // Collect the lines that are in range.
    let lines = collect_lines_in_range(top_lineno, bot_lineno);

    // Skip leading empty lines.
    let first_nonempty = lines.iter().position(|ln| !ln.borrow().data.is_empty());
    let lines = match first_nonempty {
        None => return, // all empty
        Some(idx) => &lines[idx..],
    };

    if lines.is_empty() {
        return;
    }

    // Build indentation string.
    let indentation = build_indentation();

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::Indent, None);

    for line in lines.iter() {
        let real_indent = if line.borrow().data.is_empty() {
            ""
        } else {
            &indentation
        };
        indent_a_line(line, real_indent);
        #[cfg(not(feature = "tiny"))]
        update_multiline_undo(line.borrow().lineno, real_indent);
    }

    set_modified();
    ensure_firstcolumn_is_aligned();
    with_state_mut(|s| {
        s.refresh_needed = true;
        s.shift_held = true;
    });
}

/// Build the indentation string (tab or spaces) depending on flags.
#[cfg(not(feature = "tiny"))]
fn build_indentation() -> String {
    #[cfg(feature = "color")]
    {
        let tabstring = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.syntax.as_ref())
                .and_then(|syn_ptr| {
                    let syn = unsafe { &**syn_ptr };
                    syn.tabstring.clone()
                })
        });
        if let Some(ts) = tabstring {
            return ts;
        }
    }

    let tabsize = state().tabsize as usize;
    if ISSET!(TABS_TO_SPACES) {
        " ".repeat(tabsize)
    } else {
        "\t".to_string()
    }
}

/// Collect all LinePtr in range [top_lineno, bot_lineno] inclusive.
fn collect_lines_in_range(top_lineno: usize, bot_lineno: usize) -> Vec<LinePtr> {
    let mut result = Vec::new();
    let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
    let mut cur = filetop;
    while let Some(line) = cur {
        let lineno = line.borrow().lineno as usize;
        if lineno > bot_lineno {
            break;
        }
        if lineno >= top_lineno {
            result.push(line.clone());
        }
        cur = line.borrow().next.clone();
    }
    result
}

/* C: size_t length_of_white(const char *text) */
#[cfg(not(feature = "tiny"))]
pub fn length_of_white<T: AsRef<[u8]> + ?Sized>(text: &T) -> usize {
    let bytes = text.as_ref();
    #[cfg(feature = "color")]
    {
        let tabstring = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.syntax.as_ref())
                .and_then(|syn_ptr| {
                    let syn = unsafe { &**syn_ptr };
                    syn.tabstring.clone()
                })
        });
        if let Some(ref ts) = tabstring {
            let ts_len = ts.len();
            if bytes.starts_with(ts.as_bytes()) {
                return ts_len;
            }
        }
    }

    let tabsize = state().tabsize as usize;
    let mut white_count = 0usize;
    loop {
        if white_count >= bytes.len() {
            return white_count;
        }
        let b = bytes[white_count];
        if b == b'\t' {
            return white_count + 1;
        }
        if b != b' ' {
            return white_count;
        }
        white_count += 1;
        if white_count == tabsize {
            return tabsize;
        }
    }
}

/* C: void compensate_leftward(linestruct *line, size_t leftshift) */
pub fn compensate_leftward(line: &LinePtr, leftshift: usize) {
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            let mark_is_this = f.mark.as_ref().map(|m| m.as_ptr()) == Some(line.as_ptr());
            if mark_is_this {
                if f.mark_x < leftshift {
                    f.mark_x = 0;
                } else {
                    f.mark_x -= leftshift;
                }
            }
            let cur_is_this = f.current.as_ref().map(|c| c.as_ptr()) == Some(line.as_ptr());
            if cur_is_this {
                if f.current_x < leftshift {
                    f.current_x = 0;
                } else {
                    f.current_x -= leftshift;
                }
                f.placewewant = xplustabs();
            }
        }
    });
}

/* C: void unindent_a_line(linestruct *line, size_t indent_len) */
#[cfg(not(feature = "tiny"))]
pub fn unindent_a_line(line: &LinePtr, indent_len: usize) {
    if indent_len == 0 {
        return;
    }
    {
        let mut node = line.borrow_mut();
        if node.data.len() >= indent_len {
            node.data = LineData::from_internal(node.data[indent_len..].to_vec());
        }
    }
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            if f.totsize >= indent_len {
                f.totsize -= indent_len;
            }
        }
    });
    compensate_leftward(line, indent_len);
}

/* C: void do_unindent(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_unindent() {
    let (top_lineno, bot_lineno) = get_range();
    let lines = collect_lines_in_range(top_lineno, bot_lineno);

    // Skip leading lines that cannot be unindented.
    let first_indented = lines
        .iter()
        .position(|ln| length_of_white(&ln.borrow().data) > 0);
    let lines = match first_indented {
        None => return,
        Some(idx) => &lines[idx..],
    };

    if lines.is_empty() {
        return;
    }

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::Unindent, None);

    for line in lines.iter() {
        let indent_len = length_of_white(&line.borrow().data);
        let indentation = {
            let data = line.borrow().data.clone();
            measured_copy(&data, indent_len)
        };
        unindent_a_line(line, indent_len);
        #[cfg(not(feature = "tiny"))]
        update_multiline_undo(line.borrow().lineno, &indentation);
    }

    set_modified();
    ensure_firstcolumn_is_aligned();
    with_state_mut(|s| {
        s.refresh_needed = true;
        s.shift_held = true;
    });
}

/* C: void handle_indent_action(undostruct *u, bool undoing, bool add_indent) */
#[cfg(not(feature = "tiny"))]
pub fn handle_indent_action(u: &UndoStruct, undoing: bool, add_indent: bool) {
    if let Some(ref group) = u.grouping {
        // When redoing, reposition cursor.
        if !undoing {
            goto_line_posx(u.head_lineno, u.head_x);
        }

        let top = group.top_line;
        let bot = group.bottom_line;
        let lines = collect_lines_in_range(top as usize, bot as usize);

        for line in &lines {
            let lineno = line.borrow().lineno;
            let idx = (lineno - top) as usize;
            if idx < group.indentations.len() {
                let blanks = group.indentations[idx].clone();
                if undoing ^ add_indent {
                    indent_a_line(line, &blanks);
                } else {
                    unindent_a_line(line, blanks.len());
                }
            }
        }

        if undoing {
            goto_line_posx(u.head_lineno, u.head_x);
        }
    }
    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// Section 4: Comment toggling (ENABLE_COMMENT)
// ---------------------------------------------------------------------------

/* C: bool comment_line(undo_type action, linestruct *line, const char *comment_seq) */
#[cfg(feature = "comment")]
pub fn comment_line<T: AsRef<[u8]> + ?Sized>(
    action: UndoType,
    line: &LinePtr,
    comment_seq: &T,
) -> bool {
    let comment_seq = comment_seq.as_ref();
    let (pre_seq, post_seq) =
        if let Some(pipe_pos) = comment_seq.iter().position(|&byte| byte == b'|') {
            (&comment_seq[..pipe_pos], Some(&comment_seq[pipe_pos + 1..]))
        } else {
            (comment_seq, None)
        };

    let pre_len = pre_seq.len();
    let post_len = post_seq.map_or(0, |p| p.len());

    // Don't comment the magic last line unless NO_NEWLINES is set.
    {
        let is_filebot = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filebot.as_ref())
                .map(|bot| bot.as_ptr())
                == Some(line.as_ptr())
        });
        if !ISSET!(NO_NEWLINES) && is_filebot {
            return false;
        }
    }

    let line_data = line.borrow().data.clone();
    let line_len = line_data.len();

    match action {
        UndoType::Comment => {
            // Add comment markers.
            let mut new_data = LineData::from_internal(pre_seq.to_vec());
            new_data.extend_bytes(line_data.as_bytes());
            if let Some(post) = post_seq {
                new_data.extend_bytes(post);
            }
            line.borrow_mut().data = new_data;

            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.totsize += pre_len + post_len;
                    let mark_is_this = f.mark.as_ref().map(|m| m.as_ptr()) == Some(line.as_ptr());
                    if mark_is_this && f.mark_x > 0 {
                        f.mark_x += pre_len;
                    }
                    let cur_is_this = f.current.as_ref().map(|c| c.as_ptr()) == Some(line.as_ptr());
                    if cur_is_this && f.current_x > 0 {
                        f.current_x += pre_len;
                        f.placewewant = xplustabs();
                    }
                }
            });
            true
        }
        UndoType::Preflight => {
            // Check if the line is commented.
            if line_data.starts_with(pre_seq) {
                if let Some(post) = post_seq {
                    if line_len >= post_len && &line_data[line_len - post_len..] == post {
                        return true;
                    }
                    return false;
                }
                return true;
            }
            false
        }
        UndoType::Uncomment => {
            // Verify and remove comment markers.
            let is_commented = if line_data.starts_with(pre_seq) {
                if let Some(post) = post_seq {
                    // Both markers must fit WITHOUT overlapping; otherwise the line
                    // is not properly commented and the slice below would panic
                    // (e.g. "/*/" with pre="/*", post="*/": end < pre_len).
                    pre_len + post_len <= line_len && &line_data[line_len - post_len..] == post
                } else {
                    true
                }
            } else {
                false
            };

            if !is_commented {
                return false;
            }

            let end = if post_len > 0 {
                line_len - post_len
            } else {
                line_len
            };
            let new_data = LineData::from_internal(line_data[pre_len..end].to_vec());
            line.borrow_mut().data = new_data;

            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    if f.totsize >= pre_len + post_len {
                        f.totsize -= pre_len + post_len;
                    }
                }
            });
            compensate_leftward(line, pre_len);
            true
        }
        _ => false,
    }
}

/* C: void do_comment(void) */
#[cfg(feature = "comment")]
pub fn do_comment() {
    let comment_seq = {
        #[cfg(feature = "color")]
        {
            let seq = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.syntax.as_ref())
                    .and_then(|syn_ptr| {
                        let syn = unsafe { &**syn_ptr };
                        #[cfg(feature = "comment")]
                        return syn.comment.clone();
                        #[cfg(not(feature = "comment"))]
                        return None;
                    })
            });
            if let Some(s) = seq {
                if s.is_empty() {
                    statusline(
                        MessageType::Ahem,
                        tr!("Commenting is not supported for this file type"),
                    );
                    return;
                }
                s
            } else {
                GENERAL_COMMENT_CHARACTER.to_string()
            }
        }
        #[cfg(not(feature = "color"))]
        GENERAL_COMMENT_CHARACTER.to_string()
    };

    let (top_lineno, bot_lineno) = get_range();
    let lines = collect_lines_in_range(top_lineno, bot_lineno);

    // If only the magic line is selected, do nothing.
    {
        let filebot_ptr = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filebot.as_ref())
                .map(|b| b.as_ptr())
        });
        if lines.len() == 1 {
            let single_ptr = lines[0].as_ptr();
            if Some(single_ptr) == filebot_ptr && !ISSET!(NO_NEWLINES) {
                statusline(MessageType::Ahem, tr!("Cannot comment past end of file"));
                return;
            }
        }
    }

    // Determine whether to comment or uncomment.
    let mut action = UndoType::Uncomment;
    let mut all_empty = true;
    for line in &lines {
        let data = line.borrow().data.clone();
        let empty = white_string(&data);
        if !empty && !comment_line(UndoType::Preflight, line, &comment_seq) {
            action = UndoType::Comment;
            break;
        }
        all_empty = all_empty && empty;
    }

    if all_empty {
        action = UndoType::Comment;
    }

    add_undo(action, None);

    // Store the inserted comment bytes in the undo record's payload.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            if !f.current_undo.is_null() {
                // The active cursor was created through a unique Box path in
                // add_undo; keep using that provenance for record mutation.
                unsafe {
                    (*f.current_undo).payload = Some(LineData::from_utf8(&comment_seq));
                }
            }
        }
    });

    for line in &lines {
        if comment_line(action, line, &comment_seq) {
            update_multiline_undo(line.borrow().lineno, "");
        }
    }

    set_modified();
    ensure_firstcolumn_is_aligned();
    with_state_mut(|s| {
        s.refresh_needed = true;
        s.shift_held = true;
    });
}

/* C: void handle_comment_action(undostruct *u, bool undoing, bool add_comment) */
#[cfg(feature = "comment")]
pub fn handle_comment_action(u: &UndoStruct, undoing: bool, add_comment: bool) {
    let comment_seq = u.payload.clone().unwrap_or_default();

    if !undoing {
        goto_line_posx(u.head_lineno, u.head_x);
    }

    // Walk the grouping chain.
    let mut group_opt = u.grouping.as_deref();
    while let Some(group) = group_opt {
        let lines = collect_lines_in_range(group.top_line as usize, group.bottom_line as usize);
        let sub_action = if undoing ^ add_comment {
            UndoType::Comment
        } else {
            UndoType::Uncomment
        };
        for line in &lines {
            comment_line(sub_action, line, &comment_seq);
        }
        group_opt = group.next.as_deref();
    }

    if undoing {
        goto_line_posx(u.head_lineno, u.head_x);
    }

    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// Section 4b: UndoType helpers
// ---------------------------------------------------------------------------

/// Return true if the undo type is a "simple text operation" (ADD..REPLACE),
/// i.e. the first 6 variants that operate on the current single line.
/// C: u->type <= REPLACE
fn undo_type_is_simple(t: UndoType) -> bool {
    matches!(
        t,
        UndoType::Add
            | UndoType::Enter
            | UndoType::Back
            | UndoType::Del
            | UndoType::Join
            | UndoType::Replace
    )
}

// ---------------------------------------------------------------------------
// Section 5: Undo/Redo (not NANO_TINY)
// ---------------------------------------------------------------------------

// Aliases mirroring C #define redo_paste undo_cut / undo_paste redo_cut
#[cfg(not(feature = "tiny"))]
fn redo_paste(u: &UndoStruct) {
    undo_cut(u);
}

#[cfg(not(feature = "tiny"))]
fn undo_paste(u: &UndoStruct) {
    redo_cut(u);
}

/* C: void undo_cut(undostruct *u) */
#[cfg(not(feature = "tiny"))]
pub fn undo_cut(u: &UndoStruct) {
    let pos_x = if (u.xflags & WAS_WHOLE_LINE) != 0 {
        0
    } else {
        u.head_x
    };
    goto_line_posx(u.head_lineno, pos_x);

    // Clear an inherited anchor but not a user-placed one.
    if (u.xflags & HAD_ANCHOR_AT_START) == 0 {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                if let Some(ref cur) = f.current {
                    #[cfg(not(feature = "tiny"))]
                    {
                        cur.borrow_mut().has_anchor = false;
                    }
                }
            }
        });
    }

    if let Some(ref cb) = u.cutbuffer {
        copy_from_buffer(cb);
    }

    // If originally the last line was cut too, remove an extra magic line.
    if (u.xflags & INCLUDED_LAST_LINE) != 0 && !ISSET!(NO_NEWLINES) {
        let should_remove = with_state(|s| {
            let f = s.openfile.as_ref()?;
            let filebot = f.filebot.as_ref()?;
            let current = f.current.as_ref()?;
            let filebot_ne_current = filebot.as_ptr() != current.as_ptr();
            let prev_of_bot_empty = filebot
                .borrow()
                .prev
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|p| p.borrow().data.is_empty())
                .unwrap_or(false);
            Some(filebot_ne_current && prev_of_bot_empty)
        })
        .unwrap_or(false);
        if should_remove {
            remove_magicline();
        }
    }

    if (u.xflags & CURSOR_WAS_AT_HEAD) != 0 {
        goto_line_posx(u.head_lineno, u.head_x);
    }
}

/* C: void redo_cut(undostruct *u) */
#[cfg(not(feature = "tiny"))]
pub fn redo_cut(u: &UndoStruct) {
    let old_cutbuffer = get_cutbuffer();
    set_cutbuffer(None);

    let mark_line = get_line_from_number(u.head_lineno);
    let mark_x = if (u.xflags & WAS_WHOLE_LINE) != 0 {
        0
    } else {
        u.head_x
    };

    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.mark = mark_line;
            f.mark_x = mark_x;
        }
    });

    goto_line_posx(u.tail_lineno, u.tail_x);

    do_snip(true, false, u.r#type == UndoType::Zap);

    let new_cut = get_cutbuffer();
    free_lines(new_cut);
    set_cutbuffer(old_cutbuffer);
}

/// C: line_from_number() — utils.c — find a LinePtr by its 1-based line number.
#[inline]
fn get_line_from_number(lineno: isize) -> Option<LinePtr> {
    crate::utils::line_from_number(lineno)
}

/* C: void do_undo(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_undo() {
    let u_ptr =
        with_state(|s| s.openfile.as_ref().map(|f| f.current_undo)).unwrap_or(std::ptr::null_mut());

    if u_ptr.is_null() {
        statusline(MessageType::Ahem, tr!("Nothing to undo"));
        return;
    }

    // Extract only the Copy scalars from the undo record — do NOT ptr::read the full struct
    // (it owns Box/String/Rc fields; a bitwise copy + Drop = double-free).
    let (u_type, u_xflags, u_head_lineno, u_head_x, u_tail_lineno, u_tail_x, u_wassize) = unsafe {
        let u = &*u_ptr;
        (
            u.r#type,
            u.xflags,
            u.head_lineno,
            u.head_x,
            u.tail_lineno,
            u.tail_x,
            u.wassize,
        )
    };

    let mut undidmsg: Option<&str> = None;

    let line = if undo_type_is_simple(u_type) {
        get_line_from_number(u_tail_lineno)
    } else {
        None
    };

    match u_type {
        UndoType::Add => {
            undidmsg = Some(tr!("addition"));
            if (u_xflags & INCLUDED_LAST_LINE) != 0 && !ISSET!(NO_NEWLINES) {
                remove_magicline();
            }
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let strdata_len = strdata.len();
                let mut data = ln.borrow().data.clone();
                if let Some(range) = checked_edit_range(&data, u_head_x, strdata_len) {
                    data.replace_range_bytes(range, &[]);
                    ln.borrow_mut().data = data;
                }
            }
            goto_line_posx(u_head_lineno, u_head_x);
        }
        UndoType::Enter => {
            undidmsg = Some(tr!("line break"));
            let original_x = if u_head_x == 0 { u_tail_x } else { u_head_x };
            let regain_from_x = if u_head_x == 0 { 0 } else { u_tail_x };
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let suffix = if regain_from_x <= strdata.len() {
                    LineData::from_internal(strdata[regain_from_x..].to_vec())
                } else {
                    LineData::empty()
                };
                {
                    let mut node = ln.borrow_mut();
                    node.data.push_str(&suffix);
                }
                let next_anchor = {
                    ln.borrow()
                        .next
                        .as_ref()
                        .map(|nx| {
                            #[cfg(not(feature = "tiny"))]
                            {
                                nx.borrow().has_anchor
                            }
                            #[cfg(feature = "tiny")]
                            false
                        })
                        .unwrap_or(false)
                };
                #[cfg(not(feature = "tiny"))]
                {
                    ln.borrow_mut().has_anchor |= next_anchor;
                }
                let next = ln.borrow().next.clone();
                if let Some(nx) = next {
                    unlink_node(&nx);
                    renumber_from(ln);
                }
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(ln.clone());
                    }
                });
            }
            goto_line_posx(u_head_lineno, original_x);
        }
        UndoType::Back | UndoType::Del => {
            undidmsg = Some(tr!("deletion"));
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let mut data = ln.borrow().data.clone();
                if safe_edit_boundary(&data, u_head_x) == u_head_x {
                    data.insert_str(u_head_x, &strdata);
                    ln.borrow_mut().data = data;
                }
            }
            goto_line_posx(u_tail_lineno, u_tail_x);
        }
        UndoType::Join => {
            undidmsg = Some(tr!("line join"));
            if (u_xflags & WAS_BACKSPACE_AT_EOF) != 0 && !ISSET!(NO_NEWLINES) {
                let filebot_lineno = with_state(|s| {
                    s.openfile
                        .as_ref()
                        .and_then(|f| f.filebot.as_ref())
                        .map(|b| b.borrow().lineno)
                        .unwrap_or(0)
                });
                goto_line_posx(filebot_lineno, 0);
                state_mut().focusing = false;
            } else if let Some(ref ln) = line {
                {
                    let mut node = ln.borrow_mut();
                    if safe_edit_boundary(&node.data, u_tail_x) == u_tail_x {
                        node.data.truncate(u_tail_x);
                    }
                }
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let intruder = make_new_node(Some(ln.clone()));
                intruder.borrow_mut().data = strdata;
                splice_node(ln, intruder.clone());
                renumber_from(&intruder);
                goto_line_posx(u_head_lineno, u_head_x);
            }
        }
        UndoType::Replace => {
            undidmsg = Some(tr!("replacement"));
            if let Some(ref ln) = line {
                // Swap the saved payload with line data through the raw pointer.
                let strdata = unsafe { (*u_ptr).payload.take().unwrap_or_default() };
                let old_data = ln.borrow().data.clone();
                ln.borrow_mut().data = strdata;
                unsafe {
                    (*u_ptr).payload = Some(old_data);
                }
            }
            goto_line_posx(u_head_lineno, u_head_x);
        }
        #[cfg(feature = "wrapping")]
        UndoType::SplitBegin => {
            undidmsg = Some(tr!("addition"));
        }
        #[cfg(feature = "wrapping")]
        UndoType::SplitEnd => {
            advance_current_undo();
            loop {
                let t = with_state(|s| {
                    s.openfile.as_ref().and_then(|f| {
                        let ptr = f.current_undo;
                        if ptr.is_null() {
                            None
                        } else {
                            Some(unsafe { (*ptr).r#type })
                        }
                    })
                });
                if t == Some(UndoType::SplitBegin) || t.is_none() {
                    break;
                }
                do_undo();
            }
            return;
        }
        UndoType::Zap => {
            undidmsg = Some(tr!("erasure"));
            // Pass a reference through the raw pointer — valid as long as we don't drop the Box.
            let u_ref = unsafe { &*u_ptr };
            undo_cut(u_ref);
        }
        UndoType::CutToEof | UndoType::Cut => {
            undidmsg = Some(tr!("cut"));
            let u_ref = unsafe { &*u_ptr };
            undo_cut(u_ref);
        }
        UndoType::Paste => {
            undidmsg = Some(tr!("paste"));
            let u_ref = unsafe { &*u_ptr };
            undo_paste(u_ref);
            if (u_xflags & INCLUDED_LAST_LINE) != 0 && !ISSET!(NO_NEWLINES) {
                let should_remove = with_state(|s| {
                    let f = s.openfile.as_ref()?;
                    let filebot = f.filebot.as_ref()?;
                    let current = f.current.as_ref()?;
                    Some(filebot.as_ptr() != current.as_ptr())
                })
                .unwrap_or(false);
                if should_remove {
                    remove_magicline();
                }
            }
        }
        UndoType::Insert => {
            undidmsg = Some(tr!("insertion"));
            let old_cutbuffer = get_cutbuffer();
            set_cutbuffer(None);
            goto_line_posx(u_head_lineno, u_head_x);
            let tail_line = get_line_from_number(u_tail_lineno);
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.mark = tail_line;
                    f.mark_x = u_tail_x;
                }
            });
            cut_marked_region();
            let new_cut = get_cutbuffer();
            unsafe {
                (*u_ptr).cutbuffer = new_cut;
            }
            set_cutbuffer(old_cutbuffer);
            if (u_xflags & INCLUDED_LAST_LINE) != 0 && !ISSET!(NO_NEWLINES) {
                let should_remove = with_state(|s| {
                    let f = s.openfile.as_ref()?;
                    let filebot = f.filebot.as_ref()?;
                    let current = f.current.as_ref()?;
                    Some(filebot.as_ptr() != current.as_ptr())
                })
                .unwrap_or(false);
                if should_remove {
                    remove_magicline();
                }
            }
        }
        UndoType::CoupleBegin => {
            undidmsg = unsafe { (*u_ptr).description.as_deref() };
            goto_line_posx(u_head_lineno, u_head_x);
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.cursor_row = u_tail_lineno;
                }
            });
            adjust_viewport(UpdateType::Stationary);
        }
        UndoType::CoupleEnd => {
            let cursor_row = with_state(|s| s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0));
            unsafe {
                (*u_ptr).head_lineno = cursor_row;
            }
            advance_current_undo();
            do_undo();
            do_undo();
            do_undo();
            return;
        }
        UndoType::Indent => {
            let u_ref = unsafe { &*u_ptr };
            handle_indent_action(u_ref, true, true);
            undidmsg = Some(tr!("indent"));
        }
        UndoType::Unindent => {
            let u_ref = unsafe { &*u_ptr };
            handle_indent_action(u_ref, true, false);
            undidmsg = Some(tr!("unindent"));
        }
        #[cfg(feature = "comment")]
        UndoType::Comment => {
            let u_ref = unsafe { &*u_ptr };
            handle_comment_action(u_ref, true, true);
            undidmsg = Some(tr!("comment"));
        }
        #[cfg(feature = "comment")]
        UndoType::Uncomment => {
            let u_ref = unsafe { &*u_ptr };
            handle_comment_action(u_ref, true, false);
            undidmsg = Some(tr!("uncomment"));
        }
        _ => {}
    }

    // The CoupleBegin case borrows u_ptr.description as undidmsg; after this
    // point we must not mutate that description for CoupleBegin.
    let pletion_line_is_none = state().pletion_line.is_none();
    if let Some(msg) = undidmsg {
        if !ISSET!(ZERO) && pletion_line_is_none {
            statusline(MessageType::Hush, &format!("{} {}", tr!("Undid"), msg));
        }
    }

    advance_current_undo();

    let placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.last_action = UndoType::Other;
            f.mark = None;
            f.placewewant = placewewant;
            f.totsize = u_wassize;
        }
    });

    #[cfg(feature = "color")]
    {
        if undo_type_is_simple(u_type) {
            let cur_line = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
            if let Some(ref ln) = cur_line {
                check_the_multis(ln);
            }
        } else if u_type == UndoType::Insert || u_type == UndoType::CoupleBegin {
            state_mut().recook = true;
        }
    }

    let (current_undo_ptr, last_saved_ptr) = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| (f.current_undo, f.last_saved))
            .unwrap_or((std::ptr::null_mut(), std::ptr::null_mut()))
    });
    if current_undo_ptr == last_saved_ptr {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.modified = false;
            }
        });
        titlebar(None);
    } else {
        set_modified();
    }
}

#[cfg(feature = "tiny")]
pub fn do_undo() {}

/// Advance openfile.current_undo to the next (older) item in the stack.
#[cfg(not(feature = "tiny"))]
fn advance_current_undo() {
    unsafe {
        let ptr = with_state(|s| s.openfile.as_ref().map(|f| f.current_undo))
            .unwrap_or(std::ptr::null_mut());
        if !ptr.is_null() {
            // Preserve mutable provenance for a cursor that will later be used
            // to update the record.  Casting a shared `&UndoStruct` to `*mut`
            // makes the subsequent unique dereference undefined behaviour.
            let next_ptr = (*ptr)
                .next
                .as_deref_mut()
                .map(|record| record as *mut UndoStruct)
                .unwrap_or(std::ptr::null_mut());
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current_undo = next_ptr;
                }
            });
        }
    }
}

/* C: void do_redo(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_redo() {
    let current_undo_ptr = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.current_undo)
            .unwrap_or(std::ptr::null_mut())
    });

    // Find the item before current_undo in the chain (the item to redo).  Each
    // candidate pointer is obtained through `as_deref_mut`, because redo may
    // swap owned fields in the selected record.
    let (has_undo, u_ptr): (bool, *mut UndoStruct) = with_state_mut(|s| {
        let Some(f) = s.openfile.as_mut() else {
            return (false, std::ptr::null_mut());
        };
        let has_undo = f.undotop.is_some();
        let mut candidate = f
            .undotop
            .as_deref_mut()
            .map(|record| record as *mut UndoStruct)
            .unwrap_or(std::ptr::null_mut());

        if candidate == current_undo_ptr {
            // Refresh the stored cursor with the provenance of this unique
            // traversal before returning: obtaining a new mutable path through
            // the Box supersedes an older raw reborrow of the same record.
            f.current_undo = candidate;
            return (has_undo, std::ptr::null_mut());
        }

        unsafe {
            while !candidate.is_null() {
                let next = (*candidate)
                    .next
                    .as_deref_mut()
                    .map(|record| record as *mut UndoStruct)
                    .unwrap_or(std::ptr::null_mut());
                if next == current_undo_ptr {
                    f.current_undo = next;
                    break;
                }
                candidate = next;
            }
        }
        (has_undo, candidate)
    });

    if !has_undo {
        statusline(MessageType::Ahem, tr!("Nothing to redo"));
        return;
    }

    if u_ptr.is_null() {
        statusline(MessageType::Ahem, tr!("Nothing to redo"));
        return;
    }

    // Extract only Copy scalars; never ptr::read the full struct.
    let (u_type, u_xflags, u_head_lineno, u_head_x, u_tail_lineno, u_tail_x, u_newsize) = unsafe {
        let u = &*u_ptr;
        (
            u.r#type,
            u.xflags,
            u.head_lineno,
            u.head_x,
            u.tail_lineno,
            u.tail_x,
            u.newsize,
        )
    };

    let mut redidmsg: Option<&str> = None;
    let mut suppress_modification = false;

    let line = if undo_type_is_simple(u_type) {
        get_line_from_number(u_tail_lineno)
    } else {
        None
    };

    match u_type {
        UndoType::Add => {
            redidmsg = Some(tr!("addition"));
            if (u_xflags & INCLUDED_LAST_LINE) != 0 && !ISSET!(NO_NEWLINES) {
                new_magicline();
            }
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let mut data = ln.borrow().data.clone();
                if safe_edit_boundary(&data, u_head_x) == u_head_x {
                    data.insert_str(u_head_x, &strdata);
                    ln.borrow_mut().data = data;
                }
            }
            goto_line_posx(u_tail_lineno, u_tail_x);
        }
        UndoType::Enter => {
            redidmsg = Some(tr!("line break"));
            if let Some(ref ln) = line {
                {
                    let mut node = ln.borrow_mut();
                    if safe_edit_boundary(&node.data, u_head_x) == u_head_x {
                        node.data.truncate(u_head_x);
                    }
                }
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let intruder = make_new_node(Some(ln.clone()));
                intruder.borrow_mut().data = strdata;
                splice_node(ln, intruder.clone());
                renumber_from(&intruder);
            }
            goto_line_posx(u_head_lineno + 1, u_tail_x);
        }
        UndoType::Back | UndoType::Del => {
            redidmsg = Some(tr!("deletion"));
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                let mut data = ln.borrow().data.clone();
                if let Some(range) = checked_edit_range(&data, u_head_x, strdata.len()) {
                    data.replace_range_bytes(range, &[]);
                    ln.borrow_mut().data = data;
                }
            }
            goto_line_posx(u_head_lineno, u_head_x);
        }
        UndoType::Join => {
            redidmsg = Some(tr!("line join"));
            if (u_xflags & WAS_BACKSPACE_AT_EOF) != 0 && !ISSET!(NO_NEWLINES) {
                goto_line_posx(u_tail_lineno, u_tail_x);
            } else if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.clone().unwrap_or_default() };
                {
                    let mut node = ln.borrow_mut();
                    node.data.push_str(&strdata);
                }
                let next = ln.borrow().next.clone();
                if let Some(nx) = next {
                    unlink_node(&nx);
                    renumber_from(ln);
                }
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(ln.clone());
                    }
                });
                goto_line_posx(u_tail_lineno, u_tail_x);
            }
        }
        UndoType::Replace => {
            redidmsg = Some(tr!("replacement"));
            if let Some(ref ln) = line {
                let strdata = unsafe { (*u_ptr).payload.take().unwrap_or_default() };
                let old_data = ln.borrow().data.clone();
                ln.borrow_mut().data = strdata;
                unsafe {
                    (*u_ptr).payload = Some(old_data);
                }
            }
            goto_line_posx(u_head_lineno, u_head_x);
        }
        #[cfg(feature = "wrapping")]
        UndoType::SplitBegin => {
            set_current_undo_to(u_ptr);
            loop {
                let t = with_state(|s| {
                    s.openfile.as_ref().and_then(|f| {
                        let ptr = f.current_undo;
                        if ptr.is_null() {
                            None
                        } else {
                            Some(unsafe { (*ptr).r#type })
                        }
                    })
                });
                if t == Some(UndoType::SplitEnd) || t.is_none() {
                    break;
                }
                do_redo();
            }
            // Recursive redos can reborrow the undo chain, so use the scalars
            // copied before recursion instead of dereferencing the old cursor.
            goto_line_posx(u_head_lineno, u_head_x);
            ensure_firstcolumn_is_aligned();
            return;
        }
        #[cfg(feature = "wrapping")]
        UndoType::SplitEnd => {
            redidmsg = Some(tr!("addition"));
        }
        UndoType::Zap => {
            redidmsg = Some(tr!("erasure"));
            let u_ref = unsafe { &*u_ptr };
            redo_cut(u_ref);
        }
        UndoType::CutToEof | UndoType::Cut => {
            redidmsg = Some(tr!("cut"));
            let u_ref = unsafe { &*u_ptr };
            redo_cut(u_ref);
        }
        UndoType::Paste => {
            redidmsg = Some(tr!("paste"));
            let u_ref = unsafe { &*u_ptr };
            redo_paste(u_ref);
        }
        UndoType::Insert => {
            redidmsg = Some(tr!("insertion"));
            goto_line_posx(u_head_lineno, u_head_x);
            let has_cutbuffer = unsafe { (*u_ptr).cutbuffer.is_some() };
            if has_cutbuffer {
                let cb = unsafe { (*u_ptr).cutbuffer.clone().unwrap() };
                copy_from_buffer(&cb);
            } else {
                suppress_modification = true;
            }
            unsafe {
                (*u_ptr).cutbuffer = None;
            }
        }
        UndoType::CoupleBegin => {
            set_current_undo_to(u_ptr);
            do_redo();
            do_redo();
            do_redo();
            return;
        }
        UndoType::CoupleEnd => {
            redidmsg = unsafe { (*u_ptr).description.as_deref() };
            goto_line_posx(u_tail_lineno, u_tail_x);
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.cursor_row = u_head_lineno;
                }
            });
            adjust_viewport(UpdateType::Stationary);
        }
        UndoType::Indent => {
            let u_ref = unsafe { &*u_ptr };
            handle_indent_action(u_ref, false, true);
            redidmsg = Some(tr!("indent"));
        }
        UndoType::Unindent => {
            let u_ref = unsafe { &*u_ptr };
            handle_indent_action(u_ref, false, false);
            redidmsg = Some(tr!("unindent"));
        }
        #[cfg(feature = "comment")]
        UndoType::Comment => {
            let u_ref = unsafe { &*u_ptr };
            handle_comment_action(u_ref, false, true);
            redidmsg = Some(tr!("comment"));
        }
        #[cfg(feature = "comment")]
        UndoType::Uncomment => {
            let u_ref = unsafe { &*u_ptr };
            handle_comment_action(u_ref, false, false);
            redidmsg = Some(tr!("uncomment"));
        }
        _ => {}
    }

    if let Some(msg) = redidmsg {
        if !ISSET!(ZERO) {
            statusline(MessageType::Hush, &format!("{} {}", tr!("Redid"), msg));
        }
    }

    set_current_undo_to(u_ptr);

    let placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.last_action = UndoType::Other;
            f.mark = None;
            f.placewewant = placewewant;
            f.totsize = u_newsize;
        }
    });

    #[cfg(feature = "color")]
    {
        if undo_type_is_simple(u_type) {
            let cur_line = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
            if let Some(ref ln) = cur_line {
                check_the_multis(ln);
            }
        } else if u_type == UndoType::Insert || u_type == UndoType::CoupleEnd {
            state_mut().recook = true;
        }
    }

    let (current_undo_ptr, last_saved_ptr) = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| (f.current_undo, f.last_saved))
            .unwrap_or((std::ptr::null_mut(), std::ptr::null_mut()))
    });
    if current_undo_ptr == last_saved_ptr {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.modified = false;
            }
        });
        titlebar(None);
    } else if !suppress_modification {
        set_modified();
    }
}

#[cfg(feature = "tiny")]
pub fn do_redo() {}

/// Set openfile.current_undo to the given raw pointer.
#[cfg(not(feature = "tiny"))]
fn set_current_undo_to(ptr: *mut UndoStruct) {
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current_undo = ptr;
        }
    });
}

// ---------------------------------------------------------------------------
// Section 6: Enter (newline insertion)
// ---------------------------------------------------------------------------

/* C: void do_enter(void) */
pub fn do_enter() {
    let (current_line, current_x) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        (f.current.clone().expect("a current line"), f.current_x)
    });

    let mut extra: usize = 0;
    #[cfg(not(feature = "tiny"))]
    let mut allblanks = false;
    #[cfg(not(feature = "tiny"))]
    let mut sampleline = current_line.clone();

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(AUTOINDENT) {
            #[cfg(feature = "justify")]
            {
                // When the next line is in the same paragraph, use its indent.
                let next = current_line.borrow().next.clone();
                if ISSET!(BREAK_LONG_LINES) {
                    if let Some(ref nx) = next {
                        if inpar(nx) && !begpar(nx, 0) {
                            sampleline = nx.clone();
                        }
                    }
                }
            }

            let sample_data = sampleline.borrow().data.clone();
            extra = indent_length(&sample_data);

            // When breaking inside the indentation, limit.
            if extra > current_x {
                extra = current_x;
            } else if extra == current_x {
                let cur_indent = indent_length(&current_line.borrow().data);
                allblanks = cur_indent == extra;
            }
        }
    }

    // Build new line data: indentation + rest of current line after cursor.
    let newline_data = {
        let cur_data = current_line.borrow().data.clone();
        let rest = if current_x <= cur_data.len() {
            &cur_data[current_x..]
        } else {
            &[]
        };
        #[cfg(not(feature = "tiny"))]
        {
            if ISSET!(AUTOINDENT) && extra > 0 {
                let sample_data = sampleline.borrow().data.clone();
                let indent_part = if extra <= sample_data.len() {
                    &sample_data[..extra]
                } else {
                    &sample_data[..]
                };
                let mut combined = LineData::from_internal(indent_part.to_vec());
                combined.extend_bytes(rest);
                combined
            } else {
                LineData::from_internal(rest.to_vec())
            }
        }
        #[cfg(feature = "tiny")]
        LineData::from_internal(rest.to_vec())
    };

    // Adjust mark if on the current line after cursor.
    #[cfg(not(feature = "tiny"))]
    {
        let mark_on_current = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.mark.as_ref())
                .map(|m| m.as_ptr())
                == Some(current_line.as_ptr())
        });
        let mark_x = with_state(|s| s.openfile.as_ref().map(|f| f.mark_x).unwrap_or(0));
        if mark_on_current && mark_x > current_x {
            // Mark will be on the new line.
        }
    }

    // C order (text.c:885-897): when AUTOINDENT and the prefix was all blanks,
    // reset current_x to 0 BEFORE truncating, so the blanks are actually removed
    // from the old line.  Truncating first (the old behaviour) kept the blanks,
    // duplicating them onto the new line and under-counting totsize.
    #[cfg(not(feature = "tiny"))]
    if ISSET!(AUTOINDENT) && allblanks {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current_x = 0;
            }
        });
    }

    // Make the current line end at the (possibly reset) cursor position.
    let trunc_x = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.current_x)
            .unwrap_or(current_x)
    });
    {
        let mut node = current_line.borrow_mut();
        if safe_edit_boundary(&node.data, trunc_x) == trunc_x {
            node.data.truncate(trunc_x);
        }
    }

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::Enter, None);

    // Create and splice the new node.
    let newnode = make_new_node(Some(current_line.clone()));
    newnode.borrow_mut().data = newline_data;
    splice_node(&current_line, newnode.clone());
    renumber_from(&newnode);

    // Update mark if it was on current line after cursor.
    #[cfg(not(feature = "tiny"))]
    {
        let mark_on_current_and_after = with_state(|s| {
            let f = s.openfile.as_ref()?;
            let mark = f.mark.as_ref()?;
            if mark.as_ptr() != current_line.as_ptr() {
                return None;
            }
            if f.mark_x > current_x {
                Some(f.mark_x)
            } else {
                None
            }
        });
        if let Some(old_mark_x) = mark_on_current_and_after {
            let new_mark_x = old_mark_x + extra - current_x;
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.mark = Some(newnode.clone());
                    f.mark_x = new_mark_x;
                }
            });
        }
        if ISSET!(AUTOINDENT) && allblanks {
            let mark_moved = with_state(|s| {
                let f = s.openfile.as_ref()?;
                Some(f.mark.as_ref().map(|m| m.as_ptr()) == Some(current_line.as_ptr()))
            })
            .unwrap_or(false);
            if mark_moved {
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.mark_x = 0;
                    }
                });
            }
        }
    }

    // Move cursor to new line.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current = Some(newnode.clone());
            f.current_x = extra;
            f.totsize += 1;
        }
    });

    let new_placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.placewewant = new_placewewant;
        }
    });

    set_modified();

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(AUTOINDENT) && !allblanks {
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.totsize += extra;
                }
            });
        }
        update_undo(UndoType::Enter);
    }

    with_state_mut(|s| {
        s.refresh_needed = true;
        s.focusing = false;
    });
}

// ---------------------------------------------------------------------------
// Section 7: inject — insert string at cursor
// ---------------------------------------------------------------------------

/* C: void inject(char *buf, size_t buf_len) */
pub fn inject<T: AsRef<[u8]> + ?Sized>(buf: &T, buf_len: usize) {
    let bytes = buf.as_ref();
    let insertion_end = safe_edit_boundary(bytes, buf_len.min(bytes.len()));
    let raw_insertion = &bytes[..insertion_end];
    if raw_insertion.is_empty() {
        return;
    }
    let insertion = LineData::from_external(raw_insertion);

    let (current_line, requested_x) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        (f.current.clone().expect("a current line"), f.current_x)
    });

    // Keep malformed restored state or a partially ported caller from making
    // splitting a valid UTF-8 scalar or leaving the cursor beyond end-of-line.
    let current_x = {
        let line = current_line.borrow();
        safe_edit_boundary(&line.data, requested_x)
    };
    if current_x != requested_x {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current_x = current_x;
            }
        });
    }

    #[cfg(not(feature = "tiny"))]
    let continues_previous_add = with_state(|s| {
        let Some(f) = s.openfile.as_ref() else {
            return false;
        };
        if f.last_action != UndoType::Add || f.current_undo.is_null() {
            return false;
        }

        let lineno = current_line.borrow().lineno;
        // SAFETY: current_undo points into this buffer's owned undo chain.  A
        // non-null pointer paired with last_action == Add is the active record.
        let undo = unsafe { &*f.current_undo };
        undo.r#type == UndoType::Add && undo.tail_lineno == lineno && undo.tail_x == current_x
    });

    #[cfg(not(feature = "tiny"))]
    {
        if !continues_previous_add {
            add_undo(UndoType::Add, None);
        }
    }

    {
        let mut node = current_line.borrow_mut();
        let mut data = node.data.clone();
        if current_x <= data.len() {
            data.insert_str(current_x, &insertion);
        } else {
            data.push_str(&insertion);
        }
        node.data = data;
    }

    let insertion_len = insertion.len();
    let insertion_chars = mbstrlen(&insertion);
    let new_x = current_x + insertion_len;
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current_x = new_x;
            f.totsize += insertion_chars;
        }
    });

    // Adjust mark if it is on the current line at or after the insertion point.
    #[cfg(not(feature = "tiny"))]
    {
        let mark_adjust = with_state(|s| {
            let f = s.openfile.as_ref()?;
            let mark = f.mark.as_ref()?;
            if mark.as_ptr() == current_line.as_ptr() && f.mark_x > current_x {
                Some(f.mark_x + insertion_len)
            } else {
                None
            }
        });
        if let Some(new_mark_x) = mark_adjust {
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.mark_x = new_mark_x;
                }
            });
        }
    }

    // Handle magic last line.
    let (is_filebot, no_newlines) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        let is_bot =
            f.filebot.as_ref().map(|b| b.as_ptr()) == f.current.as_ref().map(|c| c.as_ptr());
        (
            is_bot,
            s.flags[flag_index(NO_NEWLINES)] & flag_mask(NO_NEWLINES) != 0,
        )
    });

    if is_filebot && !no_newlines {
        new_magicline();
        #[cfg(not(feature = "tiny"))]
        {
            let ptr = with_state(|s| s.openfile.as_ref().map(|f| f.current_undo))
                .unwrap_or(std::ptr::null_mut());
            if !ptr.is_null() {
                unsafe {
                    (*ptr).xflags |= INCLUDED_LAST_LINE;
                }
            }
        }
    }

    let new_placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.placewewant = new_placewewant;
        }
    });

    #[cfg(not(feature = "tiny"))]
    update_undo(UndoType::Add);

    #[cfg(feature = "color")]
    {
        let cur_line = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
        if let Some(ref ln) = cur_line {
            check_the_multis(ln);
        }
    }

    set_modified();

    with_state_mut(|s| {
        s.refresh_needed = true;
    });
}

// ---------------------------------------------------------------------------
// Section 8: Undo stack management (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void discard_until(const undostruct *thisitem) */
#[cfg(not(feature = "tiny"))]
pub fn discard_until(thisitem: *const UndoStruct) {
    loop {
        let undotop_ptr = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.undotop.as_deref())
                .map(|b| b as *const UndoStruct)
                .unwrap_or(std::ptr::null())
        });

        if undotop_ptr == thisitem || undotop_ptr.is_null() {
            break;
        }

        // Pop the top item.
        let dropit = with_state_mut(|s| s.openfile.as_mut().and_then(|f| f.undotop.take()));

        if let Some(mut dropit) = dropit {
            // Re-attach the rest of the chain.
            let next_chain = dropit.next.take();
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.undotop = next_chain;
                }
            });
            // dropit is now dropped with its payload, cutbuffer, and grouping.
        }
    }

    // Re-resolve the retained item through mutable links.  `thisitem` is only
    // an identity token: it may have originated from a shared lookup and must
    // never itself be cast into the mutable undo cursor.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            let mut retained = f
                .undotop
                .as_deref_mut()
                .map(|record| record as *mut UndoStruct)
                .unwrap_or(std::ptr::null_mut());
            unsafe {
                while !retained.is_null() && !std::ptr::eq(retained.cast_const(), thisitem) {
                    retained = (*retained)
                        .next
                        .as_deref_mut()
                        .map(|record| record as *mut UndoStruct)
                        .unwrap_or(std::ptr::null_mut());
                }
            }
            f.current_undo = retained;
        }
    });

    // Prevent action chaining.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.last_action = UndoType::Other;
        }
    });
}

#[cfg(feature = "tiny")]
pub fn discard_until(_thisitem: *const UndoStruct) {}

/* C: void add_undo(undo_type action, const char *message) */
#[cfg(not(feature = "tiny"))]
pub fn add_undo(action: UndoType, message: Option<&str>) {
    let (thisline, current_x, totsize, current_undo_ptr) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        let ln = f.current.clone().expect("a current line");
        (ln, f.current_x, f.totsize, f.current_undo)
    });
    let lineno = thisline.borrow().lineno;

    let mut u = Box::new(UndoStruct {
        r#type: action,
        payload: None,
        description: None,
        cutbuffer: None,
        head_lineno: lineno,
        head_x: current_x,
        tail_lineno: lineno,
        tail_x: current_x,
        wassize: totsize,
        newsize: totsize,
        grouping: None,
        xflags: 0,
        next: None,
    });

    // Discard any undone items.
    discard_until(current_undo_ptr);

    #[cfg(feature = "wrapping")]
    {
        if u.r#type == UndoType::SplitBegin {
            // Insert under the top item.
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    if let Some(top) = f.undotop.as_deref_mut() {
                        u.wassize = top.wassize;
                        // Move top's next into u.next, then put u after top.
                        u.next = top.next.take();
                        top.next = Some(u);
                        // Refresh current_undo after uniquely traversing the
                        // Box that owns the active top record.
                        f.current_undo = top as *mut UndoStruct;
                    }
                }
            });
            // Do not update current_undo for SPLIT_BEGIN.
            store_undo_record_fields(action, &thisline, current_x, message);
            return;
        }
    }

    // Prepend to undo stack.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            u.next = f.undotop.take();
            f.undotop = Some(u);
            f.current_undo = f
                .undotop
                .as_deref_mut()
                .map(|record| record as *mut UndoStruct)
                .unwrap_or(std::ptr::null_mut());
        }
    });

    // Fill in action-specific fields.
    fill_undo_fields(action, &thisline, current_x, message);
}

#[cfg(feature = "tiny")]
pub fn add_undo(_action: UndoType, _message: Option<&str>) {}

/// Fill in action-specific fields of the freshly prepended undo record.
#[cfg(not(feature = "tiny"))]
fn fill_undo_fields(action: UndoType, thisline: &LinePtr, current_x: usize, message: Option<&str>) {
    unsafe {
        let ptr = with_state(|s| s.openfile.as_ref().map(|f| f.current_undo))
            .unwrap_or(std::ptr::null_mut());
        if ptr.is_null() {
            return;
        }
        let u = &mut *ptr;

        let filebot_ptr = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filebot.as_ref())
                .map(|b| b.as_ptr())
        });
        let is_filebot = Some(thisline.as_ptr()) == filebot_ptr;

        match action {
            UndoType::Add => {
                if is_filebot {
                    u.xflags |= INCLUDED_LAST_LINE;
                }
            }
            UndoType::Enter => {}
            UndoType::Back => {
                let next_is_filebot = thisline
                    .borrow()
                    .next
                    .as_ref()
                    .map(|nx| Some(nx.as_ptr()) == filebot_ptr)
                    .unwrap_or(false);
                let has_data = !thisline.borrow().data.is_empty();
                if next_is_filebot && has_data {
                    u.xflags |= WAS_BACKSPACE_AT_EOF;
                }
                // Fall-through to DEL logic
                let data = thisline.borrow().data.clone();
                if current_x < data.len() {
                    let charlen = char_length(&data[current_x..]);
                    u.payload = Some(LineData::from_internal(
                        data[current_x..current_x + charlen].to_vec(),
                    ));
                    if action == UndoType::Back {
                        u.tail_x += charlen;
                    }
                } else {
                    // Join action
                    let next_data = thisline
                        .borrow()
                        .next
                        .as_ref()
                        .map(|nx| nx.borrow().data.clone());
                    u.payload = next_data;
                    if action == UndoType::Back {
                        u.head_lineno = thisline
                            .borrow()
                            .next
                            .as_ref()
                            .map(|nx| nx.borrow().lineno)
                            .unwrap_or(u.head_lineno);
                        u.head_x = 0;
                    }
                    u.r#type = UndoType::Join;
                }
            }
            UndoType::Del => {
                let data = thisline.borrow().data.clone();
                if current_x < data.len() {
                    let charlen = char_length(&data[current_x..]);
                    u.payload = Some(LineData::from_internal(
                        data[current_x..current_x + charlen].to_vec(),
                    ));
                } else {
                    let next_data = thisline
                        .borrow()
                        .next
                        .as_ref()
                        .map(|nx| nx.borrow().data.clone());
                    u.payload = next_data;
                    u.r#type = UndoType::Join;
                }
            }
            UndoType::Replace => {
                u.payload = Some(thisline.borrow().data.clone());
            }
            #[cfg(feature = "wrapping")]
            UndoType::SplitBegin | UndoType::SplitEnd => {}
            UndoType::CutToEof => {
                u.xflags |= INCLUDED_LAST_LINE | CURSOR_WAS_AT_HEAD;
                #[cfg(not(feature = "tiny"))]
                {
                    if thisline.borrow().has_anchor {
                        u.xflags |= HAD_ANCHOR_AT_START;
                    }
                }
            }
            UndoType::Zap | UndoType::Cut => {
                let (mark, mark_x, cut_from_cursor) = with_state(|s| {
                    let f = s.openfile.as_ref().expect("an open buffer");
                    (
                        f.mark.clone(),
                        f.mark_x,
                        s.flags[flag_index(CUT_FROM_CURSOR)] & flag_mask(CUT_FROM_CURSOR) != 0,
                    )
                });

                if let Some(ref m) = mark {
                    let mark_lineno = m.borrow().lineno;
                    let _cur_lineno = thisline.borrow().lineno;
                    let _cur_x = current_x;

                    if mark_is_before_cursor() {
                        u.head_lineno = mark_lineno;
                        u.head_x = mark_x;
                        u.xflags |= MARK_WAS_SET;
                    } else {
                        u.tail_lineno = mark_lineno;
                        u.tail_x = mark_x;
                        u.xflags |= MARK_WAS_SET | CURSOR_WAS_AT_HEAD;
                    }

                    let filebot_lineno = with_state(|s| {
                        s.openfile
                            .as_ref()
                            .and_then(|f| f.filebot.as_ref())
                            .map(|b| b.borrow().lineno)
                            .unwrap_or(0)
                    });
                    if u.tail_lineno == filebot_lineno {
                        u.xflags |= INCLUDED_LAST_LINE;
                    }
                } else if !cut_from_cursor {
                    u.xflags |= WAS_WHOLE_LINE | CURSOR_WAS_AT_HEAD;
                    u.tail_x = 0;
                } else {
                    u.xflags |= CURSOR_WAS_AT_HEAD;
                }

                // Anchor tracking
                let had_anchor = with_state(|s| {
                    let f = s.openfile.as_ref()?;
                    if let Some(ref m) = f.mark {
                        if mark_is_before_cursor() {
                            return Some(m.borrow().has_anchor);
                        }
                    }
                    f.current.as_ref().map(|c| c.borrow().has_anchor)
                })
                .unwrap_or(false);
                if had_anchor {
                    u.xflags |= HAD_ANCHOR_AT_START;
                }
            }
            UndoType::Paste => {
                let cb = get_cutbuffer();
                if let Some(ref cb_line) = cb {
                    u.cutbuffer = Some(copy_buffer(cb_line));
                }
                // Fall-through to INSERT logic
                if is_filebot {
                    u.xflags |= INCLUDED_LAST_LINE;
                }
            }
            UndoType::Insert => {
                if is_filebot {
                    u.xflags |= INCLUDED_LAST_LINE;
                }
            }
            UndoType::CoupleBegin => {
                let cursor_row =
                    with_state(|s| s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0));
                u.tail_lineno = cursor_row;
                if let Some(msg) = message {
                    u.description = Some(msg.to_string());
                }
            }
            UndoType::CoupleEnd => {
                if let Some(msg) = message {
                    u.description = Some(msg.to_string());
                }
            }
            UndoType::Indent | UndoType::Unindent => {}
            #[cfg(feature = "comment")]
            UndoType::Comment | UndoType::Uncomment => {}
            _ => {
                // Unknown action type — this is a programming error in the C source.
                // In C: die("Bad undo type -- please report a bug\n")
                eprintln!("Bad undo type -- please report a bug");
            }
        }

        // Update last_action.
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.last_action = action;
            }
        });
    }
}

/// Stub for SPLIT_BEGIN special case store.
#[cfg(not(feature = "tiny"))]
#[cfg(feature = "wrapping")]
fn store_undo_record_fields(
    action: UndoType,
    thisline: &LinePtr,
    current_x: usize,
    message: Option<&str>,
) {
    fill_undo_fields(action, thisline, current_x, message);
}

/* C: void update_multiline_undo(ssize_t lineno, char *indentation) */
#[cfg(not(feature = "tiny"))]
pub fn update_multiline_undo<T: AsRef<[u8]> + ?Sized>(lineno: isize, indentation: &T) {
    let indentation = indentation.as_ref();
    with_state_mut(|s| {
        let f = s.openfile.as_mut().expect("an open buffer");
        let u_ptr = f.current_undo;
        if u_ptr.is_null() {
            return;
        }
        let u = unsafe { &mut *u_ptr };

        if let Some(ref mut group) = u.grouping {
            if group.bottom_line + 1 == lineno {
                group.bottom_line = lineno;
                group
                    .indentations
                    .push(LineData::from_internal(indentation.to_vec()));
                u.newsize = f.totsize;
                return;
            }
        }

        // Create a new group.
        let born = Box::new(GroupStruct {
            top_line: lineno,
            bottom_line: lineno,
            indentations: vec![LineData::from_internal(indentation.to_vec())],
            next: u.grouping.take(),
        });
        u.grouping = Some(born);
        u.newsize = f.totsize;
    });
}

#[cfg(feature = "tiny")]
pub fn update_multiline_undo<T: AsRef<[u8]> + ?Sized>(_lineno: isize, _indentation: &T) {}

/* C: void update_undo(undo_type action) */
#[cfg(not(feature = "tiny"))]
pub fn update_undo(action: UndoType) {
    let (current_line, current_x, totsize) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        (
            f.current.clone().expect("a current line"),
            f.current_x,
            f.totsize,
        )
    });

    unsafe {
        let ptr = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.current_undo)
                .unwrap_or(std::ptr::null_mut())
        });
        if ptr.is_null() {
            return;
        }
        let u = &mut *ptr;

        // Verify type match.
        if u.r#type != action {
            eprintln!("Mismatching undo type -- please report a bug");
            return;
        }

        u.newsize = totsize;

        let data = current_line.borrow().data.clone();

        match u.r#type {
            UndoType::Add => {
                let newlen = if current_x >= u.head_x {
                    current_x - u.head_x
                } else {
                    0
                };
                if u.head_x <= data.len() {
                    let start = safe_edit_boundary(&data, u.head_x);
                    let raw_end = u.head_x + newlen.min(data.len() - u.head_x);
                    let end = safe_edit_boundary_end(&data, raw_end);
                    u.payload = Some(LineData::from_internal(data[start..end].to_vec()));
                }
                u.tail_x = current_x;
            }
            UndoType::Enter => {
                u.payload = Some(data.clone());
                u.tail_x = current_x;
            }
            UndoType::Back | UndoType::Del => {
                let cur_safe = safe_edit_boundary(&data, current_x);
                let text_at_pos = &data[cur_safe..];
                let charlen = char_length(text_at_pos);
                let _datalen = u.payload.as_deref().map(|s| s.len()).unwrap_or(0);

                if current_x == u.head_x {
                    // Deleted more forward.
                    let addition = if charlen > 0 {
                        let s = safe_edit_boundary(&data, current_x);
                        let e = safe_edit_boundary_end(&data, current_x + charlen);
                        LineData::from_internal(data[s..e].to_vec())
                    } else {
                        LineData::empty()
                    };
                    if let Some(ref mut sd) = u.payload {
                        sd.push_str(&addition);
                    } else {
                        u.payload = Some(addition);
                    }
                    u.tail_x = current_x;
                } else if current_x + charlen == u.head_x {
                    // Backspaced further.
                    let addition = if charlen > 0 && current_x + charlen <= data.len() {
                        LineData::from_internal(data[current_x..current_x + charlen].to_vec())
                    } else {
                        LineData::empty()
                    };
                    let existing = u.payload.take().unwrap_or_default();
                    let mut combined = addition;
                    combined.push_str(&existing);
                    u.payload = Some(combined);
                    u.head_x = current_x;
                } else {
                    // Deleted elsewhere — start new undo.
                    add_undo(u.r#type, None);
                }
            }
            UndoType::Replace => {}
            #[cfg(feature = "wrapping")]
            UndoType::SplitBegin | UndoType::SplitEnd => {}
            UndoType::Zap | UndoType::CutToEof | UndoType::Cut => {
                let cb = get_cutbuffer();
                if u.r#type == UndoType::Zap {
                    u.cutbuffer = cb;
                } else if let Some(ref cb_line) = cb {
                    u.cutbuffer = Some(copy_buffer(cb_line));
                } else {
                    return;
                }

                let xflags = u.xflags;
                if (xflags & MARK_WAS_SET) == 0 {
                    // Count lines in cut buffer.
                    let mut count: isize = 0;
                    let mut bottomline = u.cutbuffer.clone();
                    let mut last_line = bottomline.clone();
                    while let Some(bl) = bottomline {
                        if bl.borrow().next.is_none() {
                            last_line = Some(bl.clone());
                            break;
                        }
                        count += 1;
                        bottomline = bl.borrow().next.clone();
                    }
                    u.tail_lineno = u.head_lineno + count;

                    let cut_from_cursor = ISSET!(CUT_FROM_CURSOR);
                    if cut_from_cursor || u.r#type == UndoType::CutToEof {
                        let ll_len = last_line
                            .as_ref()
                            .map(|l| l.borrow().data.len())
                            .unwrap_or(0);
                        u.tail_x = ll_len;
                        if count == 0 {
                            u.tail_x += u.head_x;
                        }
                    } else {
                        let at_filebot = with_state(|s| {
                            s.openfile
                                .as_ref()
                                .and_then(|f| {
                                    f.current.as_ref().map(|c| {
                                        f.filebot
                                            .as_ref()
                                            .map(|b| b.as_ptr() == c.as_ptr())
                                            .unwrap_or(false)
                                    })
                                })
                                .unwrap_or(false)
                        });
                        if at_filebot && ISSET!(NO_NEWLINES) {
                            u.tail_x = last_line
                                .as_ref()
                                .map(|l| l.borrow().data.len())
                                .unwrap_or(0);
                        }
                    }
                }
            }
            UndoType::CoupleBegin => {}
            UndoType::CoupleEnd | UndoType::Paste | UndoType::Insert => {
                u.tail_lineno = current_line.borrow().lineno;
                u.tail_x = current_x;
            }
            _ => {
                eprintln!("Bad undo type -- please report a bug");
            }
        }
    }
}

#[cfg(feature = "tiny")]
pub fn update_undo(_action: UndoType) {}

// ---------------------------------------------------------------------------
// Section 9: Wrapping (ENABLE_WRAPPING)
// ---------------------------------------------------------------------------

/* C: void do_wrap(void) */
#[cfg(feature = "wrapping")]
pub fn do_wrap() {
    let (line, current_x_val) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        (f.current.clone().expect("a current line"), f.current_x)
    });

    let line_data = line.borrow().data.clone();
    let line_len = line_data.len();

    #[cfg(feature = "justify")]
    let quot_len = quote_length(&line_data);
    #[cfg(not(feature = "justify"))]
    let quot_len: usize = 0;

    let lead_len = {
        #[cfg(feature = "justify")]
        {
            quot_len + indent_length(&line_data[quot_len..])
        }
        #[cfg(not(feature = "justify"))]
        {
            indent_length(&line_data)
        }
    };

    let wrap_at = state().wrap_at;
    let lead_width = wideness(&line_data, lead_len);
    let wrap_loc_rel = break_line(
        &line_data[lead_len..],
        (wrap_at as isize) - (lead_width as isize),
        false,
    );

    if wrap_loc_rel < 0 || lead_len + (wrap_loc_rel as usize) == line_len {
        return;
    }

    let after_blank = step_right(&line_data[lead_len..], wrap_loc_rel as usize);
    let wrap_loc = lead_len + after_blank;

    if wrap_loc >= line_len || line_data.as_bytes().get(wrap_loc) == Some(&b'\0') {
        return;
    }
    // Actually wrap_loc is within the string if wrap_loc < line_len.
    if wrap_loc >= line_len {
        return;
    }

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::SplitBegin, None);

    #[cfg(feature = "justify")]
    let autowhite = ISSET!(AUTOINDENT);
    #[cfg(feature = "justify")]
    if quot_len > 0 {
        UNSET!(AUTOINDENT);
    }

    let remainder = LineData::from_internal(line_data[wrap_loc..].to_vec());
    let rest_length = remainder.len();

    // If there's a spillage line we can prepend to, try joining first.
    let spillage_opt = with_state(|s| {
        #[cfg(feature = "wrapping")]
        {
            s.openfile.as_ref().and_then(|f| f.spillage_line.clone())
        }
        #[cfg(not(feature = "wrapping"))]
        {
            None::<LinePtr>
        }
    });

    let next_line = line.borrow().next.clone();
    let is_spillage_next = spillage_opt
        .as_ref()
        .and_then(|sp| next_line.as_ref().map(|nx| sp.as_ptr() == nx.as_ptr()))
        .unwrap_or(false);

    if is_spillage_next {
        if let Some(ref next) = next_line {
            let next_breadth = breadth(&next.borrow().data);
            if rest_length + next_breadth <= wrap_at {
                // Go to end of this line and join.
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current_x = line_len;
                    }
                });

                // If remainder doesn't end in blank, add space.
                let last_char_pos = step_left(&line_data, line_len);
                let last_char = &line_data[last_char_pos..];
                if !is_blank_char(last_char) {
                    #[cfg(not(feature = "tiny"))]
                    add_undo(UndoType::Add, None);
                    {
                        let mut node = line.borrow_mut();
                        node.data.push(' ');
                    }
                    with_state_mut(|s| {
                        if let Some(ref mut f) = s.openfile {
                            f.totsize += 1;
                            f.current_x += 1;
                        }
                    });
                    #[cfg(not(feature = "tiny"))]
                    update_undo(UndoType::Add);
                }

                expunge(UndoType::Del);

                #[cfg(feature = "justify")]
                {
                    let lead_matches = {
                        let cur_x =
                            with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                        let d = line.borrow().data.clone();
                        d.starts_with(&line_data[..lead_len])
                            && d[cur_x..].starts_with(&line_data[..lead_len])
                    };
                    if lead_matches {
                        for _ in 0..lead_len {
                            expunge(UndoType::Del);
                        }
                    }
                }

                // Remove any extra blanks.
                loop {
                    let cur_x =
                        with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                    let d = line.borrow().data.clone();
                    if cur_x < d.len() && is_blank_char(&d[cur_x..]) {
                        expunge(UndoType::Del);
                    } else {
                        break;
                    }
                }
            }
        }
    }

    // Go to wrap location.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current_x = wrap_loc;
        }
    });

    // Trim trailing blanks if requested.
    if ISSET!(TRIM_BLANKS) {
        let line_data_now = line.borrow().data.clone();
        let rear_x = step_left(&line_data_now, wrap_loc);
        let typed_x = step_left(&line_data_now, current_x_val);

        let mut rr = rear_x;
        loop {
            let _cur_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
            let d = line.borrow().data.clone();
            if rr == 0 {
                break;
            }
            if (rr != typed_x || current_x_val >= wrap_loc) && is_blank_char(&d[rr..]) {
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current_x = rr;
                    }
                });
                expunge(UndoType::Del);
                let d2 = line.borrow().data.clone();
                rr = step_left(&d2, rr);
            } else {
                break;
            }
        }
    }

    // Split the line.
    do_enter();

    // When wrapping a partially visible line, adjust edittop.
    #[cfg(not(feature = "tiny"))]
    {
        let (edittop, firstcolumn) = with_state(|s| {
            let f = s.openfile.as_ref().expect("an open buffer");
            (f.edittop.clone(), f.firstcolumn)
        });
        if let Some(mut et) = edittop {
            if LinePtr::ptr_eq(&et, &line) && firstcolumn > 0 && current_x_val >= wrap_loc {
                let mut fc = firstcolumn;
                crate::winio::go_forward_chunks(1, &mut et, &mut fc);
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.edittop = Some(et);
                        f.firstcolumn = fc;
                    }
                });
            }
        }
    }

    #[cfg(feature = "justify")]
    {
        if quot_len > 0 {
            let new_line = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
            if let Some(ref nl) = new_line {
                let nl_data = nl.borrow().data.clone();
                let _nl_len = nl_data.len();
                let prev_data = nl
                    .borrow()
                    .prev
                    .as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|p| {
                        let data = p.borrow().data.clone();
                        LineData::from_internal(data[..lead_len.min(data.len())].to_vec())
                    })
                    .unwrap_or_default();
                let mut new_data = prev_data;
                new_data.push_str(&nl_data);
                nl.borrow_mut().data = new_data;

                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current_x += lead_len;
                        f.totsize += lead_len;
                    }
                });
                #[cfg(not(feature = "tiny"))]
                {
                    // Update the ENTER undo record.
                    let ptr = with_state(|s| {
                        s.openfile
                            .as_ref()
                            .map(|f| f.current_undo)
                            .unwrap_or(std::ptr::null_mut())
                    });
                    if !ptr.is_null() {
                        unsafe {
                            (*ptr).payload = None;
                        }
                    }
                    update_undo(UndoType::Enter);
                }
            }
            if autowhite {
                SET!(AUTOINDENT);
            }
        }
    }

    // Mark the spillage line.
    let new_current = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
    with_state_mut(|s| {
        #[cfg(feature = "wrapping")]
        if let Some(ref mut f) = s.openfile {
            f.spillage_line = new_current.clone();
        }
    });

    if current_x_val < wrap_loc {
        let prev_line = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.current.as_ref())
                .and_then(|c| c.borrow().prev.as_ref().and_then(|w| w.upgrade()))
        });
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current = prev_line;
                f.current_x = current_x_val;
            }
        });
    } else {
        let new_x = current_x_val - wrap_loc;
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current_x += new_x;
            }
        });
    }

    let new_placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.placewewant = new_placewewant;
        }
    });

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::SplitEnd, None);

    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// Section 10: break_line (ENABLE_HELP || ENABLE_WRAPPING || ENABLE_JUSTIFY)
// ---------------------------------------------------------------------------

/* C: ssize_t break_line(const char *textstart, ssize_t goal, bool snap_at_nl) */
#[cfg(any(feature = "help", feature = "wrapping", feature = "justify"))]
pub fn break_line<T: AsRef<[u8]> + ?Sized>(textstart: &T, goal: isize, snap_at_nl: bool) -> isize {
    let inhelp = state().inhelp;
    let mut lastblank: Option<usize> = None;
    let mut pos = 0usize;
    let mut column: usize = 0;
    let bytes = textstart.as_ref();

    // Skip over leading whitespace.
    while pos < bytes.len() && is_blank_char(&bytes[pos..]) {
        pos += advance_over(&bytes[pos..], &mut column);
    }

    // Find the last blank that does not overshoot the goal.
    while pos < bytes.len() && (column as isize) <= goal {
        let ch = &bytes[pos..];
        if is_blank_char(ch) {
            if !inhelp || column > 17 || goal < 40 {
                lastblank = Some(pos);
            }
        }
        #[cfg(feature = "help")]
        {
            if snap_at_nl && bytes[pos] == b'\n' {
                lastblank = Some(pos);
                break;
            }
        }
        pos += advance_over(&bytes[pos..], &mut column);
    }

    // If the whole text fits within the goal.
    if (column as isize) <= goal {
        return pos as isize;
    }

    #[cfg(feature = "help")]
    {
        if snap_at_nl && lastblank.is_none() {
            return step_left(bytes, pos) as isize;
        }
    }

    // If no blank was found within the goal, seek one after it.
    if lastblank.is_none() {
        while pos < bytes.len() {
            if is_blank_char(&bytes[pos..]) {
                lastblank = Some(pos);
                break;
            }
            pos += char_length(&bytes[pos..]);
        }
        if lastblank.is_none() {
            return -1;
        }
    }

    let mut lb = lastblank.unwrap();
    let lb_charlen = char_length(&bytes[lb..]);
    let mut after_lb = lb + lb_charlen;

    // Skip consecutive blanks after the last blank.
    while after_lb < bytes.len() && is_blank_char(&bytes[after_lb..]) {
        lb = after_lb;
        after_lb += char_length(&bytes[after_lb..]);
    }

    lb as isize
}

// ---------------------------------------------------------------------------
// Section 11: indent_length (not NANO_TINY || ENABLED_WRAPORJUSTIFY)
// ---------------------------------------------------------------------------

/* C: size_t indent_length(const char *line) */
#[cfg(any(not(feature = "tiny"), feature = "wrapping", feature = "justify"))]
pub fn indent_length<T: AsRef<[u8]> + ?Sized>(line: &T) -> usize {
    let mut pos = 0usize;
    let bytes = line.as_ref();
    while pos < bytes.len() && is_blank_char(&bytes[pos..]) {
        pos += char_length(&bytes[pos..]);
    }
    pos
}

// ---------------------------------------------------------------------------
// Section 12: Justify (ENABLE_JUSTIFY)
// ---------------------------------------------------------------------------

/* C: size_t quote_length(const char *line) */
#[cfg(feature = "justify")]
pub fn quote_length<T: AsRef<[u8]> + ?Sized>(line: &T) -> usize {
    let bytes = line.as_ref();
    with_state(|s| {
        if let Some(ref re) = s.quotereg {
            if let Some(m) = re.find(bytes) {
                if m.start() == 0 {
                    return m.end();
                }
            }
        }
        0usize
    })
}

/* C: bool begpar(const linestruct *const line, int depth) */
#[cfg(feature = "justify")]
pub fn begpar_fn(line: &LinePtr, depth: i32) -> bool {
    if line.borrow().prev.is_none() {
        return true;
    }
    if depth > 222 {
        return false;
    }

    let data = line.borrow().data.clone();
    let quot_len = quote_length(&data);
    let indent_len = indent_length(&data[quot_len..]);

    // If line contains no text, it is not a BOP.
    if data[quot_len + indent_len..].is_empty() {
        return false;
    }

    if ISSET!(BOOKSTYLE) && !ISSET!(AUTOINDENT) && is_blank_char(&data[..]) {
        return true;
    }

    // If quote part of preceding line differs.
    let prev_data = line
        .borrow()
        .prev
        .as_ref()
        .and_then(|w| w.upgrade())
        .map(|p| p.borrow().data.clone())
        .unwrap_or_default();

    let prev_quot_len = quote_length(&prev_data);
    if quot_len != prev_quot_len || &data[..quot_len] != &prev_data[..prev_quot_len] {
        return true;
    }

    let prev_indent_len = indent_length(&prev_data[prev_quot_len..]);

    // If previous line contains no text.
    if prev_data[prev_quot_len + prev_indent_len..].is_empty() {
        return true;
    }

    // If indentations are equal, not a BOP.
    if wideness(&prev_data, prev_quot_len + prev_indent_len)
        == wideness(&data, quot_len + indent_len)
    {
        return false;
    }

    // BOP if previous line is not.
    let prev_line = line.borrow().prev.as_ref().and_then(|w| w.upgrade());
    if let Some(ref pl) = prev_line {
        !begpar_fn(pl, depth + 1)
    } else {
        true
    }
}

/* C: bool inpar(const linestruct *const line) */
#[cfg(feature = "justify")]
pub fn inpar_fn(line: &LinePtr) -> bool {
    let data = line.borrow().data.clone();
    let quot_len = quote_length(&data);
    let indent_len = indent_length(&data[quot_len..]);
    !data[quot_len + indent_len..].is_empty()
}

/* C: bool find_paragraph(linestruct **firstline, size_t *const linecount) */
#[cfg(feature = "justify")]
pub fn find_paragraph(firstline: &mut LinePtr, linecount: &mut usize) -> bool {
    let mut line = firstline.clone();

    // Skip non-paragraph lines.
    while !inpar_fn(&line) {
        let next = line.borrow().next.clone();
        if let Some(nx) = next {
            line = nx;
        } else {
            break;
        }
    }

    *firstline = line.clone();

    // Move to paragraph end.
    let end_line = do_para_end(line.clone());

    if !inpar_fn(&end_line) {
        return false;
    }

    *linecount = (end_line.borrow().lineno - firstline.borrow().lineno + 1) as usize;
    true
}

/* C: void concat_paragraph(linestruct *line, size_t count) */
#[cfg(feature = "justify")]
pub fn concat_paragraph(line: &LinePtr, count: usize) {
    let mut remaining = count;
    let cur = line.clone();
    while remaining > 1 {
        let next_line = cur.borrow().next.clone();
        if let Some(ref nl) = next_line {
            let next_data = nl.borrow().data.clone();
            let next_quot_len = quote_length(&next_data);
            let next_indent_len = indent_length(&next_data[next_quot_len..]);
            let next_lead_len = next_quot_len + next_indent_len;
            let stripped = &next_data[next_lead_len..];

            {
                let mut node = cur.borrow_mut();
                if !node.data.is_empty() && !node.data.ends_with(b" ") {
                    node.data.push(' ');
                }
                node.data.push_str(stripped);
            }

            #[cfg(not(feature = "tiny"))]
            {
                let next_anchor = nl.borrow().has_anchor;
                cur.borrow_mut().has_anchor |= next_anchor;
            }

            unlink_node(nl);
        }
        remaining -= 1;
    }
}

/* C: void copy_character(char **from, char **to) */
// (Internal to squeeze — handled inline)

/* C: void squeeze(linestruct *line, size_t skip) */
#[cfg(feature = "justify")]
pub fn squeeze(line: &LinePtr, skip: usize) {
    let data = line.borrow().data.clone();
    let punct = state().punct.clone().unwrap_or_default();
    let brackets = state().brackets.clone().unwrap_or_default();

    let start_bytes = &data[skip..];
    let mut result = LineData::from_internal(data[..skip].to_vec());

    let mut from = start_bytes;

    while !from.is_empty() {
        if is_blank_char(from) {
            let charlen = char_length(from);
            from = &from[charlen..];
            result.push(' ');
            while !from.is_empty() && is_blank_char(from) {
                let cl = char_length(from);
                from = &from[cl..];
            }
        } else if mbstrchr(&punct, from).is_some() {
            // Copy punctuation character.
            let charlen = char_length(from);
            result.push_str(&from[..charlen]);
            from = &from[charlen..];

            // Optional trailing bracket.
            if !from.is_empty() && mbstrchr(&brackets, from).is_some() {
                let cl = char_length(from);
                result.push_str(&from[..cl]);
                from = &from[cl..];
            }

            // Up to two spaces after punctuation.
            if !from.is_empty() && is_blank_char(from) {
                let cl = char_length(from);
                from = &from[cl..];
                result.push(' ');
            }
            if !from.is_empty() && is_blank_char(from) {
                let cl = char_length(from);
                from = &from[cl..];
                result.push(' ');
            }
            while !from.is_empty() && is_blank_char(from) {
                let cl = char_length(from);
                from = &from[cl..];
            }
        } else {
            let charlen = char_length(from);
            result.push_str(&from[..charlen]);
            from = &from[charlen..];
        }
    }

    // Remove trailing spaces.
    while result.len() > skip && result.ends_with(b" ") {
        result.truncate(result.len() - 1);
    }

    line.borrow_mut().data = result;
}

/* C: void rewrap_paragraph(linestruct **line, char *lead_string, size_t lead_len) */
#[cfg(feature = "justify")]
pub fn rewrap_paragraph(line: &mut LinePtr, lead_string: &LineData, lead_len: usize) {
    let wrap_at = state().wrap_at;

    loop {
        let line_data = line.borrow().data.clone();
        if breadth(&line_data) <= wrap_at {
            break;
        }
        let line_len = line_data.len();
        let lead_width = wideness(&line_data, lead_len);
        let break_pos = break_line(
            &line_data[lead_len..],
            (wrap_at as isize) - (lead_width as isize),
            false,
        );

        if break_pos < 0 || lead_len + (break_pos as usize) == line_len {
            break;
        }

        let break_pos_abs = lead_len + break_pos as usize + 1; // +1 to skip blank

        // Create new line after current.
        let mut new_data = lead_string.clone();
        new_data.push_str(&line_data[break_pos_abs..]);
        let new_node = make_new_node(Some(line.clone()));
        new_node.borrow_mut().data = new_data;

        // Determine truncation point (trim trailing spaces if TRIM_BLANKS).
        let mut trunc = break_pos_abs;
        if ISSET!(TRIM_BLANKS) {
            while trunc > 0 && line_data.as_bytes().get(trunc - 1) == Some(&b' ') {
                trunc -= 1;
            }
        }

        {
            let mut node = line.borrow_mut();
            node.data.truncate(trunc);
        }

        splice_node(line, new_node.clone());

        *line = new_node;
    }

    #[cfg(feature = "color")]
    {
        let editwinrows = state().editwinrows;
        if line.borrow().lineno >= editwinrows as isize {
            state_mut().recook = true;
        }
    }

    // Advance to the line after the paragraph.
    let next = line.borrow().next.clone();
    if let Some(nx) = next {
        *line = nx;
    }
}

/* C: void justify_paragraph(linestruct **line, size_t count) */
#[cfg(feature = "justify")]
pub fn justify_paragraph(line: &mut LinePtr, count: usize) {
    let sampleline = if count == 1 {
        line.clone()
    } else {
        line.borrow().next.clone().unwrap_or_else(|| line.clone())
    };

    let sample_data = sampleline.borrow().data.clone();
    let quot_len = quote_length(&sample_data);
    let lead_len = quot_len + indent_length(&sample_data[quot_len..]);
    let lead_string = measured_copy(&sample_data, lead_len);

    concat_paragraph(line, count);

    let first_quot_len = {
        let d = line.borrow().data.clone();
        let ql = quote_length(&d);
        ql + indent_length(&d[ql..])
    };
    squeeze(line, first_quot_len);

    rewrap_paragraph(line, &lead_string, lead_len);
}

/* C: void justify_text(bool whole_buffer) */
#[cfg(feature = "justify")]
pub fn justify_text(whole_buffer: bool) {
    let mut linecount: usize = 0;
    let was_cutbuffer = get_cutbuffer();

    // These are set by the marked and non-marked branches.
    let startline: LinePtr;
    let start_x: usize;
    let endline: LinePtr;
    let end_x: usize;

    #[cfg(not(feature = "tiny"))]
    let was_the_linenumber = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.as_ref())
            .map(|c| c.borrow().lineno)
            .unwrap_or(0)
    });

    #[cfg(not(feature = "tiny"))]
    let marked_backward = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.mark.is_some() && !mark_is_before_cursor())
            .unwrap_or(false)
    });

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::CoupleBegin, Some("justification"));

    #[cfg(not(feature = "tiny"))]
    let has_mark = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.mark.is_some())
            .unwrap_or(false)
    });
    #[cfg(feature = "tiny")]
    let has_mark = false;

    if has_mark {
        #[cfg(not(feature = "tiny"))]
        {
            // Get region boundaries.
            let (sl, sx, el, ex) = {
                // Adjust region: recede over blanks at start, advance over blanks at end.
                let (s_lineno, mut sx_v, e_lineno, mut ex_v) = get_region();
                let (Some(sl_ptr), Some(el_ptr)) = (
                    get_line_from_number(s_lineno as isize),
                    get_line_from_number(e_lineno as isize),
                ) else {
                    statusline(
                        MessageType::Alert,
                        "Internal error: region refers to a nonexistent line",
                    );
                    return;
                };

                let sl_data = sl_ptr.borrow().data.clone();
                let el_data = el_ptr.borrow().data.clone();

                let quot_len = quote_length(&sl_data);
                let fore_len = quot_len + indent_length(&sl_data[quot_len..]);
                if sx_v <= fore_len {
                    sx_v = 0;
                }
                while sx_v > 0 && is_blank_char(&sl_data[sx_v - 1..]) {
                    sx_v = step_left(&sl_data, sx_v);
                }

                let eq_len = quote_length(&el_data);
                let ef_len = eq_len + indent_length(&el_data[eq_len..]);
                if 0 < ex_v && ex_v < ef_len {
                    ex_v = ef_len;
                }
                while ex_v > 0 && is_blank_char(&el_data[ex_v..]) {
                    ex_v = step_right(&el_data, ex_v);
                }

                (sl_ptr, sx_v, el_ptr, ex_v)
            };

            if sl.as_ptr() == el.as_ptr() && sx == ex {
                statusline(MessageType::Ahem, tr!("Selection is empty"));
                let undotop_next_ptr = with_state(|s| {
                    s.openfile
                        .as_ref()
                        .and_then(|f| f.undotop.as_deref())
                        .and_then(|u| u.next.as_deref())
                        .map(|b| b as *const UndoStruct)
                        .unwrap_or(std::ptr::null())
                });
                discard_until(undotop_next_ptr);
                return;
            }

            // Find the sample line for leading-part determination.
            let mut sampleline = sl.clone();
            while sampleline.borrow().prev.is_some()
                && inpar_fn(&sampleline)
                && !begpar_fn(&sampleline, 0)
            {
                let prev = sampleline.borrow().prev.as_ref().and_then(|w| w.upgrade());
                if let Some(p) = prev {
                    sampleline = p;
                } else {
                    break;
                }
            }
            while sampleline.borrow().next.is_some() && !inpar_fn(&sampleline) {
                let next = sampleline.borrow().next.clone();
                if let Some(nx) = next {
                    sampleline = nx;
                } else {
                    break;
                }
            }

            let sample_data = sampleline.borrow().data.clone();
            let s_quot_len = quote_length(&sample_data);
            let primary_len = s_quot_len + indent_length(&sample_data[s_quot_len..]);
            let primary_lead = measured_copy(&sample_data, primary_len);

            // Secondary lead: quote from first line + indent from second.
            let (secondary_lead, secondary_len) = {
                let next_sample =
                    if sampleline.borrow().next.is_some() && sl.as_ptr() != el.as_ptr() {
                        sampleline.borrow().next.clone().unwrap()
                    } else {
                        sampleline.clone()
                    };
                let ns_data = next_sample.borrow().data.clone();
                let ns_quot_len = quote_length(&ns_data);
                let ns_white_len = indent_length(&ns_data[ns_quot_len..]);
                let sec_len = s_quot_len + ns_white_len;
                let sec_lead = {
                    let sl_data_ref = sl.borrow().data.clone();
                    let part1 = if s_quot_len <= sl_data_ref.len() {
                        &sl_data_ref[..s_quot_len]
                    } else {
                        &sl_data_ref[..]
                    };
                    let part2 = if ns_quot_len + ns_white_len <= ns_data.len() {
                        &ns_data[ns_quot_len..ns_quot_len + ns_white_len]
                    } else {
                        &[]
                    };
                    let mut combined = LineData::from_internal(part1.to_vec());
                    combined.push_str(part2);
                    combined
                };
                (sec_lead, sec_len)
            };

            let before_eol = {
                let el_data = el.borrow().data.clone();
                el_data.as_bytes().get(ex).copied() != Some(b'\0') && ex < el_data.len()
            };
            linecount = el.borrow().lineno as usize - sl.borrow().lineno as usize
                + if ex > 0 { 1 } else { 0 };

            // Include leading/trailing blanks in region.
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.mark = Some(sl.clone());
                    f.mark_x = sx;
                    f.current = Some(el.clone());
                    f.current_x = ex;
                }
            });

            // Cut the region.
            add_undo(UndoType::Cut, None);
            set_cutbuffer(None);
            extract_segment(sl.clone(), sx, el.clone(), ex);
            update_undo(UndoType::Cut);

            // Trim leading part from first cutbuffer line and replace with primary_lead.
            let cb = get_cutbuffer();
            if let Some(ref cb_line) = cb {
                let cb_data = cb_line.borrow().data.clone();
                let cb_quot = quote_length(&cb_data);
                let cb_fore = cb_quot + indent_length(&cb_data[cb_quot..]);
                let mut new_data = primary_lead.clone();
                new_data.push_str(&cb_data[cb_fore..]);
                cb_line.borrow_mut().data = new_data;

                // Justify the cut region.
                let mut jusline = cb_line.clone();
                concat_paragraph(&jusline, linecount);
                squeeze(&jusline, primary_len);
                rewrap_paragraph(&mut jusline, &secondary_lead, secondary_len);

                // If region started in middle of line, prepend an empty line.
                if sx > 0 {
                    let empty = state_mut().lines.alloc(LineNode {
                        data: LineData::empty(),
                        lineno: 0,
                        next: None,
                        prev: None,
                        #[cfg(feature = "color")]
                        multidata: Vec::new(),
                        has_anchor: false,
                    });
                    // Link empty before cutbuffer.
                    let cur_cb = get_cutbuffer().unwrap();
                    empty.borrow_mut().next = Some(cur_cb.clone());
                    cur_cb.borrow_mut().prev = Some(LinePtr::downgrade(&empty));
                    set_cutbuffer(Some(empty));
                }

                // If region ended in middle of line, append lead-only line.
                if ex > 0 && before_eol {
                    let trail = state_mut().lines.alloc(LineNode {
                        data: primary_lead.clone(),
                        lineno: 0,
                        next: None,
                        prev: None,
                        #[cfg(feature = "color")]
                        multidata: Vec::new(),
                        has_anchor: false,
                    });
                    // Append trail after jusline.
                    jusline.borrow_mut().next = Some(trail.clone());
                    trail.borrow_mut().prev = Some(LinePtr::downgrade(&jusline));
                }
            }

            // Wipe inherited anchor.
            {
                let ft = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone()));
                if let Some(ref ft) = ft {
                    #[cfg(not(feature = "tiny"))]
                    {
                        ft.borrow_mut().has_anchor = false;
                    }
                }
            }

            add_undo(UndoType::Paste, None);
            let cb_final = get_cutbuffer();
            if let Some(cb_line) = cb_final {
                ingraft_buffer(cb_line);
            }
            update_undo(UndoType::Paste);

            set_cutbuffer(was_cutbuffer);

            // After backward-marked justification, swap mark and cursor.
            if marked_backward {
                let bottom = with_state(|s| {
                    s.openfile
                        .as_ref()
                        .and_then(|f| f.current.clone())
                        .expect("a current line")
                });
                let bottom_x =
                    with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                let mark =
                    with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.clone()).unwrap());
                let mark_x = with_state(|s| s.openfile.as_ref().map(|f| f.mark_x).unwrap_or(0));
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(mark.clone());
                        f.current_x = mark_x;
                        f.mark = Some(bottom);
                        f.mark_x = bottom_x;
                    }
                });
            } else {
                state_mut().focusing = false;
            }

            add_undo(UndoType::CoupleEnd, Some("justification"));
            statusline(MessageType::Remark, tr!("Justified selection"));

            let new_placewewant = xplustabs();
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.placewewant = new_placewewant;
                }
            });

            set_modified();
            with_state_mut(|s| {
                s.refresh_needed = true;
                s.shift_held = true;
            });
            return;
        }
        // Under NANO_TINY there is no mark support: has_mark is always
        // false, so this branch is never taken; satisfy the compiler.
        #[cfg(feature = "tiny")]
        {
            if let Some((sl, sx, el, ex)) = prepare_justify_region(whole_buffer, &mut linecount) {
                startline = sl;
                start_x = sx;
                endline = el;
                end_x = ex;
            } else {
                return;
            }
        }
    } else {
        let Some((sl, sx, el, ex)) = prepare_justify_region(whole_buffer, &mut linecount) else {
            // No paragraph from the cursor to EOF: C just leaves the cursor
            // at the end of the last line and skips the justification.
            return;
        };
        startline = sl;
        start_x = sx;
        endline = el;
        end_x = ex;
    }

    // Non-marked path.
    do_justify_buffer(
        whole_buffer,
        startline.clone(),
        start_x,
        endline.clone(),
        end_x,
        &mut linecount,
    );

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::CoupleEnd, Some("justification"));

    set_cutbuffer(was_cutbuffer);

    #[cfg(not(feature = "tiny"))]
    if !has_mark && whole_buffer {
        goto_line_posx(was_the_linenumber, 0);
    }

    if whole_buffer {
        statusline(MessageType::Remark, tr!("Justified file"));
    } else {
        statusbar(tr!("Justified paragraph"));
    }

    let new_placewewant = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.placewewant = new_placewewant;
        }
    });

    set_modified();
    with_state_mut(|s| {
        s.refresh_needed = true;
        s.shift_held = true;
    });
}

/// Resolve the marked region to line pointers.  Returns None (after
/// reporting on the status bar) when a lineno doesn't resolve — that
/// would mean the mark/undo bookkeeping has become inconsistent, where
/// C would dereference a stray pointer.
fn get_region_as_lines() -> Option<(LinePtr, usize, LinePtr, usize)> {
    let (top_lineno, top_x, bot_lineno, bot_x) = get_region();
    match (
        get_line_from_number(top_lineno as isize),
        get_line_from_number(bot_lineno as isize),
    ) {
        (Some(tl), Some(bl)) => Some((tl, top_x, bl, bot_x)),
        _ => {
            statusline(
                MessageType::Alert,
                "Internal error: region refers to a nonexistent line",
            );
            None
        }
    }
}

#[cfg(feature = "justify")]
fn prepare_justify_region(
    whole_buffer: bool,
    linecount: &mut usize,
) -> Option<(LinePtr, usize, LinePtr, usize)> {
    if whole_buffer {
        let filetop = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filetop.clone())
                .expect("a top line")
        });
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current = Some(filetop.clone());
            }
        });
    } else {
        let cur = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.current.clone())
                .expect("a current line")
        });
        if inpar_fn(&cur) && !begpar_fn(&cur, 0) {
            let first = do_para_begin(cur.clone());
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current = Some(first);
                }
            });
        }
    }

    let cur = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.clone())
            .expect("a current line")
    });
    let mut firstline = cur.clone();

    if !find_paragraph(&mut firstline, linecount) {
        let filebot_end = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filebot.as_ref())
                .map(|b| b.borrow().data.len())
                .unwrap_or(0)
        });
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current_x = filebot_end;
            }
        });
        #[cfg(not(feature = "tiny"))]
        {
            let undotop_next = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.undotop.as_deref())
                    .and_then(|u| u.next.as_deref())
                    .map(|b| b as *const UndoStruct)
                    .unwrap_or(std::ptr::null())
            });
            discard_until(undotop_next);
        }
        state_mut().refresh_needed = true;
        // Nothing to justify: the caller must stop here (C returns without
        // running the justification or touching the undo stack further).
        return None;
    }

    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current = Some(firstline.clone());
            f.current_x = 0;
        }
    });

    let startline = firstline.clone();
    let start_x = 0usize;

    let (endline, end_x) = if whole_buffer {
        let fb = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filebot.clone())
                .expect("a bottom line")
        });
        (fb, 0usize)
    } else {
        let mut el = startline.clone();
        for _ in 1..*linecount {
            let next = el.borrow().next.clone();
            if let Some(nx) = next {
                el = nx;
            }
        }
        let next = el.borrow().next.clone();
        if let Some(nx) = next {
            (nx, 0usize)
        } else {
            let end = el.borrow().data.len();
            (el, end)
        }
    };

    Some((startline, start_x, endline, end_x))
}

#[cfg(feature = "justify")]
fn do_justify_buffer(
    whole_buffer: bool,
    startline: LinePtr,
    start_x: usize,
    endline: LinePtr,
    end_x: usize,
    linecount: &mut usize,
) {
    let was_cutbuffer = get_cutbuffer();
    set_cutbuffer(None);

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::Cut, None);

    extract_segment(startline.clone(), start_x, endline.clone(), end_x);

    #[cfg(not(feature = "tiny"))]
    update_undo(UndoType::Cut);

    // Justify the cutbuffer.
    let cb = get_cutbuffer();
    if let Some(ref cb_line) = cb {
        let mut jusline = cb_line.clone();
        justify_paragraph(&mut jusline, *linecount);

        if whole_buffer {
            while find_paragraph(&mut jusline, linecount) {
                justify_paragraph(&mut jusline, *linecount);
                if jusline.borrow().next.is_none() {
                    break;
                }
            }
        }
    }

    #[cfg(not(feature = "tiny"))]
    {
        // Wipe inherited anchor on first paragraph.
        if whole_buffer {
            let has_anchor = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.current.as_ref())
                    .map(|c| {
                        #[cfg(not(feature = "tiny"))]
                        {
                            c.borrow().has_anchor
                        }
                        #[cfg(feature = "tiny")]
                        {
                            false
                        }
                    })
                    .unwrap_or(false)
            });
            if !has_anchor {
                if let Some(cb) = get_cutbuffer() {
                    #[cfg(not(feature = "tiny"))]
                    {
                        cb.borrow_mut().has_anchor = false;
                    }
                }
            }
        }
        add_undo(UndoType::Paste, None);
    }

    let cb = get_cutbuffer();
    if let Some(cb_line) = cb {
        ingraft_buffer(cb_line);
    }

    #[cfg(not(feature = "tiny"))]
    update_undo(UndoType::Paste);

    set_cutbuffer(was_cutbuffer);
}

/* C: void do_justify(void) */
#[cfg(feature = "justify")]
pub fn do_justify() {
    justify_text(false);
}

/* C: void do_full_justify(void) */
#[cfg(feature = "justify")]
pub fn do_full_justify() {
    justify_text(true);
    state_mut().ran_a_tool = true;
    #[cfg(feature = "color")]
    with_state_mut(|s| s.recook = true);
}

// ---------------------------------------------------------------------------
// Section 13: External tool helpers (SPELLER, FORMATTER, LINTER)
// ---------------------------------------------------------------------------

/* C: void construct_argument_list(char ***arguments, char *command, char *filename) */
#[cfg(any(feature = "speller", feature = "linter", feature = "formatter"))]
pub fn construct_argument_list(command: &str, filename: &str) -> Vec<String> {
    let mut args: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
    args.push(filename.to_string());
    args
}

/* C: bool replace_buffer(const char *filename, undo_type action, const char *operation) */
#[cfg(all(
    unix,
    any(not(feature = "tiny"), feature = "speller", feature = "formatter")
))]
pub fn replace_buffer(filename: &str, action: UndoType, operation: &str) -> bool {
    let replacement = match std::fs::read(filename) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::CoupleBegin, Some(operation));

    if action == UndoType::CutToEof {
        let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current = filetop;
                f.current_x = 0;
            }
        });
    }

    let was_cutbuffer = get_cutbuffer();
    set_cutbuffer(None);

    #[cfg(not(feature = "tiny"))]
    add_undo(action, None);

    let has_mark = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.mark.is_some())
            .unwrap_or(false)
    });
    do_snip(has_mark, !has_mark, false);

    #[cfg(not(feature = "tiny"))]
    update_undo(action);

    let new_cut = get_cutbuffer();
    free_lines(new_cut);
    set_cutbuffer(was_cutbuffer);

    // The complete replacement was read before mutating the buffer, so no
    // filesystem error can expose a cut-without-insert intermediate state.
    if !read_file(std::io::Cursor::new(replacement), true, filename, true) {
        return false;
    }

    #[cfg(not(feature = "tiny"))]
    add_undo(UndoType::CoupleEnd, Some(operation));

    true
}

/* Windows stub for replace_buffer */
#[cfg(all(
    not(unix),
    any(not(feature = "tiny"), feature = "speller", feature = "formatter")
))]
pub fn replace_buffer(_filename: &str, _action: UndoType, _operation: &str) -> bool {
    false // Not supported on Windows
}

/* C: void treat(char *tempfile_name, char *theprogram, bool spelling) */
#[cfg(any(feature = "speller", feature = "formatter"))]
pub fn treat(tempfile_name: &str, theprogram: &str, spelling: bool) {
    use std::fs;
    use std::process::Command;

    if cfg!(not(unix)) {
        let tool = if spelling { "speller" } else { "formatter" };
        statusline(
            MessageType::Alert,
            &format!(
                "External {} execution is not supported on this platform",
                tool
            ),
        );
        return;
    }

    let was_lineno = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.as_ref())
            .map(|c| c.borrow().lineno)
            .unwrap_or(0)
    });
    let was_pww = with_state(|s| s.openfile.as_ref().map(|f| f.placewewant).unwrap_or(0));
    let mut was_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
    let was_at_eol = with_state(|s| {
        let of = s.openfile.as_ref()?;
        let cur = of.current.as_ref()?;
        let data = cur.borrow().data.clone();
        let cx = of.current_x;
        Some(cx >= data.len())
    })
    .unwrap_or(false);

    // Stat the temp file.
    let meta = fs::metadata(tempfile_name).ok();
    if let Some(ref m) = meta {
        if m.len() == 0 {
            #[cfg(not(feature = "tiny"))]
            let in_mark = with_state(|s| {
                s.openfile
                    .as_ref()
                    .map(|f| f.mark.is_some())
                    .unwrap_or(false)
            });
            #[cfg(not(feature = "tiny"))]
            if spelling && in_mark {
                statusline(MessageType::Ahem, tr!("Selection is empty"));
            } else {
                statusline(MessageType::Ahem, tr!("Buffer is empty"));
            }
            #[cfg(feature = "tiny")]
            statusline(MessageType::Ahem, tr!("Buffer is empty"));
            return;
        }
    }

    let timestamp_sec = meta
        .as_ref()
        .map(|m| {
            use std::time::UNIX_EPOCH;
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        })
        .flatten();
    let timestamp_nsec = meta
        .as_ref()
        .map(|m| {
            use std::time::UNIX_EPOCH;
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.subsec_nanos())
        })
        .flatten();

    if spelling {
        // Leave terminal raw mode for interactive spell checker.
        let _ = crossterm::terminal::disable_raw_mode();
    } else {
        statusbar(tr!("Invoking formatter..."));
    }

    let args = construct_argument_list(theprogram, tempfile_name);

    // Fork and exec.
    block_sigwinch(true);
    let status = Command::new(&args[0]).args(&args[1..]).status();
    block_sigwinch(false);

    if spelling {
        terminal_init();
        doupdate();
        #[cfg(not(feature = "tiny"))]
        {
            let resized = state().the_window_resized;
            if resized {
                regenerate_screen();
            }
        }
    } else {
        full_refresh();
    }

    match status {
        Err(e) => {
            statusline(MessageType::Alert, &format!(tr!("Could not fork: {}"), e));
            return;
        }
        Ok(s) => {
            // C: if (!WIFEXITED(status) || WEXITSTATUS(status) > 2) -> "Error invoking" + return;
            //    else if (WEXITSTATUS(status) != 0) -> "Program complained".
            // A process killed by a signal has no exit code (code()==None) and must
            // take the error-and-return path, not fall through to read the temp file.
            let exited_normally = s.code().is_some();
            let code = s.code().unwrap_or(-1);
            if !exited_normally || code > 2 {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Error invoking '{}'"), args[0]),
                );
                return;
            } else if code != 0 {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Program '{}' complained"), args[0]),
                );
            }
        }
    }

    // Check if the temp file changed.
    if let (Some(ts), Some(tn)) = (timestamp_sec, timestamp_nsec) {
        if let Ok(new_meta) = fs::metadata(tempfile_name) {
            use std::time::UNIX_EPOCH;
            let new_sec = new_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let new_nsec = new_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            if ts > 0 && new_sec == ts && new_nsec == tn {
                statusline(MessageType::Remark, tr!("Nothing changed"));
                return;
            }
        }
    }

    let replaced;
    #[cfg(not(feature = "tiny"))]
    {
        let in_mark = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.mark.is_some())
                .unwrap_or(false)
        });
        if spelling && in_mark {
            let was_mark_lineno = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.mark.as_ref())
                    .map(|m| m.borrow().lineno)
                    .unwrap_or(0)
            });
            let upright = mark_is_before_cursor();

            replaced = replace_buffer(tempfile_name, UndoType::Cut, "spelling correction");

            if upright {
                was_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
            } else {
                let new_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.mark_x = new_x;
                    }
                });
            }

            let mark_line = get_line_from_number(was_mark_lineno);
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.mark = mark_line;
                }
            });
        } else {
            let op = if spelling {
                "spelling correction"
            } else {
                "formatting"
            };
            replaced = replace_buffer(tempfile_name, UndoType::CutToEof, op);
        }
    }
    #[cfg(feature = "tiny")]
    {
        replaced = replace_buffer(
            tempfile_name,
            UndoType::CutToEof,
            if spelling {
                "spelling correction"
            } else {
                "formatting"
            },
        );
    }

    let _cur_x_now = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
    goto_line_posx(was_lineno, was_x);

    let cur_data_len = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.as_ref())
            .map(|c| c.borrow().data.len())
            .unwrap_or(0)
    });
    if was_at_eol {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current_x = cur_data_len;
            }
        });
    } else {
        let cur_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
        if cur_x > cur_data_len {
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current_x = cur_data_len;
                }
            });
        }
    }

    if replaced {
        #[cfg(not(feature = "tiny"))]
        {
            let _filetop_anchor = {
                let ft = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
                if let Some(ref ft) = ft {
                    ft.borrow_mut().has_anchor = false;
                }
            };
            update_undo(UndoType::CoupleEnd);
        }
    }

    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.placewewant = was_pww;
        }
    });
    adjust_viewport(UpdateType::Stationary);

    if !replaced {
        statusline(MessageType::Alert, tr!("Could not apply tool output"));
    } else if spelling {
        statusline(MessageType::Remark, tr!("Finished checking spelling"));
    } else {
        statusline(MessageType::Remark, tr!("Buffer has been processed"));
    }
}

// ---------------------------------------------------------------------------
// Section 14: Spell checker (ENABLE_SPELLER)
// ---------------------------------------------------------------------------

/* C: bool fix_spello(const char *word) */
#[cfg(feature = "speller")]
pub fn fix_spello(word: &str) -> bool {
    let was_edittop = with_state(|s| s.openfile.as_ref().and_then(|f| f.edittop.clone()));
    let was_current = with_state(|s| {
        s.openfile
            .as_ref()
            .and_then(|f| f.current.clone())
            .expect("a current line")
    });
    let was_firstcolumn = with_state(|s| s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0));
    let was_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
    let mut proceed = false;

    #[cfg(not(feature = "tiny"))]
    {
        let in_mark = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.mark.is_some())
                .unwrap_or(false)
        });
        if in_mark {
            let Some((top, top_x, bot, bot_x)) = get_region_as_lines_coords() else {
                return false;
            };
            let right_side_up = mark_is_before_cursor();
            if right_side_up {
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(top.clone());
                        f.current_x = top_x;
                        f.mark = Some(bot.clone());
                        f.mark_x = bot_x;
                    }
                });
            }
        } else {
            let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current = filetop;
                    f.current_x = 0;
                }
            });
        }
    }
    #[cfg(feature = "tiny")]
    {
        let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current = filetop;
                f.current_x = 0;
            }
        });
    }

    let result = findnextstr(word, true, INREGION, None, false, None, 0);

    if result == 0 {
        statusline(
            MessageType::Alert,
            &format!(tr!("Unfindable word: {}"), word),
        );
        state_mut().lastmessage = MessageType::Vacuum;
        proceed = true;
        napms(2800);
    } else if result == 1 {
        state_mut().spotlighted = true;
        let col_start = xplustabs();
        let col_end = col_start + breadth(word);
        with_state_mut(|s| {
            s.light_from_col = col_start;
            s.light_to_col = col_end;
        });

        #[cfg(not(feature = "tiny"))]
        let saved_mark = with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.clone()));
        #[cfg(not(feature = "tiny"))]
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.mark = None;
            }
        });

        edit_refresh();
        put_cursor_at_end_of_answer();

        let result2 = crate::prompt::do_prompt(
            MSPELL,
            Some(word),
            None,
            Some(edit_refresh),
            tr!("Edit a replacement"),
        );
        proceed = result2 != -1;

        state_mut().spotlighted = false;

        #[cfg(not(feature = "tiny"))]
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.mark = saved_mark;
            }
        });

        let answer = state().answer.clone();
        if proceed && word != answer {
            let mut was_x_mut = was_x;
            do_replace_loop(word, true, &was_current, &mut was_x_mut);
            statusbar(tr!("Next word..."));
            napms(400);
        }
    }

    // Restore cursor position and viewport.
    #[cfg(not(feature = "tiny"))]
    {
        let in_mark = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.mark.is_some())
                .unwrap_or(false)
        });
        if in_mark {
            // Restore mark and cursor.
            let right_side_up = mark_is_before_cursor();
            let Some((top, top_x, _bot, _bot_x)) = get_region_as_lines_coords() else {
                return proceed;
            };
            if right_side_up {
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(
                            f.mark
                                .as_ref()
                                .map(|m| m.clone())
                                .unwrap_or(was_current.clone()),
                        );
                        f.current_x = f.mark_x;
                        f.mark = Some(top.clone());
                        f.mark_x = top_x;
                    }
                });
            } else {
                with_state_mut(|s| {
                    if let Some(ref mut f) = s.openfile {
                        f.current = Some(top.clone());
                        f.current_x = top_x;
                    }
                });
            }
        } else {
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current = Some(was_current.clone());
                    f.current_x = was_x;
                }
            });
        }
    }
    #[cfg(feature = "tiny")]
    {
        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.current = Some(was_current.clone());
                f.current_x = was_x;
            }
        });
    }

    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.edittop = was_edittop;
            f.firstcolumn = was_firstcolumn;
        }
    });

    proceed
}

/// Helper: get region as (top_line, top_x, bot_line, bot_x).
#[cfg(feature = "speller")]
fn get_region_as_lines_coords() -> Option<(LinePtr, usize, LinePtr, usize)> {
    get_region_as_lines()
}

/* C: void spell_check(const char *tempfile_name) */
#[cfg(feature = "speller")]
pub fn spell_check(tempfile_name: &str) {
    use std::process::{Command, Stdio};

    if cfg!(not(unix)) {
        statusline(
            MessageType::Alert,
            "External spell checking is not supported on this platform",
        );
        return;
    }

    statusbar(tr!("Invoking spell checker..."));

    // Run: cat tempfile | hunspell -l | sort -f | uniq
    // (C reports tempfile/pipe failures via statusline ALERT and aborts.)
    let hunspell_output = match std::fs::File::open(tempfile_name) {
        Ok(f) => Command::new("hunspell")
            .arg("-l")
            .stdin(Stdio::from(f))
            .output(),
        Err(e) => {
            statusline(
                MessageType::Alert,
                &format!(tr!("Error invoking \"hunspell\": {}"), e),
            );
            return;
        }
    };

    let spell_output = match hunspell_output {
        Ok(o) if o.status.success() || o.status.code().unwrap_or(-1) <= 1 => o.stdout,
        _ => {
            // Fall back to 'spell'.  The tempfile can legitimately have
            // vanished between the two opens, so check again.
            let infile = match std::fs::File::open(tempfile_name) {
                Ok(f) => f,
                Err(e) => {
                    statusline(
                        MessageType::Alert,
                        &format!(tr!("Error invoking \"spell\": {}"), e),
                    );
                    return;
                }
            };
            match Command::new("spell").stdin(Stdio::from(infile)).output() {
                Ok(o) => o.stdout,
                Err(e) => {
                    statusline(
                        MessageType::Alert,
                        &format!(tr!("Error invoking \"spell\": {}"), e),
                    );
                    return;
                }
            }
        }
    };

    // Sort the misspelled words.
    let sort_output = {
        use std::io::Write;
        let mut sort_proc = match Command::new("sort")
            .arg("-f")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(p) => p,
            Err(e) => {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Error invoking \"sort\": {}"), e),
                );
                return;
            }
        };
        if let Some(ref mut stdin) = sort_proc.stdin {
            let _ = stdin.write_all(&spell_output);
        }
        match sort_proc.wait_with_output() {
            Ok(o) => o.stdout,
            Err(e) => {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Error reading from sort: {}"), e),
                );
                return;
            }
        }
    };

    // Unique the sorted list.
    let uniq_output = {
        use std::io::Write;
        let mut uniq_proc = match Command::new("uniq")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(p) => p,
            Err(e) => {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Error invoking \"uniq\": {}"), e),
                );
                return;
            }
        };
        if let Some(ref mut stdin) = uniq_proc.stdin {
            let _ = stdin.write_all(&sort_output);
        }
        match uniq_proc.wait_with_output() {
            Ok(o) => o.stdout,
            Err(e) => {
                statusline(
                    MessageType::Alert,
                    &format!(tr!("Error reading from uniq: {}"), e),
                );
                return;
            }
        }
    };

    let misspellings = String::from_utf8_lossy(&uniq_output);

    // Save/restore flag states for case-sensitive forward non-regex search.
    let stash = state().flags;
    SET!(CASE_SENSITIVE);
    UNSET!(BACKWARDS_SEARCH);
    UNSET!(USE_REGEXP);

    for word in misspellings.split(|c| c == '\r' || c == '\n') {
        if word.is_empty() {
            continue;
        }
        if !fix_spello(word) {
            break;
        }
    }

    state_mut().flags = stash;
    state_mut().refresh_needed = true;
    statusline(MessageType::Remark, tr!("Finished checking spelling"));
}

/* C: void do_spell(void) */
#[cfg(feature = "speller")]
pub fn do_spell() {
    state_mut().ran_a_tool = true;

    if in_restricted_mode() {
        return;
    }

    let (temp_name, _stream) = match crate::files::safe_tempfile() {
        Some(pair) => pair,
        None => {
            statusline(
                MessageType::Alert,
                tr!("Error writing temp file: cannot create"),
            );
            return;
        }
    };

    let okay;
    #[cfg(not(feature = "tiny"))]
    {
        let in_mark = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.mark.is_some())
                .unwrap_or(false)
        });
        if in_mark {
            okay = write_region_to_file_to(&temp_name, TEMPORARY);
        } else {
            okay = write_file_to(&temp_name, TEMPORARY);
        }
    }
    #[cfg(feature = "tiny")]
    {
        okay = write_file_to(&temp_name, TEMPORARY);
    }

    if !okay {
        statusline(MessageType::Alert, tr!("Error writing temp file"));
        let _ = std::fs::remove_file(&temp_name);
        return;
    }

    blank_bottombars();

    let alt_speller = with_state(|s| {
        #[cfg(feature = "speller")]
        {
            s.alt_speller.clone()
        }
        #[cfg(not(feature = "speller"))]
        {
            None::<String>
        }
    });

    if let Some(ref speller) = alt_speller {
        if !speller.is_empty() {
            treat(&temp_name, speller, true);
        } else {
            spell_check(&temp_name);
        }
    } else {
        spell_check(&temp_name);
    }

    let _ = std::fs::remove_file(&temp_name);

    with_state_mut(|s| {
        s.currmenu = MMOST;
        s.shift_held = true;
    });
}

// ---------------------------------------------------------------------------
// Section 15: Linter (ENABLE_LINTER)
// ---------------------------------------------------------------------------

/* C: void do_linter(void) */
#[cfg(feature = "linter")]
pub fn do_linter() {
    use std::process::{Command, Stdio};

    state_mut().ran_a_tool = true;

    if in_restricted_mode() {
        return;
    }

    let linter_info = with_state(|s| {
        let f = s.openfile.as_ref()?;
        #[cfg(feature = "color")]
        {
            let syn = f.syntax.as_ref()?;
            let syn_ref = unsafe { &**syn };
            let linter = syn_ref.linter.as_ref()?.clone();
            if linter.is_empty() {
                return None;
            }
            Some((linter, f.filename.clone()))
        }
        #[cfg(not(feature = "color"))]
        None::<(String, String)>
    });

    let (linter_cmd, filename) = match linter_info {
        Some(pair) => pair,
        None => {
            statusline(
                MessageType::Ahem,
                tr!("No linter is defined for this type of file"),
            );
            return;
        }
    };

    if linter_cmd.is_empty() {
        statusline(
            MessageType::Ahem,
            tr!("No linter is defined for this type of file"),
        );
        return;
    }

    #[cfg(not(feature = "tiny"))]
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.mark = None;
        }
    });

    edit_refresh();

    let modified = with_state(|s| s.openfile.as_ref().map(|f| f.modified).unwrap_or(false));
    if modified {
        let choice = ask_user(YESORNO, tr!("Save modified buffer before linting?"));
        if choice == CANCEL {
            statusbar(tr!("Cancelled"));
            return;
        } else if choice == YES && write_it_out(false, false) != 1 {
            return;
        }
    }

    blank_bottombars();
    state_mut().currmenu = MLINTER;
    statusbar(tr!("Invoking linter..."));

    let args = construct_argument_list(&linter_cmd, &filename);
    let output = match Command::new(&args[0])
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            statusline(MessageType::Alert, &format!(tr!("Could not fork: {}"), e));
            return;
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let combined = {
        let mut s = output.stdout.clone();
        s.extend_from_slice(&output.stderr);
        String::from_utf8_lossy(&s).to_string()
    };

    if exit_code > 2 {
        statusline(
            MessageType::Alert,
            &format!(tr!("Error invoking '{}'"), args[0]),
        );
        return;
    }

    // Parse linter output: "filename:line:col: message" or "filename:line:col,col: message".
    let mut lints: Vec<LintEntry> = Vec::new();
    for line in combined.lines() {
        if let Some(entry) = parse_lint_line(line) {
            lints.push(entry);
        }
    }

    if lints.is_empty() {
        statusline(
            MessageType::Remark,
            &format!(tr!("Got 0 parsable lines from command: {}"), linter_cmd),
        );
        return;
    }

    let helpless = ISSET!(NO_HELP);
    let editwinrows = state().editwinrows;
    if helpless && editwinrows > 5 {
        UNSET!(NO_HELP);
        window_init();
    }

    titlebar(None);
    bottombars(MLINTER);

    let mut cur_idx = 0usize;
    let mut last_shown: Option<usize> = None;
    let mut last_wait: Option<std::time::Instant> = None;

    loop {
        if cur_idx >= lints.len() {
            break;
        }

        // When the message is for a different file, switch to (or open)
        // that file's buffer first (C: the openfile->next walk in do_linter).
        #[cfg(feature = "multibuffer")]
        if last_shown != Some(cur_idx) {
            let entry_filename = lints[cur_idx].filename.clone();
            let current_name = with_state(|s| {
                s.openfile
                    .as_ref()
                    .map(|f| f.filename.clone())
                    .unwrap_or_default()
            });
            if !entry_filename.is_empty() && entry_filename != current_name {
                if !crate::files::rotate_to_buffer_named(&entry_filename) {
                    let choice = ask_user(
                        false,
                        &format!(
                            tr!("This message is for unopened file {}, open it in a new buffer?"),
                            entry_filename
                        ),
                    );
                    state_mut().currmenu = MLINTER;
                    if choice == CANCEL {
                        statusbar(tr!("Cancelled"));
                        break;
                    } else if choice == YES {
                        crate::files::open_buffer_impl(&entry_filename, true);
                    } else {
                        // C: drop ALL messages for the declined file (not just the
                        // consecutive run), then resume at the first remaining one.
                        lints.retain(|l| l.filename != entry_filename);
                        if lints.is_empty() {
                            statusline(MessageType::Remark, tr!("No messages for this file"));
                            break;
                        }
                        cur_idx = 0;
                        last_shown = None;
                        continue;
                    }
                }
            }
        }

        let entry = &lints[cur_idx];

        // Navigate to the lint location.
        if last_shown != Some(cur_idx) {
            goto_line_posx(entry.lineno, (entry.colno - 1).max(0) as usize);
            let new_x = with_state(|s| {
                let of = s.openfile.as_ref()?;
                let cur = of.current.as_ref()?;
                let data = cur.borrow().data.clone();
                Some(actual_x(&data, of.placewewant))
            })
            .unwrap_or(0);
            with_state_mut(|s| {
                if let Some(ref mut f) = s.openfile {
                    f.current_x = new_x;
                }
            });
            titlebar(None);
            adjust_viewport(UpdateType::Centering);
            #[cfg(feature = "linenumbers")]
            confirm_margin();
            edit_refresh();
            statusline(MessageType::Notice, &entry.msg);
            bottombars(MLINTER);
            last_shown = Some(cur_idx);
        }

        place_the_cursor();
        wnoutrefresh();

        let kbinput = get_kbinput(VISIBLE);

        #[cfg(not(feature = "tiny"))]
        {
            if crate::winio::consume_resize_request(Some(kbinput)) {
                // Repaint the current lint entry against the rebuilt windows
                // instead of drawing into stale dimensions.
                last_shown = None;
                continue;
            }
        }

        let function = crate::global::func_from_key(kbinput);

        if function == Some(crate::global::do_cancel as crate::definitions::FuncPtr)
            || function == Some(do_enter as crate::definitions::FuncPtr)
        {
            wipe_statusbar();
            break;
        } else if function == Some(crate::global::do_help as crate::definitions::FuncPtr) {
            last_shown = None;
            crate::help::do_help();
        } else if function == Some(crate::global::do_page_up as crate::definitions::FuncPtr)
            || function == Some(crate::global::to_prev_block as crate::definitions::FuncPtr)
        {
            if cur_idx > 0 {
                cur_idx -= 1;
                last_shown = None;
            } else {
                let now = std::time::Instant::now();
                let should_beep = last_wait
                    .map(|lw| now.duration_since(lw).as_secs() >= 1)
                    .unwrap_or(true);
                if should_beep {
                    statusbar(tr!("At first message"));
                    beep();
                    napms(600);
                    last_wait = Some(now);
                    statusline(MessageType::Notice, &entry.msg);
                }
            }
        } else if function == Some(crate::global::do_page_down as crate::definitions::FuncPtr)
            || function == Some(crate::global::to_next_block as crate::definitions::FuncPtr)
        {
            if cur_idx + 1 < lints.len() {
                cur_idx += 1;
                last_shown = None;
            } else {
                let now = std::time::Instant::now();
                let should_beep = last_wait
                    .map(|lw| now.duration_since(lw).as_secs() >= 1)
                    .unwrap_or(true);
                if should_beep {
                    statusbar(tr!("At last message"));
                    beep();
                    napms(600);
                    last_wait = Some(now);
                    statusline(MessageType::Notice, &entry.msg);
                }
            }
        } else {
            beep();
        }
    }

    if helpless {
        SET!(NO_HELP);
        window_init();
        state_mut().refresh_needed = true;
    }

    with_state_mut(|s| {
        s.lastmessage = MessageType::Vacuum;
        s.currmenu = MMOST;
    });
    titlebar(None);
}

/// One parsed lint diagnostic.
#[cfg(feature = "linter")]
struct LintEntry {
    filename: String,
    lineno: isize,
    colno: isize,
    msg: String,
}

/// Parse one line of linter output in the format "filename:line:col: message".
#[cfg(feature = "linter")]
fn parse_lint_line(line: &str) -> Option<LintEntry> {
    let (location, msg) = line.split_once(": ")?;

    fn split_number(text: &str) -> Option<(&str, isize)> {
        let separator = text.rfind(':')?;
        let number = text[separator + 1..].split(',').next()?.parse().ok()?;
        Some((&text[..separator], number))
    }

    let (before_last, last_number) = split_number(location)?;
    let (filename, lineno, colno) = match split_number(before_last) {
        Some((filename, line_number)) => (filename, line_number, last_number),
        None => (before_last, last_number, 1),
    };
    if lineno <= 0 {
        return None;
    }
    let colno = if colno <= 0 { 1 } else { colno };

    Some(LintEntry {
        filename: filename.to_string(),
        lineno,
        colno,
        msg: msg.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Section 16: Formatter (ENABLE_FORMATTER)
// ---------------------------------------------------------------------------

/* C: void do_formatter(void) */
#[cfg(feature = "formatter")]
pub fn do_formatter() {
    state_mut().ran_a_tool = true;

    if in_restricted_mode() {
        return;
    }

    let formatter_cmd = with_state(|s| {
        s.openfile.as_ref().and_then(|f| {
            #[cfg(feature = "color")]
            {
                let syn = f.syntax.as_ref()?;
                let syn_ref = unsafe { &**syn };
                syn_ref.formatter.clone()
            }
            #[cfg(not(feature = "color"))]
            {
                None::<String>
            }
        })
    });

    let formatter_cmd = match formatter_cmd {
        Some(cmd) if !cmd.is_empty() => cmd,
        _ => {
            statusline(
                MessageType::Ahem,
                tr!("No formatter is defined for this type of file"),
            );
            return;
        }
    };

    #[cfg(not(feature = "tiny"))]
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.mark = None;
        }
    });

    let (temp_name, _stream) = match crate::files::safe_tempfile() {
        Some(pair) => pair,
        None => {
            statusline(MessageType::Alert, tr!("Error writing temp file"));
            return;
        }
    };

    let okay = write_file_to(&temp_name, TEMPORARY);

    if !okay {
        statusline(MessageType::Alert, tr!("Error writing temp file"));
    } else {
        treat(&temp_name, &formatter_cmd, false);
    }

    let _ = std::fs::remove_file(&temp_name);
}

// ---------------------------------------------------------------------------
// Section 17: Word/character/line count (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void count_lines_words_and_characters(void) */
#[cfg(not(feature = "tiny"))]
pub fn count_lines_words_and_characters() {
    let (was_current, was_x) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        (f.current.clone().expect("a current line"), f.current_x)
    });

    let (topline, top_x, botline, bot_x, chars) = {
        let in_mark = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|f| f.mark.is_some())
                .unwrap_or(false)
        });
        if in_mark {
            let (top_lineno, tx, bot_lineno, bx) = get_region();
            let (Some(tl), Some(bl)) = (
                get_line_from_number(top_lineno as isize),
                get_line_from_number(bot_lineno as isize),
            ) else {
                statusline(
                    MessageType::Alert,
                    "Internal error: region refers to a nonexistent line",
                );
                return;
            };

            // C: if (topline != botline) chars = number_of_characters_in(topline->next, botline) + 1;
            // which equals the sum over (topline->next ..= botline) of (mbstrlen(line)+1).
            // botline itself MUST be counted, and the count is in CHARACTERS
            // (mbstrlen), not bytes — otherwise the later subtraction underflows.
            let mut char_count: usize = 0;
            if tl.as_ptr() != bl.as_ptr() {
                let mut cur = tl.borrow().next.clone();
                while let Some(ln) = cur {
                    char_count += mbstrlen(&ln.borrow().data) + 1;
                    if ln.as_ptr() == bl.as_ptr() {
                        break;
                    }
                    cur = ln.borrow().next.clone();
                }
            }

            let top_data = tl.borrow().data.clone();
            let bot_data = bl.borrow().data.clone();
            char_count += mbstrlen(&top_data[tx..]);
            if bot_lineno > top_lineno {
                char_count -= mbstrlen(&bot_data[bx..]);
            } else {
                char_count = mbstrlen(&top_data[tx..bx]);
            }

            (tl, tx, bl, bx, char_count)
        } else {
            let filetop = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.filetop.clone())
                    .expect("a top line")
            });
            let filebot = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|f| f.filebot.clone())
                    .expect("a bottom line")
            });
            let bot_x = filebot.borrow().data.len();
            let total = with_state(|s| s.openfile.as_ref().map(|f| f.totsize).unwrap_or(0));
            (filetop, 0, filebot, bot_x, total)
        }
    };

    // Compute line count.
    let lines = {
        let bot_lineno = botline.borrow().lineno;
        let top_lineno = topline.borrow().lineno;
        let diff = bot_lineno - top_lineno;
        if bot_x == 0 || (topline.as_ptr() == botline.as_ptr() && top_x == bot_x) {
            diff
        } else {
            diff + 1
        }
    };

    // Count words.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current = Some(topline.clone());
            f.current_x = top_x;
        }
    });

    let mut words: usize = 0;
    loop {
        let (cur_lineno, cur_x) = with_state(|s| {
            let f = s.openfile.as_ref().expect("an open buffer");
            let lineno = f.current.as_ref().map(|c| c.borrow().lineno).unwrap_or(0);
            (lineno, f.current_x)
        });
        let bot_lineno = botline.borrow().lineno;
        if cur_lineno > bot_lineno {
            break;
        }
        if cur_lineno == bot_lineno && cur_x >= bot_x {
            break;
        }
        if crate::move_::do_next_word(false) {
            words += 1;
        }
    }

    // Restore cursor.
    with_state_mut(|s| {
        if let Some(ref mut f) = s.openfile {
            f.current = Some(was_current.clone());
            f.current_x = was_x;
        }
    });

    let in_mark = with_state(|s| {
        s.openfile
            .as_ref()
            .map(|f| f.mark.is_some())
            .unwrap_or(false)
    });
    let prefix = if in_mark { tr!("In Selection:  ") } else { "" };

    let line_word = if lines == 1 {
        tr!("line")
    } else {
        tr!("lines")
    };
    let word_word = if words == 1 {
        tr!("word")
    } else {
        tr!("words")
    };
    let char_word = if chars == 1 {
        tr!("character")
    } else {
        tr!("characters")
    };

    statusline(
        MessageType::Info,
        &format!(
            "{}{} {},  {} {},  {} {}",
            prefix, lines, line_word, words, word_word, chars, char_word
        ),
    );
}

// ---------------------------------------------------------------------------
// Section 18: Verbatim input
// ---------------------------------------------------------------------------

/* C: void do_verbatim_input(void) */
pub fn do_verbatim_input() {
    #[cfg(not(feature = "tiny"))]
    {
        let zero_and_at_bottom = with_state(|s| {
            let editwinrows = s.editwinrows;
            let cursor_row = s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0);
            s.flags[flag_index(ZERO)] & flag_mask(ZERO) != 0
                && cursor_row == (editwinrows - 1) as isize
                && s.midwin.rows > 1
        });
        if zero_and_at_bottom {
            crate::winio::edit_scroll(true); // FORWARD = true
            edit_refresh();
        }
    }

    statusline(MessageType::Info, tr!("Verbatim Input"));
    place_the_cursor();

    let mut count: usize = 0;
    let bytes = crate::winio::get_verbatim_kbinput(&mut count);

    if count > 0 {
        let show_pos = ISSET!(CONSTANT_SHOW) || ISSET!(MINIBAR);
        if show_pos {
            state_mut().lastmessage = MessageType::Vacuum;
        }

        if count < 999 {
            inject(&bytes, count);
        }

        #[cfg(not(feature = "tiny"))]
        {
            let zero_and_main = with_state(|s| {
                s.flags[flag_index(ZERO)] & flag_mask(ZERO) != 0 && s.currmenu == MMAIN
            });
            if zero_and_main {
                // wredrawln(midwin, editwinrows - 1, 1)
                // No-op in crossterm — edit_refresh will handle it.
            } else {
                wipe_statusbar();
            }
        }
        #[cfg(feature = "tiny")]
        wipe_statusbar();
    } else {
        statusline(MessageType::Ahem, tr!("Invalid code"));
    }
}

// ---------------------------------------------------------------------------
// Section 19: Word completion (ENABLE_WORDCOMPLETION)
// ---------------------------------------------------------------------------

/* C: char *copy_completion(char *text) */
#[cfg(feature = "wordcomp")]
pub fn copy_completion<T: AsRef<[u8]> + ?Sized>(text: &T) -> LineData {
    let text = text.as_ref();
    let mut length = 0usize;
    while length < text.len() && is_word_char(&text[length..], false) {
        length = step_right(text, length);
    }
    LineData::from_internal(text[..length].to_vec())
}

// Thread-local state for complete_a_word.
#[cfg(feature = "wordcomp")]
thread_local! {
    static COMPLETIONS: RefCell<Vec<LineData>> = RefCell::new(Vec::new());
    static PLETION_X: RefCell<usize> = RefCell::new(0);
    #[cfg(feature = "multibuffer")]
    static SCOURING_ID: RefCell<usize> = RefCell::new(0);
}

/// Advance word completion to the next open buffer without rotating the
/// editor's active buffer ring.  ID zero denotes the active buffer and IDs
/// one onward map to the ring's forward order.
#[cfg(all(feature = "wordcomp", feature = "multibuffer"))]
fn next_completion_buffer_line() -> Option<LinePtr> {
    SCOURING_ID.with(|scouring| {
        let mut id = scouring.borrow_mut();
        let total = state().buffer_ring.len();

        while *id < total {
            *id += 1;
            if let Some(line) = with_state(|s| {
                s.buffer_ring
                    .get(*id - 1)
                    .and_then(|buffer| buffer.filetop.clone())
            }) {
                return Some(line);
            }
        }
        None
    })
}

/* C: void complete_a_word(void) */
#[cfg(feature = "wordcomp")]
pub fn complete_a_word() {
    #[cfg(feature = "wrapping")]
    let was_set_wrapping = ISSET!(BREAK_LONG_LINES);

    // Determine if this is a fresh attempt or continuation.
    let is_fresh = state().pletion_line.is_none();

    if is_fresh {
        // Clear previous completions.
        COMPLETIONS.with(|c| c.borrow_mut().clear());
        PLETION_X.with(|px| *px.borrow_mut() = 0);
        #[cfg(feature = "multibuffer")]
        SCOURING_ID.with(|id| *id.borrow_mut() = 0);

        with_state_mut(|s| {
            if let Some(ref mut f) = s.openfile {
                f.last_action = UndoType::Other;
            }
        });

        let filetop = with_state(|s| s.openfile.as_ref().and_then(|f| f.filetop.clone()));
        state_mut().pletion_line = filetop;

        wipe_statusbar();
    } else {
        do_undo();
    }

    // Find word fragment before cursor.
    let (cur_data, current_x) = with_state(|s| {
        let f = s.openfile.as_ref().expect("an open buffer");
        let data = f
            .current
            .as_ref()
            .map(|c| c.borrow().data.clone())
            .unwrap_or_default();
        (data, f.current_x)
    });

    let mut start_of_shard = current_x;
    loop {
        if start_of_shard == 0 {
            break;
        }
        let oneleft = step_left(&cur_data, start_of_shard);
        if !is_word_char(&cur_data[oneleft..], false) {
            break;
        }
        start_of_shard = oneleft;
    }

    if start_of_shard == current_x {
        statusline(MessageType::Ahem, tr!("No word fragment"));
        state_mut().pletion_line = None;
        return;
    }

    let shard = LineData::from_internal(cur_data[start_of_shard..current_x].to_vec());
    let shard_length = shard.len();

    // Search through all lines for a completion.
    loop {
        let pletion_line = state().pletion_line.clone();
        let pl = match pletion_line {
            Some(pl) => pl,
            None => break,
        };

        let pl_data = pl.borrow().data.clone();
        let threshold = pl_data.len().saturating_sub(shard_length) as isize;
        let pletion_x_val = PLETION_X.with(|px| *px.borrow());

        let _found_at: Option<usize> = None;
        let next_pletion_x: usize;

        // Search this line.
        let mut i = pletion_x_val;
        while (i as isize) < threshold {
            // Quick first-byte check.
            if pl_data.as_bytes().get(i) != shard.as_bytes().first() {
                i += 1;
                continue;
            }

            // Check remaining bytes.
            if pl_data.len() < i + shard_length || &pl_data[i..i + shard_length] != shard.as_bytes()
            {
                i += 1;
                continue;
            }

            let after = i + shard_length;
            // Must be longer than shard.
            if after >= pl_data.len() || !is_word_char(&pl_data[after..], false) {
                i += 1;
                continue;
            }

            // Must be at word boundary.
            if i > 0 {
                let prev = step_left(&pl_data, i);
                if is_word_char(&pl_data[prev..], false) {
                    i += 1;
                    continue;
                }
            }

            // Skip the shard itself.
            let current_x2 = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
            let is_self = with_state(|s| {
                let f = s.openfile.as_ref()?;
                let cur_ptr = f.current.as_ref()?.as_ptr();
                Some(pl.as_ptr() == cur_ptr && i == current_x2.saturating_sub(shard_length))
            })
            .unwrap_or(false);
            if is_self {
                i += 1;
                continue;
            }

            let completion = copy_completion(&pl_data[i..]);

            // Check for duplicate.
            let is_dup = COMPLETIONS.with(|c| c.borrow().iter().any(|w| w == &completion));
            if is_dup {
                i += 1;
                continue;
            }

            // Found a new completion!
            COMPLETIONS.with(|c| c.borrow_mut().push(completion.clone()));
            next_pletion_x = i + 1;

            #[cfg(feature = "wrapping")]
            UNSET!(BREAK_LONG_LINES);

            let inject_part = &completion[shard_length..];
            inject(inject_part, inject_part.len());

            #[cfg(feature = "wrapping")]
            if was_set_wrapping {
                SET!(BREAK_LONG_LINES);
                do_wrap();
            }

            PLETION_X.with(|px| *px.borrow_mut() = next_pletion_x);
            return;
        }

        // Move to next line.
        let next_line = pl.borrow().next.clone();
        state_mut().pletion_line = next_line;
        PLETION_X.with(|px| *px.borrow_mut() = 0);

        #[cfg(feature = "multibuffer")]
        {
            // When this buffer is exhausted, continue through the remaining
            // buffers in ring order, stopping before returning to the active one.
            if state().pletion_line.is_none() {
                state_mut().pletion_line = next_completion_buffer_line();
            }
        }
    }

    // No more matches.
    let has_completions = COMPLETIONS.with(|c| !c.borrow().is_empty());
    if has_completions {
        edit_refresh();
        statusline(MessageType::Ahem, tr!("No further matches"));
    } else {
        statusline(MessageType::Ahem, tr!("No matches"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "linter")]
    #[test]
    fn lint_parser_keeps_windows_drive_unc_and_space_paths() {
        let drive = parse_lint_line(r"C:\work dir\main.rs:12:7: bad token").unwrap();
        assert_eq!(drive.filename, r"C:\work dir\main.rs");
        assert_eq!((drive.lineno, drive.colno), (12, 7));
        assert_eq!(drive.msg, "bad token");

        let unc = parse_lint_line(r"\\server\share\main.rs:4: warning").unwrap();
        assert_eq!(unc.filename, r"\\server\share\main.rs");
        assert_eq!((unc.lineno, unc.colno), (4, 1));
        assert_eq!(unc.msg, "warning");
    }

    fn buffer_with_line(data: &str, current_x: usize) -> (Box<OpenFileStruct>, LinePtr) {
        let line = crate::nano::make_new_node(None);
        line.borrow_mut().data = LineData::from_utf8(data);

        let mut buffer = Box::new(OpenFileStruct::default());
        buffer.filetop = Some(line.clone());
        buffer.filebot = Some(line.clone());
        buffer.edittop = Some(line.clone());
        buffer.current = Some(line.clone());
        buffer.current_x = current_x;
        buffer.placewewant = crate::utils::wideness(data, safe_edit_boundary(data, current_x));
        buffer.totsize = mbstrlen(data);
        // Avoid terminal title updates in unit tests.
        buffer.modified = true;
        (buffer, line)
    }

    fn install_buffer(data: &str, current_x: usize) -> LinePtr {
        let (buffer, line) = buffer_with_line(data, current_x);
        with_state_mut(|s| {
            s.openfile = Some(buffer);
            s.flags[flag_index(NO_NEWLINES)] |= flag_mask(NO_NEWLINES);
            s.flags[flag_index(ZERO)] |= flag_mask(ZERO);
            s.editwincols = 80;
            s.editwinrows = 24;
        });
        line
    }

    #[cfg(not(feature = "tiny"))]
    fn install_two_line_buffer(first_data: &str, second_data: &str) -> (LinePtr, LinePtr) {
        let first = crate::nano::make_new_node(None);
        first.borrow_mut().data = LineData::from_utf8(first_data);
        let second = crate::nano::make_new_node(Some(&first));
        second.borrow_mut().data = LineData::from_utf8(second_data);
        first.borrow_mut().next = Some(second.clone());

        let mut buffer = Box::new(OpenFileStruct::default());
        buffer.filetop = Some(first.clone());
        buffer.filebot = Some(second.clone());
        buffer.edittop = Some(first.clone());
        buffer.current = Some(second.clone());
        buffer.current_x = second_data.len();
        buffer.totsize = first_data.len() + second_data.len() + 1;
        buffer.modified = true;

        with_state_mut(|s| {
            s.openfile = Some(buffer);
            s.flags[flag_index(NO_NEWLINES)] |= flag_mask(NO_NEWLINES);
            s.flags[flag_index(ZERO)] |= flag_mask(ZERO);
            s.editwincols = 80;
            s.editwinrows = 24;
        });
        (first, second)
    }

    #[cfg(not(feature = "tiny"))]
    fn current_buffer_text() -> LineData {
        let mut line = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|buffer| buffer.filetop.clone())
        });
        let mut result = LineData::empty();
        let mut first = true;
        while let Some(node) = line {
            if !first {
                result.push_byte(b'\n');
            }
            result.push_str(&node.borrow().data);
            first = false;
            line = node.borrow().next.clone();
        }
        result
    }

    #[test]
    fn inject_defends_utf8_boundaries_and_encodes_nul() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
        let line = install_buffer("éclair", 1);
        inject("X\0", 2);

        assert_eq!(line.borrow().data, "X\néclair");
        assert_eq!(state().openfile.as_ref().unwrap().current_x, 2);

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[test]
    fn inject_does_not_split_a_multibyte_burst() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
        let line = install_buffer("ok", 2);
        inject("é", 1);

        assert_eq!(line.borrow().data, "ok");
        assert_eq!(state().openfile.as_ref().unwrap().current_x, 2);

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[test]
    fn malformed_bytes_are_individual_editing_units() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);

        let bytes = [0xFF, 0xC3, 0xA9];
        assert_eq!(safe_edit_boundary(&bytes, 1), 1);
        assert_eq!(safe_edit_boundary(&bytes, 2), 1);
        assert_eq!(safe_edit_boundary_end(&bytes, 2), 3);

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn inject_undo_redo_preserves_invalid_bytes() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
        let line = install_buffer("", 0);
        let bytes = [0xFF, 0xC3];

        inject(&bytes, bytes.len());
        assert_eq!(line.borrow().data.as_bytes(), bytes);
        assert_eq!(state().openfile.as_ref().unwrap().current_x, bytes.len());

        do_undo();
        assert!(line.borrow().data.is_empty());
        do_redo();
        assert_eq!(line.borrow().data.as_bytes(), bytes);

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn inject_starts_new_undo_after_noncontiguous_cursor_move() {
        let line = install_buffer("abcd", 0);
        inject("x", 1);
        inject("y", 1);
        state_mut().openfile.as_mut().unwrap().current_x = 4;
        inject("z", 1);

        assert_eq!(line.borrow().data, "xyabzcd");
        let state_guard = state();
        let top = state_guard
            .openfile
            .as_ref()
            .unwrap()
            .undotop
            .as_ref()
            .unwrap();
        assert_eq!(top.r#type, UndoType::Add);
        assert_eq!((top.head_lineno, top.head_x), (1, 4));
        assert_eq!((top.tail_lineno, top.tail_x), (1, 5));
        let previous = top.next.as_ref().expect("separate prior ADD record");
        assert_eq!(previous.payload.as_deref(), Some(b"xy".as_slice()));
        assert!(previous.next.is_none());
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn cross_line_unicode_injection_undoes_only_the_contiguous_add() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
        let (first, second) = install_two_line_buffer("", "");

        inject("é", "é".len());
        with_state_mut(|s| {
            let buffer = s.openfile.as_mut().unwrap();
            buffer.current = Some(first.clone());
            buffer.current_x = 0;
        });
        inject("\t", 1);

        assert_eq!(first.borrow().data, "\t");
        assert_eq!(second.borrow().data, "é");
        do_undo();
        assert_eq!(first.borrow().data, "");
        assert_eq!(second.borrow().data, "é");

        do_redo();
        assert_eq!(first.borrow().data, "\t");
        assert_eq!(second.borrow().data, "é");

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn malformed_undo_byte_range_does_not_split_a_unicode_scalar() {
        let was_using_utf8 = state().using_utf8;
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
        let line = install_buffer("", 0);
        inject("é", "é".len());

        with_state_mut(|s| {
            let buffer = s.openfile.as_mut().unwrap();
            let top = buffer.undotop.as_deref_mut().unwrap();
            top.payload = Some(LineData::from_utf8("x"));
            buffer.current_undo = top as *mut UndoStruct;
        });

        do_undo();
        assert_eq!(line.borrow().data, "é");

        state_mut().using_utf8 = was_using_utf8;
        crate::chars::remember_utf8(was_using_utf8);
    }

    #[cfg(all(not(feature = "tiny"), unix))]
    #[test]
    #[cfg_attr(
        miri,
        ignore = "requires filesystem access; rerun Miri with isolation disabled"
    )]
    fn replacement_couple_is_undone_and_redone_as_one_action() {
        use std::io::Write;

        install_buffer("before", 0);
        let mut replacement = tempfile::NamedTempFile::new().unwrap();
        replacement.write_all(b"after").unwrap();
        replacement.flush().unwrap();

        assert!(replace_buffer(
            &replacement.path().to_string_lossy(),
            UndoType::CutToEof,
            "filtering",
        ));
        update_undo(UndoType::CoupleEnd);
        assert_eq!(current_buffer_text(), "after");

        do_undo();
        assert_eq!(current_buffer_text(), "before");

        do_redo();
        assert_eq!(current_buffer_text(), "after");
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn in_memory_replacement_couple_is_miri_checkable() {
        install_buffer("before", 0);
        add_undo(UndoType::CoupleBegin, Some("filtering"));

        set_cutbuffer(None);
        add_undo(UndoType::CutToEof, None);
        do_snip(false, true, false);
        update_undo(UndoType::CutToEof);

        let replacement = crate::nano::make_new_node(None);
        replacement.borrow_mut().data = LineData::from_utf8("after");
        add_undo(UndoType::Insert, None);
        ingraft_buffer(replacement);
        update_undo(UndoType::Insert);

        add_undo(UndoType::CoupleEnd, Some("filtering"));
        update_undo(UndoType::CoupleEnd);
        assert_eq!(current_buffer_text(), "after");

        do_undo();
        assert_eq!(current_buffer_text(), "before");
        do_redo();
        assert_eq!(current_buffer_text(), "after");
    }

    #[cfg(all(feature = "wordcomp", feature = "multibuffer"))]
    #[test]
    fn word_completion_scours_other_buffers() {
        let active_line = install_buffer("hel", 3);
        let (other, _) = buffer_with_line("hello", 0);
        state_mut().buffer_ring.push_back(other);

        complete_a_word();

        assert_eq!(active_line.borrow().data, "hello");
        assert_eq!(state().buffer_ring.len(), 1);
        assert_eq!(state().openfile.as_ref().unwrap().current_x, 5);
    }

    #[cfg(all(feature = "justify", not(feature = "tiny")))]
    #[test]
    fn justify_without_paragraph_skips_the_work() {
        // Issue #62: with no paragraph from the cursor onwards, ^J used to
        // run a phantom justification and flag the buffer modified.
        let undo_depth = || -> usize {
            let s = state();
            let buffer = s.openfile.as_ref().unwrap();
            let mut n = 0usize;
            let mut cursor: Option<&crate::definitions::UndoStruct> = buffer.undotop.as_deref();
            while let Some(u) = cursor {
                n += 1;
                cursor = u.next.as_deref();
            }
            n
        };

        for data in ["", "   "] {
            let line = install_buffer(data, 0);
            let before_undos = undo_depth();
            let was_modified = state().openfile.as_ref().unwrap().modified;

            justify_text(false);

            assert_eq!(
                undo_depth(),
                before_undos,
                "a failed justify must leave no undo records (input {data:?})"
            );
            assert_eq!(
                state().openfile.as_ref().unwrap().modified,
                was_modified,
                "a failed justify must not flag the buffer modified"
            );
            assert_eq!(
                line.borrow().data.as_bytes().len(),
                data.len(),
                "line content must be untouched"
            );
            // The cursor lands at the end of the last line.
            assert_eq!(state().openfile.as_ref().unwrap().current_x, data.len());
        }
    }
}

// tr! macro is defined in main.rs and available crate-wide.
