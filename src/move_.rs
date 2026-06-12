#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/move.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2018, 2020, 2024, 2026 Benno Schulenberg
//
// NOTE: Functions that operate on the edit window (go_back_chunks,
// go_forward_chunks, leftedge_for, get_softwrap_breakpoint, chunk_for,
// extra_chunks_in, actual_last_column, edit_redraw, edit_scroll,
// adjust_viewport, update_line, line_needs_update, draw_all_subwindows)
// live in winio.c in the C source.  The private functions below forward
// to the real implementations in crate::winio; indent_length / begpar /
// inpar likewise forward to crate::text.

use crate::definitions::*;
use crate::global::{with_state, with_state_mut};
use crate::ISSET;

// ── External helpers (winio.c / text.c — delegating to real implementations) ──

/// C: int go_back_chunks(int nrows, linestruct **line, size_t *leftedge)
/// Move `line` and `leftedge` backward by `nrows` soft-wrapped chunks.
/// Returns the number of chunks it could *not* advance (0 = success).
#[inline]
fn go_back_chunks(nrows: i32, line: &mut Option<LinePtr>, leftedge: &mut usize) -> i32 {
    match line {
        Some(lp) => crate::winio::go_back_chunks(nrows, lp, leftedge),
        None => nrows,
    }
}

/// C: int go_forward_chunks(int nrows, linestruct **line, size_t *leftedge)
/// Move `line` and `leftedge` forward by `nrows` soft-wrapped chunks.
/// Returns the number of chunks it could *not* advance (0 = success).
#[inline]
fn go_forward_chunks(nrows: i32, line: &mut Option<LinePtr>, leftedge: &mut usize) -> i32 {
    match line {
        Some(lp) => crate::winio::go_forward_chunks(nrows, lp, leftedge),
        None => nrows,
    }
}

/// C: size_t leftedge_for(size_t column, linestruct *line)
/// Return the starting column of the soft-wrap chunk that contains `column`.
#[cfg(not(feature = "tiny"))]
#[inline]
fn leftedge_for(column: usize, line: &LinePtr) -> usize {
    crate::winio::leftedge_for(column, &line.borrow().data)
}

/// C: size_t get_softwrap_breakpoint(const char *linedata, size_t leftedge,
///                                    bool *kickoff, bool *last_chunk)
/// Return the column where a soft-wrapped chunk breaks.
#[cfg(not(feature = "tiny"))]
#[inline]
fn get_softwrap_breakpoint(
    linedata: &str,
    leftedge: usize,
    kickoff: &mut bool,
    last_chunk: &mut bool,
) -> usize {
    crate::winio::get_softwrap_breakpoint(linedata, leftedge, kickoff, last_chunk)
}

/// C: size_t chunk_for(size_t column, linestruct *line)
/// Return the zero-based chunk index that contains the given column.
#[cfg(not(feature = "tiny"))]
#[inline]
fn chunk_for(column: usize, line: &LinePtr) -> usize {
    crate::winio::chunk_for(column, &line.borrow().data)
}

/// C: size_t extra_chunks_in(linestruct *line)
/// Return the number of extra soft-wrap chunks in `line` beyond the first.
#[cfg(not(feature = "tiny"))]
#[inline]
fn extra_chunks_in(line: &LinePtr) -> usize {
    crate::winio::extra_chunks_in(&line.borrow().data)
}

/// C: size_t actual_last_column(size_t leftedge, size_t column)
/// Return the actual column that `leftedge + column` maps to, accounting for
/// tabs that straddle soft-wrap boundaries.
#[inline]
pub fn actual_last_column(leftedge: usize, column: usize) -> usize {
    crate::winio::actual_last_column(leftedge, column)
}

/// C: void edit_redraw(linestruct *old_current, update_type manner)
/// Redraw the edit window after the cursor has moved.
#[inline]
fn edit_redraw(old_current: &LinePtr, manner: UpdateType) {
    crate::winio::edit_redraw(old_current, manner);
}

/// C: void edit_scroll(bool direction)
/// Scroll the edit window one chunk in the given direction.
#[inline]
fn edit_scroll(direction: bool) {
    crate::winio::edit_scroll(direction);
}

/// C: void adjust_viewport(update_type manner)
/// Adjust the viewport so the cursor stays on screen.
#[inline]
fn adjust_viewport(manner: UpdateType) {
    crate::winio::adjust_viewport(manner);
}

/// C: int update_line(linestruct *line, size_t index)
/// Repaint a single line of the edit window.
#[inline]
fn update_line(line: &LinePtr, index: usize) -> i32 {
    crate::winio::update_line(line, index)
}

/// C: bool line_needs_update(const size_t old_column, const size_t new_column)
/// Return whether moving from old_column to new_column crosses a page boundary.
#[inline]
fn line_needs_update(old_column: usize, new_column: usize) -> bool {
    crate::winio::line_needs_update(old_column, new_column)
}

/// C: void draw_all_subwindows(void)
/// Draw the title bar, edit window, and status bar.
#[inline]
fn draw_all_subwindows() {
    crate::winio::draw_all_subwindows();
}

/// C: void full_refresh(void)
/// Redraw the entire screen.
fn full_refresh() {
    crate::global::full_refresh();
}

/// C: void statusline(message_type importance, const char *msg, ...)
#[inline]
fn statusline(importance: MessageType, msg: &str) {
    crate::winio::statusline(importance, msg);
}

// text.c helpers (used only under ENABLE_JUSTIFY / not-NANO_TINY)

/// C: size_t indent_length(const char *line)
/// Return the length of the indentation at the start of `line`.
#[cfg(any(not(feature = "tiny"), feature = "justify"))]
#[inline]
fn indent_length(line: &str) -> usize {
    crate::text::indent_length(line)
}

/// C: bool begpar(const linestruct *const line, int depth)
/// Return whether `line` begins a paragraph.
#[cfg(feature = "justify")]
#[inline]
fn begpar(line: &LinePtr, depth: i32) -> bool {
    crate::text::begpar_fn(line, depth)
}

/// C: bool inpar(const linestruct *const line)
/// Return whether `line` is inside a paragraph.
#[cfg(feature = "justify")]
#[inline]
fn inpar(line: &LinePtr) -> bool {
    crate::text::inpar_fn(line)
}

// ── Helpers from utils.rs / chars.rs ─────────────────────────────────────────

use crate::utils::{actual_x, wideness, breadth, xplustabs};
use crate::chars::{white_string, is_word_char, step_left, step_right};
#[cfg(feature = "utf8")]
use crate::chars::is_zerowidth;

// ── move.c functions ──────────────────────────────────────────────────────────

/* C: void to_first_line(void)
 * Move to the first line of the file. */
pub fn to_first_line() {
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = of.filetop.clone();
            of.current_x = 0;
            of.placewewant = 0;
        }
        s.refresh_needed = true;
    });
}

/* C: void to_last_line(void)
 * Move to the last line of the file. */
pub fn to_last_line() {
    let inhelp = with_state(|s| s.inhelp);
    let last_x = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.filebot.as_ref())
            .map(|lb| if inhelp { 0 } else { lb.borrow().data.len() })
            .unwrap_or(0)
    });

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = of.filebot.clone();
            of.current_x = last_x;
        }
    });

    let pww = xplustabs();
    let editwinrows = with_state(|s| s.editwinrows);

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = pww;
            // Set the last line of the screen as the target for the cursor.
            of.cursor_row = (editwinrows - 1) as isize;
        }
        s.refresh_needed = true;
        #[cfg(feature = "color")]
        { s.recook |= s.perturbed; }
        s.focusing = false;
    });
}

/* C: void get_edge_and_target(size_t *leftedge, size_t *target_column)
 * Determine the actual current chunk and the target column. */
pub fn get_edge_and_target(leftedge: &mut usize, target_column: &mut usize) {
    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(SOFTWRAP) {
            let (tabsize, editwincols) = with_state(|s| (s.tabsize as usize, s.editwincols as usize));
            let shim = editwincols * (1 + (tabsize / editwincols));
            let col = xplustabs();
            let current_lp = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            if let Some(ref lp) = current_lp {
                *leftedge = leftedge_for(col, lp);
                let pww = with_state(|s| s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0));
                *target_column = (pww + shim - *leftedge) % editwincols;
            } else {
                *leftedge = 0;
                *target_column = with_state(|s| s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0));
            }
            return;
        }
    }
    *leftedge = 0;
    *target_column = with_state(|s| s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0));
}

/* C: size_t proper_x(linestruct *line, size_t *leftedge, bool forward,
 *                    size_t column, bool *shifted)
 * Return the byte index in line->data that corresponds to the given column on
 * the chunk starting at *leftedge.  If the target column has landed on a tab,
 * prevent row-boundary issues by incrementing the index. */
pub fn proper_x(
    line: &LinePtr,
    leftedge: &mut usize,
    forward: bool,
    column: usize,
    shifted: Option<&mut bool>,
) -> usize {
    let data = line.borrow().data.clone();
    let mut index = actual_x(&data, column);

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(SOFTWRAP) {
            let tabsize = with_state(|s| s.tabsize as usize);
            let editwincols = with_state(|s| s.editwincols as usize);

            if data.as_bytes().get(index).copied() == Some(b'\t') {
                let w = wideness(&data, index);
                let crossed_boundary = if forward {
                    w < *leftedge
                } else {
                    column / tabsize == (*leftedge - 1) / tabsize
                        && column / tabsize < (*leftedge + editwincols - 1) / tabsize
                };
                if crossed_boundary {
                    index += 1;
                    if let Some(sh) = shifted {
                        *sh = true;
                    }
                }
            }

            *leftedge = leftedge_for(wideness(&data, index), line);
        }
    }

    index
}

/* C: void set_proper_index_and_pww(size_t *leftedge, size_t target, bool forward)
 * Adjust current_x and placewewant in case we landed in the middle of a tab
 * that crosses a row boundary. */
pub fn set_proper_index_and_pww(leftedge: &mut usize, target: usize, forward: bool) {
    let was_edge = *leftedge;
    let mut shifted = false;

    let current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if let Some(ref lp) = current_lp {
        let new_x = proper_x(lp, leftedge, forward, actual_last_column(*leftedge, target), Some(&mut shifted));
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });

        // If the index was incremented, try going to the target column again.
        if shifted || *leftedge < was_edge {
            let mut shifted2 = false;
            let current_lp2 = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
            if let Some(ref lp2) = current_lp2 {
                let new_x2 = proper_x(lp2, leftedge, forward, actual_last_column(*leftedge, target), Some(&mut shifted2));
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current_x = new_x2;
                    }
                });
            }
        }

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.placewewant = *leftedge + target;
            }
        });
    }
}

/* C: void do_page_up(void)
 * Move up almost one screenful. */
pub fn do_page_up() {
    let editwinrows = with_state(|s| s.editwinrows);
    let mustmove: i32 = if editwinrows < 3 { 1 } else { editwinrows - 2 };
    let mut leftedge: usize = 0;
    let mut target_column: usize = 0;

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(JUMPY_SCROLLING) {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = of.edittop.clone();
                    of.cursor_row = 0;
                }
            });
            leftedge = with_state(|s| s.openfile.as_ref().map(|of| of.firstcolumn).unwrap_or(0));
            target_column = 0;
        } else {
            get_edge_and_target(&mut leftedge, &mut target_column);
        }
    }
    #[cfg(feature = "tiny")]
    {
        get_edge_and_target(&mut leftedge, &mut target_column);
    }

    // Move up the required number of lines/chunks.  If we can't, go to top.
    let mut current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if go_back_chunks(mustmove, &mut current_lp, &mut leftedge) > 0 {
        to_first_line();
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = current_lp;
        }
    });

    set_proper_index_and_pww(&mut leftedge, target_column, false);

    // Move the viewport so that the cursor stays immobile, if possible.
    adjust_viewport(UpdateType::Stationary);
    with_state_mut(|s| s.refresh_needed = true);
}

/* C: void do_page_down(void)
 * Move down almost one screenful. */
pub fn do_page_down() {
    let editwinrows = with_state(|s| s.editwinrows);
    let mustmove: i32 = if editwinrows < 3 { 1 } else { editwinrows - 2 };
    let mut leftedge: usize = 0;
    let mut target_column: usize = 0;

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(JUMPY_SCROLLING) {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = of.edittop.clone();
                    of.cursor_row = 0;
                }
            });
            leftedge = with_state(|s| s.openfile.as_ref().map(|of| of.firstcolumn).unwrap_or(0));
            target_column = 0;
        } else {
            get_edge_and_target(&mut leftedge, &mut target_column);
        }
    }
    #[cfg(feature = "tiny")]
    {
        get_edge_and_target(&mut leftedge, &mut target_column);
    }

    // Move down the required number of lines/chunks.  If we can't, go to bottom.
    let mut current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if go_forward_chunks(mustmove, &mut current_lp, &mut leftedge) > 0 {
        to_last_line();
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = current_lp;
        }
    });

    set_proper_index_and_pww(&mut leftedge, target_column, true);

    // Move the viewport so that the cursor stays immobile, if possible.
    adjust_viewport(UpdateType::Stationary);
    with_state_mut(|s| s.refresh_needed = true);
}

/* C: void to_top_row(void) — #ifndef NANO_TINY
 * Place the cursor on the first row in the viewport. */
#[cfg(not(feature = "tiny"))]
pub fn to_top_row() {
    let mut leftedge: usize = 0;
    let mut offset: usize = 0;
    get_edge_and_target(&mut leftedge, &mut offset);

    let edittop_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.edittop.clone()));
    let firstcolumn = with_state(|s| s.openfile.as_ref().map(|of| of.firstcolumn).unwrap_or(0));

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = edittop_lp;
        }
    });
    leftedge = firstcolumn;

    set_proper_index_and_pww(&mut leftedge, offset, false);

    let has_mark = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
    });
    with_state_mut(|s| s.refresh_needed = has_mark);
}

/* C: void to_bottom_row(void) — #ifndef NANO_TINY
 * Place the cursor on the last row in the viewport, when possible. */
#[cfg(not(feature = "tiny"))]
pub fn to_bottom_row() {
    let mut leftedge: usize = 0;
    let mut offset: usize = 0;
    get_edge_and_target(&mut leftedge, &mut offset);

    let edittop_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.edittop.clone()));
    let firstcolumn = with_state(|s| s.openfile.as_ref().map(|of| of.firstcolumn).unwrap_or(0));
    let editwinrows = with_state(|s| s.editwinrows);

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = edittop_lp;
        }
    });
    leftedge = firstcolumn;

    let mut current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    go_forward_chunks(editwinrows - 1, &mut current_lp, &mut leftedge);
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = current_lp;
        }
    });

    set_proper_index_and_pww(&mut leftedge, offset, true);

    let has_mark = with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
    });
    with_state_mut(|s| s.refresh_needed = has_mark);
}

/* C: void do_cycle(void) — #ifndef NANO_TINY
 * Put the cursor line at the center, then the top, then the bottom. */
#[cfg(not(feature = "tiny"))]
pub fn do_cycle() {
    let cycling_aim = with_state(|s| s.cycling_aim);
    if cycling_aim == 0 {
        adjust_viewport(UpdateType::Centering);
    } else {
        let editwinrows = with_state(|s| s.editwinrows);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.cursor_row = if cycling_aim == 1 { 0 } else { (editwinrows - 1) as isize };
            }
        });
        adjust_viewport(UpdateType::Stationary);
    }

    with_state_mut(|s| s.cycling_aim = (cycling_aim + 1) % 3);

    draw_all_subwindows();
    full_refresh();
}

/* C: void do_center(void) — #ifndef NANO_TINY
 * Scroll the line with the cursor to the center of the screen. */
#[cfg(not(feature = "tiny"))]
pub fn do_center() {
    adjust_viewport(UpdateType::Centering);
    draw_all_subwindows();
    full_refresh();
}

/* C: void do_para_begin(linestruct **line) — #ifdef ENABLE_JUSTIFY
 * Move to the first beginning of a paragraph before the current line.
 * C signature takes linestruct**; we take a LinePtr by value and return the new one. */
#[cfg(feature = "justify")]
pub fn do_para_begin(line: LinePtr) -> LinePtr {
    // Step back one line first if possible.
    let mut cur = {
        let prev_opt = line.borrow().prev.as_ref().and_then(|w| w.upgrade());
        match prev_opt {
            Some(prev) => prev,
            None => return line,
        }
    };

    // Walk backward until we reach the beginning of a paragraph.
    loop {
        if begpar(&cur, 0) {
            break;
        }
        let prev_opt = cur.borrow().prev.as_ref().and_then(|w| w.upgrade());
        match prev_opt {
            Some(prev) => cur = prev,
            None => break,
        }
    }
    cur
}

/* C: void do_para_end(linestruct **line) — #ifdef ENABLE_JUSTIFY
 * Move down to the last line of the first found paragraph. */
#[cfg(feature = "justify")]
pub fn do_para_end(line: LinePtr) -> LinePtr {
    let mut cur = line;

    // Skip forward until we enter a paragraph.
    loop {
        let has_next = cur.borrow().next.is_some();
        if !has_next || inpar(&cur) {
            break;
        }
        let next = cur.borrow().next.clone().unwrap();
        cur = next;
    }

    // Walk forward while still in the same paragraph (not at a new begpar).
    loop {
        let next_opt = cur.borrow().next.clone();
        match next_opt {
            Some(ref nxt) => {
                if !inpar(nxt) || begpar(nxt, 0) {
                    break;
                }
                cur = nxt.clone();
            }
            None => break,
        }
    }
    cur
}

/* C: void to_para_begin(void) — #ifdef ENABLE_JUSTIFY
 * Move up to first start of a paragraph before the current line. */
#[cfg(feature = "justify")]
pub fn to_para_begin() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if let Some(_wc) = was_current.clone() {
        let new_line = do_para_begin(with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.current.clone()).expect("a current line")
        }));
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(new_line);
                of.current_x = 0;
            }
        });
        if let Some(wc_lp) = was_current {
            edit_redraw(&wc_lp, UpdateType::Centering);
        }
    }
}

/* C: void to_para_end(void) — #ifdef ENABLE_JUSTIFY
 * Move down to just after the first found end of a paragraph. */
#[cfg(feature = "justify")]
pub fn to_para_end() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));

    let new_line = do_para_end(with_state(|s| {
        s.openfile.as_ref().and_then(|of| of.current.clone()).expect("a current line")
    }));

    // Step beyond the last line of the paragraph, if possible;
    // otherwise, move to the end of the line.
    let next_opt = new_line.borrow().next.clone();
    let (final_line, final_x) = if let Some(next) = next_opt {
        (next, 0usize)
    } else {
        let len = new_line.borrow().data.len();
        (new_line.clone(), len)
    };

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = Some(final_line);
            of.current_x = final_x;
        }
    });

    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Centering);
    }
    #[cfg(feature = "color")]
    with_state_mut(|s| { s.recook |= s.perturbed; });
}

/* C: void to_prev_block(void) — #ifndef NANO_TINY
 * Move to the preceding block of text. */
pub fn to_prev_block() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));

    let mut is_text = false;
    let mut seen_text = false;

    // Skip backward until first blank line after some nonblank line(s).
    loop {
        // Check if there's a prev line.
        let prev_opt = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
        });
        if prev_opt.is_none() || (seen_text && !is_text) {
            break;
        }
        let prev = prev_opt.unwrap();
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(prev);
            }
        });
        is_text = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|lp| !white_string(&lp.borrow().data))
                .unwrap_or(false)
        });
        seen_text = seen_text || is_text;
    }

    // Step forward one line again if we passed text but this line is blank.
    let (current_is_blank, has_next) = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|lp| {
                let data = lp.borrow().data.clone();
                let next = lp.borrow().next.is_some();
                (white_string(&data), next)
            })
            .unwrap_or((false, false))
    });
    if seen_text && has_next && current_is_blank {
        let next_lp = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .and_then(|lp| lp.borrow().next.clone())
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = next_lp;
            }
        });
    }

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current_x = 0;
        }
    });

    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Centering);
    }
}

/* C: void to_next_block(void) — #ifndef NANO_TINY
 * Move to the next block of text. */
pub fn to_next_block() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));

    let mut is_white = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|lp| white_string(&lp.borrow().data))
            .unwrap_or(false)
    });
    let mut seen_white = is_white;

    // Skip forward until first nonblank line after some blank line(s).
    loop {
        let has_next = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|lp| lp.borrow().next.is_some())
                .unwrap_or(false)
        });
        if !has_next || (seen_white && !is_white) {
            break;
        }
        let next_lp = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .and_then(|lp| lp.borrow().next.clone())
        });
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = next_lp;
            }
        });
        is_white = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|lp| white_string(&lp.borrow().data))
                .unwrap_or(false)
        });
        seen_white = seen_white || is_white;
    }

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current_x = 0;
        }
    });

    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Centering);
    }
    #[cfg(feature = "color")]
    with_state_mut(|s| { s.recook |= s.perturbed; });
}

/* C: void do_prev_word(void)
 * Move to the previous word. */
pub fn do_prev_word() {
    let punctuation_as_letters = ISSET!(WORD_BOUNDS);
    let mut seen_a_word = false;
    let mut step_forward = false;

    // Move backward until we pass over the start of a word.
    loop {
        let current_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));

        // If at the head of a line, move to the end of the preceding one.
        if current_x == 0 {
            let has_prev = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
                    .is_some()
            });
            if !has_prev {
                break;
            }
            let prev_lp = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
            });
            let prev_len = prev_lp.as_ref().map(|lp| lp.borrow().data.len()).unwrap_or(0);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = prev_lp;
                    of.current_x = prev_len;
                }
            });
        }

        // Step back one character.
        let (data, cur_x) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
        });
        let new_x = step_left(&data, cur_x);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });

        let (data2, cur_x2) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
        });

        if is_word_char(&data2[cur_x2..], punctuation_as_letters) {
            seen_a_word = true;
            // If at the head of a line now, this surely is a word start.
            if cur_x2 == 0 {
                break;
            }
        } else {
            #[cfg(feature = "utf8")]
            if is_zerowidth(&data2[cur_x2..]) {
                // Zero-width character: skip.
                continue;
            }
            if seen_a_word {
                // This is space now: we've overshot the start of the word.
                step_forward = true;
                break;
            }
        }
    }

    if step_forward {
        // Move one character forward again to sit on the start of the word.
        let (data, cur_x) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
        });
        let new_x = step_right(&data, cur_x);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });
    }
}

/* C: bool do_next_word(bool after_ends)
 * Move to the next word.  If after_ends is TRUE, stop at the ends of words
 * instead of at their beginnings.  Return TRUE if we started on a word. */
pub fn do_next_word(after_ends: bool) -> bool {
    let punctuation_as_letters = ISSET!(WORD_BOUNDS);
    let (data0, cur_x0) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
    });
    let started_on_word = is_word_char(&data0[cur_x0..], punctuation_as_letters);
    let mut seen_space = !started_on_word;
    #[cfg(not(feature = "tiny"))]
    let mut seen_word = started_on_word;

    // Move forward until we reach the start of a word.
    loop {
        let (data, cur_x) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
        });

        // If at the end of a line, move to the beginning of the next one.
        if data.as_bytes().get(cur_x).copied() == Some(0) || cur_x >= data.len() {
            // When at end of file, stop.
            let has_next = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .map(|lp| lp.borrow().next.is_some())
                    .unwrap_or(false)
            });
            if !has_next {
                break;
            }
            let next_lp = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|lp| lp.borrow().next.clone())
            });
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = next_lp;
                    of.current_x = 0;
                }
            });
            seen_space = true;
        } else {
            // Step forward one character.
            let new_x = step_right(&data, cur_x);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current_x = new_x;
                }
            });
        }

        let (data2, cur_x2) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
        });

        #[cfg(not(feature = "tiny"))]
        {
            if after_ends {
                // If this is a word character, continue; else it's a separator,
                // and if we've already seen a word, then it's a word end.
                if is_word_char(&data2[cur_x2..], punctuation_as_letters) {
                    seen_word = true;
                } else {
                    #[cfg(feature = "utf8")]
                    if is_zerowidth(&data2[cur_x2..]) {
                        continue;
                    }
                    if seen_word {
                        break;
                    }
                }
                continue;
            }
        }

        // Not after_ends path:
        {
            #[cfg(feature = "utf8")]
            if is_zerowidth(&data2[cur_x2..]) {
                continue;
            }
            // If this is not a word character, then it's a separator;
            // else if we've already seen a separator, then it's a word start.
            if !is_word_char(&data2[cur_x2..], punctuation_as_letters) {
                seen_space = true;
            } else if seen_space {
                break;
            }
        }
    }

    started_on_word
}

/* C: void to_prev_word(void)
 * Move to the previous word in the file, and update the screen afterwards. */
pub fn to_prev_word() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    do_prev_word();
    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }
}

/* C: void to_next_word(void)
 * Move to the next word in the file.  If the AFTER_ENDS flag is set, stop
 * at word ends instead of beginnings.  Update the screen afterwards. */
pub fn to_next_word() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    let after_ends = ISSET!(AFTER_ENDS);
    do_next_word(after_ends);
    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }
}

/* C: void do_home(void)
 * Move to the beginning of the current line (or softwrapped chunk).
 * When enabled, do a smart home.  When softwrapping, go to the beginning
 * of the full line when already at the start of a chunk. */
pub fn do_home() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    let was_column = xplustabs();
    let mut moved_off_chunk = true;

    #[cfg(not(feature = "tiny"))]
    {
        let mut moved = false;
        let mut leftedge: usize = 0;
        let mut left_x: usize = 0;

        if ISSET!(SOFTWRAP) {
            let current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
            if let Some(ref lp) = current_lp {
                leftedge = leftedge_for(was_column, lp);
                let leftedge_val = leftedge;
                left_x = proper_x(lp, &mut leftedge, false, leftedge_val, None);
            }
        }

        if ISSET!(SMART_HOME) {
            let (data, cur_x) = with_state(|s| {
                let of = s.openfile.as_ref().expect("an open buffer");
                (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
            });
            let indent_x = indent_length(&data);

            if !data[indent_x..].is_empty() {
                // If we're exactly on the indent, move fully home.  Otherwise,
                // when not softwrapping or not after the first nonblank chunk,
                // move to the first nonblank character.
                if cur_x == indent_x {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.current_x = 0;
                        }
                    });
                    moved = true;
                } else if left_x <= indent_x {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.current_x = indent_x;
                        }
                    });
                    moved = true;
                }
            }
        }

        if !moved && ISSET!(SOFTWRAP) {
            let cur_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));
            // If already at the left edge of the screen, move fully home.
            // Otherwise, move to the left edge.
            if cur_x == left_x {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current_x = 0;
                    }
                });
            } else {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.current_x = left_x;
                        of.placewewant = leftedge;
                    }
                });
                moved_off_chunk = false;
            }
        } else if !moved {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current_x = 0;
                }
            });
        }
    }

    #[cfg(feature = "tiny")]
    {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = 0;
            }
        });
    }

    if moved_off_chunk {
        let pww = xplustabs();
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.placewewant = pww;
            }
        });
    }

    // If we changed chunk, we might be offscreen.  Otherwise,
    // update current if the mark is on or we changed "page".
    let softwrap_on = ISSET!(SOFTWRAP);
    if softwrap_on && moved_off_chunk {
        if let Some(wc_lp) = was_current {
            edit_redraw(&wc_lp, UpdateType::Flowing);
        }
    } else {
        let pww = with_state(|s| s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0));
        if line_needs_update(was_column, pww) {
            let current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
            let cur_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));
            if let Some(ref lp) = current_lp {
                update_line(lp, cur_x);
            }
        }
    }
}

/* C: void do_end(void)
 * Move to the end of the current line (or softwrapped chunk).
 * When softwrapping and already at the end of a chunk, go to the
 * end of the full line. */
pub fn do_end() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    let was_column = xplustabs();
    let line_len = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|lp| lp.borrow().data.len())
            .unwrap_or(0)
    });
    let mut moved_off_chunk = true;

    #[cfg(not(feature = "tiny"))]
    {
        if ISSET!(SOFTWRAP) {
            let mut kickoff = true;
            let mut last_chunk = false;
            let current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
            if let Some(ref lp) = current_lp {
                let data = lp.borrow().data.clone();
                let leftedge = leftedge_for(was_column, lp);
                let mut rightedge = get_softwrap_breakpoint(&data, leftedge, &mut kickoff, &mut last_chunk);

                // If on last chunk, we're already at end of line.
                // Otherwise, one column past the end — shift back one.
                if !last_chunk {
                    rightedge = rightedge.saturating_sub(1);
                }

                let right_x = actual_x(&data, rightedge);
                let cur_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));

                // If already at the right edge of the screen, move fully to
                // the end of the line.  Otherwise, move to the right edge.
                if cur_x == right_x {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.current_x = line_len;
                        }
                    });
                } else {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.current_x = right_x;
                            of.placewewant = rightedge;
                        }
                    });
                    moved_off_chunk = false;
                }
            }
        } else {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current_x = line_len;
                }
            });
        }
    }

    #[cfg(feature = "tiny")]
    {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = line_len;
            }
        });
    }

    if moved_off_chunk {
        let pww = xplustabs();
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.placewewant = pww;
            }
        });
    }

    // If we changed chunk, we might be offscreen.  Otherwise,
    // update current if the mark is on or we changed "page".
    let softwrap_on = ISSET!(SOFTWRAP);
    if softwrap_on && moved_off_chunk {
        if let Some(wc_lp) = was_current {
            edit_redraw(&wc_lp, UpdateType::Flowing);
        }
    } else {
        let pww = with_state(|s| s.openfile.as_ref().map(|of| of.placewewant).unwrap_or(0));
        if line_needs_update(was_column, pww) {
            let current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
            let cur_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));
            if let Some(ref lp) = current_lp {
                update_line(lp, cur_x);
            }
        }
    }
}

/* C: void do_up(void)
 * Move the cursor to the preceding line or chunk. */
pub fn do_up() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    let mut leftedge: usize = 0;
    let mut target_column: usize = 0;

    get_edge_and_target(&mut leftedge, &mut target_column);

    // If we can't move up one line or chunk, we're at top of file.
    let mut current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if go_back_chunks(1, &mut current_lp, &mut leftedge) > 0 {
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = current_lp;
        }
    });

    set_proper_index_and_pww(&mut leftedge, target_column, false);

    let cursor_row = with_state(|s| s.openfile.as_ref().map(|of| of.cursor_row).unwrap_or(0));
    let jumpy = ISSET!(JUMPY_SCROLLING);
    let softwrap = ISSET!(SOFTWRAP);
    let tabsize = with_state(|s| s.tabsize as usize);
    let editwincols = with_state(|s| s.editwincols as usize);

    if cursor_row == 0 && !jumpy && (tabsize < editwincols || !softwrap) {
        edit_scroll(BACKWARD);
    } else if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }

    // <Up> should not change placewewant, so restore it.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = leftedge + target_column;
        }
    });
}

/* C: void do_down(void)
 * Move the cursor to the next line or chunk. */
pub fn do_down() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    let mut leftedge: usize = 0;
    let mut target_column: usize = 0;

    get_edge_and_target(&mut leftedge, &mut target_column);

    // If we can't move down one line or chunk, we're at bottom of file.
    let mut current_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
    if go_forward_chunks(1, &mut current_lp, &mut leftedge) > 0 {
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = current_lp;
        }
    });

    set_proper_index_and_pww(&mut leftedge, target_column, true);

    let cursor_row = with_state(|s| s.openfile.as_ref().map(|of| of.cursor_row).unwrap_or(0));
    let editwinrows = with_state(|s| s.editwinrows);
    let jumpy = ISSET!(JUMPY_SCROLLING);
    let softwrap = ISSET!(SOFTWRAP);
    let tabsize = with_state(|s| s.tabsize as usize);
    let editwincols = with_state(|s| s.editwincols as usize);

    if cursor_row == (editwinrows - 1) as isize && !jumpy && (tabsize < editwincols || !softwrap) {
        edit_scroll(FORWARD);
    } else if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }

    // <Down> should not change placewewant, so restore it.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = leftedge + target_column;
        }
    });
}

/* C: void do_scroll_up(void) — #if !defined(NANO_TINY) || defined(ENABLE_HELP)
 * Scroll up one line or chunk without moving the cursor textwise. */
#[cfg(any(not(feature = "tiny"), feature = "help"))]
pub fn do_scroll_up() {
    // When the top of the file is onscreen, we can't scroll.
    let (edittop_has_no_prev, firstcolumn_zero) = with_state(|s| {
        s.openfile.as_ref().map(|of| {
            let no_prev = of.edittop.as_ref()
                .map(|lp| lp.borrow().prev.is_none())
                .unwrap_or(true);
            (no_prev, of.firstcolumn == 0)
        }).unwrap_or((true, true))
    });
    if edittop_has_no_prev && firstcolumn_zero {
        return;
    }

    let cursor_row = with_state(|s| s.openfile.as_ref().map(|of| of.cursor_row).unwrap_or(0));
    let editwinrows = with_state(|s| s.editwinrows);
    if cursor_row == (editwinrows - 1) as isize {
        do_up();
    }

    let editwinrows = with_state(|s| s.editwinrows);
    if editwinrows > 1 {
        edit_scroll(BACKWARD);
    }
}

/* C: void do_scroll_down(void) — #if !defined(NANO_TINY) || defined(ENABLE_HELP)
 * Scroll down one line or chunk without moving the cursor textwise. */
#[cfg(any(not(feature = "tiny"), feature = "help"))]
pub fn do_scroll_down() {
    let cursor_row = with_state(|s| s.openfile.as_ref().map(|of| of.cursor_row).unwrap_or(0));
    if cursor_row == 0 {
        do_down();
    }

    let editwinrows = with_state(|s| s.editwinrows);
    let has_next_line = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.edittop.as_ref())
            .map(|lp| lp.borrow().next.is_some())
            .unwrap_or(false)
    });

    #[cfg(not(feature = "tiny"))]
    let softwrap_extra = if ISSET!(SOFTWRAP) {
        with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.edittop.as_ref().map(|lp| {
                    let extra = extra_chunks_in(lp);
                    let chunk = chunk_for(of.firstcolumn, lp);
                    extra > chunk
                }))
                .unwrap_or(false)
        })
    } else {
        false
    };
    #[cfg(feature = "tiny")]
    let softwrap_extra = false;

    if editwinrows > 1 && (has_next_line || softwrap_extra) {
        edit_scroll(FORWARD);
    }
}

/* C: void do_left(void)
 * Move left one character. */
pub fn do_left() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));

    let cur_x = with_state(|s| s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0));
    if cur_x > 0 {
        let data = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|lp| lp.borrow().data.clone())
                .unwrap_or_default()
        });
        let mut new_x = step_left(&data, cur_x);

        #[cfg(feature = "utf8")]
        {
            use crate::chars::is_zerowidth;
            while new_x > 0 && is_zerowidth(&data[new_x..]) {
                new_x = step_left(&data, new_x);
            }
        }

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });
    } else {
        // If we're not already at the top of the file, move to the end
        // of the previous line.
        let is_filetop = with_state(|s| {
            s.openfile.as_ref().map(|of| {
                match (&of.current, &of.filetop) {
                    (Some(c), Some(ft)) => std::rc::Rc::ptr_eq(c, ft),
                    _ => true,
                }
            }).unwrap_or(true)
        });
        if !is_filetop {
            let prev_lp = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
            });
            let prev_len = prev_lp.as_ref().map(|lp| lp.borrow().data.len()).unwrap_or(0);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = prev_lp;
                    of.current_x = prev_len;
                }
            });
        }
    }

    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }
}

/* C: void do_right(void)
 * Move right one character. */
pub fn do_right() {
    let was_current = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));

    let (data, cur_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
    });

    // If there's a character at the current position, step over it.
    if cur_x < data.len() {
        let mut new_x = step_right(&data, cur_x);

        #[cfg(feature = "utf8")]
        {
            use crate::chars::is_zerowidth;
            while new_x < data.len() && is_zerowidth(&data[new_x..]) {
                new_x = step_right(&data, new_x);
            }
        }

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });
    } else {
        // If we're not already at the bottom of the file, move to the
        // beginning of the next line.
        let is_filebot = with_state(|s| {
            s.openfile.as_ref().map(|of| {
                match (&of.current, &of.filebot) {
                    (Some(c), Some(fb)) => std::rc::Rc::ptr_eq(c, fb),
                    _ => true,
                }
            }).unwrap_or(true)
        });
        if !is_filebot {
            let next_lp = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|lp| lp.borrow().next.clone())
            });
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = next_lp;
                    of.current_x = 0;
                }
            });
        }
    }

    if let Some(wc_lp) = was_current {
        edit_redraw(&wc_lp, UpdateType::Flowing);
    }
}

/* C: void do_scroll_left(void) — #ifndef NANO_TINY
 * Scroll the viewport horizontally to the left. */
#[cfg(not(feature = "tiny"))]
pub fn do_scroll_left() {
    if ISSET!(SOFTWRAP) || ISSET!(SOLO_SIDESCROLL) {
        let flag_str = if ISSET!(SOFTWRAP) { "--softwrap" } else { "--solo" };
        // TRANSLATORS: The %s is the name of an option.
        statusline(
            MessageType::Ahem,
            &format!("Not possible with '{}'", flag_str),
        );
        return;
    }

    let tabsize = with_state(|s| s.tabsize as usize);
    let brink = with_state(|s| s.openfile.as_ref().map(|of| of.brink).unwrap_or(0));
    let step = if brink < tabsize { brink } else if tabsize < 2 { 2 } else { tabsize };

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.brink = of.brink.saturating_sub(step);
        }
    });

    let (brink2, editwincols) = with_state(|s| (
        s.openfile.as_ref().map(|of| of.brink).unwrap_or(0),
        s.editwincols as usize,
    ));

    let (data, cur_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
    });

    let frame_x = actual_x(&data, brink2 + editwincols.saturating_sub(CUSHION + 1));

    if cur_x > frame_x {
        let pww = wideness(&data, frame_x);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = frame_x;
                of.placewewant = pww;
            }
        });
    }

    with_state_mut(|s| s.refresh_needed = true);
}

/* C: void do_scroll_right(void) — #ifndef NANO_TINY
 * Scroll the viewport horizontally to the right. */
#[cfg(not(feature = "tiny"))]
pub fn do_scroll_right() {
    if ISSET!(SOFTWRAP) || ISSET!(SOLO_SIDESCROLL) {
        let flag_str = if ISSET!(SOFTWRAP) { "--softwrap" } else { "--solo" };
        statusline(
            MessageType::Ahem,
            &format!("Not possible with '{}'", flag_str),
        );
        return;
    }

    let tabsize = with_state(|s| s.tabsize as usize);
    let editwinrows = with_state(|s| s.editwinrows);

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.brink += if tabsize < 2 { 2 } else { tabsize };
        }
    });

    let (brink, edittop_lineno) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        (
            of.brink,
            of.edittop.as_ref().map(|lp| lp.borrow().lineno).unwrap_or(0),
        )
    });

    let sill = edittop_lineno + editwinrows as isize;

    // If the current line does not allow further scrolling, seek
    // in the viewport an earlier or later line that does allow it.
    // We collect the candidate line ptr rather than mutating mid-walk.
    let new_current = {
        let start_lp = with_state(|s| s.openfile.as_ref().and_then(|of| of.current.clone()));
        let mut candidate = start_lp.clone();

        // Walk backward while current line is too short.
        loop {
            let (is_short, _has_prev, not_edittop) = with_state(|s| {
                let lp_short = candidate.as_ref()
                    .map(|lp| breadth(&lp.borrow().data) < brink + CUSHION)
                    .unwrap_or(false);
                let has_prev_lp = candidate.as_ref()
                    .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
                    .is_some();
                let edittop_lp = s.openfile.as_ref().and_then(|of| of.edittop.clone());
                let is_edittop = match (&candidate, &edittop_lp) {
                    (Some(c), Some(e)) => std::rc::Rc::ptr_eq(c, e),
                    _ => true,
                };
                (lp_short, has_prev_lp, !is_edittop)
            });
            if !(not_edittop && is_short) {
                break;
            }
            let prev_lp = with_state(|_s| {
                candidate.as_ref()
                    .and_then(|lp| lp.borrow().prev.as_ref()?.upgrade())
            });
            candidate = prev_lp;
        }

        // Walk forward while line is too short and line number is within viewport.
        loop {
            let (is_short, has_next, lineno_ok) = with_state(|_s| {
                let lp_short = candidate.as_ref()
                    .map(|lp| breadth(&lp.borrow().data) < brink + CUSHION)
                    .unwrap_or(false);
                let next = candidate.as_ref().and_then(|lp| lp.borrow().next.clone());
                let has_next_lp = next.is_some();
                let lineno = candidate.as_ref().map(|lp| lp.borrow().lineno).unwrap_or(0);
                (lp_short, has_next_lp, lineno < sill)
            });
            if !(lineno_ok && is_short && has_next) {
                break;
            }
            let next_lp = with_state(|_s| {
                candidate.as_ref().and_then(|lp| lp.borrow().next.clone())
            });
            candidate = next_lp;
        }

        // Use candidate only if it's within the viewport and wide enough.
        let (fits, in_view) = with_state(|_s| {
            let lineno = candidate.as_ref().map(|lp| lp.borrow().lineno).unwrap_or(sill);
            let wide_enough = candidate.as_ref()
                .map(|lp| breadth(&lp.borrow().data) >= brink + CUSHION)
                .unwrap_or(false);
            (wide_enough, lineno < sill)
        });
        if fits && in_view {
            candidate
        } else {
            start_lp
        }
    };

    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.current = new_current;
        }
    });

    let (data, cur_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        (of.current.as_ref().expect("a current line").borrow().data.clone(), of.current_x)
    });

    let frame_x = actual_x(&data, brink + CUSHION);

    if cur_x < frame_x {
        let pww = wideness(&data, frame_x);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = frame_x;
                of.placewewant = pww;
            }
        });
    }

    with_state_mut(|s| s.refresh_needed = true);
}
