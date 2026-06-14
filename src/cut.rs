#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/cut.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014 Mark Majeres
//             Copyright (C) 2016, 2018-2020 Benno Schulenberg

use std::rc::Rc;
use std::cell::RefCell;
use crate::definitions::*;
use crate::global::{state, state_mut, with_state, with_state_mut};

// ---------------------------------------------------------------------------
// Forward stubs for functions not yet ported to Rust.
// These resolve cut.rs's dependencies; replace with real impls when available.
// ---------------------------------------------------------------------------

/// C: void add_undo(undo_type action, const char *operation)
/// Defined in text.c — records a new undo item.
#[cfg(not(feature = "tiny"))]
#[inline]
fn add_undo(action: UndoType, msg: Option<&str>) { crate::text::add_undo(action, msg) }

/// C: void update_undo(undo_type action)
/// Defined in text.c — merges the current action into the last undo item.
#[cfg(not(feature = "tiny"))]
#[inline]
fn update_undo(action: UndoType) { crate::text::update_undo(action) }

/// C: void set_modified(void)
/// Defined in files.c — marks the buffer as modified (also refreshes titlebar).
#[inline]
fn set_modified() { crate::files::set_modified() }

/// C: void wipe_statusbar(void)
/// Defined in winio.c — clears the status-bar message.
#[inline]
fn wipe_statusbar() { crate::winio::wipe_statusbar() }

/// C: void statusbar(const char *msg)
/// Defined in winio.c — shows a status-bar message (NOTICE level).
#[inline]
fn statusbar(msg: &str) { crate::winio::statusbar(msg) }

/// C: void statusline(message_type type, const char *msg)
/// Defined in winio.c — shows a typed status-bar message.
#[inline]
fn statusline(kind: MessageType, msg: &str) { crate::winio::statusline(kind, msg) }

/// C: void edit_redraw(linestruct *old_current, update_type manner)
/// Defined in winio.c — redraws the edit window after cursor movement.
#[inline]
fn edit_redraw(old_current: &LinePtr, manner: UpdateType) {
    crate::winio::edit_redraw(old_current, manner)
}

/// C: void update_line(linestruct *line, size_t index)
/// Defined in winio.c — repaints a single line.
#[inline]
fn update_line(line: &LinePtr, index: usize) -> i32 {
    crate::winio::update_line(line, index)
}

/// C: void check_the_multis(linestruct *line)
/// Defined in color.c — refreshes multiline-color state for one line.
#[cfg(feature = "color")]
#[inline]
fn check_the_multis(line: &LinePtr) { crate::color::check_the_multis(line) }

/// C: void adjust_viewport(update_type manner)
/// Defined in winio.c — adjusts the viewport after edittop changes.
#[inline]
fn adjust_viewport(manner: UpdateType) { crate::winio::adjust_viewport(manner) }

/// C: size_t extra_chunks_in(linestruct *line)
/// Defined in winio.c — number of soft-wrap continuation chunks in a line.
/// Adapter: real winio::extra_chunks_in takes &str.
#[cfg(not(feature = "tiny"))]
#[inline]
fn extra_chunks_in(line: &LinePtr) -> usize {
    crate::winio::extra_chunks_in(&line.borrow().data)
}

/// C: size_t leftedge_for(size_t column, linestruct *line)
/// Defined in winio.c — the column of the first character displayed on the
/// soft-wrap row that contains the given column.
/// Adapter: real winio::leftedge_for takes (column, &str).
#[cfg(not(feature = "tiny"))]
#[inline]
fn leftedge_for(column: usize, line: &LinePtr) -> usize {
    crate::winio::leftedge_for(column, &line.borrow().data)
}

/// C: bool less_than_a_screenful(ssize_t was_lineno, size_t was_leftedge)
/// Defined in winio.c — true when the pasted text spans less than one screen.
/// Adapter: real winio::less_than_a_screenful takes (usize, usize).
#[inline]
fn less_than_a_screenful(was_lineno: isize, was_leftedge: usize) -> bool {
    crate::winio::less_than_a_screenful(was_lineno.max(0) as usize, was_leftedge)
}

/// C: void precalc_multicolorinfo(void)
/// Defined in color.c — pre-calculates multiline highlighting for the whole buffer.
#[cfg(feature = "color")]
#[inline]
fn precalc_multicolorinfo() { crate::color::precalc_multicolorinfo() }

/// C: void do_wrap(void)
/// Defined in text.c — performs hard-wrapping on the current line.
#[cfg(feature = "wrapping")]
#[inline]
fn do_wrap() { crate::text::do_wrap() }

/// C: void do_left(void)
/// Defined in move.c — moves the cursor one character to the left.
#[inline]
fn do_left() { crate::move_::do_left() }

/// C: void do_prev_word(void)
/// Defined in move.c — moves the cursor to the start of the previous word.
#[cfg(not(feature = "tiny"))]
#[inline]
fn do_prev_word() { crate::move_::do_prev_word() }

/// C: void do_next_word(bool after_ends)
/// Defined in move.c — moves the cursor to the start of the next word (returns bool, discarded).
#[cfg(not(feature = "tiny"))]
#[inline]
fn do_next_word(after_ends: bool) { let _ = crate::move_::do_next_word(after_ends); }

// ---------------------------------------------------------------------------
// Internal helpers — count characters in a LinePtr chain (replaces the
// utils::number_of_characters_in that takes &[String] slices).
// ---------------------------------------------------------------------------

/// Count the total number of characters (not bytes) in a chain from `first`
/// to `last` (inclusive), adding 1 for each newline *between* lines.
/// Mirrors C: number_of_characters_in(first, last).
fn count_chars_in_chain(first: &LinePtr, last: &LinePtr) -> usize {
    let mut count: usize = 0;
    let mut current = first.clone();
    loop {
        let data_len = {
            let node = current.borrow();
            crate::chars::mbstrlen(&node.data)
        };
        count += data_len;
        // Check whether we have reached `last`.
        if Rc::ptr_eq(&current, last) {
            break;
        }
        // Add the newline between this line and the next.
        count += 1;
        let next = current.borrow().next.clone();
        match next {
            Some(n) => current = n,
            None => break,
        }
    }
    count
}

/// Make a fresh `LinePtr` with no links and empty data.
/// C: make_new_node(NULL) — nano.c; the NULL means "no parent".
#[inline]
fn make_new_node() -> LinePtr {
    crate::nano::make_new_node(None)
}

/// Free a single detached node (let the Rc drop).
/// C: delete_node(node) — nano.c.
#[inline]
fn delete_node(node: LinePtr) {
    crate::nano::delete_node(&node);
}

/// Free an entire chain starting at `head`.
/// C: free_lines(head) — nano.c.
#[inline]
fn free_lines(head: LinePtr) {
    crate::nano::free_lines(Some(head));
}

/// Unlink `node` from the doubly-linked list and drop it.
/// C: unlink_node(node) — nano.c; also fixes up filebot/edittop.
#[inline]
fn unlink_node(node: &LinePtr) {
    crate::nano::unlink_node(node)
}

/// Renumber every line from `start` to the end of the buffer.
/// C: renumber_from(start) — nano.c.
#[inline]
fn renumber_from(start: &LinePtr) {
    crate::nano::renumber_from(start)
}

// ---------------------------------------------------------------------------
// expunge — delete the character at the current position (or join lines)
// ---------------------------------------------------------------------------
/* C: void expunge(undo_type action) */
pub fn expunge(action: UndoType) {
    // Update placewewant first.
    let place = crate::utils::xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = place;
        }
    });

    // Check whether the cursor is in the middle of a line or at its end.
    let (in_middle, charlen, _line_len) = with_state(|s| {
        if let Some(ref of) = s.openfile {
            if let Some(ref cur) = of.current {
                let data = cur.borrow().data.clone();
                let x = of.current_x;
                if x < data.len() && data.as_bytes()[x] != 0 {
                    let cl = crate::chars::char_length(&data[x..]);
                    let ll = data.len() - x;
                    return (true, cl, ll);
                }
            }
        }
        (false, 0, 0)
    });

    if in_middle {
        // Delete the character under the cursor.

        // Capture the chunk count BEFORE the deletion so we can tell, afterwards,
        // whether the number of softwrap chunks changed (mirrors C's old_amount).
        #[cfg(not(feature = "tiny"))]
        let old_amount = with_state(|s| {
            if s.flag_isset(SOFTWRAP) {
                if let Some(ref of) = s.openfile {
                    if let Some(ref cur) = of.current {
                        return extra_chunks_in(cur);
                    }
                }
            }
            0
        });

        #[cfg(not(feature = "tiny"))]
        {
            // Add or update the undo item.
            let need_new = with_state(|s| {
                if let Some(ref of) = s.openfile {
                    if of.last_action != action {
                        return true;
                    }
                    // If current_undo is null, also need a new item.
                    if of.current_undo.is_null() {
                        return true;
                    }
                    // Compare lineno.
                    if let Some(ref cur) = of.current {
                        let cur_lineno = cur.borrow().lineno;
                        let undo_lineno = unsafe { (*of.current_undo).head_lineno };
                        return cur_lineno != undo_lineno;
                    }
                }
                true
            });
            if need_new {
                add_undo(action, None);
            } else {
                update_undo(action);
            }
        }

        // Remove the character from the line data.
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                if let Some(ref cur) = of.current {
                    let x = of.current_x;
                    let mut data = cur.borrow().data.clone();
                    // memmove: remove charlen bytes starting at x.
                    data.drain(x..x + charlen);
                    cur.borrow_mut().data = data;

                    // Adjust mark position.
                    #[cfg(not(feature = "tiny"))]
                    {
                        if let Some(ref mark) = of.mark {
                            if Rc::ptr_eq(mark, cur) && of.mark_x > x {
                                of.mark_x -= charlen;
                            }
                        }
                    }
                }
            }
        });

        #[cfg(not(feature = "tiny"))]
        {
            // When softwrapping, a changed number of chunks requires a refresh;
            // otherwise, when panning near the edge of the viewport, also refresh.
            let need_refresh = with_state(|s| {
                if s.flag_isset(SOFTWRAP) {
                    if let Some(ref of) = s.openfile {
                        if let Some(ref cur) = of.current {
                            if extra_chunks_in(cur) != old_amount {
                                return true;
                            }
                        }
                    }
                }
                if s.united_sidescroll {
                    if let Some(ref of) = s.openfile {
                        return of.placewewant < of.brink + CUSHION;
                    }
                }
                false
            });
            if need_refresh {
                state_mut().refresh_needed = true;
            }
        }

        // Update undo newsize.
        #[cfg(not(feature = "tiny"))]
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.totsize = of.totsize.saturating_sub(1);
                if !of.current_undo.is_null() {
                    unsafe { (*of.current_undo).newsize = of.totsize; }
                }
            }
        });
        #[cfg(feature = "tiny")]
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.totsize = of.totsize.saturating_sub(1);
            }
        });

    } else {
        // Cursor is at the end of the line — try to join with the next line.

        // Check whether there is a next line.
        let (has_next, _next_is_filebot, at_magic_boundary) = with_state(|s| {
            if let Some(ref of) = s.openfile {
                if let Some(ref cur) = of.current {
                    let next = cur.borrow().next.clone();
                    if let Some(ref nxt) = next {
                        let is_filebot = of.filebot.as_ref()
                            .map(|fb| Rc::ptr_eq(fb, nxt))
                            .unwrap_or(false);
                        let current_is_filebot = of.filebot.as_ref()
                            .map(|fb| Rc::ptr_eq(fb, cur))
                            .unwrap_or(false);
                        if current_is_filebot {
                            return (false, false, false);
                        }
                        let x = of.current_x;
                        let at_magic = is_filebot && x != 0 && !s.flag_isset(NO_NEWLINES);
                        return (true, is_filebot, at_magic);
                    }
                    // next is None — we are at filebot
                }
            }
            (false, false, false)
        });

        if !has_next {
            // At end of buffer: nothing to do.
            return;
        }

        if at_magic_boundary {
            // Don't eat the magic line.
            #[cfg(not(feature = "tiny"))]
            {
                if action == UndoType::Back {
                    add_undo(UndoType::Back, None);
                }
            }
            return;
        }

        // Join the current line with the next.
        #[cfg(not(feature = "tiny"))]
        add_undo(action, None);

        // Clone what we need before mutating.
        let (joining, joining_data) = with_state(|s| {
            if let Some(ref of) = s.openfile {
                if let Some(ref cur) = of.current {
                    if let Some(ref nxt) = cur.borrow().next {
                        return (Some(nxt.clone()), nxt.borrow().data.clone());
                    }
                }
            }
            (None, String::new())
        });

        if let Some(ref joining_node) = joining {
            #[cfg(not(feature = "tiny"))]
            {
                // Adjust mark if it is on the line being eaten.
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        if let Some(ref mark) = of.mark.clone() {
                            if Rc::ptr_eq(mark, joining_node) {
                                let cur_x = of.current_x;
                                of.mark = of.current.clone();
                                of.mark_x += cur_x;
                            }
                        }
                    }
                });

                // Propagate anchor from joining node.
                let joining_anchor = joining_node.borrow().has_anchor;
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        if let Some(ref cur) = of.current {
                            cur.borrow_mut().has_anchor |= joining_anchor;
                        }
                    }
                });
            }

            // Append joining_data to current->data.
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    if let Some(ref cur) = of.current {
                        cur.borrow_mut().data.push_str(&joining_data);
                    }
                }
            });

            // Unlink joining from the buffer.
            // (unlink_node also moves filebot back when joining was filebot.)
            unlink_node(joining_node);

            let cur_clone = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });

            // Renumber and refresh.
            if let Some(ref cur) = cur_clone {
                renumber_from(cur);
            }
            state_mut().refresh_needed = true;

            // Adjust totsize and undo newsize.
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.totsize = of.totsize.saturating_sub(1);
                    #[cfg(not(feature = "tiny"))]
                    if !of.current_undo.is_null() {
                        unsafe { (*of.current_undo).newsize = of.totsize; }
                    }
                }
            });

            set_modified();
            return; // Early return; the post-expunge refresh logic below is not needed.
        }

        return;
    }

    // Post-deletion refresh (only reached for the in_middle branch).
    let refresh_needed = state().refresh_needed;
    if !refresh_needed {
        #[cfg(feature = "color")]
        {
            let cur_clone = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            if let Some(ref cur) = cur_clone {
                check_the_multis(cur);
            }
        }
        let cur_clone = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.current.clone())
        });
        let cur_x = with_state(|s| {
            s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
        });
        if let Some(ref cur) = cur_clone {
            update_line(cur, cur_x);
        }
    }

    set_modified();
}

// ---------------------------------------------------------------------------
// do_delete — delete the character under the cursor
// ---------------------------------------------------------------------------
/* C: void do_delete(void) */
pub fn do_delete() {
    #[cfg(not(feature = "tiny"))]
    {
        let should_zap = with_state(|s| {
            s.openfile.as_ref()
                .map(|of| of.mark.is_some())
                .unwrap_or(false)
                && s.flag_isset(LET_THEM_ZAP)
        });
        if should_zap {
            zap_text();
            return;
        }
    }

    expunge(UndoType::Del);

    #[cfg(feature = "utf8")]
    {
        // Delete any subsequent zero-width characters.
        loop {
            let at_zerowidth = with_state(|s| {
                if let Some(ref of) = s.openfile {
                    if let Some(ref cur) = of.current {
                        let data = cur.borrow().data.clone();
                        let x = of.current_x;
                        if x < data.len() && data.as_bytes()[x] != 0 {
                            return crate::chars::is_zerowidth(&data[x..]);
                        }
                    }
                }
                false
            });
            if !at_zerowidth {
                break;
            }
            expunge(UndoType::Del);
        }
    }
}

// ---------------------------------------------------------------------------
// do_backspace — delete the character before the cursor
// ---------------------------------------------------------------------------
/* C: void do_backspace(void) */
pub fn do_backspace() {
    #[cfg(not(feature = "tiny"))]
    {
        let should_zap = with_state(|s| {
            s.openfile.as_ref()
                .map(|of| of.mark.is_some())
                .unwrap_or(false)
                && s.flag_isset(LET_THEM_ZAP)
        });
        if should_zap {
            zap_text();
            return;
        }
    }

    let current_x = with_state(|s| {
        s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
    });

    if current_x > 0 {
        // Move cursor one character to the left and delete.
        let new_x = with_state(|s| {
            if let Some(ref of) = s.openfile {
                if let Some(ref cur) = of.current {
                    let data = cur.borrow().data.clone();
                    return crate::chars::step_left(&data, of.current_x);
                }
            }
            0
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });
        expunge(UndoType::Back);
    } else {
        // At column 0 — go to end of previous line and join.
        let is_filetop = with_state(|s| {
            s.openfile.as_ref().and_then(|of| {
                let cur = of.current.as_ref()?;
                let top = of.filetop.as_ref()?;
                Some(Rc::ptr_eq(cur, top))
            }).unwrap_or(true)
        });
        if !is_filetop {
            do_left();
            expunge(UndoType::Back);
        }
    }
}

// ---------------------------------------------------------------------------
// is_cuttable — return false when a cut would not actually cut anything
// ---------------------------------------------------------------------------
/* C: bool is_cuttable(bool test_cliff) */
pub fn is_cuttable(test_cliff: bool) -> bool {
    let from = if test_cliff {
        with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0))
    } else {
        0
    };

    let uncuttable = with_state(|s| {
        if let Some(ref of) = s.openfile {
            if let Some(ref cur) = of.current {
                let data = cur.borrow().data.clone();
                let at_end_byte = data.as_bytes().get(from).copied().unwrap_or(0) == 0;
                let no_next = cur.borrow().next.is_none();

                // Case 1: empty line at EOF (and no mark in non-tiny).
                #[cfg(not(feature = "tiny"))]
                {
                    if no_next && at_end_byte && of.mark.is_none() {
                        return true;
                    }
                    // Case 2: mark covers zero characters.
                    if let Some(ref mark) = of.mark {
                        if Rc::ptr_eq(mark, cur) && of.mark_x == of.current_x {
                            return true;
                        }
                    }
                    // Case 3: test_cliff and the magic line would be cut.
                    if from > 0 && !s.flag_isset(NO_NEWLINES) && at_end_byte {
                        if let Some(ref nxt) = cur.borrow().next {
                            if let Some(ref fb) = of.filebot {
                                if Rc::ptr_eq(nxt, fb) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                #[cfg(feature = "tiny")]
                {
                    if no_next && at_end_byte {
                        return true;
                    }
                }
            }
        }
        false
    });

    if uncuttable {
        #[cfg(not(feature = "tiny"))]
        {
            statusbar(&crate::tr!("Nothing was cut"));
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = None;
                }
            });
        }
        return false;
    }

    true
}

// ---------------------------------------------------------------------------
// chop_word — delete text from cursor to the start/end of a word
// ---------------------------------------------------------------------------
/* C: void chop_word(bool forward) */
#[cfg(not(feature = "tiny"))]
fn chop_word(forward: bool) {
    // Remember the cursor position.
    let (was_current, was_x) = with_state(|s| {
        let cur = s.openfile.as_ref().and_then(|of| of.current.clone());
        let x = s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0);
        (cur, x)
    });

    // Save and blank the cutbuffer.
    let is_cutbuffer = state().cutbuffer.clone();
    state_mut().cutbuffer = None;

    if !forward {
        do_prev_word();
        // If we moved to a different line, clamp to line edge.
        let moved_line = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.current.clone())
                .zip(was_current.clone())
                .map(|(cur, was)| !Rc::ptr_eq(&cur, &was))
                .unwrap_or(false)
        });
        if moved_line {
            if was_x > 0 {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current = was_current.clone();
                        of.current_x = 0;
                    }
                });
            } else {
                // Move x to end of current (new) line.
                let new_len = with_state(|s| {
                    s.openfile.as_ref()
                        .and_then(|of| of.current.as_ref())
                        .map(|cur| cur.borrow().data.len())
                        .unwrap_or(0)
                });
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current_x = new_len;
                    }
                });
            }
        }
    } else {
        let after_ends = state().flag_isset(AFTER_ENDS);
        do_next_word(after_ends);
        let (moved_line, was_char_at_x) = with_state(|s| {
            let moved = s.openfile.as_ref().and_then(|of| of.current.clone())
                .zip(was_current.clone())
                .map(|(cur, was)| !Rc::ptr_eq(&cur, &was))
                .unwrap_or(false);
            let had_char = was_current.as_ref()
                .map(|wc| {
                    let data = wc.borrow().data.clone();
                    data.as_bytes().get(was_x).copied().unwrap_or(0) != 0
                })
                .unwrap_or(false);
            (moved, had_char)
        });
        if moved_line && was_char_at_x {
            let was_len = was_current.as_ref()
                .map(|wc| wc.borrow().data.len())
                .unwrap_or(0);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = was_current.clone();
                    of.current_x = was_len;
                }
            });
        }
    }

    // Set the mark at the word start.
    let (mark_line, mark_x) = with_state(|s| {
        let line = s.openfile.as_ref().and_then(|of| of.current.clone());
        let x = s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0);
        (line, x)
    });
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.mark = mark_line;
            of.mark_x = mark_x;
        }
    });

    // Put the cursor back where it was (so undo places it there).
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = was_current;
            of.current_x = was_x;
        }
    });

    // Kill the marked region.
    add_undo(UndoType::Cut, None);
    do_snip(true, false, false);
    update_undo(UndoType::Cut);

    // Discard the cut word and restore the original cutbuffer.
    let new_cutbuffer = state().cutbuffer.clone();
    if let Some(cb) = new_cutbuffer {
        free_lines(cb);
    }
    state_mut().cutbuffer = is_cutbuffer;
}

// ---------------------------------------------------------------------------
// chop_previous_word — delete a word leftward
// ---------------------------------------------------------------------------
/* C: void chop_previous_word(void) */
#[cfg(not(feature = "tiny"))]
pub fn chop_previous_word() {
    let at_start = with_state(|s| {
        s.openfile.as_ref().map(|of| {
            of.current.as_ref()
                .zip(of.filetop.as_ref())
                .map(|(cur, top)| Rc::ptr_eq(cur, top))
                .unwrap_or(false)
                && of.current_x == 0
        }).unwrap_or(false)
    });

    if at_start {
        statusbar(&crate::tr!("Nothing was cut"));
    } else {
        chop_word(BACKWARD);
    }
}

// ---------------------------------------------------------------------------
// chop_next_word — delete a word rightward
// ---------------------------------------------------------------------------
/* C: void chop_next_word(void) */
#[cfg(not(feature = "tiny"))]
pub fn chop_next_word() {
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.mark = None;
        }
    });

    if is_cuttable(true) {
        chop_word(FORWARD);
    }
}

// ---------------------------------------------------------------------------
// extract_segment — excise text range from the buffer into the cutbuffer
// ---------------------------------------------------------------------------
/* C: void extract_segment(linestruct *top, size_t top_x,
 *                          linestruct *bot, size_t bot_x) */
pub fn extract_segment(top: LinePtr, top_x: usize, bot: LinePtr, bot_x: usize) {
    // Whether edittop is inside the excised range.
    let edittop_inside = with_state(|s| {
        if let Some(ref of) = s.openfile {
            if let Some(ref et) = of.edittop {
                let et_no = et.borrow().lineno;
                let top_no = top.borrow().lineno;
                let bot_no = bot.borrow().lineno;
                return et_no >= top_no && et_no <= bot_no;
            }
        }
        false
    });

    #[cfg(not(feature = "tiny"))]
    let (same_line, post_marked) = with_state(|s| {
        let same = s.openfile.as_ref()
            .and_then(|of| of.mark.as_ref())
            .map(|m| Rc::ptr_eq(m, &top))
            .unwrap_or(false);
        let post = s.openfile.as_ref()
            .and_then(|of| of.mark.as_ref())
            .map(|m| {
                let m_no = m.borrow().lineno;
                let top_no = top.borrow().lineno;
                if m_no > top_no {
                    return true;
                }
                if same && s.openfile.as_ref().map(|of| of.mark_x).unwrap_or(0) > top_x {
                    return true;
                }
                false
            })
            .unwrap_or(false);
        (same, post)
    });

    // Track whether the anchor should be inherited into the cutbuffer.
    // C: static bool inherited_anchor = FALSE;
    // We use a thread_local here to mirror the C static.
    #[cfg(not(feature = "tiny"))]
    thread_local! {
        static INHERITED_ANCHOR: RefCell<bool> = RefCell::new(false);
    }

    #[cfg(not(feature = "tiny"))]
    let had_anchor = {
        let mut had = top.borrow().has_anchor;
        if !Rc::ptr_eq(&top, &bot) {
            let mut cur = top.borrow().next.clone();
            loop {
                match cur {
                    None => break,
                    Some(ref node) => {
                        if Rc::ptr_eq(node, &bot) {
                            had |= node.borrow().has_anchor;
                            break;
                        }
                        had |= node.borrow().has_anchor;
                        let nxt = node.borrow().next.clone();
                        cur = nxt;
                    }
                }
            }
            // Also include bot itself.
            had |= bot.borrow().has_anchor;
        }
        had
    };

    // Early return if top == bot and positions are identical (non-tiny only).
    #[cfg(not(feature = "tiny"))]
    if Rc::ptr_eq(&top, &bot) && top_x == bot_x {
        return;
    }

    // Determine which of the three extraction cases we are in.
    let same_node = Rc::ptr_eq(&top, &bot);
    let both_zero = !same_node && top_x == 0 && bot_x == 0;

    let (taken, last): (LinePtr, LinePtr) = if same_node {
        // Case 1: excise a portion of a single line.
        let node = make_new_node();
        {
            let top_data = top.borrow().data.clone();
            let extracted = top_data[top_x..bot_x].to_string();
            node.borrow_mut().data = extracted;
        }
        // Remove the range from top's data.
        {
            let mut top_node = top.borrow_mut();
            top_node.data.drain(top_x..bot_x);
        }
        let last = node.clone();
        (node, last)
    } else if both_zero {
        // Case 2: excise complete lines from top to bot (exclusive, since bot_x==0).
        let taken = top.clone();

        // Make a new empty last node (becomes the replacement for the lines after
        // the taken block — it will sit just before bot in the buffer).
        let last_node = make_new_node();
        {
            #[cfg(not(feature = "tiny"))]
            {
                last_node.borrow_mut().has_anchor = bot.borrow().has_anchor;
            }
        }
        // last_node.prev = bot.prev; last_node.next = None.
        let bot_prev_weak = bot.borrow().prev.clone();
        last_node.borrow_mut().prev = bot_prev_weak.clone();
        last_node.borrow_mut().next = None;
        if let Some(ref bpw) = bot_prev_weak {
            if let Some(bp) = bpw.upgrade() {
                bp.borrow_mut().next = Some(last_node.clone());
            }
        }
        last_node.borrow_mut().data = String::new();

        // Reattach bot to top's predecessor.
        let top_prev_weak = top.borrow().prev.clone();
        bot.borrow_mut().prev = top_prev_weak.clone();
        match &top_prev_weak {
            None => {
                // top was filetop; make bot the new filetop.
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.filetop = Some(bot.clone());
                    }
                });
            }
            Some(pw) => {
                if let Some(prev) = pw.upgrade() {
                    prev.borrow_mut().next = Some(bot.clone());
                }
            }
        }

        // Update current to bot.
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(bot.clone());
            }
        });

        // Sever the chain between taken and last_node (last_node is the new terminus).
        // The taken chain is now: top -> … -> (node before bot) -> last_node, next=None.
        (taken, last_node)
    } else {
        // Case 3: excise across multiple lines, with top_x != 0 or bot_x != 0.
        let new_taken = make_new_node();
        {
            let top_data = top.borrow().data[top_x..].to_string();
            new_taken.borrow_mut().data = top_data;
        }
        // Hook taken between top->next.
        let top_next = top.borrow().next.clone();
        if let Some(ref tn) = top_next {
            tn.borrow_mut().prev = Some(Rc::downgrade(&new_taken));
        }
        new_taken.borrow_mut().next = top_next;

        // top skips over the excised segment: top->next = bot->next.
        let bot_next = bot.borrow().next.clone();
        top.borrow_mut().next = bot_next.clone();
        if let Some(ref bn) = bot_next {
            bn.borrow_mut().prev = Some(Rc::downgrade(&top));
        }

        // Truncate top->data to top_x and append bot->data[bot_x..].
        {
            let bot_tail = bot.borrow().data[bot_x..].to_string();
            let mut top_node = top.borrow_mut();
            top_node.data.truncate(top_x);
            top_node.data.push_str(&bot_tail);
        }

        // bot becomes last: data[bot_x..] = "".
        {
            let mut bot_node = bot.borrow_mut();
            bot_node.data.truncate(bot_x);
            bot_node.next = None;
        }

        // Update current to top.
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(top.clone());
            }
        });

        (new_taken, bot.clone())
    };

    // Subtract the size of excised text from totsize.
    let char_count = count_chars_in_chain(&taken, &last);
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.totsize = of.totsize.saturating_sub(char_count);
        }
    });

    // Merge taken chain into the cutbuffer (append or replace).
    let cutbuffer_empty = state().cutbuffer.is_none();

    if cutbuffer_empty {
        with_state_mut(|s| {
            s.cutbuffer = Some(taken.clone());
            s.cutbottom = Some(last.clone());
            #[cfg(not(feature = "tiny"))]
            INHERITED_ANCHOR.with(|ia| *ia.borrow_mut() = taken.borrow().has_anchor);
        });
    } else {
        // Append: concatenate taken->data onto cutbottom->data, then chain the rest.
        let (_cutbottom_data, taken_data) = with_state(|s| {
            let cb = s.cutbottom.as_ref().map(|cb| cb.borrow().data.clone()).unwrap_or_default();
            let td = taken.borrow().data.clone();
            (cb, td)
        });

        // Merge taken->data into cutbottom->data.
        with_state_mut(|s| {
            if let Some(ref cb) = s.cutbottom {
                let mut node = cb.borrow_mut();
                node.data.push_str(&taken_data);
                #[cfg(not(feature = "tiny"))]
                {
                    let taken_anchor = taken.borrow().has_anchor;
                    let inherited = INHERITED_ANCHOR.with(|ia| *ia.borrow());
                    node.has_anchor = taken_anchor && !inherited;
                    INHERITED_ANCHOR.with(|ia| {
                        *ia.borrow_mut() |= taken_anchor;
                    });
                }
            }
        });

        // Chain the rest of taken's successor nodes.
        let taken_next = taken.borrow().next.clone();
        with_state_mut(|s| {
            if let Some(ref cb) = s.cutbottom.clone() {
                cb.borrow_mut().next = taken_next.clone();
            }
        });
        // Drop taken (its data has been merged into cutbottom).
        delete_node(taken);

        // Update cutbottom and fix back-link.
        let new_cutbottom_next = with_state(|s| {
            s.cutbottom.as_ref().and_then(|cb| cb.borrow().next.clone())
        });
        if let Some(ref ncbn) = new_cutbottom_next {
            let cb = state().cutbottom.clone();
            if let Some(ref cb_node) = cb {
                ncbn.borrow_mut().prev = Some(Rc::downgrade(cb_node));
            }
            state_mut().cutbottom = Some(last.clone());
        }
    }

    // Update current_x to top_x.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current_x = top_x;
        }
    });

    #[cfg(not(feature = "tiny"))]
    {
        // Restore anchor on the line that remains.
        with_state_mut(|s| {
            if let Some(ref of) = s.openfile {
                if let Some(ref cur) = of.current {
                    cur.borrow_mut().has_anchor = had_anchor;
                }
            }
        });

        // Adjust mark.
        if post_marked || same_line {
            let cur_clone = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            let cur_x = with_state(|s| {
                s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
            });
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = cur_clone;
                    if post_marked {
                        of.mark_x = cur_x;
                    }
                }
            });
        }
    }

    // Update filebot if bot was the last line.
    let bot_was_filebot = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.filebot.as_ref())
            .map(|fb| Rc::ptr_eq(fb, &bot))
            .unwrap_or(false)
    });
    if bot_was_filebot {
        let cur_clone = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.current.clone())
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.filebot = cur_clone;
            }
        });
    }

    // Renumber from current.
    let cur_clone = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.current.clone())
    });
    if let Some(ref cur) = cur_clone {
        renumber_from(cur);
    }

    // Adjust viewport if edittop was inside the excision.
    if edittop_inside {
        adjust_viewport(UpdateType::Stationary);
        state_mut().refresh_needed = true;
    }

    // Ensure the buffer ends with a newline if required.
    let needs_magic = with_state(|s| {
        !s.flag_isset(NO_NEWLINES) && s.openfile.as_ref()
            .and_then(|of| of.filebot.as_ref())
            .map(|fb| !fb.borrow().data.is_empty())
            .unwrap_or(false)
    });
    if needs_magic {
        crate::utils::new_magicline();
    }
}

// ---------------------------------------------------------------------------
// ingraft_buffer — insert a line chain at the current cursor position
// ---------------------------------------------------------------------------
/* C: void ingraft_buffer(linestruct *topline) */
pub fn ingraft_buffer(topline: LinePtr) {
    // Snapshot what we need from openfile.
    let (line, length, xpos, tailtext) = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let cur = of.current.as_ref().expect("current").clone();
        let data = cur.borrow().data.clone();
        let x = of.current_x;
        let tail = data[x..].to_string();
        let len = data.len();
        (cur, len, x, tail)
    });

    #[cfg(not(feature = "tiny"))]
    let mark_follows = with_state(|s| {
        let of = s.openfile.as_ref()?;
        let mark = of.mark.as_ref()?;
        if !Rc::ptr_eq(mark, &line) {
            return None;
        }
        // mark is on the same line as cursor.
        Some(!crate::utils::mark_is_before_cursor())
    }).unwrap_or(false);

    // Find botline (last node of the topline chain).
    let mut botline = topline.clone();
    loop {
        let next = botline.borrow().next.clone();
        match next {
            Some(n) => botline = n,
            None => break,
        }
    }

    let is_single = Rc::ptr_eq(&topline, &botline);

    // Add the grafted text's size to totsize.
    let graft_chars = count_chars_in_chain(&topline, &botline);
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.totsize += graft_chars;
        }
    });

    let extralen = topline.borrow().data.len();
    let _effective_length = if !is_single { xpos } else { length };

    if extralen > 0 {
        // Insert topline->data at xpos in line->data.
        let insert_text = topline.borrow().data.clone();
        {
            let mut node = line.borrow_mut();
            // Make room: move data after xpos aside, then insert.
            let original_tail = node.data[xpos..].to_string();
            node.data.truncate(xpos);
            node.data.push_str(&insert_text);
            if is_single {
                // Single-node paste: just continue with the tail.
                node.data.push_str(&original_tail);
            }
            // For multi-node paste the tail gets appended to botline later.
        }
    }

    if !is_single {
        // Multi-line graft.

        // Check if line is currently the last in the buffer.
        let line_next = line.borrow().next.clone();
        if line_next.is_none() {
            // Update filebot to botline.
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.filebot = Some(botline.clone());
                }
            });
        }

        // Terminate line->data at xpos + extralen.
        {
            let mut node = line.borrow_mut();
            let cut_at = xpos + extralen;
            node.data.truncate(cut_at);
        }

        // Hook grafted lines: botline->next = line->next;
        //                     line->next = topline->next (the first grafted continuation).
        let topline_next = topline.borrow().next.clone();

        botline.borrow_mut().next = line_next.clone();
        if let Some(ref ln) = line_next {
            ln.borrow_mut().prev = Some(Rc::downgrade(&botline));
        }
        line.borrow_mut().next = topline_next.clone();
        if let Some(ref tn) = topline_next {
            tn.borrow_mut().prev = Some(Rc::downgrade(&line));
        }

        // Append tailtext to botline.
        let bot_len = botline.borrow().data.len();
        {
            let mut bot_node = botline.borrow_mut();
            bot_node.data.push_str(&tailtext);
        }

        // Move cursor to end of grafted text (botline, at old bot_len position).
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(botline.clone());
                of.current_x = bot_len;
            }
        });

        // Adjust mark when it follows the cursor on the same original line.
        #[cfg(not(feature = "tiny"))]
        if mark_follows {
            let mark_x = with_state(|s| {
                s.openfile.as_ref().map(|of| of.mark_x).unwrap_or(0)
            });
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = Some(botline.clone());
                    of.mark_x = mark_x + bot_len - xpos;
                }
            });
        }
    } else {
        // Single-node graft: cursor advances by extralen.
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x += extralen;
            }
        });

        #[cfg(not(feature = "tiny"))]
        if mark_follows {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark_x += extralen;
                }
            });
        }
    }

    // Drop topline (its data was merged into line).
    delete_node(topline);

    // Renumber from line.
    renumber_from(&line);

    // Ensure buffer ends with a newline if required.
    let needs_magic = with_state(|s| {
        !s.flag_isset(NO_NEWLINES) && s.openfile.as_ref()
            .and_then(|of| of.filebot.as_ref())
            .map(|fb| !fb.borrow().data.is_empty())
            .unwrap_or(false)
    });
    if needs_magic {
        crate::utils::new_magicline();
    }
}

// ---------------------------------------------------------------------------
// copy_from_buffer — graft a copy of a buffer at the cursor
// ---------------------------------------------------------------------------
/* C: void copy_from_buffer(linestruct *somebuffer) */
fn copy_from_buffer(somebuffer: &LinePtr) {
    #[cfg(feature = "color")]
    let threshold = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.edittop.as_ref())
            .map(|et| et.borrow().lineno)
            .unwrap_or(0)
            + state().editwinrows as isize - 1
    });

    let the_copy = copy_buffer(somebuffer);
    ingraft_buffer(the_copy);

    #[cfg(feature = "color")]
    {
        let cur_lineno = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|c| c.borrow().lineno)
                .unwrap_or(0)
        });
        if cur_lineno > threshold || state().flag_isset(SOFTWRAP) {
            state_mut().recook = true;
        } else {
            state_mut().perturbed = true;
        }
    }
}

// ---------------------------------------------------------------------------
// cut_marked_region — cut the marked region into the cutbuffer
// ---------------------------------------------------------------------------
/* C: void cut_marked_region(void) */
#[cfg(not(feature = "tiny"))]
fn cut_marked_region() {
    // get_region returns (top_lineno, top_x, bot_lineno, bot_x) — but we need
    // LinePtr handles, not line numbers.  Retrieve them directly from openfile.
    let (top, top_x, bot, bot_x) = {
        let before = crate::utils::mark_is_before_cursor();
        with_state(|s| {
            let of = s.openfile.as_ref().expect("openfile");
            let mark = of.mark.as_ref().expect("mark");
            let cur = of.current.as_ref().expect("current");
            if before {
                (mark.clone(), of.mark_x, cur.clone(), of.current_x)
            } else {
                (cur.clone(), of.current_x, mark.clone(), of.mark_x)
            }
        })
    };

    extract_segment(top, top_x, bot, bot_x);

    let place = crate::utils::xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = place;
        }
    });
}

// ---------------------------------------------------------------------------
// do_snip — the common workhorse for cut/zap/copy operations
// ---------------------------------------------------------------------------
/* C: void do_snip(bool marked, bool until_eof, bool append) */
pub fn do_snip(marked: bool, until_eof: bool, append: bool) {
    let _line = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.current.clone())
    });

    #[cfg(not(feature = "tiny"))]
    {
        let last_was_copy = with_state(|s| {
            s.openfile.as_ref().map(|of| of.last_action == UndoType::Copy).unwrap_or(false)
        });
        with_state_mut(|s| {
            s.keep_cutbuffer &= !last_was_copy;
        });
    }

    // If cuts were not continuous, or cutting a region, clear the cutbuffer.
    let keep = state().keep_cutbuffer;
    if (marked || until_eof || !keep) && !append {
        let old_cb = state().cutbuffer.clone();
        if let Some(cb) = old_cb {
            free_lines(cb);
        }
        state_mut().cutbuffer = None;
    }

    #[cfg(not(feature = "tiny"))]
    {
        if until_eof {
            let (cur, cur_x, filebot, filebot_len) = with_state(|s| {
                let of = s.openfile.as_ref().expect("openfile");
                let cur = of.current.as_ref().expect("current").clone();
                let cur_x = of.current_x;
                let fb = of.filebot.as_ref().expect("filebot").clone();
                let fb_len = fb.borrow().data.len();
                (cur, cur_x, fb, fb_len)
            });
            extract_segment(cur, cur_x, filebot, filebot_len);
        } else {
            let has_mark = with_state(|s| {
                s.openfile.as_ref().map(|of| of.mark.is_some()).unwrap_or(false)
            });
            if has_mark {
                cut_marked_region();
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.mark = None;
                    }
                });
            } else if state().flag_isset(CUT_FROM_CURSOR) {
                let (line_clone, cur_x, has_data, _next_is_filebot, is_filebot) =
                    with_state(|s| {
                        let of = s.openfile.as_ref().expect("openfile");
                        let cur = of.current.as_ref().expect("current").clone();
                        let x = of.current_x;
                        let data = cur.borrow().data.clone();
                        let has_data = x < data.len() && data.as_bytes()[x] != 0;
                        let next_fb = cur.borrow().next.as_ref()
                            .zip(of.filebot.as_ref())
                            .map(|(n, fb)| Rc::ptr_eq(n, fb))
                            .unwrap_or(false);
                        let is_fb = of.filebot.as_ref()
                            .map(|fb| Rc::ptr_eq(fb, &cur))
                            .unwrap_or(false);
                        (cur, x, has_data, next_fb, is_fb)
                    });
                if has_data {
                    let data_len = line_clone.borrow().data.len();
                    extract_segment(line_clone.clone(), cur_x, line_clone, data_len);
                } else if !is_filebot {
                    let next = line_clone.borrow().next.clone().expect("next");
                    extract_segment(line_clone, cur_x, next, 0);
                    let place = crate::utils::xplustabs();
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.placewewant = place;
                        }
                    });
                }
            } else {
                // Standard line cut or end-of-buffer cut.
                let (line_clone, is_filebot, data_len) = with_state(|s| {
                    let of = s.openfile.as_ref().expect("openfile");
                    let cur = of.current.as_ref().expect("current").clone();
                    let is_fb = of.filebot.as_ref()
                        .map(|fb| Rc::ptr_eq(fb, &cur))
                        .unwrap_or(false);
                    let len = cur.borrow().data.len();
                    (cur, is_fb, len)
                });
                if !is_filebot {
                    let next = line_clone.borrow().next.clone().expect("next");
                    extract_segment(line_clone, 0, next, 0);
                } else {
                    extract_segment(line_clone.clone(), 0, line_clone, data_len);
                }
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.placewewant = 0;
                    }
                });
            }
        }
    }

    #[cfg(feature = "tiny")]
    {
        let (line_clone, is_filebot, data_len) = with_state(|s| {
            let of = s.openfile.as_ref().expect("openfile");
            let cur = of.current.as_ref().expect("current").clone();
            let is_fb = of.filebot.as_ref()
                .map(|fb| Rc::ptr_eq(fb, &cur))
                .unwrap_or(false);
            let len = cur.borrow().data.len();
            (cur, is_fb, len)
        });
        if !is_filebot {
            let next = line_clone.borrow().next.clone().expect("next");
            extract_segment(line_clone, 0, next, 0);
        } else {
            extract_segment(line_clone.clone(), 0, line_clone, data_len);
        }
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.placewewant = 0;
            }
        });
    }

    // After a line operation, future ones should add to the cutbuffer.
    with_state_mut(|s| {
        s.keep_cutbuffer = !marked && !until_eof;
    });

    set_modified();
    state_mut().refresh_needed = true;

    #[cfg(feature = "color")]
    with_state_mut(|s| s.perturbed = true);
}

// ---------------------------------------------------------------------------
// cut_text — move text from the current buffer into the cutbuffer
// ---------------------------------------------------------------------------
/* C: void cut_text(void) */
pub fn cut_text() {
    #[cfg(not(feature = "tiny"))]
    {
        let test_cliff = with_state(|s| {
            s.flag_isset(CUT_FROM_CURSOR)
                && s.openfile.as_ref().map(|of| of.mark.is_none()).unwrap_or(true)
        });
        if !is_cuttable(test_cliff) {
            return;
        }

        let need_new_undo = with_state(|s| {
            let of = s.openfile.as_ref().expect("openfile");
            of.last_action != UndoType::Cut || !s.keep_cutbuffer
        });
        if need_new_undo {
            state_mut().keep_cutbuffer = false;
            add_undo(UndoType::Cut, None);
        }

        let has_mark = with_state(|s| {
            s.openfile.as_ref().map(|of| of.mark.is_some()).unwrap_or(false)
        });
        do_snip(has_mark, false, false);
        update_undo(UndoType::Cut);
    }

    #[cfg(feature = "tiny")]
    {
        if is_cuttable(false) {
            do_snip(false, false, false);
        }
    }

    wipe_statusbar();
}

// ---------------------------------------------------------------------------
// cut_till_eof — cut from cursor to end of file
// ---------------------------------------------------------------------------
/* C: void cut_till_eof(void) */
#[cfg(not(feature = "tiny"))]
pub fn cut_till_eof() {
    state_mut().ran_a_tool = true;

    let nothing_to_cut = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let cur = of.current.as_ref().expect("current");
        let data = cur.borrow().data.clone();
        let x = of.current_x;
        let at_end = x >= data.len() || data.as_bytes()[x] == 0;
        if !at_end {
            return false;
        }
        // At end of line; check if there is nothing further.
        let no_next = cur.borrow().next.is_none();
        if no_next {
            return true;
        }
        // If NO_NEWLINES is off and we are just before the magic line and x > 0:
        if !s.flag_isset(NO_NEWLINES) && x > 0 {
            if let Some(ref nxt) = cur.borrow().next {
                if let Some(ref fb) = of.filebot {
                    if Rc::ptr_eq(nxt, fb) {
                        return true;
                    }
                }
            }
        }
        false
    });

    if nothing_to_cut {
        statusbar(&crate::tr!("Nothing was cut"));
        return;
    }

    add_undo(UndoType::CutToEof, None);
    do_snip(false, true, false);
    update_undo(UndoType::CutToEof);
    wipe_statusbar();
}

// ---------------------------------------------------------------------------
// zap_text — erase text without saving to the persistent cutbuffer
// ---------------------------------------------------------------------------
/* C: void zap_text(void) */
#[cfg(not(feature = "tiny"))]
pub fn zap_text() {
    let was_cutbuffer = state().cutbuffer.clone();

    let test_cliff = with_state(|s| {
        s.flag_isset(CUT_FROM_CURSOR)
            && s.openfile.as_ref().map(|of| of.mark.is_none()).unwrap_or(true)
    });
    if !is_cuttable(test_cliff) {
        return;
    }

    let need_new_undo = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        of.last_action != UndoType::Zap || !s.keep_cutbuffer
    });
    if need_new_undo {
        add_undo(UndoType::Zap, None);
    }

    // Use the cutbuffer from the ZAP undo item so the cut can be undone.
    let undo_cutbuffer = with_state(|s| {
        let of = s.openfile.as_ref()?;
        if of.current_undo.is_null() {
            return None;
        }
        unsafe { (*of.current_undo).cutbuffer.clone() }
    });
    state_mut().cutbuffer = undo_cutbuffer;

    let has_mark = with_state(|s| {
        s.openfile.as_ref().map(|of| of.mark.is_some()).unwrap_or(false)
    });
    do_snip(has_mark, false, true);

    update_undo(UndoType::Zap);
    wipe_statusbar();

    state_mut().cutbuffer = was_cutbuffer;
}

// ---------------------------------------------------------------------------
// copy_marked_region — copy the marked region into the cutbuffer
// ---------------------------------------------------------------------------
/* C: void copy_marked_region(void) */
#[cfg(not(feature = "tiny"))]
fn copy_marked_region() {
    let before = crate::utils::mark_is_before_cursor();
    let (topline, top_x, botline, bot_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let mark = of.mark.as_ref().expect("mark").clone();
        let cur = of.current.as_ref().expect("current").clone();
        if before {
            (mark, of.mark_x, cur, of.current_x)
        } else {
            (cur, of.current_x, mark, of.mark_x)
        }
    });

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.last_action = UndoType::Other;
            of.mark = None;
        }
        s.keep_cutbuffer = false;
        s.refresh_needed = true;
    });

    if Rc::ptr_eq(&topline, &botline) && top_x == bot_x {
        statusbar(&crate::tr!("Copied nothing"));
        return;
    }

    // Make the marked area look like a separate buffer: temporarily detach
    // botline->next, logically truncate botline at bot_x, and move topline's
    // data to topline->data + top_x.  C does this NON-destructively (a single
    // '\0' written at bot_x, the rest of the bytes left in place) and restores
    // the exact original strings afterwards, so we must do the same — these are
    // LIVE document nodes, not copies.
    let afterline = botline.borrow().next.clone();
    botline.borrow_mut().next = None;

    // Preserve the full original contents of both boundary nodes before any
    // mutation (handles topline == botline, where the two clones alias one node).
    let saved_top_data = topline.borrow().data.clone();
    let saved_bot_data = botline.borrow().data.clone();

    // Truncate botline at bot_x first, then drop topline's prefix before top_x.
    // When topline == botline this yields data[top_x..bot_x] (truncate runs
    // first, so the [top_x..] slice sees the already-truncated string), exactly
    // matching C's pointer-bump-past-the-NUL behaviour.
    botline.borrow_mut().data.truncate(bot_x);
    {
        let mut top_node = topline.borrow_mut();
        let moved = top_node.data[top_x..].to_string();
        top_node.data = moved;
    }

    // Deep-copy the (temporarily modified) chain.
    let cutbuf = copy_buffer(&topline);
    state_mut().cutbuffer = Some(cutbuf);

    // Restore both boundary nodes to their exact original state.
    topline.borrow_mut().data = saved_top_data;
    {
        let mut bot_node = botline.borrow_mut();
        bot_node.data = saved_bot_data;
        bot_node.next = afterline;
    }
}

// ---------------------------------------------------------------------------
// copy_text — copy text into the cutbuffer without removing it
// ---------------------------------------------------------------------------
/* C: void copy_text(void) */
pub fn copy_text() {
    let (at_eol, sans_newline_base, from_x, was_current) = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let cur = of.current.as_ref().expect("current").clone();
        let data = cur.borrow().data.clone();
        let x = of.current_x;
        let at_eol = x >= data.len() || data.as_bytes()[x] == 0;
        let no_next = cur.borrow().next.is_none();
        let sans_nl = s.flag_isset(NO_NEWLINES) && no_next;
        let from = if s.flag_isset(CUT_FROM_CURSOR) { x } else { 0 };
        (at_eol, sans_nl, from, cur)
    });

    #[cfg(not(feature = "tiny"))]
    {
        let reset = with_state(|s| {
            let of = s.openfile.as_ref().expect("openfile");
            of.mark.is_some() || of.last_action != UndoType::Copy
        });
        if reset {
            state_mut().keep_cutbuffer = false;
        }
    }

    let keep = state().keep_cutbuffer;
    if !keep {
        let old_cb = state().cutbuffer.clone();
        if let Some(cb) = old_cb {
            free_lines(cb);
        }
        state_mut().cutbuffer = None;
    }

    wipe_statusbar();

    #[cfg(not(feature = "tiny"))]
    {
        let has_mark = with_state(|s| {
            s.openfile.as_ref().map(|of| of.mark.is_some()).unwrap_or(false)
        });
        if has_mark {
            copy_marked_region();
            return;
        }
    }

    // When at the very end of the buffer, nothing to copy.
    let nothing_to_copy = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let cur = of.current.as_ref().expect("current");
        let no_next = cur.borrow().next.is_none();
        let cutbuf_exists = s.cutbuffer.is_some();
        no_next && at_eol
            && (s.flag_isset(CUT_FROM_CURSOR) || of.current_x == 0 || cutbuf_exists)
    });
    if nothing_to_copy {
        statusbar(&crate::tr!("Copied nothing"));
        return;
    }

    // Build the addition node.
    let addition = make_new_node();
    {
        let cur_data = was_current.borrow().data.clone();
        addition.borrow_mut().data = cur_data[from_x..].to_string();
    }

    let sans_newline = if state().flag_isset(CUT_FROM_CURSOR) {
        !at_eol
    } else {
        sans_newline_base
    };

    // Insert/append addition into the cutbuffer in the right position.
    let cutbuf_empty = state().cutbuffer.is_none();

    if cutbuf_empty && sans_newline {
        // cutbuffer = addition; cutbottom = addition.
        with_state_mut(|s| {
            s.cutbuffer = Some(addition.clone());
            s.cutbottom = Some(addition.clone());
        });
    } else if cutbuf_empty {
        // cutbuffer = addition; cutbottom = new empty sentinel.
        let sentinel = make_new_node();
        sentinel.borrow_mut().prev = Some(Rc::downgrade(&addition));
        addition.borrow_mut().next = Some(sentinel.clone());
        with_state_mut(|s| {
            s.cutbuffer = Some(addition.clone());
            s.cutbottom = Some(sentinel);
        });
    } else if sans_newline {
        // Replace cutbottom with addition (no trailing sentinel).
        with_state_mut(|s| {
            if let Some(ref cb) = s.cutbottom.clone() {
                let prev_weak = cb.borrow().prev.clone();
                addition.borrow_mut().prev = prev_weak.clone();
                if let Some(ref pw) = prev_weak {
                    if let Some(prev) = pw.upgrade() {
                        prev.borrow_mut().next = Some(addition.clone());
                    }
                }
                delete_node(cb.clone());
            }
            s.cutbottom = Some(addition.clone());
        });
    } else if state().flag_isset(CUT_FROM_CURSOR) {
        // Append addition after cutbottom (no sentinel manipulation).
        with_state_mut(|s| {
            if let Some(ref cb) = s.cutbottom.clone() {
                addition.borrow_mut().prev = Some(Rc::downgrade(cb));
                cb.borrow_mut().next = Some(addition.clone());
            }
            s.cutbottom = Some(addition.clone());
        });
    } else {
        // Insert addition before cutbottom (standard line copy accumulation).
        with_state_mut(|s| {
            if let Some(ref cb) = s.cutbottom.clone() {
                let prev_weak = cb.borrow().prev.clone();
                addition.borrow_mut().prev = prev_weak.clone();
                if let Some(ref pw) = prev_weak {
                    if let Some(prev) = pw.upgrade() {
                        prev.borrow_mut().next = Some(addition.clone());
                    }
                }
                addition.borrow_mut().next = Some(cb.clone());
                cb.borrow_mut().prev = Some(Rc::downgrade(&addition));
            }
        });
    }

    // Move cursor to next line (or end of current line).
    let move_to_next = with_state(|s| {
        let of = s.openfile.as_ref().expect("openfile");
        let cur = of.current.as_ref().expect("current");
        let has_next = cur.borrow().next.is_some();
        (!s.flag_isset(CUT_FROM_CURSOR) || at_eol) && has_next
    });

    if move_to_next {
        let next_line = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .and_then(|cur| cur.borrow().next.clone())
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = next_line;
                of.current_x = 0;
            }
        });
    } else {
        let end_x = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|cur| cur.borrow().data.len())
                .unwrap_or(0)
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = end_x;
            }
        });
    }

    edit_redraw(&was_current, UpdateType::Flowing);

    #[cfg(not(feature = "tiny"))]
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.last_action = UndoType::Copy;
        }
    });

    state_mut().keep_cutbuffer = true;
}

// ---------------------------------------------------------------------------
// paste_text — paste the cutbuffer into the current buffer
// ---------------------------------------------------------------------------
/* C: void paste_text(void) */
pub fn paste_text() {
    let cutbuffer_empty = state().cutbuffer.is_none();
    if cutbuffer_empty {
        statusline(MessageType::Ahem, &crate::tr!("Cutbuffer is empty"));
        return;
    }

    #[cfg(any(feature = "wrapping", not(feature = "tiny")))]
    let was_current = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.current.clone())
    });

    #[cfg(not(feature = "tiny"))]
    let had_anchor = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|cur| cur.borrow().has_anchor)
            .unwrap_or(false)
    });

    let was_lineno = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|cur| cur.borrow().lineno)
            .unwrap_or(0)
    });
    let mut was_leftedge: usize = 0;

    #[cfg(not(feature = "tiny"))]
    {
        add_undo(UndoType::Paste, None);

        if state().flag_isset(SOFTWRAP) {
            let col = crate::utils::xplustabs();
            let line = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            if let Some(ref ln) = line {
                was_leftedge = leftedge_for(col, ln);
            }
        }
    }

    // Graft a copy of the cutbuffer into the current buffer.
    let cutbuf = state().cutbuffer.clone().expect("cutbuffer");
    copy_from_buffer(&cutbuf);

    #[cfg(not(feature = "tiny"))]
    {
        // Wipe anchors from the pasted region.
        if let Some(ref wc) = was_current {
            let _cur_next = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|cur| cur.borrow().next.clone())
            });
            let mut line = wc.clone();
            loop {
                line.borrow_mut().has_anchor = false;
                let at_end = with_state(|s| {
                    s.openfile.as_ref()
                        .and_then(|of| of.current.clone())
                        .map(|cur| Rc::ptr_eq(&line, &cur))
                        .unwrap_or(false)
                });
                if at_end {
                    break;
                }
                let next = line.borrow().next.clone();
                match next {
                    Some(n) => line = n,
                    None => break,
                }
            }
            // Restore anchor on was_current.
            wc.borrow_mut().has_anchor = had_anchor;
        }

        update_undo(UndoType::Paste);
    }

    #[cfg(feature = "wrapping")]
    {
        let same_line = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.clone())
                .zip(was_current.as_ref().cloned())
                .map(|(cur, wc)| Rc::ptr_eq(&cur, &wc))
                .unwrap_or(false)
        });
        if same_line && state().flag_isset(BREAK_LONG_LINES) {
            do_wrap();
        }
    }

    // If we pasted less than a screenful, don't center the cursor.
    if less_than_a_screenful(was_lineno, was_leftedge) {
        state_mut().focusing = false;
    } else {
        #[cfg(feature = "color")]
        precalc_multicolorinfo();
    }

    // Set placewewant to the end of the pasted text.
    let place = crate::utils::xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = place;
        }
    });

    set_modified();
    wipe_statusbar();
    state_mut().refresh_needed = true;
}

// ---------------------------------------------------------------------------
// copy_buffer — deep-copy a line chain
// ---------------------------------------------------------------------------
/* C: linestruct *copy_buffer(const linestruct *src) */
pub fn copy_buffer(src: &LinePtr) -> LinePtr {
    // Create the first node.
    let head = make_new_node();
    {
        let src_node = src.borrow();
        let mut head_node = head.borrow_mut();
        head_node.data = src_node.data.clone();
        head_node.lineno = src_node.lineno;
        #[cfg(not(feature = "tiny"))]
        {
            head_node.has_anchor = src_node.has_anchor;
        }
    }

    let mut prev_copy = head.clone();
    let mut src_cur = src.borrow().next.clone();

    while let Some(ref src_next) = src_cur.clone() {
        let new_node = make_new_node();
        {
            let src_n = src_next.borrow();
            let mut new_n = new_node.borrow_mut();
            new_n.data = src_n.data.clone();
            new_n.lineno = src_n.lineno;
            #[cfg(not(feature = "tiny"))]
            {
                new_n.has_anchor = src_n.has_anchor;
            }
        }
        // Link new_node after prev_copy.
        new_node.borrow_mut().prev = Some(Rc::downgrade(&prev_copy));
        prev_copy.borrow_mut().next = Some(new_node.clone());

        prev_copy = new_node;
        src_cur = src_next.borrow().next.clone();
    }

    head
}

// tr! is defined in main.rs and re-exported via #[macro_use]; use crate::tr! here.
