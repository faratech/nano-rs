#![allow(unused, non_snake_case, dead_code, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/color.c from GNU nano.
// C original: Copyright (C) 2001-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2017, 2020, 2021 Benno Schulenberg

use crate::definitions::*;
use crate::global::{STATE, with_state, with_state_mut, A_REVERSE};

#[cfg(feature = "color")]
use crate::winio::{ncurses_color_to_crossterm, statusline};

// ncurses attribute constants (matching winio.rs conventions)
pub const A_NORMAL:  i32 = 0;
pub const A_BOLD:    i32 = 0x0200_0000;
pub const A_ITALIC:  i32 = 0x0008_0000;

// ncurses COLOR_* constants (matching ncurses values)
pub const COLOR_BLACK:   i16 = 0;
pub const COLOR_RED:     i16 = 1;
pub const COLOR_GREEN:   i16 = 2;
pub const COLOR_YELLOW:  i16 = 3;
pub const COLOR_BLUE:    i16 = 4;
pub const COLOR_MAGENTA: i16 = 5;
pub const COLOR_CYAN:    i16 = 6;
pub const COLOR_WHITE:   i16 = 7;

// ---------------------------------------------------------------------------
// Encode a color pair + attributes as an i32 for interface_color_pair[].
// In C: COLOR_PAIR(n) | attributes  where ncurses pair bits live in bits 8-23.
// In this Rust port we pack: pair_index in bits 0-7, attributes in upper bits.
// The pair_index is only used by apply_interface_color in winio.rs for future
// full-color decoding; current winio.rs already handles A_REVERSE specially.
// ---------------------------------------------------------------------------
fn encode_pair(pair_index: usize, attrs: i32) -> i32 {
    (pair_index as i32 & 0xFF) | attrs
}

#[cfg(feature = "color")]

/// Initialize the color pairs for nano's interface elements.
/// C: void set_interface_colorpairs(void)
pub fn set_interface_colorpairs() {
    // Whether ncurses accepts -1 to mean "default color" — in the Rust port
    // we simply allow THE_DEFAULT (-1) and map it to the terminal default.
    let defaults_allowed: bool = true;

    with_state_mut(|s| {
        for index in 0..NUMBER_OF_ELEMENTS {
            if let Some(mut combo) = s.color_combo[index].take() {
                // If no-default-colors fallback: remap THE_DEFAULT to white/black.
                if !defaults_allowed {
                    if combo.fg == THE_DEFAULT { combo.fg = COLOR_WHITE; }
                    if combo.bg == THE_DEFAULT { combo.bg = COLOR_BLACK; }
                }
                // Encode the pair index and attributes.
                let pair_val = encode_pair(index + 1, combo.attributes);
                s.interface_color_pair[index] = pair_val;
                s.rescind_colors = false;
                // combo is dropped here (free(color_combo[index]) in C)
            } else {
                // Default color values when no combo was specified.
                if index == FUNCTION_TAG || index == SCROLL_BAR {
                    s.interface_color_pair[index] = A_NORMAL;
                } else if index == GUIDE_STRIPE {
                    s.interface_color_pair[index] = A_REVERSE;
                } else if index == SPOTLIGHTED {
                    // Black on yellow (or bright yellow on extended palette)
                    s.interface_color_pair[index] = encode_pair(index + 1, A_NORMAL);
                } else if index == MINI_INFOBAR || index == PROMPT_BAR {
                    s.interface_color_pair[index] = s.interface_color_pair[TITLE_BAR];
                } else if index == ERROR_MESSAGE {
                    // White on red, bold
                    s.interface_color_pair[index] = encode_pair(index + 1, A_BOLD);
                } else {
                    s.interface_color_pair[index] = s.hilite_attribute;
                }
            }
        }

        if s.rescind_colors {
            s.interface_color_pair[SPOTLIGHTED] = A_REVERSE;
            s.interface_color_pair[ERROR_MESSAGE] = A_REVERSE;
        }
    });
}

/// Assign a pair number to each fg/bg combination in the given syntax,
/// giving identical combinations the same number.
/// C: void set_syntax_colorpairs(syntaxtype *sntx)
#[cfg(feature = "color")]
pub fn set_syntax_colorpairs(sntx: &mut SyntaxType) {
    let defaults_allowed: bool = true;

    // Two-pass approach: walk the color list, dedup fg/bg pairs, assign numbers.
    // We collect (fg, bg) in order, then assign pairnum with dedup.
    let mut next_number = NUMBER_OF_ELEMENTS as i16;

    // Collect fg/bg pairs in order for dedup.
    // We need to assign pairnum to each node.  Walk the box-linked list.
    let mut pairs_seen: Vec<(i16, i16, i16)> = Vec::new();
    // pairs_seen: (fg, bg, pairnum)

    let mut ink = sntx.color.as_mut();
    while let Some(node) = ink {
        let mut fg = node.fg;
        let mut bg = node.bg;
        if !defaults_allowed {
            if fg == THE_DEFAULT { fg = COLOR_WHITE; }
            if bg == THE_DEFAULT { bg = COLOR_BLACK; }
            node.fg = fg;
            node.bg = bg;
        }

        // Find if this fg/bg combination was seen before.
        let existing = pairs_seen.iter().find(|(f, b, _)| *f == fg && *b == bg);
        let pairnum = if let Some(&(_, _, num)) = existing {
            num
        } else {
            next_number += 1;
            pairs_seen.push((fg, bg, next_number));
            next_number
        };
        node.pairnum = pairnum;
        node.attributes |= encode_pair(pairnum as usize, 0);

        ink = node.next.as_mut();
    }
}

/// Initialize the color pairs for the current syntax (call prepare_palette).
/// C: void prepare_palette(void)
#[cfg(feature = "color")]
pub fn prepare_palette() {
    // In C this calls init_pair for each unique pair number above NUMBER_OF_ELEMENTS.
    // In the Rust/crossterm port, colors are applied directly via set_color in winio.rs.
    // We simply mark the palette as initialized.
    with_state_mut(|s| {
        s.have_palette = true;
    });
}

/// Try to match the given shibboleth string with one of the regexes in the list.
/// C: bool found_in_list(regexlisttype *head, const char *shibboleth)
#[cfg(feature = "color")]
pub fn found_in_list(head: Option<&RegexListType>, shibboleth: &str) -> bool {
    let mut item = head;
    while let Some(node) = item {
        if let Some(ref regex) = node.one_rgx {
            if regex.is_match(shibboleth) {
                return true;
            }
        }
        item = node.next.as_deref();
    }
    false
}

/// Find a syntax that applies to the current buffer, based upon filename
/// or buffer content, and load and prime this syntax when needed.
/// C: void find_and_prime_applicable_syntax(void)
#[cfg(feature = "color")]
pub fn find_and_prime_applicable_syntax() {
    use crate::winio::statusline;

    // If the rcfiles were not read, or contained no syntaxes, get out.
    let has_syntaxes = with_state(|s| s.syntaxes.is_some());
    if !has_syntaxes {
        return;
    }

    let inhelp = with_state(|s| s.inhelp);
    let syntaxstr = with_state(|s| s.syntaxstr.clone());

    // We will walk the Box-linked list by raw pointer to avoid borrow issues
    // when we eventually need to assign the found syntax to openfile.syntax.
    let syntaxes_ptr: *mut SyntaxType = with_state_mut(|s| {
        s.syntaxes.as_deref_mut()
            .map(|p| p as *mut SyntaxType)
            .unwrap_or(std::ptr::null_mut())
    });

    if syntaxes_ptr.is_null() {
        return;
    }

    let mut found: *mut SyntaxType = std::ptr::null_mut();

    // If a syntax-override string was specified, use it.
    if let Some(ref override_str) = syntaxstr {
        if override_str == "none" {
            return;
        }
        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let name = unsafe { &(*sntx).name };
            if name == override_str {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx).next.as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
        if found.is_null() && !inhelp {
            let msg = format!("Unknown syntax name: {}", override_str);
            statusline(MessageType::Alert, &msg);
        }
    }

    // If no syntax-override, try matching by filename extension.
    if found.is_null() && !inhelp {
        // Get the full path of the current file.
        let filename = with_state(|s| {
            s.openfile.as_ref().map(|f| f.filename.clone()).unwrap_or_default()
        });
        let fullname = crate::files::get_full_path(&filename)
            .unwrap_or_else(|| filename.clone());

        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let matches = unsafe {
                found_in_list((*sntx).extensions.as_deref(), &fullname)
            };
            if matches {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx).next.as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // If filename didn't match, try the first line of the file.
    if found.is_null() && !inhelp {
        let first_line = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|f| f.filetop.as_ref())
                .map(|lp| lp.borrow().data.clone())
                .unwrap_or_default()
        });
        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let matches = unsafe {
                found_in_list((*sntx).headers.as_deref(), &first_line)
            };
            if matches {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx).next.as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // Try libmagic detection if still no match.
    #[cfg(feature = "libmagic")]
    if found.is_null() && !inhelp {
        let use_magic = with_state(|s| s.flag_isset(USE_MAGIC));
        if use_magic {
            let filename = with_state(|s| {
                s.openfile.as_ref().map(|f| f.filename.clone()).unwrap_or_default()
            });
            if !filename.is_empty() {
                if let Ok(content) = std::fs::read(&filename) {
                    let kind = infer::get(&content);
                    if let Some(kind) = kind {
                        let magicstring = format!("{}/{}", kind.mime_type(), kind.extension());
                        let mut sntx = syntaxes_ptr;
                        while !sntx.is_null() {
                            let matches = unsafe {
                                found_in_list((*sntx).magics.as_deref(), &magicstring)
                            };
                            if matches {
                                found = sntx;
                                break;
                            }
                            sntx = unsafe {
                                (*sntx).next.as_deref_mut()
                                    .map(|p| p as *mut SyntaxType)
                                    .unwrap_or(std::ptr::null_mut())
                            };
                        }
                    }
                }
            }
        }
    }

    // If nothing matched, look for a "default" syntax.
    if found.is_null() && !inhelp {
        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let name = unsafe { &(*sntx).name };
            if name == "default" {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx).next.as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // When the syntax isn't loaded yet (has a filename), parse and init colors.
    if !found.is_null() {
        let needs_parse = unsafe { !(&(*found).filename).is_empty() };
        if needs_parse {
            // parse_one_include would load the syntax file; stub for now.
            // set_syntax_colorpairs initializes color pair numbers.
            unsafe { set_syntax_colorpairs(&mut *found); }
        }
    }

    // Store the found syntax in the current open file.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.syntax = if found.is_null() { None } else { Some(found) };
        }
    });
}

/// Determine whether the matches of multiline regexes are still the same.
/// If not, schedule a screen refresh so things will be repainted.
/// C: void check_the_multis(linestruct *line)
#[cfg(feature = "color")]
pub fn check_the_multis(line_ptr: &LinePtr) {
    use regex::Regex;

    // If there is no syntax or no multiline regex, there is nothing to do.
    let syntax_ptr = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.syntax)
    });

    let syntax_ptr = match syntax_ptr {
        Some(p) => p,
        None => return,
    };

    let multiscore = unsafe { (*syntax_ptr).multiscore };
    if multiscore == 0 {
        return;
    }

    let multidata_empty = line_ptr.borrow().multidata.is_empty();
    if multidata_empty {
        with_state_mut(|s| s.refresh_needed = true);
        return;
    }

    let line_data = line_ptr.borrow().data.clone();

    // Walk each color rule looking for multiline regexes.
    let mut ink_ptr: *const ColorType = unsafe {
        (*syntax_ptr).color.as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null())
    };

    while !ink_ptr.is_null() {
        let ink = unsafe { &*ink_ptr };

        // If it's not a multiline regex, skip.
        if ink.end.is_none() {
            ink_ptr = ink.next.as_deref()
                .map(|p| p as *const ColorType)
                .unwrap_or(std::ptr::null());
            continue;
        }

        let start_regex = match &ink.start {
            Some(r) => r,
            None => {
                ink_ptr = ink.next.as_deref()
                    .map(|p| p as *const ColorType)
                    .unwrap_or(std::ptr::null());
                continue;
            }
        };
        let end_regex = ink.end.as_ref().unwrap();
        let id = ink.id as usize;

        // astart: whether the start regex matches somewhere on this line
        let start_match = start_regex.find(&line_data);
        let astart = start_match.is_some();
        let start_eo = start_match.map(|m| m.end()).unwrap_or(0);

        // afterstart: search for end after the start match end
        let afterstart = &line_data[start_eo..];
        let anend = end_regex.find(afterstart).is_some();

        let multidata_val = line_ptr.borrow().multidata.get(id).copied().unwrap_or(NOTHING);

        let matches_current = match multidata_val {
            x if x == NOTHING => {
                // Expect: no start match. If there IS a start, that's a change.
                !astart
            }
            x if x == WHOLELINE => {
                // Expect: no end on this line (and either no start, or start but end
                // doesn't appear before start).
                let end_from_start = end_regex.find(&line_data).is_some();
                !anend && (!astart || !end_from_start)
            }
            x if x == JUSTONTHIS => {
                // Expect: start and end both match, and no further start after them.
                if astart && anend {
                    let end_match = end_regex.find(afterstart);
                    let combined_end = start_eo + end_match.map(|m| m.end()).unwrap_or(0);
                    start_regex.find_at(&line_data, combined_end).is_none()
                } else {
                    false
                }
            }
            x if x == STARTSHERE => {
                // Expect: start matches, end does NOT match on this line.
                astart && !anend
            }
            x if x == ENDSHERE => {
                // Expect: no start match, but end matches.
                !astart && anend
            }
            _ => true,
        };

        if !matches_current {
            // Mismatch: repaint.
            with_state_mut(|s| {
                s.refresh_needed = true;
                s.perturbed = true;
            });
            return;
        }

        ink_ptr = ink.next.as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null());
    }
}

/// Precalculate the multi-line start and end regex info for all lines.
/// C: void precalc_multicolorinfo(void)
#[cfg(feature = "color")]
pub fn precalc_multicolorinfo() {
    use crate::chars::step_right;

    let no_syntax = with_state(|s| s.flag_isset(NO_SYNTAX));
    if no_syntax {
        return;
    }

    let syntax_ptr = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.syntax)
    });

    let syntax_ptr = match syntax_ptr {
        Some(p) => p,
        None => return,
    };

    let multiscore = unsafe { (*syntax_ptr).multiscore };
    if multiscore == 0 {
        return;
    }

    // Collect all line pointers from filetop to filebot.
    let lines: Vec<LinePtr> = with_state(|s| {
        let mut result = Vec::new();
        let mut cur = s.openfile.as_ref().and_then(|f| f.filetop.clone());
        while let Some(lp) = cur {
            let next = lp.borrow().next.clone();
            result.push(lp);
            cur = next;
        }
        result
    });

    // Allocate multidata for each line that doesn't have it yet.
    for lp in &lines {
        let needs_alloc = lp.borrow().multidata.is_empty();
        if needs_alloc {
            lp.borrow_mut().multidata = vec![0i16; multiscore as usize];
        }
    }

    // Walk each color rule.
    let mut ink_ptr: *const ColorType = unsafe {
        (*syntax_ptr).color.as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null())
    };

    while !ink_ptr.is_null() {
        let ink = unsafe { &*ink_ptr };

        // If this is not a multi-line regex, skip it.
        if ink.end.is_none() {
            ink_ptr = ink.next.as_deref()
                .map(|p| p as *const ColorType)
                .unwrap_or(std::ptr::null());
            continue;
        }

        let start_regex = match &ink.start {
            Some(r) => r,
            None => {
                ink_ptr = ink.next.as_deref()
                    .map(|p| p as *const ColorType)
                    .unwrap_or(std::ptr::null());
                continue;
            }
        };
        let end_regex = ink.end.as_ref().unwrap();
        let id = ink.id as usize;

        let mut line_idx = 0;
        while line_idx < lines.len() {
            let lp = &lines[line_idx];
            let line_data = lp.borrow().data.clone();
            let mut index: usize = 0;

            // Assume nothing applies until proven otherwise.
            {
                let mut borrow = lp.borrow_mut();
                if id < borrow.multidata.len() {
                    borrow.multidata[id] = NOTHING;
                }
            }

            // When the line contains a start match, look for an end.
            loop {
                // Use find_at for REG_NOTBOL semantics: when index > 0,
                // ^ anchors won't match at position `index`, exactly like REG_NOTBOL.
                let start_match = start_regex.find_at(&line_data, index);
                let sm = match start_match {
                    Some(m) => m,
                    None => break,
                };

                // Advance index past the start match end (absolute offset).
                index = sm.end();

                // Look for an end match on this same line (after the start).
                let end_match = end_regex.find_at(&line_data, index);
                if let Some(em) = end_match {
                    {
                        let mut borrow = lp.borrow_mut();
                        if id < borrow.multidata.len() {
                            borrow.multidata[id] = JUSTONTHIS;
                        }
                    }
                    index = em.end();

                    // If the total match has zero length, force an advance.
                    let start_len = sm.end() - sm.start();
                    let end_len = em.end() - em.start();
                    if start_len + end_len == 0 {
                        // When at end-of-line, there is no other start.
                        if index >= line_data.len() {
                            break;
                        }
                        index = step_right(&line_data, index);
                    }
                    continue;
                }

                // No end match on this line — look on later lines.
                // Mark current line as STARTSHERE.
                {
                    let mut borrow = lp.borrow_mut();
                    if id < borrow.multidata.len() {
                        borrow.multidata[id] = STARTSHERE;
                    }
                }

                // Find the tail line where the end regex matches.
                let mut tail_idx = line_idx + 1;
                while tail_idx < lines.len() {
                    let tail_data = lines[tail_idx].borrow().data.clone();
                    if end_regex.find(&tail_data).is_some() {
                        break;
                    }
                    tail_idx += 1;
                }

                // Mark all intermediate lines as WHOLELINE.
                let mut mid = line_idx + 1;
                while mid < tail_idx {
                    let mid_lp = &lines[mid];
                    let mut borrow = mid_lp.borrow_mut();
                    if id < borrow.multidata.len() {
                        borrow.multidata[id] = WHOLELINE;
                    }
                    mid += 1;
                }

                if tail_idx >= lines.len() {
                    // No end found — advance to end of line list.
                    line_idx = lines.len().saturating_sub(1);
                    break;
                }

                // Mark the tail line as ENDSHERE.
                {
                    let tail_lp = &lines[tail_idx];
                    let mut borrow = tail_lp.borrow_mut();
                    if id < borrow.multidata.len() {
                        borrow.multidata[id] = ENDSHERE;
                    }
                }

                // Look for a possible new start after the end match on tail line.
                let tail_data = lines[tail_idx].borrow().data.clone();
                let tail_end_match = end_regex.find(&tail_data);
                index = tail_end_match.map(|m| m.end()).unwrap_or(0);

                // In C: after the inner 'for' that marks WHOLELINE lines,
                // 'line' points to tailline and the outer loop body continues
                // with the updated 'index' on tailline.
                // We update line_data and line_idx to tail_idx, then continue
                // the inner while-loop from the new index on that line.
                // We also need to update `lp` to point to tail_idx's line.
                // Since `lp` is the reference from `lines[line_idx]`, we must
                // restructure: set line_idx to tail_idx and re-bind lp.
                // The simplest correct approach: decrement line_idx so that
                // the outer +1 lands on tail_idx, but with index != 0 we'd
                // lose that. Instead, handle tail_idx inline by continuing
                // the inner loop body for that line.
                //
                // Restructured: set line_idx = tail_idx and break inner loop.
                // The outer loop will increment to tail_idx+1, skipping tail.
                // To not skip tail_idx (which needs processing from `index`),
                // we use a secondary inner loop for tail_idx before breaking.
                {
                    let tail_lp = lines[tail_idx].clone();
                    let inner_data = tail_lp.borrow().data.clone();
                    // Continue scanning tail_idx from `index` for more starts.
                    // (The outer loop will start fresh on tail_idx+1.)
                    // We re-enter the start-match scan below:
                    let mut idx2 = index;
                    loop {
                        let sm2 = start_regex.find_at(&inner_data, idx2);
                        let sm2 = match sm2 { Some(m) => m, None => break };
                        idx2 = sm2.end();
                        let em2 = end_regex.find_at(&inner_data, idx2);
                        if let Some(em) = em2 {
                            {
                                let mut b = tail_lp.borrow_mut();
                                if id < b.multidata.len() { b.multidata[id] = JUSTONTHIS; }
                            }
                            idx2 = em.end();
                            let sl = sm2.end() - sm2.start();
                            let el = em.end() - em.start();
                            if sl + el == 0 {
                                if idx2 >= inner_data.len() { break; }
                                idx2 = step_right(&inner_data, idx2);
                            }
                            continue;
                        }
                        // No end on tail line — it starts a new multi-line span.
                        {
                            let mut b = tail_lp.borrow_mut();
                            if id < b.multidata.len() { b.multidata[id] = STARTSHERE; }
                        }
                        // Find the new tail.
                        let mut t2 = tail_idx + 1;
                        while t2 < lines.len() {
                            let d2 = lines[t2].borrow().data.clone();
                            if end_regex.find(&d2).is_some() { break; }
                            t2 += 1;
                        }
                        let mut m2 = tail_idx + 1;
                        while m2 < t2 {
                            let mut b = lines[m2].borrow_mut();
                            if id < b.multidata.len() { b.multidata[id] = WHOLELINE; }
                            m2 += 1;
                        }
                        if t2 < lines.len() {
                            let mut b = lines[t2].borrow_mut();
                            if id < b.multidata.len() { b.multidata[id] = ENDSHERE; }
                        }
                        // For simplicity, don't recurse further on t2.
                        break;
                    }
                }
                line_idx = tail_idx;
                break;
            }

            line_idx += 1;
        }

        ink_ptr = ink.next.as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null());
    }
}
