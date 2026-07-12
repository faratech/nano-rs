#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]
// Port of src/color.c from GNU nano.
// C original: Copyright (C) 2001-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2017, 2020, 2021 Benno Schulenberg

#[allow(unused_imports)] // some of these are used only under feature gates
use crate::definitions::*;
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::global::{A_REVERSE, state, state_mut, with_state, with_state_mut};
#[allow(unused_imports)] // some of these are used only under feature gates

// ncurses attribute constants (matching winio.rs conventions)
pub const A_NORMAL: i32 = 0;
pub const A_BOLD: i32 = 0x0200_0000;
pub const A_ITALIC: i32 = 0x0008_0000;

// ncurses COLOR_* constants (matching ncurses values)
pub const COLOR_BLACK: i16 = 0;
pub const COLOR_RED: i16 = 1;
pub const COLOR_GREEN: i16 = 2;
pub const COLOR_YELLOW: i16 = 3;
pub const COLOR_BLUE: i16 = 4;
pub const COLOR_MAGENTA: i16 = 5;
pub const COLOR_CYAN: i16 = 6;
pub const COLOR_WHITE: i16 = 7;

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
                    if combo.fg == THE_DEFAULT {
                        combo.fg = COLOR_WHITE;
                    }
                    if combo.bg == THE_DEFAULT {
                        combo.bg = COLOR_BLACK;
                    }
                }
                // Encode the pair index and attributes.
                let pair_val = encode_pair(index + 1, combo.attributes);
                s.interface_color_pair[index] = pair_val;
                // Remember the configured fg/bg so winio can actually emit them
                // (the encoded pair only carries the index + attributes; the colours
                // were previously discarded — set_color does emit them for syntax).
                s.interface_color_rgb[index + 1] = (combo.fg, combo.bg);
                s.rescind_colors = false;
                // combo is dropped here (free(color_combo[index]) in C)
            } else {
                // Default color values when no combo was specified.
                if index == FUNCTION_TAG || index == SCROLL_BAR {
                    s.interface_color_pair[index] = A_NORMAL;
                } else if index == GUIDE_STRIPE {
                    s.interface_color_pair[index] = A_REVERSE;
                } else if index == SPOTLIGHTED {
                    // Black on yellow (built-in default).
                    s.interface_color_pair[index] = encode_pair(index + 1, A_NORMAL);
                    s.interface_color_rgb[index + 1] = (COLOR_BLACK, COLOR_YELLOW);
                } else if index == MINI_INFOBAR || index == PROMPT_BAR {
                    s.interface_color_pair[index] = s.interface_color_pair[TITLE_BAR];
                } else if index == ERROR_MESSAGE {
                    // White on red, bold (built-in default).
                    s.interface_color_pair[index] = encode_pair(index + 1, A_BOLD);
                    s.interface_color_rgb[index + 1] = (COLOR_WHITE, COLOR_RED);
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
            if fg == THE_DEFAULT {
                fg = COLOR_WHITE;
            }
            if bg == THE_DEFAULT {
                bg = COLOR_BLACK;
            }
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
pub fn found_in_list(head: Option<&RegexListType>, shibboleth: &[u8]) -> bool {
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
    let has_syntaxes = state().syntaxes.is_some();
    if !has_syntaxes {
        return;
    }

    let inhelp = state().inhelp;
    let syntaxstr = state().syntaxstr.clone();

    // We will walk the Box-linked list by raw pointer to avoid borrow issues
    // when we eventually need to assign the found syntax to openfile.syntax.
    let syntaxes_ptr: *mut SyntaxType = with_state_mut(|s| {
        s.syntaxes
            .as_deref_mut()
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
                (*sntx)
                    .next
                    .as_deref_mut()
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
            s.openfile
                .as_ref()
                .map(|f| f.filename.clone())
                .unwrap_or_default()
        });
        let fullname = crate::files::get_full_path(&filename).unwrap_or_else(|| filename.clone());

        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let matches =
                unsafe { found_in_list((*sntx).extensions.as_deref(), fullname.as_bytes()) };
            if matches {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx)
                    .next
                    .as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // If filename didn't match, try the first line of the file.
    if found.is_null() && !inhelp {
        let first_line = with_state(|s| {
            s.openfile
                .as_ref()
                .and_then(|f| f.filetop.as_ref())
                .map(|lp| lp.borrow().data.clone())
                .unwrap_or_default()
        });
        let mut sntx = syntaxes_ptr;
        while !sntx.is_null() {
            let matches =
                unsafe { found_in_list((*sntx).headers.as_deref(), first_line.as_bytes()) };
            if matches {
                found = sntx;
                break;
            }
            sntx = unsafe {
                (*sntx)
                    .next
                    .as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // Try libmagic detection if still no match.
    #[cfg(feature = "libmagic")]
    if found.is_null() && !inhelp {
        let use_magic = state().flag_isset(USE_MAGIC);
        if use_magic {
            let filename = with_state(|s| {
                s.openfile
                    .as_ref()
                    .map(|f| f.filename.clone())
                    .unwrap_or_default()
            });
            if !filename.is_empty() {
                let description = magic::Cookie::open(Default::default())
                    .ok()
                    .and_then(|cookie| cookie.load(&Default::default()).ok())
                    .and_then(|cookie| cookie.file(&filename).ok());
                if let Some(magicstring) = description {
                    let mut sntx = syntaxes_ptr;
                    while !sntx.is_null() {
                        let matches = unsafe {
                            found_in_list((*sntx).magics.as_deref(), magicstring.as_bytes())
                        };
                        if matches {
                            found = sntx;
                            break;
                        }
                        sntx = unsafe {
                            (*sntx)
                                .next
                                .as_deref_mut()
                                .map(|p| p as *mut SyntaxType)
                                .unwrap_or(std::ptr::null_mut())
                        };
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
                (*sntx)
                    .next
                    .as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            };
        }
    }

    // When the syntax isn't loaded yet (has a filename marker), lazily
    // parse its color rules now (C: parse_one_include(sntx->filename, sntx)).
    if !found.is_null() {
        let needs_parse = unsafe { !(&(*found).filename).is_empty() };
        if needs_parse {
            let name = unsafe { (*found).name.clone() };
            let file = unsafe { (*found).filename.clone() };

            // The Rust parser treats the head of s.syntaxes as C's
            // live_syntax, so move the wanted syntax to the head first.
            with_state_mut(|s| move_syntax_to_head(s, &name));
            crate::rcfile::parse_one_include(&file, true);

            // The list head (and thus our pointer) may have changed.
            found = with_state_mut(|s| {
                s.syntaxes
                    .as_deref_mut()
                    .map(|p| p as *mut SyntaxType)
                    .unwrap_or(std::ptr::null_mut())
            });
            if !found.is_null() {
                // Indicate that this syntax has been loaded.
                unsafe {
                    (*found).filename.clear();
                }
            }
        }
        if !found.is_null() {
            unsafe {
                set_syntax_colorpairs(&mut *found);
            }
        }
    }

    // Store the found syntax in the current open file.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.syntax = if found.is_null() { None } else { Some(found) };
        }
    });
}

/// Detach the syntax with the given name from the list and re-attach it at
/// the head, so the parser's "live syntax is the list head" convention holds.
#[cfg(feature = "color")]
fn move_syntax_to_head(s: &mut crate::global::AppState, name: &str) {
    if s.syntaxes
        .as_ref()
        .map(|sx| sx.name == name)
        .unwrap_or(true)
    {
        return; // already at head (or list empty)
    }
    let mut detached: Option<Box<SyntaxType>> = None;
    let mut prev = s.syntaxes.as_mut();
    while let Some(p) = prev {
        if p.next.as_ref().map(|nx| nx.name == name).unwrap_or(false) {
            let mut target = p.next.take().expect("checked above");
            p.next = target.next.take();
            detached = Some(target);
            break;
        }
        prev = p.next.as_mut();
    }
    if let Some(mut t) = detached {
        t.next = s.syntaxes.take();
        s.syntaxes = Some(t);
    }
}

/// Determine whether the matches of multiline regexes are still the same.
/// If not, schedule a screen refresh so things will be repainted.
/// C: void check_the_multis(linestruct *line)
#[cfg(feature = "color")]
pub fn check_the_multis(line_ptr: &LinePtr) {
    // If there is no syntax or no multiline regex, there is nothing to do.
    let syntax_ptr = with_state(|s| s.openfile.as_ref().and_then(|f| f.syntax));

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
        state_mut().refresh_needed = true;
        return;
    }

    let line_data = line_ptr.borrow().data.clone();

    // Walk each color rule looking for multiline regexes.
    let mut ink_ptr: *const ColorType = unsafe {
        (*syntax_ptr)
            .color
            .as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null())
    };

    while !ink_ptr.is_null() {
        let ink = unsafe { &*ink_ptr };

        // If it's not a multiline regex, skip.
        if ink.end.is_none() {
            ink_ptr = ink
                .next
                .as_deref()
                .map(|p| p as *const ColorType)
                .unwrap_or(std::ptr::null());
            continue;
        }

        let start_regex = match &ink.start {
            Some(r) => r,
            None => {
                ink_ptr = ink
                    .next
                    .as_deref()
                    .map(|p| p as *const ColorType)
                    .unwrap_or(std::ptr::null());
                continue;
            }
        };
        let end_regex = ink.end.as_ref().unwrap();
        let id = ink.id as usize;

        // astart: whether the start regex matches somewhere on this line
        let start_match = start_regex.find(line_data.as_bytes());
        let astart = start_match.is_some();
        let start_eo = start_match.map(|m| m.end()).unwrap_or(0);

        // afterstart: search for end after the start match end
        let afterstart = &line_data.as_bytes()[start_eo..];
        let anend = end_regex.find(afterstart).is_some();

        let multidata_val = line_ptr
            .borrow()
            .multidata
            .get(id)
            .copied()
            .unwrap_or(NOTHING);

        let matches_current = match multidata_val {
            x if x == NOTHING => {
                // Expect: no start match. If there IS a start, that's a change.
                !astart
            }
            x if x == WHOLELINE => {
                // Expect: no end on this line (and either no start, or start but end
                // doesn't appear before start).
                let end_from_start = end_regex.find(line_data.as_bytes()).is_some();
                !anend && (!astart || !end_from_start)
            }
            x if x == JUSTONTHIS => {
                // Expect: start and end both match, and no further start after them.
                if astart && anend {
                    let end_match = end_regex.find(afterstart);
                    let combined_end = start_eo + end_match.map(|m| m.end()).unwrap_or(0);
                    start_regex
                        .find_at(line_data.as_bytes(), combined_end)
                        .is_none()
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

        ink_ptr = ink
            .next
            .as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null());
    }
}

/// Precalculate the multi-line start and end regex info for all lines.
/// C: void precalc_multicolorinfo(void)
#[cfg(feature = "color")]
pub fn precalc_multicolorinfo() {
    use crate::chars::step_right;

    let no_syntax = state().flag_isset(NO_SYNTAX);
    if no_syntax {
        return;
    }

    let syntax_ptr = with_state(|s| s.openfile.as_ref().and_then(|f| f.syntax));

    let syntax_ptr = match syntax_ptr {
        Some(p) => p,
        None => return,
    };

    let multiscore = unsafe { (*syntax_ptr).multiscore };
    if multiscore == 0 {
        return;
    }

    let (filetop, filebot) = with_state(|s| {
        (
            s.openfile.as_ref().and_then(|f| f.filetop.clone()),
            s.openfile.as_ref().and_then(|f| f.filebot.clone()),
        )
    });

    // For each line, allocate cache space for the multiline-regex info.
    let mut walker = filetop.clone();
    while let Some(lp) = walker {
        {
            let mut b = lp.borrow_mut();
            if b.multidata.is_empty() {
                b.multidata = vec![0i16; multiscore as usize];
            }
        }
        walker = lp.borrow().next.clone();
    }

    // Walk each color rule.
    let mut ink_ptr: *const ColorType = unsafe {
        (*syntax_ptr)
            .color
            .as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null())
    };

    while !ink_ptr.is_null() {
        let ink = unsafe { &*ink_ptr };

        // If this is not a multi-line regex, skip it.
        let (Some(start_regex), Some(end_regex)) = (&ink.start, &ink.end) else {
            ink_ptr = ink
                .next
                .as_deref()
                .map(|p| p as *const ColorType)
                .unwrap_or(std::ptr::null());
            continue;
        };
        let id = ink.id as usize;

        let mut line = filetop.clone();
        while let Some(mut lp) = line {
            let mut index: usize = 0;

            // Assume nothing applies until proven otherwise below.
            {
                let mut b = lp.borrow_mut();
                if id < b.multidata.len() {
                    b.multidata[id] = NOTHING;
                }
            }

            // When the line contains a start match, look for an end,
            // and if found, mark all the lines that are affected.
            // (find_at gives REG_NOTBOL semantics: ^ only matches at
            // the real start of the string, never at index > 0.)
            loop {
                let sm = {
                    let b = lp.borrow();
                    start_regex
                        .find_at(b.data.as_bytes(), index)
                        .map(|m| (m.start(), m.end()))
                };
                let Some((sm_so, sm_eo)) = sm else { break };

                // Begin looking for an end match after the start match.
                index = sm_eo;

                // If there is an end match on this same line, mark the line,
                // but continue looking for other starts after it.
                let em = {
                    let b = lp.borrow();
                    end_regex.find_at(b.data.as_bytes(), index).map(|m| m.end())
                };
                if let Some(em_eo) = em {
                    {
                        let mut b = lp.borrow_mut();
                        if id < b.multidata.len() {
                            b.multidata[id] = JUSTONTHIS;
                        }
                    }

                    // If the total match has zero length, force an advance.
                    // (C: startmatch.rm_eo - rm_so + endmatch.rm_eo == 0,
                    // where endmatch offsets are relative to `index`.)
                    let zero_length = (sm_eo - sm_so) + (em_eo - index) == 0;
                    index = em_eo;
                    if zero_length {
                        // When at end-of-line, there is no other start.
                        let at_eol = index >= lp.borrow().data.len();
                        if at_eol {
                            break;
                        }
                        index = {
                            let b = lp.borrow();
                            step_right(&b.data, index)
                        };
                    }
                    continue;
                }

                // Look for an end match on later lines.
                let mut tailline = lp.borrow().next.clone();
                let mut tail_end_eo = 0usize;
                loop {
                    let Some(tl) = tailline.clone() else { break };
                    let found = {
                        let b = tl.borrow();
                        end_regex.find(b.data.as_bytes()).map(|m| m.end())
                    };
                    match found {
                        Some(eo) => {
                            tail_end_eo = eo;
                            break;
                        }
                        None => tailline = tl.borrow().next.clone(),
                    }
                }

                {
                    let mut b = lp.borrow_mut();
                    if id < b.multidata.len() {
                        b.multidata[id] = STARTSHERE;
                    }
                }

                // Mark all lines between this one and the tail as WHOLELINE.
                // (In C this also advances `line` in the main loop.)
                let mut mid = lp.borrow().next.clone();
                while let Some(m) = mid {
                    if tailline.as_ref().is_some_and(|t| LinePtr::ptr_eq(&m, t)) {
                        break;
                    }
                    {
                        let mut b = m.borrow_mut();
                        if id < b.multidata.len() {
                            b.multidata[id] = WHOLELINE;
                        }
                    }
                    mid = m.borrow().next.clone();
                }

                match tailline {
                    None => {
                        // C: line = openfile->filebot; break;
                        if let Some(fb) = filebot.clone() {
                            lp = fb;
                        }
                        break;
                    }
                    Some(t) => {
                        {
                            let mut b = t.borrow_mut();
                            if id < b.multidata.len() {
                                b.multidata[id] = ENDSHERE;
                            }
                        }
                        // Look for a possible new start after the end match,
                        // continuing the scan on the tail line.
                        index = tail_end_eo;
                        lp = t;
                    }
                }
            }

            line = lp.borrow().next.clone();
        }

        ink_ptr = ink
            .next
            .as_deref()
            .map(|p| p as *const ColorType)
            .unwrap_or(std::ptr::null());
    }
}

#[cfg(all(test, feature = "color"))]
mod tests {
    use super::found_in_list;
    use crate::definitions::RegexListType;
    use regex::bytes::RegexBuilder;

    #[test]
    fn regex_lists_scan_malformed_utf8_as_raw_bytes() {
        let regex = RegexBuilder::new("tag$").unicode(true).build().unwrap();
        let list = RegexListType {
            one_rgx: Some(regex),
            next: None,
        };

        assert!(found_in_list(Some(&list), b"\xfftag"));
        assert!(!found_in_list(Some(&list), b"\xfftag!"));
    }
}
