#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
use crate::definitions::*;
use crate::global::STATE;

// The tr! translation macro is defined once in main.rs (#[macro_export]).
use crate::tr;

// ---------------------------------------------------------------------------
// Everything inside ENABLE_HELP is gated by #[cfg(feature = "help")].
// do_help() is always present (it has an #else branch that calls beep).
// ---------------------------------------------------------------------------

/// File-level statics from help.c.
/// We use thread_local! RefCells for the same reason as in history.rs:
/// these are file-scope static variables in C.
#[cfg(feature = "help")]
mod help_state {
    use std::cell::RefCell;

    thread_local! {
        /// The text displayed in the help window.
        pub static HELP_TEXT: RefCell<Option<String>> = RefCell::new(None);
        /// The byte offset of the part just after the title.
        pub static START_OF_BODY_OFFSET: RefCell<usize> = RefCell::new(0);
        /// The byte offset where shortcut descriptions begin.
        pub static END_OF_INTRO_OFFSET: RefCell<usize> = RefCell::new(0);
        /// The offset (in bytes) of the topleft of the shown help text.
        pub static LOCATION: RefCell<usize> = RefCell::new(0);
    }
}

// ---------------------------------------------------------------------------
// Local stubs for AppState buffer-manipulation operations that have no
// free-function equivalent yet.  These are no-ops that let the code compile.
// ---------------------------------------------------------------------------
#[cfg(feature = "help")]
mod stubs {
    use crate::global::STATE;

    /// Set the data of the current line in the open buffer.
    pub fn set_current_line_data(data: &str) {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                if let Some(ref cur) = of.current.clone() {
                    cur.borrow_mut().data = data.to_string();
                }
            }
        });
    }

    /// Append a new empty line after the current line and advance current.
    /// C (help.c): openfile->current->next = make_new_node(openfile->current);
    ///             openfile->current = openfile->current->next;
    ///             openfile->current->data = copy_of("");
    pub fn append_new_line_after_current() {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                if let Some(cur) = of.current.clone() {
                    let new = crate::nano::make_new_node(Some(&cur));
                    cur.borrow_mut().next = Some(new.clone());
                    of.current = Some(new);
                }
            }
        });
    }

    /// Set filebot to current.
    pub fn set_filebot_to_current() {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                of.filebot = of.current.clone();
            }
        });
    }

    /// Set current to filetop.
    pub fn set_current_to_filetop() {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                of.current = of.filetop.clone();
            }
        });
    }

    /// Return the length of the current line's data.
    pub fn current_line_data_len() -> usize {
        STATE.with(|s| {
            let st = s.borrow();
            st.current_line_data().len()
        })
    }

    /// Advance current to the next line.  Returns false if already at filebot.
    pub fn advance_current_to_next() -> bool {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                let next = of.current.as_ref().and_then(|c| c.borrow().next.clone());
                if let Some(n) = next {
                    of.current = Some(n);
                    return true;
                }
            }
            false
        })
    }

    /// Set edittop to the current line.
    pub fn set_edittop_to_current() {
        STATE.with(|s| {
            let st = s.borrow_mut();
            if let Some(ref mut of) = st.openfile {
                of.edittop = of.current.clone();
            }
        });
    }

    /// Compute byte offset of edittop into the file (used to restore the
    /// scroll position when the help text is re-wrapped or re-entered).
    /// C (help.c): sums strlen(line->data) from filetop up to edittop.
    pub fn compute_edittop_byte_offset() -> usize {
        STATE.with(|s| {
            let st = s.borrow();
            let Some(ref of) = st.openfile else { return 0 };
            let Some(ref edittop) = of.edittop else { return 0 };
            let mut sum = 0usize;
            let mut line = of.filetop.clone();
            while let Some(l) = line {
                if std::rc::Rc::ptr_eq(&l, edittop) {
                    break;
                }
                sum += l.borrow().data.len();
                line = l.borrow().next.clone();
            }
            sum
        })
    }

    /// Return the line number of edittop.
    pub fn edittop_lineno() -> isize {
        STATE.with(|s| {
            let st = s.borrow();
            st.openfile.as_ref()
                .and_then(|of| of.edittop.as_ref())
                .map(|lp| lp.borrow().lineno as isize)
                .unwrap_or(1)
        })
    }

    /// Return the line number of filebot.
    pub fn filebot_lineno() -> isize {
        STATE.with(|s| {
            let st = s.borrow();
            st.openfile.as_ref()
                .and_then(|of| of.filebot.as_ref())
                .map(|lp| lp.borrow().lineno as isize)
                .unwrap_or(1)
        })
    }

    /// ncurses curs_set — stub (no ncurses here).
    pub fn curs_set(_visibility: i32) {}
}

// ---------------------------------------------------------------------------
// help_init
// ---------------------------------------------------------------------------

/// Allocate space for the help text for the current menu,
/// and concatenate the different pieces of text into it.
/* C: void help_init(void) */
#[cfg(feature = "help")]
pub fn help_init() {
    use help_state::*;
    

    let currmenu: u32 = STATE.with(|s| s.borrow().currmenu);

    // ------------------------------------------------------------------
    // Select the introductory text chunks based on the current menu.
    // htx[0..2] follow the original C: htx[0] is always set, htx[1] and
    // htx[2] may be None.
    // ------------------------------------------------------------------
    let (htx0, htx1, htx2): (&str, Option<&str>, Option<&str>) =
        if (currmenu & (MWHEREIS | MREPLACE)) != 0 {
            (
                tr!("Search Command Help Text\n\n \
Enter the words or characters you would like to \
search for, and then press Enter.  If there is a \
match for the text you entered, the screen will be \
updated to the location of the nearest match for the \
search string.\n\n The previous search string will be \
shown in brackets after the search prompt.  Hitting \
Enter without entering any text will perform the \
previous search.  "),
                Some(tr!("If you have selected text with the mark and then \
search to replace, only matches in the selected text \
will be replaced.\n\n The following function keys are \
available in Search mode:\n\n")),
                None,
            )
        } else if currmenu == MREPLACEWITH {
            (
                tr!("=== Replacement ===\n\n \
Type the characters that should replace what you \
typed at the previous prompt, and press Enter.\n\n"),
                Some(tr!(" The following function keys \
are available at this prompt:\n\n")),
                None,
            )
        } else if currmenu == MGOTOLINE {
            (
                tr!("Go To Line Help Text\n\n \
Enter the line number that you wish to go to and hit \
Enter.  If there are fewer lines of text than the \
number you entered, you will be brought to the last \
line of the file.\n\n The following function keys are \
available in Go To Line mode:\n\n"),
                None,
                None,
            )
        } else if currmenu == MINSERTFILE {
            (
                tr!("Insert File Help Text\n\n \
Type in the name of a file to be inserted into the \
current file buffer at the current cursor \
location.\n\n If you have compiled nano with multiple \
file buffer support, and enable multiple file buffers \
with the -F or --multibuffer command line flags, the \
Meta-F toggle, or a nanorc file, inserting a file \
will cause it to be loaded into a separate buffer \
(use Meta-< and > to switch between file buffers).  "),
                Some(tr!("If you need another blank buffer, do not enter \
any filename, or type in a nonexistent filename at \
the prompt and press Enter.\n\n The following \
function keys are available in Insert File mode:\n\n")),
                None,
            )
        } else if currmenu == MWRITEFILE {
            (
                tr!("Write File Help Text\n\n \
Type the name that you wish to save the current file \
as and press Enter to save the file.\n\n If you have \
selected text with the mark, you will be prompted to \
save only the selected portion to a separate file.  To \
reduce the chance of overwriting the current file with \
just a portion of it, the current filename is not the \
default in this mode.\n\n The following function keys \
are available in Write File mode:\n\n"),
                None,
                None,
            )
        }
        // ---- ENABLE_BROWSER menus ----
        else if cfg!(feature = "browser") && currmenu == MBROWSER {
            (
                tr!("File Browser Help Text\n\n \
The file browser is used to visually browse the \
directory structure to select a file for reading \
or writing.  You may use the arrow keys or Page Up/\
Down to browse through the files, and S or Enter to \
choose the selected file or enter the selected \
directory.  To move up one level, select the \
directory called \"..\" at the top of the file \
list.\n\n The following function keys are available \
in the file browser:\n\n"),
                None,
                None,
            )
        } else if cfg!(feature = "browser") && currmenu == MWHEREISFILE {
            (
                tr!("Browser Search Command Help Text\n\n \
Enter the words or characters you would like to \
search for, and then press Enter.  If there is a \
match for the text you entered, the screen will be \
updated to the location of the nearest match for the \
search string.\n\n The previous search string will be \
shown in brackets after the search prompt.  Hitting \
Enter without entering any text will perform the \
previous search.\n\n"),
                Some(tr!(" The following function keys \
are available at this prompt:\n\n")),
                None,
            )
        } else if cfg!(feature = "browser") && currmenu == MGOTODIR {
            (
                tr!("Browser Go To Directory Help Text\n\n \
Enter the name of the directory you would like to \
browse to.\n\n If tab completion has not been \
disabled, you can use the Tab key to (attempt to) \
automatically complete the directory name.\n\n The \
following function keys are available in Browser Go \
To Directory mode:\n\n"),
                None,
                None,
            )
        }
        // ---- ENABLE_SPELLER ----
        else if cfg!(feature = "speller") && currmenu == MSPELL {
            (
                tr!("Spell Check Help Text\n\n \
The spell checker checks the spelling of all text in \
the current file.  When an unknown word is \
encountered, it is highlighted and a replacement can \
be edited.  It will then prompt to replace every \
instance of the given misspelled word in the current \
file, or, if you have selected text with the mark, in \
the selected text.\n\n The following function keys \
are available in Spell Check mode:\n\n"),
                None,
                None,
            )
        }
        // ---- !NANO_TINY menus ----
        else if !cfg!(feature = "tiny") && currmenu == MEXECUTE {
            (
                tr!("Execute Command Help Text\n\n \
This mode allows you to insert the output of a \
command run by the shell into the current buffer (or \
into a new buffer).  If the command is preceded by '|' \
(the pipe symbol), the current contents of the buffer \
(or marked region) will be piped to the command.  "),
                Some(tr!("If you just need another blank buffer, do not enter any \
command.\n\n You can also pick one of four tools, or cut a \
large piece of the buffer, or put the editor to sleep.\n\n")),
                Some(tr!(" The following function keys \
are available at this prompt:\n\n")),
            )
        } else if !cfg!(feature = "tiny") && currmenu == MLINTER {
            (
                tr!("=== Linter ===\n\n \
In this mode, the status bar shows an error message or \
warning, and the cursor is put at the corresponding \
position in the file.  With PageUp and PageDown you \
can switch to earlier and later messages.\n\n"),
                Some(tr!(" The following function keys are \
available in Linter mode:\n\n")),
                None,
            )
        } else {
            // Default: main help text.
            (
                tr!("Main nano help text\n\n \
The nano editor is designed to emulate the \
functionality and ease-of-use of the UW Pico text \
editor.  There are four main sections of the editor.  \
The top line shows the program version, the current \
filename being edited, and whether or not the file \
has been modified.  Next is the main editor window \
showing the file being edited.  The status line is \
the third line from the bottom and shows important \
messages.  "),
                Some(tr!("The bottom two lines show the most commonly used \
shortcuts in the editor.\n\n Shortcuts are written as \
follows: Control-key sequences are notated with a '^' \
and can be entered either by using the Ctrl key or \
pressing the Esc key twice.  Meta-key sequences are \
notated with 'M-' and can be entered using either the \
Alt, Cmd, or Esc key, depending on your keyboard setup.  ")),
                Some(tr!("Also, pressing Esc twice and then typing a \
three-digit decimal number from 000 to 255 will enter \
the character with the corresponding value.  The \
following keystrokes are available in the main editor \
window.  Alternative keys are shown in \
parentheses:\n\n")),
            )
        };

    // ------------------------------------------------------------------
    // Build the help_text string.
    // We skip the C precomputation of allocsize; Rust String auto-grows.
    // ------------------------------------------------------------------
    let mut text = String::new();
    text.push_str(htx0);
    if let Some(h) = htx1 { text.push_str(h); }
    if let Some(h) = htx2 { text.push_str(h); }

    // Remember the end-of-introduction offset.
    let intro_end = text.len();
    END_OF_INTRO_OFFSET.with(|e| *e.borrow_mut() = intro_end);

    // ------------------------------------------------------------------
    // Append shortcut descriptions.
    // Layout rules from the C code (reproduce the formatted columns):
    //   column 1: keystr padded to 7 printable cells (9 bytes if arrow present)
    //   column 2: (keystr) padded to 10 printable cells (12 if arrow)
    //   OR: \t\t  (two tabs + space) when no shortcuts exist
    //   Then: phrase\n  (plus \n if blank_after)
    //
    // The C code checks strstr(s->keystr, "\xE2") to detect UTF-8 arrows
    // (3 bytes per char instead of 1), adjusting the byte advance.
    // ------------------------------------------------------------------
    STATE.with(|s| {
        let st = s.borrow();

        // Iterate over all functions.
        for f in &st.allfuncs {
            if (f.menus as u32 & currmenu) == 0 {
                continue;
            }

            let mut tally: i32 = 0;
            let mut shortcut_col = String::new();

            // Show the first two shortcuts (if any) for each function.
            for sc in &st.sclist {
                if (sc.menus as u32 & currmenu) != 0
                    && sc.func == f.func
                    && !sc.keystr.is_empty()
                {
                    let has_arrow = sc.keystr.contains('\u{2192}')
                        || sc.keystr.contains('\u{2190}')
                        || sc.keystr.contains('\u{2191}')
                        || sc.keystr.contains('\u{2193}')
                        // Generic 3-byte UTF-8 check matching strstr(s, "\xE2"):
                        || sc.keystr.as_bytes().iter().any(|&b| b == 0xE2);

                    tally += 1;
                    if tally == 1 {
                        // First keystroke: pad to 7 printable cells.
                        // With arrow: use 9 bytes of padding column width.
                        if has_arrow {
                            shortcut_col.push_str(&format!("{:<9}", sc.keystr));
                        } else {
                            shortcut_col.push_str(&format!("{:<7}", sc.keystr));
                        }
                    } else {
                        // Second keystroke: parens hug the key, then trailing spaces
                        // (C: "(%s)       "), not padding INSIDE the parens.  Same
                        // total field width as before: 12 cells with an arrow, else 10.
                        let total: usize = if has_arrow { 12 } else { 10 };
                        let content = format!("({})", sc.keystr);
                        let pad = total.saturating_sub(content.chars().count());
                        shortcut_col.push_str(&content);
                        shortcut_col.push_str(&" ".repeat(pad));
                        break;
                    }
                }
            }

            if tally == 0 {
                text.push_str("\t\t ");
            } else {
                text.push_str(&shortcut_col);
                if tally == 1 {
                    // Need to pad another 10 cells for the empty second column.
                    text.push_str("          ");
                }
            }

            // The shortcut's description.
            text.push_str(f.phrase);
            text.push('\n');

            if f.blank_after {
                text.push('\n');
            }
        }

        // ------------------------------------------------------------------
        // For the main menu, also append the toggles in ordinal order.
        // (#ifndef NANO_TINY)
        // ------------------------------------------------------------------
        #[cfg(not(feature = "tiny"))]
        if currmenu == MMAIN {
            // Determine the maximum ordinal.
            let mut maximum: i32 = 0;
            for sc in &st.sclist {
                if sc.toggle != 0 && sc.ordinal > maximum {
                    maximum = sc.ordinal;
                }
            }

            // Now show them in the original order.
            let mut counter: i32 = 0;
            while counter < maximum {
                counter += 1;
                for sc2 in &st.sclist {
                    if sc2.toggle != 0 && sc2.ordinal == counter {
                        let keystr = if (sc2.menus as u32 & MMAIN) != 0 {
                            sc2.keystr
                        } else {
                            ""
                        };
                        let epithet = crate::global::epithet_of_flag(sc2.toggle as u32);
                        text.push_str(&format!(
                            "{}\t\t {} {}\n",
                            keystr, epithet, tr!("enable/disable")
                        ));
                        // Add a blank line between two groups (after NO_SYNTAX).
                        if sc2.toggle as u32 == NO_SYNTAX {
                            text.push('\n');
                        }
                        break;
                    }
                }
            }
        }
    });

    HELP_TEXT.with(|ht| *ht.borrow_mut() = Some(text));
}

// ---------------------------------------------------------------------------
// wrap_help_text_into_buffer
// ---------------------------------------------------------------------------

/// Hard-wrap the concatenated help text, and write it into a new buffer.
/* C: void wrap_help_text_into_buffer(void) */
#[cfg(feature = "help")]
pub fn wrap_help_text_into_buffer() {
    use help_state::*;
    use stubs::*;
    
    

    let text_opt = HELP_TEXT.with(|ht| ht.borrow().clone());
    let text = match text_opt {
        Some(t) => t,
        None => return,
    };

    let start_offset = START_OF_BODY_OFFSET.with(|s| *s.borrow());
    let intro_end    = END_OF_INTRO_OFFSET.with(|e| *e.borrow());
    let location_val = LOCATION.with(|l| *l.borrow());

    let (cols, rows, sidebar, minibar_set, empty_line_set, _editwinrows) =
        STATE.with(|s| {
            let st = s.borrow();
            (
                st.midwin.cols as usize,
                (st.topwin.rows + st.midwin.rows + st.footwin.rows) as usize, // LINES
                st.sidebar as usize,
                st.flag_isset(MINIBAR),
                st.flag_isset(EMPTY_LINE),
                st.editwinrows,
            )
        });

    // Avoid overtight and overwide paragraphs in the introductory text.
    // wrapping_point = ((COLS < 40) ? 40 : (COLS > 74) ? 74 : COLS) - sidebar
    let mut wrapping_point: usize =
        (if cols < 40 { 40 } else if cols > 74 { 74 } else { cols })
            .saturating_sub(sidebar);

    // Make a new buffer in AppState.
    crate::files::make_new_buffer();

    // Ensure there is a blank line at the top.
    if (minibar_set || !empty_line_set) && rows > 6 {
        set_current_line_data(" ");
        append_new_line_after_current();
    }

    // Copy the help text into the buffer, hard-wrapping.
    let bytes = text.as_bytes();
    let total = bytes.len();
    let mut pos: usize = start_offset;

    while pos < total {
        // Adjust wrapping point at end of intro.
        if pos == intro_end {
            wrapping_point =
                (if cols < 40 { 40 } else { cols })
                    .saturating_sub(sidebar);
        }

        let (oneline, length) = if pos < intro_end || (pos > 0 && bytes[pos - 1] == b'\n') {
            // Introductory section or beginning of a line: straight wrap, using the
            // faithful break_line (keystroke-area guard, step_left fallback, -1 case).
            let chunk = &text[pos..];
            let raw = crate::text::break_line(chunk, wrapping_point as isize, true);
            let length = if raw < 0 { chunk.len() } else { (raw as usize).min(chunk.len()) };
            // C: snprintf(oneline, length+shim, "%s", ptr) copies length+shim-1 bytes,
            // i.e. it drops the break char only when it is a space.
            let shim = if length > 0 && chunk.as_bytes().get(length - 1) == Some(&b' ') {
                0usize
            } else {
                1usize
            };
            let take = (length + shim).saturating_sub(1).min(chunk.len());
            (chunk[..take].to_string(), length)
        } else {
            // Shortcut column: indented continuation.
            let chunk = &text[pos..];
            let sc_wrap = (if cols < 40 { 22isize } else { cols as isize - 18 }) - sidebar as isize;
            let raw = crate::text::break_line(chunk, sc_wrap, true);
            let length = if raw < 0 { chunk.len() } else { (raw as usize).min(chunk.len()) };
            (format!("\t\t  {}", &chunk[..length]), length)
        };

        set_current_line_data(&oneline);

        let mut pos2 = pos + length;
        if pos2 < total && bytes[pos2] != b'\n' && pos2 > 0 {
            pos2 -= 1;
        }
        pos = pos2;

        // Create new lines for each \n.
        loop {
            append_new_line_after_current();
            if pos < total {
                pos += 1;
            }
            if pos >= total || bytes[pos] != b'\n' {
                break;
            }
        }
    }

    set_filebot_to_current();
    set_current_to_filetop();
    crate::utils::remove_magicline();
    #[cfg(feature = "color")]
    crate::color::find_and_prime_applicable_syntax();
    crate::files::prepare_for_display();

    // Move to the position in the file where we were before.
    let mut byte_sum: usize = 0;
    loop {
        let line_len = current_line_data_len();
        byte_sum += line_len;
        if byte_sum > location_val {
            break;
        }
        if !advance_current_to_next() {
            break;
        }
    }
    set_edittop_to_current();
}

// ---------------------------------------------------------------------------
// show_help
// ---------------------------------------------------------------------------

/// Assemble a help text, display it, and allow scrolling through it.
/* C: void show_help(void) */
#[cfg(feature = "help")]
pub fn show_help() {
    use help_state::*;
    use stubs::*;
    use crate::definitions::FuncPtr;
    use crate::global::{
        flag_index, flag_mask,
        interpret,
        do_left, do_right, do_up, do_down,
        do_search_backward, do_search_forward,
        do_findprevious, do_findnext,
        do_scroll_up, do_scroll_down,
        do_page_up, do_page_down,
        to_first_line, to_last_line, do_exit,
    };
    use crate::winio::{bottombars, titlebar, edit_refresh, blank_statusbar, get_kbinput};
    use crate::nano::window_init;

    // Save state that we need to restore afterward.
    let (oldmenu, was_tabsize) = STATE.with(|s| {
        let st = s.borrow();
        (st.currmenu, st.tabsize)
    });

    #[cfg(feature = "linenumbers")]
    let was_margin = STATE.with(|s| s.borrow().margin);

    #[cfg(feature = "color")]
    let was_syntax = STATE.with(|s| s.borrow().syntaxstr.clone());

    let saved_answer = STATE.with(|s| s.borrow().answer.clone());

    // Save the settings of all flags.
    let stash = STATE.with(|s| s.borrow().flags.clone());

    // Ensure the help screen's shortcut list can be displayed.
    let (no_help_set, zero_set) = STATE.with(|s| {
        let st = s.borrow();
        (st.flag_isset(NO_HELP), st.flag_isset(ZERO))
    });
    if no_help_set || zero_set {
        STATE.with(|s| {
            let st = s.borrow_mut();
            st.flags[flag_index(NO_HELP)] &= !flag_mask(NO_HELP);
            st.flags[flag_index(ZERO)]    &= !flag_mask(ZERO);
        });
        window_init();
    } else {
        blank_statusbar();
    }

    // When searching, do it forward, case insensitive, and without regexes.
    STATE.with(|s| {
        let st = s.borrow_mut();
        st.flags[flag_index(BACKWARDS_SEARCH)]   &= !flag_mask(BACKWARDS_SEARCH);
        st.flags[flag_index(CASE_SENSITIVE)]      &= !flag_mask(CASE_SENSITIVE);
        st.flags[flag_index(USE_REGEXP)]          &= !flag_mask(USE_REGEXP);
        st.flags[flag_index(WHITESPACE_DISPLAY)]  &= !flag_mask(WHITESPACE_DISPLAY);
    });

    #[cfg(feature = "linenumbers")]
    STATE.with(|s| {
        let st = s.borrow_mut();
        let cols = st.midwin.cols as i32;
        let sidebar = st.sidebar;
        st.editwincols = cols - sidebar;
        st.margin = 0;
    });

    STATE.with(|s| s.borrow_mut().tabsize = 8);

    #[cfg(feature = "color")]
    STATE.with(|s| s.borrow_mut().syntaxstr = Some("nanohelp".to_string()));

    curs_set(0);

    // Save the current editing buffer: wrap_help_text_into_buffer() calls
    // make_new_buffer() which replaces openfile. We restore it on exit.
    let saved_openfile = crate::global::with_state_mut(|s| s.openfile.take());

    // Compose the help text from all the relevant pieces.
    help_init();

    STATE.with(|s| {
        let st = s.borrow_mut();
        st.inhelp = true;
    });
    LOCATION.with(|l| *l.borrow_mut() = 0);
    STATE.with(|s| s.borrow_mut().didfind = 0);

    bottombars(MHELP);

    // Extract the title from the head of the help text.
    let title = HELP_TEXT.with(|ht| {
        let text_opt = ht.borrow();
        if let Some(ref text) = *text_opt {
            // C: length = break_line(help_text, HIGHEST_POSITIVE, TRUE) returns the
            // index OF the '\n', so the title excludes the trailing newline.
            let raw = crate::text::break_line(text, isize::MAX, true);
            let length = if raw < 0 { text.len() } else { (raw as usize).min(text.len()) };
            text[..length].to_string()
        } else {
            String::new()
        }
    });

    STATE.with(|s| {
        let st = s.borrow_mut();
        st.title = Some(title.clone());
    });
    titlebar(None);

    // Skip over the title to point at the start of the body text.
    let body_offset = HELP_TEXT.with(|ht| {
        if let Some(ref text) = *ht.borrow() {
            let raw = crate::text::break_line(text, isize::MAX, true);
            let length = if raw < 0 { text.len() } else { (raw as usize).min(text.len()) };
            let mut off = length;
            let bytes = text.as_bytes();
            while off < bytes.len() && bytes[off] == b'\n' {
                off += 1;
            }
            off
        } else {
            0
        }
    });
    START_OF_BODY_OFFSET.with(|s| *s.borrow_mut() = body_offset);

    wrap_help_text_into_buffer();
    edit_refresh();

    // Main help input loop.
    loop {
        STATE.with(|s| {
            let st = s.borrow_mut();
            st.lastmessage = MessageType::Vacuum;
            st.focusing = true;
        });

        let show_cursor = STATE.with(|s| {
            let st = s.borrow();
            st.didfind == 1 || st.flag_isset(SHOW_CURSOR)
        });

        let kbinput = get_kbinput(show_cursor);

        STATE.with(|s| s.borrow_mut().didfind = 0);

        #[cfg(not(feature = "tiny"))]
        STATE.with(|s| s.borrow_mut().spotlighted = false);

        let function = interpret(kbinput);

        let show_cursor_now = STATE.with(|s| s.borrow().flag_isset(SHOW_CURSOR));

        if show_cursor_now
            && (function == Some(do_left as FuncPtr)
                || function == Some(do_right as FuncPtr)
                || function == Some(do_up as FuncPtr)
                || function == Some(do_down as FuncPtr))
        {
            if let Some(f) = function { f(); }
        } else if function == Some(do_up as FuncPtr)
            || function == Some(do_scroll_up as FuncPtr)
        {
            do_scroll_up();
        } else if function == Some(do_down as FuncPtr)
            || function == Some(do_scroll_down as FuncPtr)
        {
            let can_scroll = edittop_lineno() + STATE.with(|s| s.borrow().editwinrows) as isize - 1
                < filebot_lineno();
            if can_scroll {
                do_scroll_down();
            }
        } else if function == Some(do_page_up as FuncPtr)
            || function == Some(do_page_down as FuncPtr)
            || function == Some(to_first_line as FuncPtr)
            || function == Some(to_last_line as FuncPtr)
        {
            if let Some(f) = function { f(); }
        } else if function == Some(do_search_backward as FuncPtr)
            || function == Some(do_search_forward as FuncPtr)
            || function == Some(do_findprevious as FuncPtr)
            || function == Some(do_findnext as FuncPtr)
        {
            if let Some(f) = function { f(); }
            bottombars(MHELP);
        } else {
            // Handle implant (nanorc string bind).
            #[cfg(feature = "nanorc")]
            if let Some(func) = function {
                if let Some(expansion) = STATE.with(|s| {
                    s.borrow().sclist.iter()
                        .find(|sc| (sc.menus as u32 & MHELP) != 0 && sc.func == Some(func))
                        .and_then(|sc| sc.expansion.clone())
                }) {
                    crate::winio::implant(&expansion);
                } else {
                    handle_unrecognized_input(kbinput, function);
                }
            } else {
                handle_unrecognized_input(kbinput, function);
            }

            #[cfg(not(feature = "nanorc"))]
            handle_unrecognized_input(kbinput, function);
        }

        edit_refresh();

        // Count how far (in bytes) edittop is into the file.
        let new_location = compute_edittop_byte_offset();
        LOCATION.with(|l| *l.borrow_mut() = new_location);

        // Check for exit after processing.
        if function == Some(do_exit as FuncPtr) {
            break;
        }
    }

    // Discard the help-text buffer and restore the original editing buffer.
    // (The help buffer was created while `openfile` was taken out, so it is
    // standalone — not in the buffer ring; overwriting it drops it.  Don't
    // call close_buffer_impl() here: that would pop a user buffer off the
    // ring, which the restore below would then leak.)
    crate::global::with_state_mut(|s| s.openfile = saved_openfile);

    // Restore the settings of all flags.
    STATE.with(|s| s.borrow_mut().flags = stash);

    #[cfg(feature = "linenumbers")]
    STATE.with(|s| {
        let st = s.borrow_mut();
        st.margin = was_margin;
        let cols = st.midwin.cols as i32;
        let margin = st.margin;
        let sidebar = st.sidebar;
        st.editwincols = cols - margin - sidebar;
    });

    STATE.with(|s| s.borrow_mut().tabsize = was_tabsize);

    #[cfg(feature = "color")]
    STATE.with(|s| {
        let st = s.borrow_mut();
        st.syntaxstr = was_syntax;
        st.have_palette = false;
    });

    STATE.with(|s| {
        let st = s.borrow_mut();
        st.title = None;
    });
    STATE.with(|s| s.borrow_mut().answer = saved_answer);

    HELP_TEXT.with(|ht| *ht.borrow_mut() = None);

    STATE.with(|s| s.borrow_mut().inhelp = false);
    curs_set(0);

    let (no_help_now, zero_now) = STATE.with(|s| {
        let st = s.borrow();
        (st.flag_isset(NO_HELP), st.flag_isset(ZERO))
    });
    if no_help_now || zero_now {
        window_init();
    } else {
        blank_statusbar();
    }

    bottombars(oldmenu);

    #[cfg(feature = "browser")]
    {
        let in_browser = (oldmenu & (MBROWSER | MGOTODIR | MWHEREISFILE)) != 0;
        if in_browser {
            crate::browser::browser_refresh();
            return;
        }
    }

    titlebar(None);
    edit_refresh();
}

// ---------------------------------------------------------------------------
// handle_unrecognized_input — used from show_help's else branch
// ---------------------------------------------------------------------------
#[cfg(feature = "help")]
fn handle_unrecognized_input(kbinput: i32, function: Option<FuncPtr>) {
    use crate::definitions::FuncPtr;
    use crate::global::{full_refresh, do_exit};

    if function == Some(full_refresh as FuncPtr) {
        full_refresh();
    } else if function == Some(do_exit as FuncPtr) {
        // exit is handled in the loop; nothing extra to do here
    } else {
        // Handle bracketed paste (#ifndef NANO_TINY).
        #[cfg(not(feature = "tiny"))]
        if kbinput == START_OF_PASTE as i32 {
            loop {
                let k = crate::winio::get_kbinput(false);
                if k == END_OF_PASTE as i32 { break; }
            }
            crate::winio::statusline(MessageType::Ahem, tr!("Paste is ignored"));
            return;
        }
        #[cfg(not(feature = "tiny"))]
        if kbinput == THE_WINDOW_RESIZED as i32 {
            return;
        }

        #[cfg(feature = "mouse")]
        if kbinput == crate::winio::KEY_MOUSE_CODE {
            return;
        }

        crate::nano::unbound_key(kbinput);
    }
}

// ---------------------------------------------------------------------------
// do_help  (always present, the public entry point)
// ---------------------------------------------------------------------------

/* C: void do_help(void) */
/// Start the help viewer, or indicate that there is no help.
pub fn do_help() {
    #[cfg(feature = "help")]
    {
        show_help();
    }
    #[cfg(not(feature = "help"))]
    {
        let currmenu: u32 = STATE.with(|s| s.borrow().currmenu);
        if (currmenu & (MMAIN | MBROWSER)) != 0 {
            crate::winio::statusline(
                MessageType::Info,
                tr!("^W = Ctrl+W    M-W = Alt+W"),
            );
        } else {
            // beep — stub
        }
    }
}
