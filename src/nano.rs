#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]
// Port of src/nano.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2026 Benno Schulenberg

use std::io::Write as _;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[cfg(not(feature = "tiny"))]
use crate::TOGGLE;
use crate::definitions::*;
use crate::global::{state, state_mut, with_state, with_state_mut};
use crate::{ISSET, SET, UNSET};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::{color, files, history, prompt, rcfile, search, winio};

// ---------------------------------------------------------------------------
// Atomic signal flags (replacing volatile sig_atomic_t globals in C)
// ---------------------------------------------------------------------------

/// Whether Ctrl+C was pressed (set by SIGINT handler).
/// C: bool control_C_was_pressed
pub static CONTROL_C_WAS_PRESSED: AtomicBool = AtomicBool::new(false);

/// Whether SIGWINCH has fired.
/// C: bool the_window_resized
pub static THE_WINDOW_RESIZED: AtomicBool = AtomicBool::new(false);

/// Fatal-but-recoverable termination requested by SIGHUP or SIGTERM.  The
/// handler only stores the signal; emergency saving happens at a safe point in
/// the main loop.
#[cfg(unix)]
static TERMINATION_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// SIGTSTP is likewise deferred so terminal I/O and AppState mutation never run
/// from an asynchronous signal context.
#[cfg(all(unix, not(feature = "tiny")))]
static SUSPEND_REQUESTED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// make_new_node — create a new linestruct node
// ---------------------------------------------------------------------------
/* C: linestruct *make_new_node(linestruct *prevnode) */
pub fn make_new_node(prev: Option<&LinePtr>) -> LinePtr {
    let lineno = prev.map(|p| p.borrow().lineno + 1).unwrap_or(1);
    let prev_link = prev.map(LinePtr::downgrade);
    // Allocate after dropping any read guard so the slab may grow freely.
    state_mut().lines.alloc(LineNode {
        data: LineData::empty(),
        lineno,
        next: None,
        prev: prev_link,
        #[cfg(feature = "color")]
        multidata: Vec::new(),
        has_anchor: false,
    })
}

// ---------------------------------------------------------------------------
// splice_node — splice a new node into an existing linked list
// ---------------------------------------------------------------------------
/* C: void splice_node(linestruct *afterthis, linestruct *newnode) */
pub fn splice_node(afterthis: &LinePtr, newnode: LinePtr) {
    // newnode->next = afterthis->next
    let old_next = afterthis.borrow().next.clone();
    newnode.borrow_mut().next = old_next.clone();
    // newnode->prev = afterthis (weak)
    newnode.borrow_mut().prev = Some(LinePtr::downgrade(afterthis));
    // if afterthis->next: afterthis->next->prev = newnode
    if let Some(ref next_node) = old_next {
        next_node.borrow_mut().prev = Some(LinePtr::downgrade(&newnode));
    }
    // afterthis->next = newnode
    afterthis.borrow_mut().next = Some(newnode.clone());

    // Update filebot when inserting a node at the end of file.
    // (C compares pointers, so a cutbuffer node can never match.)
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            let is_filebot = of
                .filebot
                .as_ref()
                .map(|b| LinePtr::ptr_eq(b, afterthis))
                .unwrap_or(false);
            if is_filebot {
                of.filebot = Some(newnode.clone());
            }
        }
    });
}

// ---------------------------------------------------------------------------
// delete_node — free the data in a node
// ---------------------------------------------------------------------------
/* C: void delete_node(linestruct *line) */
pub fn delete_node(line: &LinePtr) {
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            // If the first line on the screen gets deleted, step one back.
            let is_edittop = of
                .edittop
                .as_ref()
                .map(|e| LinePtr::ptr_eq(e, line))
                .unwrap_or(false);
            if is_edittop {
                let prev = line.borrow().prev.clone().and_then(|w| w.upgrade());
                of.edittop = prev;
            }
            // ENABLE_WRAPPING: if this is spillage_line, clear it
            #[cfg(feature = "wrapping")]
            {
                let is_spillage = of
                    .spillage_line
                    .as_ref()
                    .map(|sl| LinePtr::ptr_eq(sl, line))
                    .unwrap_or(false);
                if is_spillage {
                    of.spillage_line = None;
                }
            }
        }
    });
    // The node's data is freed automatically when the last LinePtr handle
    // drops and the arena refcount reaches zero.
}

// ---------------------------------------------------------------------------
// unlink_node — disconnect a node from the linked list and delete it
// ---------------------------------------------------------------------------
/* C: void unlink_node(linestruct *line) */
pub fn unlink_node(line: &LinePtr) {
    let prev_weak = line.borrow().prev.clone();
    let next_opt = line.borrow().next.clone();

    if let Some(ref prev_w) = prev_weak {
        if let Some(ref prev) = prev_w.upgrade() {
            prev.borrow_mut().next = next_opt.clone();
        }
    }
    if let Some(ref next) = next_opt {
        next.borrow_mut().prev = prev_weak.clone();
    }

    // Update filebot when removing a node at the end of file.
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            let is_filebot = of
                .filebot
                .as_ref()
                .map(|b| LinePtr::ptr_eq(b, line))
                .unwrap_or(false);
            if is_filebot {
                of.filebot = prev_weak.and_then(|w| w.upgrade());
            }
        }
    });

    delete_node(line);
}

// ---------------------------------------------------------------------------
// free_lines — free an entire linked list of linestructs
// ---------------------------------------------------------------------------
/* C: void free_lines(linestruct *src) */
pub fn free_lines(mut src: Option<LinePtr>) {
    // Walk forward, severing each node's links as we go. Severing forward links
    // iteratively (rather than letting one head handle drop the whole chain)
    // keeps the arena refcount cascade flat instead of recursing the list depth.
    while let Some(node) = src {
        let next = node.borrow().next.clone();
        node.borrow_mut().next = None;
        node.borrow_mut().prev = None;
        src = next;
    }
}

// ---------------------------------------------------------------------------
// copy_node — make a copy of a linestruct node
// ---------------------------------------------------------------------------
/* C: linestruct *copy_node(const linestruct *src) */
pub fn copy_node(src: &LinePtr) -> LinePtr {
    let node = {
        let src_b = src.borrow();
        LineNode {
            data: src_b.data.clone(),
            lineno: src_b.lineno,
            next: None,
            prev: None,
            #[cfg(feature = "color")]
            multidata: Vec::new(),
            has_anchor: src_b.has_anchor,
        }
    };
    state_mut().lines.alloc(node)
}

// ---------------------------------------------------------------------------
// copy_buffer — duplicate an entire linked list of linestructs
// ---------------------------------------------------------------------------
/* C: linestruct *copy_buffer(const linestruct *src) */
pub fn copy_buffer(src: &LinePtr) -> LinePtr {
    let head = copy_node(src);
    let mut item = head.clone();
    let mut cur_src = src.borrow().next.clone();

    while let Some(ref next_src) = cur_src.clone() {
        let new_node = copy_node(next_src);
        new_node.borrow_mut().prev = Some(LinePtr::downgrade(&item));
        item.borrow_mut().next = Some(new_node.clone());
        item = new_node;
        cur_src = next_src.borrow().next.clone();
    }
    item.borrow_mut().next = None;
    head
}

// ---------------------------------------------------------------------------
// renumber_from — renumber lines starting from the given line
// ---------------------------------------------------------------------------
/* C: void renumber_from(linestruct *line) */
pub fn renumber_from(start: &LinePtr) {
    let start_number = {
        let b = start.borrow();
        if b.prev.is_none() {
            0isize
        } else {
            b.prev
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|p| p.borrow().lineno)
                .unwrap_or(0)
        }
    };

    let mut number = start_number;
    let mut cur = Some(start.clone());
    while let Some(node) = cur {
        number += 1;
        node.borrow_mut().lineno = number;
        cur = node.borrow().next.clone();
    }
}

// ---------------------------------------------------------------------------
// print_view_warning — display a warning about a key disabled in view mode
// ---------------------------------------------------------------------------
/* C: void print_view_warning(void) */
pub fn print_view_warning() {
    winio::statusline(MessageType::Ahem, "Key is invalid in view mode");
}

// ---------------------------------------------------------------------------
// in_restricted_mode — warn and return true when in restricted mode
// ---------------------------------------------------------------------------
/* C: bool in_restricted_mode(void) */
pub fn in_restricted_mode() -> bool {
    if ISSET!(RESTRICTED) {
        winio::statusline(
            MessageType::Ahem,
            "This function is disabled in restricted mode",
        );
        winio::beep();
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// suggest_ctrlT_ctrlZ — tell user how to suspend
// ---------------------------------------------------------------------------
/* C: void suggest_ctrlT_ctrlZ(void) */
#[cfg(not(feature = "tiny"))]
pub fn suggest_ctrlT_ctrlZ() {
    #[cfg(feature = "nanorc")]
    {
        use crate::global::first_sc_for;
        // Check if ^T is bound to do_execute and ^Z is bound to do_suspend in MEXECUTE.
        let exec_bound = first_sc_for(
            MMAIN,
            crate::global::do_execute as crate::definitions::FuncPtr,
        )
        .map(|(kc, _)| kc == 0x14)
        .unwrap_or(false);
        let susp_bound = first_sc_for(
            MEXECUTE,
            crate::nano::do_suspend as crate::definitions::FuncPtr,
        )
        .map(|(kc, _)| kc == 0x1A)
        .unwrap_or(false);
        if exec_bound && susp_bound {
            winio::statusline(MessageType::Ahem, "To suspend, type ^T^Z");
        }
    }
    #[cfg(not(feature = "nanorc"))]
    {
        winio::statusline(MessageType::Ahem, "To suspend, type ^T^Z");
    }
}

// ---------------------------------------------------------------------------
// restore_terminal — make cursor visible, exit curses, restore terminal state
// ---------------------------------------------------------------------------
/* C: void restore_terminal(void) */
pub fn restore_terminal() {
    #[cfg(not(feature = "tiny"))]
    if winio::terminal_is_active() {
        // Disable bracketed-paste mode (through the shared buffer, so it is
        // ordered after any pending paint).
        use std::io::Write;
        let _ = write!(crate::winio::out(), "\x1B[?2004l");
        crate::winio::flush_out();
    }
    if winio::terminal_exit().is_err() {
        // Retry through the independent fallback.  This matters most on fatal
        // exits, where there will be no later redraw or teardown opportunity.
        #[cfg(unix)]
        winio::signal_safe_terminal_restore();
        #[cfg(not(unix))]
        winio::emergency_terminal_restore();
    }
}

// ---------------------------------------------------------------------------
// finish — exit normally
// ---------------------------------------------------------------------------
/* C: void finish(void) */
pub fn finish() {
    winio::blank_statusbar();
    winio::blank_bottombars();
    restore_terminal();

    #[cfg(any(feature = "nanorc", feature = "histories"))]
    rcfile::display_rcfile_errors();

    let status = state().final_status;
    process::exit(status);
}

// ---------------------------------------------------------------------------
// close_and_go — close current buffer, terminate if it is the only one
// ---------------------------------------------------------------------------
/* C: void close_and_go(void) */
pub fn close_and_go() {
    #[cfg(not(feature = "tiny"))]
    {
        let (lock_filename, lock_path, lock_file) = with_state_mut(|s| match s.openfile.as_mut() {
            Some(of) => (
                of.lock_filename.take(),
                of.lock_path.take(),
                of.lock_file.take(),
            ),
            None => (None, None, None),
        });
        if let Some(ref lp) = lock_path {
            files::delete_lockfile(lp, lock_file.as_ref());
        } else if let Some(ref lf) = lock_filename {
            files::delete_lockfile(lf, lock_file.as_ref());
        }
        drop(lock_file);
    }

    #[cfg(feature = "histories")]
    {
        let has_filename = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|of| !of.filename.is_empty())
                .unwrap_or(false)
        });
        if ISSET!(POSITIONLOG) && has_filename {
            history::update_positions_register();
        }
    }

    // If there is another buffer, close this one; otherwise terminate.
    #[cfg(feature = "multibuffer")]
    {
        let has_other = with_state(|s| !s.buffer_ring.is_empty());
        if has_other {
            // C: switch_to_next_buffer(); openfile = openfile->prev;
            //    close_buffer(); openfile = openfile->next; titlebar(NULL);
            // i.e. close the current buffer and land on the next one.
            files::switch_to_next_buffer();
            with_state_mut(|s| {
                // Drop the buffer we just left (now at the back of the ring).
                let _orphan = s.buffer_ring.pop_back();
                if s.buffer_ring.is_empty() {
                    if let Some(idx) = s.exitfunc {
                        s.allfuncs[idx].tag = s.exit_tag;
                    }
                }
            });
            winio::titlebar(None);
            return;
        }
    }

    #[cfg(feature = "histories")]
    {
        if ISSET!(HISTORYLOG) {
            history::save_history();
        }
    }

    finish();
}

// ---------------------------------------------------------------------------
// do_exit — close the current buffer / exit nano
// ---------------------------------------------------------------------------
/* C: void do_exit(void) */
pub fn do_exit() {
    let (is_modified, view_mode, save_on_exit, has_filename) = with_state(|s| {
        let of = s.openfile.as_ref();
        let modified = of.map(|f| f.modified).unwrap_or(false);
        let view_mode = (s.flags[crate::global::flag_index(VIEW_MODE)]
            & crate::global::flag_mask(VIEW_MODE))
            != 0;
        let save_on_exit = (s.flags[crate::global::flag_index(SAVE_ON_EXIT)]
            & crate::global::flag_mask(SAVE_ON_EXIT))
            != 0;
        let has_filename = of.map(|f| !f.filename.is_empty()).unwrap_or(false);
        (modified, view_mode, save_on_exit, has_filename)
    });

    let choice = if !is_modified || view_mode {
        NO
    } else if save_on_exit && has_filename {
        YES
    } else {
        if save_on_exit {
            winio::warn_and_briefly_pause("No file name");
        }
        prompt::ask_user(YESORNO, "Save modified buffer? ")
    };

    if choice == NO || (choice == YES && files::write_it_out(true, true) > 0) {
        close_and_go();
    } else if choice != YES {
        winio::statusbar("Cancelled");
    }
}

// ---------------------------------------------------------------------------
// emergency_save — save the buffer to filename.save
// ---------------------------------------------------------------------------
/* C: void emergency_save(const char *filename) */
pub fn emergency_save(filename: &std::path::Path) {
    let plainname = if filename.as_os_str().is_empty() {
        std::path::PathBuf::from(format!("nano.{}", std::process::id()))
    } else {
        filename.to_path_buf()
    };

    let targetname = files::get_next_filename_path(&plainname, ".save");

    if targetname.as_os_str().is_empty() {
        let _ = writeln!(std::io::stderr().lock(), "\nToo many .save files");
    } else if files::write_file(
        &targetname,
        None,
        SPECIAL,
        KindOfWritingType::Emergency,
        NONOTES,
    ) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "\nBuffer written to {}",
            targetname.display(),
        );
    }
}

// ---------------------------------------------------------------------------
// die — restore terminal state and save any modified buffers, then exit
// ---------------------------------------------------------------------------
/// Delete the lock files of all open buffers and emergency-save every
/// modified one (C: the buffer loop at the end of die()).
fn save_all_modified_buffers() {
    let restricted = ISSET!(RESTRICTED);

    // Walk the whole circle of buffers, like C's
    // `while (openfile) { ...; openfile = openfile->next; }`.
    // emergency_save writes the *current* buffer, so rotate each one in.
    let total = {
        #[cfg(feature = "multibuffer")]
        {
            with_state(|s| s.buffer_ring.len() + usize::from(s.openfile.is_some()))
        }
        #[cfg(not(feature = "multibuffer"))]
        {
            usize::from(state().openfile.is_some())
        }
    };

    for _ in 0..total {
        let (lock, lock_path, lock_file, save, filename) = with_state_mut(|s| {
            let save = s.openfile.as_ref().map(|f| f.modified).unwrap_or(false) && !restricted;
            let filename = s
                .openfile
                .as_ref()
                .map(|f| {
                    if f.filename_path.as_os_str().is_empty() {
                        std::path::PathBuf::from(&f.filename)
                    } else {
                        f.filename_path.clone()
                    }
                })
                .unwrap_or_default();
            #[cfg(not(feature = "tiny"))]
            let (lock, lock_path, lock_file) = match s.openfile.as_mut() {
                Some(file) => (
                    file.lock_filename.take(),
                    file.lock_path.take(),
                    file.lock_file.take(),
                ),
                None => (None, None, None),
            };
            #[cfg(feature = "tiny")]
            let (lock, lock_path, lock_file): (
                Option<String>,
                Option<std::path::PathBuf>,
                Option<std::fs::File>,
            ) = (None, None, None);
            (lock, lock_path, lock_file, save, filename)
        });

        if let Some(ref lp) = lock_path {
            files::delete_lockfile(lp, lock_file.as_ref());
        } else if let Some(ref lf) = lock {
            files::delete_lockfile(lf, lock_file.as_ref());
        }
        drop(lock_file);
        if save {
            emergency_save(&filename);
        }

        // Rotate to the next buffer in the ring.
        #[cfg(feature = "multibuffer")]
        with_state_mut(|s| {
            if let Some(next) = s.buffer_ring.pop_front() {
                if let Some(cur) = s.openfile.replace(next) {
                    s.buffer_ring.push_back(cur);
                }
            }
        });
    }
}

/* C: void die(const char *msg, ...) */
pub fn die(msg: &str) -> ! {
    static STABS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    if STABS.fetch_add(1, Ordering::SeqCst) > 0 {
        process::exit(11);
    }

    restore_terminal();

    // A dead PTY makes stderr writes fail with EIO.  Save first, and never let
    // diagnostics panic before the emergency copy reaches disk.
    save_all_modified_buffers();

    #[cfg(feature = "nanorc")]
    rcfile::display_rcfile_errors();

    let _ = writeln!(std::io::stderr().lock(), "{}", msg);

    process::exit(1);
}

// ---------------------------------------------------------------------------
// window_init — initialize the three window portions nano uses
// ---------------------------------------------------------------------------
/* C: void window_init(void) */
pub fn window_init() {
    winio::window_init();
}

// ---------------------------------------------------------------------------
// mouse support helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "mouse")]
pub fn disable_mouse_support() {
    // crossterm: mouse support is toggled via event::DisableMouseCapture
    use crossterm::{event::DisableMouseCapture, execute};
    let _ = execute!(crate::winio::out(), DisableMouseCapture);
}

#[cfg(feature = "mouse")]
pub fn enable_mouse_support() {
    use crossterm::{event::EnableMouseCapture, execute};
    let _ = execute!(crate::winio::out(), EnableMouseCapture);
}

#[cfg(feature = "mouse")]
pub fn mouse_init() {
    if ISSET!(USE_MOUSE) {
        enable_mouse_support();
    } else {
        disable_mouse_support();
    }
}

// ---------------------------------------------------------------------------
// print_opt — print the usage line for the given option
// ---------------------------------------------------------------------------
/* C: void print_opt(const char *shortflag, const char *longflag, const char *description) */
pub fn print_opt(shortflag: &str, longflag: &str, description: &str) {
    let firstwidth = shortflag.len();
    let secondwidth = longflag.len();

    print!(" {}", shortflag);
    if firstwidth < 14 {
        print!("{:width$}", " ", width = 14 - firstwidth);
    }
    print!(" {}", longflag);
    if secondwidth < 24 {
        print!("{:width$}", " ", width = 24 - secondwidth);
    }
    println!("{}", description);
}

// ---------------------------------------------------------------------------
// usage — explain how to properly use nano and its command-line options
// ---------------------------------------------------------------------------
/* C: void usage(void) */
pub fn usage() {
    println!("Usage: nano [OPTIONS] [[+LINE[,COLUMN]] FILE]...\n");
    #[cfg(not(feature = "tiny"))]
    {
        println!("To place the cursor on a specific line of a file, put the line number with");
        println!("a '+' before the filename.  The column number can be added after a comma.");
        println!("When a filename is '-', nano reads data from standard input.\n");
        print_opt("Option", "Long option", "Meaning");
        print_opt("-A", "--smarthome", "Enable smart home key");
        if !ISSET!(RESTRICTED) {
            print_opt("-B", "--backup", "Save backups of existing files");
            print_opt(
                "-C <dir>",
                "--backupdir=<dir>",
                "Directory for saving unique backup files",
            );
        }
    }
    print_opt("-D", "--boldtext", "Use bold instead of reverse video text");
    #[cfg(not(feature = "tiny"))]
    print_opt("-E", "--tabstospaces", "Convert typed tabs to spaces");
    #[cfg(feature = "multibuffer")]
    {
        if !ISSET!(RESTRICTED) {
            print_opt(
                "-F",
                "--multibuffer",
                "Read a file into a new buffer by default",
            );
        }
    }
    #[cfg(not(feature = "tiny"))]
    print_opt("-G", "--locking", "Use (vim-style) lock files");
    #[cfg(feature = "histories")]
    {
        if !ISSET!(RESTRICTED) {
            print_opt(
                "-H",
                "--historylog",
                "Save & reload old search/replace strings",
            );
        }
    }
    #[cfg(feature = "nanorc")]
    print_opt("-I", "--ignorercfiles", "Don't look at nanorc files");
    #[cfg(not(feature = "tiny"))]
    print_opt(
        "-J <number>",
        "--guidestripe=<number>",
        "Show a guiding bar at this column",
    );
    print_opt(
        "-K",
        "--rawsequences",
        "Fix numeric keypad key confusion problem",
    );
    #[cfg(not(feature = "tiny"))]
    print_opt("-L", "--nonewlines", "Don't add an automatic newline");
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    print_opt("-M", "--trimblanks", "Trim tail spaces when hard-wrapping");
    #[cfg(not(feature = "tiny"))]
    {
        print_opt("-N", "--noconvert", "Don't convert files from DOS format");
        print_opt(
            "-O",
            "--bookstyle",
            "Leading whitespace means new paragraph",
        );
    }
    #[cfg(feature = "histories")]
    {
        if !ISSET!(RESTRICTED) {
            print_opt(
                "-P",
                "--positionlog",
                "Save & restore position of the cursor",
            );
        }
    }
    #[cfg(feature = "justify")]
    print_opt(
        "-Q <regex>",
        "--quotestr=<regex>",
        "Regular expression to match quoting",
    );
    if !ISSET!(RESTRICTED) {
        print_opt("-R", "--restricted", "Restrict access to the filesystem");
    }
    #[cfg(not(feature = "tiny"))]
    {
        print_opt(
            "-S",
            "--softwrap",
            "Display overlong lines on multiple rows",
        );
        print_opt(
            "-T <number>",
            "--tabsize=<number>",
            "Make a tab this number of columns wide",
        );
    }
    print_opt("-U", "--quickblank", "Wipe status bar upon next keystroke");
    print_opt("-V", "--version", "Print version information and exit");
    #[cfg(not(feature = "tiny"))]
    {
        print_opt(
            "-W",
            "--wordbounds",
            "Detect word boundaries more accurately",
        );
        print_opt(
            "-X <string>",
            "--wordchars=<string>",
            "Which other characters are word parts",
        );
    }
    #[cfg(feature = "color")]
    print_opt(
        "-Y <name>",
        "--syntax=<name>",
        "Syntax definition to use for coloring",
    );
    #[cfg(not(feature = "tiny"))]
    {
        print_opt("-Z", "--zap", "Let Bsp and Del erase a marked region");
        print_opt(
            "-a",
            "--atblanks",
            "When soft-wrapping, do it at whitespace",
        );
    }
    #[cfg(feature = "wrapping")]
    print_opt(
        "-b",
        "--breaklonglines",
        "Automatically hard-wrap overlong lines",
    );
    print_opt("-c", "--constantshow", "Constantly show cursor position");
    print_opt(
        "-d",
        "--rebinddelete",
        "Fix Backspace/Delete confusion problem",
    );
    #[cfg(not(feature = "tiny"))]
    print_opt(
        "-e",
        "--emptyline",
        "Keep the line below the title bar empty",
    );
    #[cfg(feature = "nanorc")]
    print_opt(
        "-f <file>",
        "--rcfile=<file>",
        "Use only this file for configuring nano",
    );
    #[cfg(any(feature = "browser", feature = "help"))]
    print_opt(
        "-g",
        "--showcursor",
        "Show cursor in file browser & help text",
    );
    print_opt("-h", "--help", "Show this help text and exit");
    #[cfg(not(feature = "tiny"))]
    {
        print_opt("-i", "--autoindent", "Automatically indent new lines");
        print_opt(
            "-j",
            "--jumpyscrolling",
            "Scroll per half-screen, not per line",
        );
        print_opt("-k", "--cutfromcursor", "Cut from cursor to end of line");
    }
    #[cfg(feature = "linenumbers")]
    print_opt(
        "-l",
        "--linenumbers",
        "Show line numbers in front of the text",
    );
    #[cfg(feature = "mouse")]
    print_opt("-m", "--mouse", "Enable the use of the mouse");
    #[cfg(not(feature = "tiny"))]
    print_opt("-n", "--noread", "Do not read the file (only write it)");
    #[cfg(feature = "operatingdir")]
    print_opt(
        "-o <dir>",
        "--operatingdir=<dir>",
        "Set operating directory",
    );
    print_opt("-p", "--preserve", "Preserve XON (^Q) and XOFF (^S) keys");
    #[cfg(not(feature = "tiny"))]
    print_opt("-q", "--indicator", "Show a position+portion indicator");
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    print_opt(
        "-r <number>",
        "--fill=<number>",
        "Set width for hard-wrap and justify",
    );
    #[cfg(feature = "speller")]
    {
        if !ISSET!(RESTRICTED) {
            print_opt(
                "-s <program>",
                "--speller=<program>",
                "Use this alternative spell checker",
            );
        }
    }
    print_opt("-t", "--saveonexit", "Save changes on exit, don't prompt");
    #[cfg(not(feature = "tiny"))]
    print_opt("-u", "--unix", "Save a file by default in Unix format");
    print_opt("-v", "--view", "View mode (read-only)");
    #[cfg(feature = "wrapping")]
    print_opt("-w", "--nowrap", "Don't hard-wrap long lines [default]");
    print_opt("-x", "--nohelp", "Don't show the two help lines");
    #[cfg(not(feature = "tiny"))]
    print_opt("-y", "--afterends", "Make Ctrl+Right stop at word ends");
    #[cfg(feature = "color")]
    print_opt(
        "-z",
        "--listsyntaxes",
        "List the names of available syntaxes",
    );
    #[cfg(feature = "libmagic")]
    print_opt("-!", "--magic", "Also try magic to determine syntax");
    #[cfg(not(feature = "tiny"))]
    {
        print_opt(
            "-@",
            "--colonparsing",
            "Accept 'filename:linenumber' notation",
        );
        print_opt("-%", "--stateflags", "Show some states on the title bar");
        print_opt("-_", "--minibar", "Show a feedback bar at the bottom");
        print_opt("-0", "--zero", "Hide all bars, use whole terminal");
        print_opt(
            "-1",
            "--solosidescroll",
            "Scroll only the current line sideways",
        );
    }
    print_opt("-/", "--modernbindings", "Use better-known key bindings");
    print_opt("", "--install", "Install nano to a directory on your PATH");
    print_opt(
        "",
        "--update",
        "Download and install the latest release from GitHub",
    );
    print_opt(
        "",
        "--check",
        "Check GitHub for a newer release (no install)",
    );
    print_opt(
        "",
        "--force",
        "With --install/--update: act even if up to date",
    );
}

// ---------------------------------------------------------------------------
// version — display version and compiled options
// ---------------------------------------------------------------------------
/* C: void version(void) */
pub fn version() {
    println!(" GNU nano, version {}", GNU_NANO_VERSION);
    println!(
        " nano-rs {} (Rust port) \u{2014} https://github.com/faratech/nano-rs",
        env!("CARGO_PKG_VERSION")
    );
    #[cfg(not(feature = "tiny"))]
    println!(" (C) 2026 the Free Software Foundation and various contributors");
    print!(" Compiled options:");

    #[cfg(feature = "tiny")]
    {
        print!(" --enable-tiny");
        #[cfg(feature = "browser")]
        print!(" --enable-browser");
        #[cfg(feature = "color")]
        print!(" --enable-color");
        #[cfg(feature = "formatter")]
        print!(" --enable-formatter");
        #[cfg(feature = "help")]
        print!(" --enable-help");
        #[cfg(feature = "histories")]
        print!(" --enable-histories");
        #[cfg(feature = "justify")]
        print!(" --enable-justify");
        #[cfg(feature = "libmagic")]
        print!(" --enable-libmagic");
        #[cfg(feature = "linenumbers")]
        print!(" --enable-linenumbers");
        #[cfg(feature = "linter")]
        print!(" --enable-linter");
        #[cfg(feature = "mouse")]
        print!(" --enable-mouse");
        #[cfg(feature = "nanorc")]
        print!(" --enable-nanorc");
        #[cfg(feature = "multibuffer")]
        print!(" --enable-multibuffer");
        #[cfg(feature = "operatingdir")]
        print!(" --enable-operatingdir");
        #[cfg(feature = "speller")]
        print!(" --enable-speller");
        #[cfg(feature = "tabcomp")]
        print!(" --enable-tabcomp");
        #[cfg(feature = "wrapping")]
        print!(" --enable-wrapping");
    }
    #[cfg(not(feature = "tiny"))]
    {
        #[cfg(not(feature = "browser"))]
        print!(" --disable-browser");
        #[cfg(not(feature = "color"))]
        print!(" --disable-color");
        #[cfg(not(feature = "comment"))]
        print!(" --disable-comment");
        #[cfg(not(feature = "formatter"))]
        print!(" --disable-formatter");
        #[cfg(not(feature = "help"))]
        print!(" --disable-help");
        #[cfg(not(feature = "histories"))]
        print!(" --disable-histories");
        #[cfg(not(feature = "justify"))]
        print!(" --disable-justify");
        #[cfg(not(feature = "libmagic"))]
        print!(" --disable-libmagic");
        #[cfg(not(feature = "linenumbers"))]
        print!(" --disable-linenumbers");
        #[cfg(not(feature = "linter"))]
        print!(" --disable-linter");
        #[cfg(not(feature = "mouse"))]
        print!(" --disable-mouse");
        #[cfg(not(feature = "multibuffer"))]
        print!(" --disable-multibuffer");
        #[cfg(not(feature = "nanorc"))]
        print!(" --disable-nanorc");
        #[cfg(not(feature = "operatingdir"))]
        print!(" --disable-operatingdir");
        #[cfg(not(feature = "speller"))]
        print!(" --disable-speller");
        #[cfg(not(feature = "tabcomp"))]
        print!(" --disable-tabcomp");
        #[cfg(not(feature = "wordcomp"))]
        print!(" --disable-wordcomp");
        #[cfg(not(feature = "wrapping"))]
        print!(" --disable-wrapping");
    }
    #[cfg(not(feature = "utf8"))]
    print!(" --disable-utf8");
    #[cfg(feature = "utf8")]
    print!(" --enable-utf8");
    println!();
}

// ---------------------------------------------------------------------------
// list_syntax_names — list names of available syntaxes
// ---------------------------------------------------------------------------
/* C: void list_syntax_names(void) */
#[cfg(feature = "color")]
pub fn list_syntax_names() {
    println!("Available syntaxes:");
    // Iterate the linked list of syntaxes.
    // In a full port the SyntaxType list is in state.syntaxes.
    // For now we just print whatever names are available.
    with_state(|s| {
        let mut width = 0usize;
        let mut cur: Option<&SyntaxType> = s.syntaxes.as_deref();
        while let Some(sntx) = cur {
            if width > 45 {
                println!();
                width = 0;
            }
            print!(" {}", sntx.name);
            width += sntx.name.len();
            cur = sntx.next.as_deref();
        }
    });
    println!();
}

// ---------------------------------------------------------------------------
// make_a_note — register that Ctrl+C was pressed
// ---------------------------------------------------------------------------
/* C: void make_a_note(int signal) */
pub fn make_a_note(_signal: i32) {
    CONTROL_C_WAS_PRESSED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// install_handler_for_Ctrl_C / restore_handler_for_Ctrl_C
// ---------------------------------------------------------------------------
/* C: void install_handler_for_Ctrl_C(void) */
pub fn install_handler_for_Ctrl_C() {
    CONTROL_C_WAS_PRESSED.store(false, Ordering::SeqCst);
    // Deliberately omit SA_RESTART: a Ctrl-C must wake a blocking FIFO/stdin
    // read so the reader can observe CONTROL_C_WAS_PRESSED promptly.
    #[cfg(unix)]
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = make_a_note_trampoline as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

/* C: void restore_handler_for_Ctrl_C(void) */
pub fn restore_handler_for_Ctrl_C() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
}

/// Trampoline for SIGINT that is safe to use as a C function pointer.
#[cfg(unix)]
extern "C" fn make_a_note_trampoline(_sig: libc::c_int) {
    CONTROL_C_WAS_PRESSED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// scoop_stdin — read from standard input into a new buffer
// ---------------------------------------------------------------------------
/* C: bool scoop_stdin(void) */
#[cfg(any(not(feature = "tiny"), windows))]
/* C: void reconnect_and_store_state(void)
 * Reconnect standard input to the keyboard after it was used as a pipe. */
pub fn reconnect_and_store_state() {
    #[cfg(unix)]
    unsafe {
        let tty = std::ffi::CString::new("/dev/tty").unwrap();
        let thetty = libc::open(tty.as_ptr(), libc::O_RDONLY);
        if thetty < 0 || libc::dup2(thetty, 0) < 0 {
            die(crate::tr!("Could not reconnect stdin to keyboard\n"));
        }
        libc::close(thetty);
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{STD_INPUT_HANDLE, SetStdHandle};

        let console = match std::fs::File::open("CONIN$") {
            Ok(file) => file,
            Err(error) => {
                winio::statusline(
                    MessageType::Alert,
                    &format!("Could not reconnect console input: {}", error),
                );
                return;
            }
        };
        let raw = console.into_raw_handle();
        if unsafe { SetStdHandle(STD_INPUT_HANDLE, HANDLE(raw)) }.is_err() {
            // Reconstitute ownership so a failed handoff does not leak.
            use std::os::windows::io::FromRawHandle;
            drop(unsafe { std::fs::File::from_raw_handle(raw) });
            winio::statusline(MessageType::Alert, "Could not reconnect console input");
        }
    }
}

#[cfg(all(feature = "tiny", not(windows)))]
pub fn reconnect_and_store_state() {}

fn scoop_stdin_from<R: std::io::Read>(reader: R, stdin_is_terminal: bool) -> bool {
    restore_terminal();

    if stdin_is_terminal {
        eprintln!("Reading data from keyboard; type ^D or ^D^D to finish.");
    }

    install_handler_for_Ctrl_C();
    files::make_new_buffer();
    let read_succeeded = files::read_file_impl(reader, true, "stdin", false);
    restore_handler_for_Ctrl_C();

    if !stdin_is_terminal {
        reconnect_and_store_state();
    }
    terminal_init();

    if !read_succeeded {
        files::discard_transient_buffer();
        return false;
    }

    #[cfg(feature = "color")]
    color::find_and_prime_applicable_syntax();

    if !ISSET!(VIEW_MODE) {
        let totsize = with_state(|s| s.openfile.as_ref().map(|of| of.totsize).unwrap_or(0));
        if totsize > 0 {
            files::set_modified();
        }
    }

    true
}

pub fn scoop_stdin() -> bool {
    let stdin = std::io::stdin();
    let stdin_is_terminal = std::io::IsTerminal::is_terminal(&stdin);
    scoop_stdin_from(stdin.lock(), stdin_is_terminal)
}

/// Preserve redirected standard input before switching the process standard
/// handle back to the Windows console.  Crossterm needs a console input handle
/// in order to enter raw mode, while `nano -` still needs to consume the pipe.
#[cfg(windows)]
fn prepare_windows_terminal_input(retain_redirected_input: bool) -> Option<std::fs::File> {
    use std::io::IsTerminal;
    use std::os::windows::io::AsHandle;

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }

    let retained = retain_redirected_input.then(|| {
        stdin
            .as_handle()
            .try_clone_to_owned()
            .unwrap_or_else(|error| die(&format!("Could not retain redirected stdin: {error}\n")))
    });
    reconnect_and_store_state();
    if !std::io::stdin().is_terminal() {
        die(crate::tr!("Could not reconnect stdin to keyboard\n"));
    }
    retained.map(std::fs::File::from)
}

// ---------------------------------------------------------------------------
// Signal handlers
// ---------------------------------------------------------------------------

/* C: void handle_hupterm(int signal) */
#[cfg(unix)]
extern "C" fn handle_hupterm(signal: libc::c_int) {
    TERMINATION_SIGNAL.store(signal, Ordering::SeqCst);
}

/* C: void handle_crash(int signal) */
#[cfg(all(unix, not(feature = "tiny"), not(debug_assertions)))]
extern "C" fn handle_crash(signal: libc::c_int) {
    // A corrupted process cannot safely allocate, lock stdout, traverse editor
    // state, or attempt an emergency save.  Restore the display and the exact
    // pre-raw termios snapshot through the async-signal-safe fallback, emit a
    // static diagnostic, and terminate immediately.
    const MESSAGE: &[u8] = b"nano-rs crashed; modified buffers were not saved\n";
    winio::signal_safe_terminal_restore();
    unsafe {
        let _ = libc::write(libc::STDERR_FILENO, MESSAGE.as_ptr().cast(), MESSAGE.len());
        libc::_exit(128 + signal);
    }
}

#[cfg(all(unix, not(feature = "tiny")))]
extern "C" fn request_suspend(_signal: libc::c_int) {
    SUSPEND_REQUESTED.store(true, Ordering::SeqCst);
}

pub(crate) fn process_pending_signal_requests() {
    #[cfg(unix)]
    {
        let signal = TERMINATION_SIGNAL.swap(0, Ordering::SeqCst);
        if signal != 0 {
            die(crate::tr!("Received SIGHUP or SIGTERM\n"));
        }
    }

    #[cfg(all(unix, not(feature = "tiny")))]
    if SUSPEND_REQUESTED.swap(false, Ordering::SeqCst) {
        suspend_nano(0);
    }
}

/* C: void suspend_nano(int signal) */
#[cfg(not(feature = "tiny"))]
pub fn suspend_nano(_signal: i32) {
    #[cfg(feature = "mouse")]
    disable_mouse_support();
    restore_terminal();
    println!("\n");
    println!("Use \"fg\" to return to nano.");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    state_mut().lastmessage = MessageType::Hush;
    #[cfg(unix)]
    unsafe {
        libc::kill(0, libc::SIGSTOP);
    }
}

/* C: void do_suspend(void) */
#[cfg(not(feature = "tiny"))]
pub fn do_suspend() {
    if in_restricted_mode() {
        return;
    }
    suspend_nano(0);
    state_mut().ran_a_tool = true;
}

/* C: void continue_nano(int signal) */
#[cfg(unix)]
extern "C" fn continue_nano(_signal: libc::c_int) {
    #[cfg(feature = "mouse")]
    {
        if ISSET!(USE_MOUSE) {
            // Can't safely call enable_mouse_support from signal handler.
        }
    }
    #[cfg(not(feature = "tiny"))]
    {
        THE_WINDOW_RESIZED.store(true, Ordering::SeqCst);
    }
    // On tiny: put_back(KEY_FRESH) — not safe from signal handler; skip.
}

/* C: void block_sigwinch(bool blockit) */
#[cfg(any(not(feature = "tiny"), feature = "speller", feature = "color"))]
pub fn block_sigwinch(blockit: bool) {
    #[cfg(unix)]
    unsafe {
        let mut winch: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut winch);
        libc::sigaddset(&mut winch, libc::SIGWINCH);
        libc::sigprocmask(
            if blockit {
                libc::SIG_BLOCK
            } else {
                libc::SIG_UNBLOCK
            },
            &winch,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(all(feature = "tiny", not(any(feature = "speller", feature = "color"))))]
pub fn block_sigwinch(_blockit: bool) {}

/* C: void handle_sigwinch(int signal) */
#[cfg(all(unix, not(feature = "tiny")))]
extern "C" fn handle_sigwinch(_signal: libc::c_int) {
    THE_WINDOW_RESIZED.store(true, Ordering::SeqCst);
}

/* C: void set_up_sigwinch_handler(void) */
#[cfg(not(feature = "tiny"))]
pub fn set_up_sigwinch_handler() {
    #[cfg(all(unix, target_os = "linux"))]
    unsafe {
        let mut deed: libc::sigaction = std::mem::zeroed();
        deed.sa_sigaction = handle_sigwinch as *const () as libc::sighandler_t;
        libc::sigaction(libc::SIGWINCH, &deed, std::ptr::null_mut());
    }
    // crossterm handles SIGWINCH automatically via Event::Resize.
}

/* C: void set_up_signal_handlers(void) */
pub fn set_up_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        // Trap SIGINT and SIGQUIT (ignore them).
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        // Don't die when a pipe we write to (e.g. the spell checker) closes.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        // SIGHUP and SIGTERM.
        libc::signal(
            libc::SIGHUP,
            handle_hupterm as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_hupterm as *const () as libc::sighandler_t,
        );
        // SIGTSTP / suspend.
        #[cfg(not(feature = "tiny"))]
        {
            libc::signal(
                libc::SIGTSTP,
                request_suspend as *const () as libc::sighandler_t,
            );
        }
        // SIGCONT.
        libc::signal(
            libc::SIGCONT,
            continue_nano as *const () as libc::sighandler_t,
        );
        // SIGSEGV / SIGABRT crash handler (not debug, not tiny).
        #[cfg(all(not(feature = "tiny"), not(debug_assertions)))]
        {
            if std::env::var("NANO_NOCATCH").is_err() {
                libc::signal(
                    libc::SIGSEGV,
                    handle_crash as *const () as libc::sighandler_t,
                );
                libc::signal(
                    libc::SIGABRT,
                    handle_crash as *const () as libc::sighandler_t,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// terminal_init helpers
// ---------------------------------------------------------------------------

/* C: void disable_extended_io(void) */
pub fn disable_extended_io() {
    #[cfg(unix)]
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut settings);
        settings.c_lflag &= !libc::IEXTEN;
        settings.c_oflag &= !libc::OPOST;
        libc::tcsetattr(0, libc::TCSANOW, &settings);
    }
}

/* C: void disable_kb_interrupt(void) */
pub fn disable_kb_interrupt() {
    #[cfg(unix)]
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut settings);
        settings.c_lflag &= !libc::ISIG;
        libc::tcsetattr(0, libc::TCSANOW, &settings);
    }
}

/* C: void enable_kb_interrupt(void) */
pub fn enable_kb_interrupt() {
    #[cfg(unix)]
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut settings);
        settings.c_lflag |= libc::ISIG;
        libc::tcsetattr(0, libc::TCSANOW, &settings);
    }
}

/* C: void disable_flow_control(void) */
pub fn disable_flow_control() {
    #[cfg(unix)]
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut settings);
        settings.c_iflag &= !libc::IXON;
        libc::tcsetattr(0, libc::TCSANOW, &settings);
    }
}

/* C: void enable_flow_control(void) */
pub fn enable_flow_control() {
    #[cfg(unix)]
    unsafe {
        let mut settings: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut settings);
        settings.c_iflag |= libc::IXON;
        libc::tcsetattr(0, libc::TCSANOW, &settings);
    }
}

/* C: void terminal_init(void) */
pub fn terminal_init() {
    if let Err(error) = winio::terminal_init() {
        // `winio::terminal_init` has already unwound any partial transition.
        // Treat the missing interactive terminal as fatal instead of running
        // the editor with an unknown mix of cooked/raw and main/alternate
        // screen state.
        die(&format!("Could not initialize terminal: {}\n", error));
    }
    if ISSET!(PRESERVE) {
        enable_flow_control();
    } else {
        disable_flow_control();
    }
    disable_kb_interrupt();
    #[cfg(not(feature = "tiny"))]
    {
        // Enable bracketed-paste mode (through the shared buffer).
        use std::io::Write;
        let _ = write!(crate::winio::out(), "\x1B[?2004h");
        crate::winio::flush_out();
    }
}

// ---------------------------------------------------------------------------
// get_keycode — ask for a keycode or return standard fallback
// ---------------------------------------------------------------------------
/* C: int get_keycode(const char *keyname, const int standard) */
pub fn get_keycode(_keyname: &str, standard: i32) -> i32 {
    // crossterm does not expose terminfo; just use the standard code.
    standard
}

// ---------------------------------------------------------------------------
// confirm_margin — ensure margin can accommodate the highest line number
// ---------------------------------------------------------------------------
/* C: void confirm_margin(void) */
#[cfg(feature = "linenumbers")]
pub fn confirm_margin() {
    // Use the real terminal width (COLS), exactly as C does.  The old
    // editwincols+margin approximation omitted the sidebar (so editwincols below
    // subtracted it twice) and, at the first call, read the 12345 margin sentinel.
    let cols = winio::terminal_size().0 as i32;
    let needed_margin = with_state(|s| {
        let last_lineno = s
            .openfile
            .as_ref()
            .and_then(|of| of.filebot.as_ref())
            .map(|bot| bot.borrow().lineno)
            .unwrap_or(1);
        crate::utils::digits(last_lineno) + 1
    });

    let line_numbers_set = ISSET!(LINE_NUMBERS);
    let final_margin = if !line_numbers_set || needed_margin > cols - 4 {
        0
    } else {
        needed_margin
    };

    let current_margin = state().margin;
    if final_margin != current_margin {
        let keep_focus = with_state_mut(|s| {
            let keep_focus = (s.margin > 0) && s.focusing;
            s.margin = final_margin;
            s.editwincols = cols - s.margin - s.sidebar;
            keep_focus
        });
        // Uses its own state borrows, so it must run outside the closure.
        #[cfg(not(feature = "tiny"))]
        winio::ensure_firstcolumn_is_aligned();
        with_state_mut(|s| {
            s.focusing = keep_focus;
            s.refresh_needed = true;
        });
    }
}

// ---------------------------------------------------------------------------
// unbound_key — say that an unbound key was struck
// ---------------------------------------------------------------------------
/* C: void unbound_key(int code) */
pub fn unbound_key(code: i32) {
    if code == FOREIGN_SEQUENCE as i32 {
        winio::statusline(MessageType::Ahem, "Unknown sequence");
        winio::set_blankdelay_to_one();
        return;
    }
    #[cfg(feature = "nanorc")]
    if code == NO_SUCH_FUNCTION as i32 {
        let name = state().commandname.clone().unwrap_or_default();
        winio::statusline(MessageType::Ahem, &format!("Unknown function: {}", name));
        winio::set_blankdelay_to_one();
        return;
    }
    #[cfg(feature = "nanorc")]
    if code == MISSING_BRACE as i32 {
        winio::statusline(MessageType::Ahem, "Missing }");
        winio::set_blankdelay_to_one();
        return;
    }
    #[cfg(not(feature = "tiny"))]
    if code > crate::global::KEY_F0 && code < crate::global::KEY_F0 + 25 {
        let fn_num = code - crate::global::KEY_F0;
        winio::statusline(MessageType::Ahem, &format!("Unbound key: F{}", fn_num));
        winio::set_blankdelay_to_one();
        return;
    }
    if code > 0x7F {
        winio::statusline(MessageType::Ahem, "Unbound key");
    } else {
        let meta = state().meta_key;
        if meta {
            #[cfg(not(feature = "tiny"))]
            {
                if code < 0x20 {
                    winio::statusline(
                        MessageType::Ahem,
                        &format!("Unbindable key: M-^{}", (code + 0x40) as u8 as char),
                    );
                    winio::set_blankdelay_to_one();
                    return;
                }
            }
            #[cfg(feature = "nanorc")]
            {
                let shifted = state().shifted_metas;
                if shifted && code >= b'A' as i32 && code <= b'Z' as i32 {
                    winio::statusline(
                        MessageType::Ahem,
                        &format!("Unbound key: Sh-M-{}", code as u8 as char),
                    );
                    winio::set_blankdelay_to_one();
                    return;
                }
            }
            let uc = (code as u8 as char).to_ascii_uppercase();
            winio::statusline(MessageType::Ahem, &format!("Unbound key: M-{}", uc));
        } else if code == ESC_CODE as i32 {
            winio::statusline(MessageType::Ahem, "Unbindable key: ^[");
        } else if code < 0x20 {
            winio::statusline(
                MessageType::Ahem,
                &format!("Unbound key: ^{}", (code + 0x40) as u8 as char),
            );
        } else {
            #[cfg(any(feature = "browser", feature = "help"))]
            {
                winio::statusline(
                    MessageType::Ahem,
                    &format!("Unbound key: {}", code as u8 as char),
                );
            }
        }
    }
    winio::set_blankdelay_to_one();
}

// ---------------------------------------------------------------------------
// process_click — handle a mouse click in the edit window
// ---------------------------------------------------------------------------
/* C: int process_click(void) — upstream nano.c do_mouse() */
#[cfg(feature = "mouse")]
pub fn process_click() -> i32 {
    let mut click_row = 0i32;
    let mut click_col = 0i32;
    let retval = winio::get_mouseinput(&mut click_row, &mut click_col);

    // If the click is wrong or already handled, we're done.
    if retval != 0 {
        return retval;
    }

    let (editwin_rows, editwin_y) = with_state(|s| (s.editwinrows as i32, s.midwin.y as i32));

    // If the click was in the edit window, put the cursor in that spot.
    if click_row >= editwin_y && click_row < editwin_y + editwin_rows {
        let adj_row = click_row - editwin_y;

        let (current, cursor_row, was_x) = with_state(|s| {
            let f = s.openfile.as_ref();
            (
                f.and_then(|f| f.current.clone()),
                f.map(|f| f.cursor_row).unwrap_or(0),
                f.map(|f| f.current_x).unwrap_or(0),
            )
        });
        let Some(mut line) = current else { return 2 };
        let was_current = line.clone();

        let row_count = adj_row - cursor_row as i32;

        let col = crate::utils::xplustabs();
        #[cfg(not(feature = "tiny"))]
        let mut leftedge = if state().flag_isset(SOFTWRAP) {
            let b = line.borrow();
            winio::leftedge_for(col, &b.data)
        } else {
            crate::utils::get_page_start(col)
        };
        #[cfg(feature = "tiny")]
        let mut leftedge = crate::utils::get_page_start(col);

        // Move current up or down to the row that was clicked on.
        if row_count < 0 {
            winio::go_back_chunks(-row_count, &mut line, &mut leftedge);
        } else {
            winio::go_forward_chunks(row_count, &mut line, &mut leftedge);
        }

        // Make the clicked line current first: actual_last_column() reads
        // openfile->current internally (as in C, where current has already
        // been advanced by the chunk walk at this point).
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current = Some(line.clone());
            }
        });
        let new_x = {
            let b = line.borrow();
            // Clamp the click column to >= 0 before the usize cast: clicking in
            // the line-number margin yields a negative column, which would wrap to
            // a huge usize and panic in actual_last_column.
            crate::utils::actual_x(
                &b.data,
                winio::actual_last_column(leftedge, click_col.max(0) as usize),
            )
        };
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.current_x = new_x;
            }
        });

        // Clicking there where the cursor already is toggles the mark.
        #[cfg(not(feature = "tiny"))]
        if row_count == 0 && new_x == was_x {
            crate::text::do_mark();
            if ISSET!(STATEFLAGS) {
                winio::titlebar(None);
            }
        } else {
            // The cursor moved; clean the cutbuffer on the next cut.
            state_mut().keep_cutbuffer = false;
        }
        #[cfg(feature = "tiny")]
        with_state_mut(|s| s.keep_cutbuffer = false);

        winio::edit_redraw(&was_current, UpdateType::Centering);
    }

    // No more handling is needed.
    2
}

// ---------------------------------------------------------------------------
// wanted_to_move — return true if the function is a cursor-moving command
// ---------------------------------------------------------------------------
/* C: bool wanted_to_move(void (*func)(void)) */
pub fn wanted_to_move(func: FuncPtr) -> bool {
    if func == crate::global::do_left as FuncPtr
        || func == crate::global::do_right as FuncPtr
        || func == crate::global::do_up as FuncPtr
        || func == crate::global::do_down as FuncPtr
        || func == crate::global::do_home as FuncPtr
        || func == crate::global::do_end as FuncPtr
        || func == crate::global::to_prev_word as FuncPtr
        || func == crate::global::to_next_word as FuncPtr
        || func == crate::global::to_prev_block as FuncPtr
        || func == crate::global::to_next_block as FuncPtr
        || func == crate::global::do_page_up as FuncPtr
        || func == crate::global::do_page_down as FuncPtr
        || func == crate::global::to_first_line as FuncPtr
        || func == crate::global::to_last_line as FuncPtr
    {
        return true;
    }
    #[cfg(feature = "justify")]
    if func == crate::global::to_para_begin as FuncPtr
        || func == crate::global::to_para_end as FuncPtr
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// changes_something — return true if the function modifies the buffer
// ---------------------------------------------------------------------------
/* C: bool changes_something(functionptrtype f) */
pub fn changes_something(f: FuncPtr) -> bool {
    if f == crate::global::do_savefile as FuncPtr
        || f == crate::global::do_writeout as FuncPtr
        || f == crate::global::do_enter as FuncPtr
        || f == crate::global::do_tab as FuncPtr
        || f == crate::global::do_delete as FuncPtr
        || f == crate::global::do_backspace as FuncPtr
        || f == crate::global::cut_text as FuncPtr
        || f == crate::global::paste_text as FuncPtr
        || f == crate::global::do_replace as FuncPtr
        || f == crate::global::do_verbatim_input as FuncPtr
    {
        return true;
    }
    #[cfg(not(feature = "tiny"))]
    if f == crate::global::chop_previous_word as FuncPtr
        || f == crate::global::chop_next_word as FuncPtr
        || f == crate::global::zap_text as FuncPtr
        || f == crate::global::cut_till_eof as FuncPtr
        || f == crate::global::do_execute as FuncPtr
        || f == crate::global::do_indent as FuncPtr
        || f == crate::global::do_unindent as FuncPtr
    {
        return true;
    }
    #[cfg(feature = "justify")]
    if f == crate::global::do_justify as FuncPtr || f == crate::global::do_full_justify as FuncPtr {
        return true;
    }
    #[cfg(feature = "comment")]
    if f == crate::global::do_comment as FuncPtr {
        return true;
    }
    #[cfg(feature = "speller")]
    if f == crate::global::do_spell as FuncPtr {
        return true;
    }
    #[cfg(feature = "formatter")]
    if f == crate::global::do_formatter as FuncPtr {
        return true;
    }
    #[cfg(feature = "wordcomp")]
    if f == crate::global::complete_a_word as FuncPtr {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// suck_up_input_and_paste_it — read waiting bytes and paste in one go
// ---------------------------------------------------------------------------
/* C: void suck_up_input_and_paste_it(void) */
#[cfg(not(feature = "tiny"))]
pub fn suck_up_input_and_paste_it() {
    let was_cutbuffer = state().cutbuffer.clone();
    let was_cutbottom = state().cutbottom.clone();

    // Create a new line node as start of paste buffer.
    let head = state_mut().lines.alloc(LineNode {
        data: LineData::empty(),
        lineno: 1,
        next: None,
        prev: None,
        #[cfg(feature = "color")]
        multidata: Vec::new(),
        has_anchor: false,
    });

    with_state_mut(|s| {
        s.cutbuffer = Some(head.clone());
        s.cutbottom = Some(head.clone());
    });

    let mut line = head.clone();
    let mut pending = Vec::new();

    fn flush_paste_bytes(line: &LinePtr, pending: &mut Vec<u8>) {
        if pending.is_empty() {
            return;
        }
        line.borrow_mut().data.extend_external(pending);
        pending.clear();
    }

    let input = loop {
        let input = winio::get_kbinput(BLIND);

        let ch = input as u32;
        if (0x20 <= ch && ch <= 0xFF && ch != DEL_CODE) || ch == b'\t' as u32 {
            pending.push(input as u8);
        } else if input == b'\r' as i32 || input == b'\n' as i32 {
            flush_paste_bytes(&line, &mut pending);
            let lineno = line.borrow().lineno + 1;
            let prev_link = Some(LinePtr::downgrade(&line));
            let new_line = state_mut().lines.alloc(LineNode {
                data: LineData::empty(),
                lineno,
                next: None,
                prev: prev_link,
                #[cfg(feature = "color")]
                multidata: Vec::new(),
                has_anchor: false,
            });
            line.borrow_mut().next = Some(new_line.clone());
            line = new_line;
            state_mut().cutbottom = Some(line.clone());
        } else {
            break input;
        }
    };
    flush_paste_bytes(&line, &mut pending);

    if ISSET!(VIEW_MODE) {
        print_view_warning();
    } else {
        crate::global::paste_text();
    }

    if input != END_OF_PASTE as i32 {
        winio::statusline(
            MessageType::Alert,
            "Paste interrupted; received data may be incomplete",
        );
    }

    // Free the temporary cutbuffer and restore the original.
    let tmp = state().cutbuffer.clone();
    free_lines(tmp);
    with_state_mut(|s| {
        s.cutbuffer = was_cutbuffer;
        s.cutbottom = was_cutbottom;
    });
}

// ---------------------------------------------------------------------------
// inject — insert a short burst of bytes into the edit buffer
// ---------------------------------------------------------------------------
/* C: void inject(char *burst, size_t count) */
pub fn inject(burst: &[u8]) {
    // Encode embedded NUL bytes as 0x0A.
    let data: Vec<u8> = burst
        .iter()
        .map(|&b| if b == 0 { b'\n' } else { b })
        .collect();
    let count = data.len();

    let (_datalen, lineno, current_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        let cur = of.current.as_ref().expect("a current line");
        let b = cur.borrow();
        (b.data.len(), b.lineno, of.current_x)
    });

    // Capture, before the insertion, the softwrap geometry that determines
    // whether a full refresh is needed afterwards (C inject: original_row / old_amount).
    #[cfg(not(feature = "tiny"))]
    let pre_col = crate::utils::xplustabs();
    #[cfg(not(feature = "tiny"))]
    let (original_row, old_amount) = with_state(|s| {
        if s.flag_isset(SOFTWRAP) {
            if let Some(ref of) = s.openfile {
                if let Some(ref cur) = of.current {
                    let data = cur.borrow().data.clone();
                    let orig = if of.cursor_row == (s.editwinrows - 1) as isize {
                        winio::chunk_for(pre_col, &data)
                    } else {
                        0
                    };
                    return (orig, winio::extra_chunks_in(&data));
                }
            }
        }
        (0usize, 0usize)
    });

    // Add undo record if needed.
    #[cfg(not(feature = "tiny"))]
    {
        let need_new_undo = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            of.last_action != UndoType::Add
                || of.current_undo.is_null()
                || unsafe { (*of.current_undo).tail_lineno != lineno as isize }
                || unsafe { (*of.current_undo).tail_x != current_x }
        });
        if need_new_undo {
            crate::text::add_undo(UndoType::Add, None);
        }
    }

    // Insert the bytes into the current line.
    with_state_mut(|s| {
        let of = s.openfile.as_mut().expect("an open buffer");
        let cur = of.current.as_ref().expect("a current line").clone();
        let mut b = cur.borrow_mut();
        let safe_x = of.current_x.min(b.data.len());
        of.current_x = safe_x;
        b.data.insert_bytes(safe_x, &data);
        // Update mark position if needed.
        #[cfg(not(feature = "tiny"))]
        {
            if let Some(ref mark) = of.mark.clone() {
                let mark_lineno = mark.borrow().lineno;
                if mark_lineno == lineno as isize && of.current_x < of.mark_x {
                    of.mark_x += count;
                }
            }
        }
    });

    // When the current line is edittop and shown from a non-first chunk, the
    // inserted text may shift the preceding chunk; realign firstcolumn (C 1505-1508).
    #[cfg(not(feature = "tiny"))]
    {
        let needs_align = with_state(|s| {
            if let Some(ref of) = s.openfile {
                if let (Some(cur), Some(top)) = (of.current.as_ref(), of.edittop.as_ref()) {
                    return LinePtr::ptr_eq(cur, top) && of.firstcolumn > 0;
                }
            }
            false
        });
        if needs_align {
            winio::ensure_firstcolumn_is_aligned();
            state_mut().refresh_needed = true;
        }
    }

    // totsize counts CHARACTERS, not bytes (C: openfile->totsize += mbstrlen(burst)).
    let char_count = crate::chars::mbstrlen(&data);
    with_state_mut(|s| {
        let of = s.openfile.as_mut().expect("an open buffer");
        of.current_x += count;
        of.totsize += char_count;
    });

    files::set_modified();

    // If text was added to the magic line, create a new magic line.
    let at_filebot = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        let cur_lineno = of.current.as_ref().expect("a current line").borrow().lineno;
        let bot_lineno = of.filebot.as_ref().unwrap().borrow().lineno;
        cur_lineno == bot_lineno && !ISSET!(NO_NEWLINES)
    });
    if at_filebot {
        crate::utils::new_magicline();
    }

    #[cfg(not(feature = "tiny"))]
    crate::text::update_undo(UndoType::Add);

    #[cfg(feature = "wrapping")]
    if ISSET!(BREAK_LONG_LINES) {
        crate::text::do_wrap();
    }

    let placewewant = crate::utils::xplustabs();
    with_state_mut(|s| {
        s.openfile.as_mut().expect("an open buffer").placewewant = placewewant;
    });

    // When panning near the viewport edge, or when softwrapping and the line's
    // chunk count changed (or the cursor moved to a new chunk on the last edit
    // row), schedule a full refresh (C inject 1543-1556).
    #[cfg(not(feature = "tiny"))]
    {
        let need_refresh = with_state(|s| {
            if let Some(ref of) = s.openfile {
                // panning: placewewant > brink + editwincols - CUSHION - 1
                if s.united_sidescroll
                    && of.placewewant + CUSHION + 1 > of.brink + s.editwincols as usize
                {
                    return true;
                }
                if s.flag_isset(SOFTWRAP) {
                    if let Some(ref cur) = of.current {
                        let data = cur.borrow().data.clone();
                        if winio::extra_chunks_in(&data) != old_amount {
                            return true;
                        }
                        if of.cursor_row == (s.editwinrows - 1) as isize
                            && winio::chunk_for(of.placewewant, &data) > original_row
                        {
                            return true;
                        }
                    }
                }
            }
            false
        });
        if need_refresh {
            with_state_mut(|s| {
                s.refresh_needed = true;
                s.focusing = false;
            });
        }
    }

    let refresh = state().refresh_needed;
    if !refresh {
        let (current, current_x) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (of.current.clone(), of.current_x)
        });
        if let Some(cur) = current {
            #[cfg(feature = "color")]
            color::check_the_multis(&cur);
            winio::update_line(&cur, current_x);
        }
    }
}

// ---------------------------------------------------------------------------
// regenerate_screen — reinitialize and redraw the screen completely
// ---------------------------------------------------------------------------
/* C: void regenerate_screen(void) */
#[cfg(not(feature = "tiny"))]
pub fn regenerate_screen() {
    THE_WINDOW_RESIZED.store(false, Ordering::SeqCst);

    winio::recalculate_screensize();

    let (cols, lines) = winio::terminal_size();
    with_state_mut(|s| {
        s.sidebar = if s.flag_isset(INDICATOR) && lines > 5 && cols > 9 {
            1
        } else {
            0
        };
        let needed = lines as usize;
        s.bardata.resize(needed, 0);
        s.editwincols = cols as i32 - s.margin - s.sidebar;
    });

    terminal_init();
    window_init();

    let running = state().we_are_running;
    if running {
        #[cfg(not(feature = "tiny"))]
        winio::ensure_firstcolumn_is_aligned();
        winio::draw_all_subwindows();
    }
}

#[cfg(feature = "tiny")]
pub fn regenerate_screen() {
    THE_WINDOW_RESIZED.store(false, Ordering::SeqCst);
    winio::recalculate_screensize();
    terminal_init();
    window_init();
    if state().we_are_running {
        winio::draw_all_subwindows();
    }
}

// ---------------------------------------------------------------------------
// toggle_this — invert the given global flag and adjust things
// ---------------------------------------------------------------------------
/* C: void toggle_this(int flag) */
#[cfg(not(feature = "tiny"))]
pub fn toggle_this(flag: u32) {
    let enabled = !ISSET!(flag);
    TOGGLE!(flag);
    state_mut().focusing = false;

    match flag {
        f if f == ZERO => {
            window_init();
            winio::draw_all_subwindows();
            return;
        }
        f if f == NO_HELP => {
            // C compares against LINES (the full terminal height), not
            // editwinrows + midwin.y, which omits the footer rows.
            let lines = winio::terminal_size().1 as i32;
            let (zero, minibar) = with_state(|s| (s.flag_isset(ZERO), s.flag_isset(MINIBAR)));
            let minimum = if zero {
                3
            } else if minibar {
                4
            } else {
                5
            };
            if lines < minimum {
                winio::statusline(MessageType::Ahem, "Too tiny");
                TOGGLE!(flag);
                return;
            }
            window_init();
            winio::draw_all_subwindows();
        }
        f if f == CONSTANT_SHOW => {
            let lines = winio::terminal_size().1 as i32;
            let (zero, minibar) = with_state(|s| (s.flag_isset(ZERO), s.flag_isset(MINIBAR)));
            if lines == 1 {
                winio::statusline(MessageType::Ahem, "Too tiny");
                TOGGLE!(flag);
            } else if zero {
                SET!(CONSTANT_SHOW);
                toggle_this(ZERO);
            } else if !minibar {
                winio::wipe_statusbar();
            }
            return;
        }
        f if f == SOFTWRAP => {
            if !ISSET!(SOFTWRAP) {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.firstcolumn = 0;
                    }
                });
            }
            state_mut().refresh_needed = true;
        }
        f if f == WHITESPACE_DISPLAY => {
            winio::titlebar(None);
            state_mut().refresh_needed = true;
        }
        #[cfg(feature = "color")]
        f if f == NO_SYNTAX => {
            color::precalc_multicolorinfo();
            state_mut().refresh_needed = true;
        }
        #[cfg(feature = "color")]
        f if f == TABS_TO_SPACES => {
            let has_tabstring = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|of| of.syntax)
                    .map(|sx| unsafe { (*sx).tabstring.is_some() })
                    .unwrap_or(false)
            });
            if has_tabstring {
                winio::statusline(MessageType::Ahem, "Current syntax determines Tab");
                TOGGLE!(flag);
                return;
            }
        }
        #[cfg(feature = "mouse")]
        f if f == USE_MOUSE => {
            mouse_init();
        }
        _ => {}
    }

    if flag == AUTOINDENT || flag == BREAK_LONG_LINES || flag == SOFTWRAP {
        let (minibar, zero, stateflags) = with_state(|s| {
            (
                s.flag_isset(MINIBAR),
                s.flag_isset(ZERO),
                s.flag_isset(STATEFLAGS),
            )
        });
        if minibar && !zero && stateflags {
            return;
        }
        if stateflags {
            winio::titlebar(None);
        }
    }

    if flag == NO_HELP || flag == LINE_NUMBERS || flag == WHITESPACE_DISPLAY {
        let (minibar, zero, lines) = with_state(|s| {
            let total = s.editwinrows + s.midwin.y as i32;
            (s.flag_isset(MINIBAR), s.flag_isset(ZERO), total)
        });
        if minibar || zero || lines == 1 {
            return;
        }
    }

    let final_enabled = if flag == NO_HELP || flag == NO_SYNTAX {
        !enabled
    } else {
        enabled
    };

    let epithet = crate::global::epithet_of_flag(flag);
    let state_str = if final_enabled { "enabled" } else { "disabled" };
    winio::statusline(MessageType::Remark, &format!("{} {}", epithet, state_str));
}

// ---------------------------------------------------------------------------
// process_a_keystroke — read in a keystroke and execute its command or insert
// ---------------------------------------------------------------------------
/* C: void process_a_keystroke(void) */
pub fn process_a_keystroke() {
    // Read a keystroke.
    let input = winio::get_kbinput(VISIBLE);

    state_mut().lastmessage = MessageType::Vacuum;

    if winio::consume_resize_request(Some(input)) {
        return;
    }

    // Static input buffer.
    use std::cell::RefCell;
    thread_local! {
        static PUDDLE: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(12));
        static GIVE_A_HINT: RefCell<bool> = RefCell::new(true);
    }

    // Handle mouse click.
    #[cfg(feature = "mouse")]
    let input = if input == winio::KEY_MOUSE_CODE {
        match process_click() {
            1 => winio::get_kbinput(BLIND),
            _ => return,
        }
    } else {
        input
    };

    // A shortcut click may require reading one more event.  Treat a resize in
    // that slot exactly like the primary input instead of interpreting the
    // synthetic keycode as an editor command.
    if winio::consume_resize_request(Some(input)) {
        return;
    }

    #[cfg(not(feature = "tiny"))]
    let was_mark = with_state(|s| s.openfile.as_ref().and_then(|of| of.mark.clone()));

    // Look up shortcut.
    let function: Option<FuncPtr> = crate::global::get_shortcut(input);

    // Look up toggle value for this shortcut (not-tiny only).
    #[cfg(not(feature = "tiny"))]
    let shortcut_toggle: i32 = if let Some(f) = function {
        with_state(|s| {
            s.sclist
                .iter()
                .find(|sc| {
                    sc.func == Some(f) && (sc.menus as u32 & s.currmenu) != 0 && sc.keycode == input
                })
                .map(|sc| sc.toggle)
                .unwrap_or(0)
        })
    } else {
        0
    };

    // Look up expansion for string-bind shortcuts (nanorc only).
    #[cfg(feature = "nanorc")]
    let shortcut_expansion: Option<String> = if let Some(f) = function {
        with_state(|s| {
            s.sclist
                .iter()
                .find(|sc| {
                    sc.func == Some(f) && (sc.menus as u32 & s.currmenu) != 0 && sc.keycode == input
                })
                .and_then(|sc| sc.expansion.clone())
        })
    } else {
        None
    };

    // If not a command, handle as character or unknown.
    if function.is_none() {
        let meta = state().meta_key;
        if input < 0x20 || input > 0xFF || meta {
            unbound_key(input);
        } else if ISSET!(VIEW_MODE) {
            print_view_warning();
        } else {
            #[cfg(not(feature = "tiny"))]
            {
                let softmark = with_state(|s| {
                    s.openfile
                        .as_ref()
                        .map(|of| of.mark.is_some() && of.softmark)
                        .unwrap_or(false)
                });
                if softmark {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.mark = None;
                        }
                        s.refresh_needed = true;
                    });
                }
            }
            PUDDLE.with(|p| {
                p.borrow_mut().push(input as u8);
            });
        }
    }

    // If there are gathered bytes and we have a command or no waiting keys, inject.
    let depth = PUDDLE.with(|p| p.borrow().len());
    let waiting = winio::waiting_keycodes();
    if depth > 0 && (function.is_some() || waiting == 0) {
        PUDDLE.with(|p| {
            let bytes = p.borrow().clone();
            inject(&bytes);
            p.borrow_mut().clear();
        });
    }

    #[cfg(not(feature = "tiny"))]
    if function != Some(crate::global::do_cycle as FuncPtr) {
        state_mut().cycling_aim = 0;
    }

    if function.is_none() {
        with_state_mut(|s| {
            s.pletion_line = None;
            s.keep_cutbuffer = false;
        });
        return;
    }

    let func = function.unwrap();

    if ISSET!(VIEW_MODE) && changes_something(func) {
        print_view_warning();
        return;
    }

    // Give hint about help if at top of file.
    GIVE_A_HINT.with(|hint| {
        let give = *hint.borrow();
        let meta = state().meta_key;
        let at_top_empty = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|of| {
                    of.current_x == 0
                        && of
                            .current
                            .as_ref()
                            .and_then(|c| {
                                of.filetop
                                    .as_ref()
                                    .map(|ft| c.borrow().lineno == ft.borrow().lineno)
                            })
                            .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        let nohelp = ISSET!(NO_HELP);
        if input == b'\x08' as i32 && give && at_top_empty && !nohelp {
            winio::statusbar("^W = Ctrl+W    M-W = Alt+W");
            *hint.borrow_mut() = false;
        } else if meta {
            *hint.borrow_mut() = false;
        }
    });

    // Handle string-bind expansion (implant has a different signature; detect by expansion).
    #[cfg(feature = "nanorc")]
    {
        if let Some(ref expansion) = shortcut_expansion {
            // If there is an expansion string, call implant and return.
            crate::winio::implant(expansion);
            return;
        }
    }

    // Handle toggle commands.
    #[cfg(not(feature = "tiny"))]
    {
        if func == crate::global::do_toggle as FuncPtr {
            toggle_this(shortcut_toggle as u32);
            if shortcut_toggle as u32 == CUT_FROM_CURSOR {
                state_mut().keep_cutbuffer = false;
            }
            return;
        }
    }

    // When not cutting or copying, drop the cutbuffer next time.
    if func != crate::global::cut_text as FuncPtr && func != crate::global::copy_text as FuncPtr {
        #[cfg(not(feature = "tiny"))]
        {
            if func != crate::global::zap_text as FuncPtr
                && func != crate::global::record_macro as FuncPtr
                && func != crate::global::run_macro as FuncPtr
            {
                state_mut().keep_cutbuffer = false;
            }
        }
        #[cfg(feature = "tiny")]
        with_state_mut(|s| s.keep_cutbuffer = false);
    }

    #[cfg(feature = "wordcomp")]
    if func != crate::global::complete_a_word as FuncPtr {
        state_mut().pletion_line = None;
    }

    // Save cursor position before executing.
    #[cfg(not(feature = "tiny"))]
    let (was_current_lineno, was_x) = with_state(|s| {
        let of = s.openfile.as_ref().expect("an open buffer");
        let lineno = of.current.as_ref().map(|c| c.borrow().lineno).unwrap_or(0);
        (lineno, of.current_x)
    });

    // If Shift + movement, set the mark.
    #[cfg(not(feature = "tiny"))]
    {
        let shift_held = state().shift_held;
        let mark_is_none = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|of| of.mark.is_none())
                .unwrap_or(true)
        });
        if shift_held && mark_is_none {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = of.current.clone();
                    of.mark_x = of.current_x;
                    of.softmark = true;
                }
            });
        }
    }

    // Execute the function.
    func();

    // Post-execution cleanup for soft marks.
    #[cfg(not(feature = "tiny"))]
    {
        let (shift_held, softmark, mark_some) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            (s.shift_held, of.softmark, of.mark.is_some())
        });
        let (cur_lineno, cur_x) = with_state(|s| {
            let of = s.openfile.as_ref().expect("an open buffer");
            let ln = of.current.as_ref().map(|c| c.borrow().lineno).unwrap_or(0);
            (ln, of.current_x)
        });

        if mark_some
            && softmark
            && !shift_held
            && (cur_lineno != was_current_lineno || cur_x != was_x || wanted_to_move(func))
        {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.mark = None;
                }
                s.refresh_needed = true;
            });
        } else if cur_lineno != was_current_lineno {
            state_mut().also_the_last = false;
        }

        // Update titlebar if mark state changed.
        let stateflags = ISSET!(STATEFLAGS);
        let mark_changed = with_state(|s| {
            let cur_mark = s.openfile.as_ref().and_then(|of| of.mark.clone());
            // Compare by lineno as a proxy for identity.
            let old_lineno = was_mark.as_ref().map(|m| m.borrow().lineno).unwrap_or(-1);
            let new_lineno = cur_mark.as_ref().map(|m| m.borrow().lineno).unwrap_or(-1);
            old_lineno != new_lineno
        });
        if stateflags && mark_changed {
            winio::titlebar(None);
        }
    }
}

// ---------------------------------------------------------------------------
// nano_main — main entry point
// ---------------------------------------------------------------------------
/* C: int main(int argc, char **argv) */
fn parse_position_numbers(spec: &str) -> Result<(isize, isize), ()> {
    if spec.is_empty() {
        return Err(());
    }
    let separator = spec.find([',', '.', ':']);
    match separator {
        None => crate::utils::parse_num(spec)
            .map(|line| (line, 0))
            .ok_or(()),
        Some(index) => {
            let line = if index == 0 {
                0
            } else {
                crate::utils::parse_num(&spec[..index]).ok_or(())?
            };
            let column = crate::utils::parse_num(&spec[index + 1..]).ok_or(())?;
            Ok((line, column))
        }
    }
}

fn long_option_is_available(option: &str) -> bool {
    match option {
        "smarthome" | "backup" | "backupdir" | "tabstospaces" | "locking" | "guidestripe"
        | "nonewlines" | "noconvert" | "bookstyle" | "softwrap" | "tabsize" | "wordbounds"
        | "wordchars" | "zap" | "atblanks" | "emptyline" | "autoindent" | "jumpyscrolling"
        | "cutfromcursor" | "noread" | "indicator" | "unix" | "afterends" | "whitespacedisplay"
        | "colonparsing" | "stateflags" | "minibar" | "zero" => !cfg!(feature = "tiny"),
        "multibuffer" => cfg!(feature = "multibuffer"),
        "historylog" | "positionlog" => cfg!(feature = "histories"),
        "ignorercfiles" | "rcfile" => cfg!(feature = "nanorc"),
        "trimblanks" | "fill" => cfg!(any(feature = "wrapping", feature = "justify")),
        "quotestr" => cfg!(feature = "justify"),
        "breaklonglines" | "nowrap" => cfg!(feature = "wrapping"),
        "syntax" | "listsyntaxes" => cfg!(feature = "color"),
        "showcursor" => cfg!(any(feature = "browser", feature = "help")),
        "linenumbers" => cfg!(feature = "linenumbers"),
        "mouse" => cfg!(feature = "mouse"),
        "operatingdir" => cfg!(feature = "operatingdir"),
        "speller" => cfg!(feature = "speller"),
        "magic" => cfg!(feature = "libmagic"),
        _ => true,
    }
}

fn short_option_is_available(option: char) -> bool {
    match option {
        'A' | 'B' | 'C' | 'E' | 'G' | 'J' | 'L' | 'N' | 'O' | 'S' | 'T' | 'W' | 'X' | 'Z' | 'a'
        | 'e' | 'i' | 'j' | 'k' | 'n' | 'q' | 'u' | 'y' | '@' | '%' | '_' | '0' => {
            !cfg!(feature = "tiny")
        }
        'F' => cfg!(feature = "multibuffer"),
        'H' | 'P' => cfg!(feature = "histories"),
        'I' | 'f' => cfg!(feature = "nanorc"),
        'M' | 'r' => cfg!(any(feature = "wrapping", feature = "justify")),
        'Q' => cfg!(feature = "justify"),
        'b' | 'w' => cfg!(feature = "wrapping"),
        'Y' | 'z' => cfg!(feature = "color"),
        'g' => cfg!(any(feature = "browser", feature = "help")),
        'l' => cfg!(feature = "linenumbers"),
        'm' => cfg!(feature = "mouse"),
        'o' => cfg!(feature = "operatingdir"),
        's' => cfg!(feature = "speller"),
        '!' => cfg!(feature = "libmagic"),
        _ => true,
    }
}

/// Actions that must wait until the whole command line has been validated
/// before they run (`--install`, `--update`, `--check`).
#[derive(Default, PartialEq, Eq, Debug, Clone, Copy)]
enum DeferredAction {
    #[default]
    None,
    Install,
    Update,
    Check,
}

/// Tracks installer-related options while walking argv.  `--force` only
/// counts when it appears before the first FILE argument, so a file literally
/// named "--force" can never trigger a forced reinstall.
#[derive(Default)]
struct ArgScan {
    action: DeferredAction,
    force: bool,
    saw_positional: bool,
}

impl ArgScan {
    fn note_positional(&mut self) {
        self.saw_positional = true;
    }

    fn note_option(&mut self, opt: &str) {
        match opt {
            "force" if !self.saw_positional => self.force = true,
            "install" => self.action = DeferredAction::Install,
            "update" => self.action = DeferredAction::Update,
            "check" => self.action = DeferredAction::Check,
            _ => {}
        }
    }
}

fn reject_unavailable_option(argv0: &str, option: &str) -> ! {
    eprintln!("Option '{option}' is not available in this build.");
    eprintln!("Type '{argv0} -h' for a list of available options.");
    process::exit(1);
}

#[cfg(not(feature = "tiny"))]
fn parse_colon_notation(filename: &str) -> Option<(String, isize, isize)> {
    if std::fs::metadata(filename).is_ok() {
        return None;
    }

    let last_colon = filename.rfind(':')?;
    let last_number = &filename[last_colon + 1..];
    if last_number.is_empty() || !last_number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let one_number_path = &filename[..last_colon];
    if std::fs::metadata(one_number_path).is_ok() {
        let (line, column) = parse_position_numbers(last_number).ok()?;
        return Some((one_number_path.to_string(), line, column));
    }

    let previous_colon = one_number_path.rfind(':')?;
    let two_number_path = &filename[..previous_colon];
    let numbers = &filename[previous_colon + 1..];
    if std::fs::metadata(two_number_path).is_ok() {
        let (line, column) = parse_position_numbers(numbers).ok()?;
        return Some((two_number_path.to_string(), line, column));
    }
    None
}

pub fn nano_main() {
    // Install this before update processing and argument parsing.  It is a
    // no-op until terminal initialization changes process-visible state, but
    // guarantees that later Rust panics restore raw/alternate-screen modes.
    winio::install_terminal_panic_hook();

    // Apply any previously-downloaded update before doing anything else (this
    // may swap the executable on disk so the new version is used next launch).
    let _ = crate::installer::apply_pending_update();

    // Parse command-line arguments.  args_os() never panics on non-UTF-8
    // arguments; positional FILE arguments keep their exact OS bytes, while
    // option words themselves are required to be valid UTF-8.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let argv0: String = args
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "nano".to_string());

    // If executable starts with 'r', activate restricted mode.
    let tail_char = {
        let t = crate::utils::tail(&argv0);
        t.chars().next().unwrap_or('\0')
    };
    if tail_char == 'r' {
        SET!(RESTRICTED);
    }

    // Set sensible default: NO_WRAP (like nano does before options).
    SET!(NO_WRAP);

    // Back up stdin flags and ensure blocking mode.
    #[cfg(unix)]
    unsafe {
        let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL, 0);
        if flags != -1 {
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
        // Back up original terminal state.
        let _ignore: libc::termios = std::mem::zeroed(); // stored locally; we use winio::terminal_exit for restore.
    }

    // Set up locale / UTF-8 detection.
    #[cfg(feature = "utf8")]
    {
        // Use libc setlocale and check for UTF-8.
        unsafe {
            let locale_str = std::ffi::CString::new("").unwrap();
            let result = libc::setlocale(libc::LC_ALL, locale_str.as_ptr());
            if !result.is_null() {
                // Try nl_langinfo(CODESET).
                // On Linux nl_langinfo is available via libc.
                #[cfg(target_os = "linux")]
                {
                    let codeset = libc::nl_langinfo(libc::CODESET);
                    if !codeset.is_null() {
                        let cs = std::ffi::CStr::from_ptr(codeset).to_string_lossy();
                        if cs == "UTF-8" {
                            state_mut().using_utf8 = true;
                            crate::chars::remember_utf8(true);
                        }
                    }
                }
            }
        }
    }
    #[cfg(all(not(feature = "utf8"), unix))]
    unsafe {
        let locale_str = std::ffi::CString::new("").unwrap();
        libc::setlocale(libc::LC_ALL, locale_str.as_ptr());
    }
    #[cfg(not(unix))]
    {
        state_mut().using_utf8 = true;
        crate::chars::remember_utf8(true);
    }

    // ----------------------------------------------------------------
    // Argument parsing (replaces getopt_long)
    // ----------------------------------------------------------------

    // Variables that correspond to C option state.
    #[cfg(feature = "nanorc")]
    let mut ignore_rcfiles = false;
    #[cfg(any(feature = "wrapping", feature = "justify"))]
    let mut fill_used = false;
    #[cfg(feature = "wrapping")]
    let mut hardwrap: i32 = -2; // -2 = not set, 0 = --nowrap, 1 = --breaklonglines

    let mut idx = 1usize;
    let mut file_args: Vec<std::ffi::OsString> = Vec::new();
    let mut done_with_options = false;
    let mut scan = ArgScan::default();

    while idx < args.len() {
        let arg_os = &args[idx];

        // End of options marker.
        if !done_with_options && arg_os == "--" {
            done_with_options = true;
            scan.note_positional();
            idx += 1;
            continue;
        }

        // Collect file/+LINE arguments once done with options.
        // (A bare "-" means "read from standard input", not an option.)
        // The dash test uses the lossy form; '-' is ASCII, so an invalid
        // leading byte can never masquerade as an option.
        let looks_like_option = arg_os.to_string_lossy().starts_with('-') && arg_os != "-";
        if done_with_options || !looks_like_option {
            // Positional FILE arguments are preserved byte for byte.
            scan.note_positional();
            file_args.push(arg_os.clone());
            idx += 1;
            continue;
        }

        // The option word itself must be valid UTF-8.
        let arg: &str = match arg_os.to_str() {
            Some(arg) => arg,
            None => {
                eprintln!("Option '{}' is not valid UTF-8.", arg_os.to_string_lossy());
                process::exit(1);
            }
        };

        // Long option.
        if arg.starts_with("--") {
            let (opt, val) = if let Some(eq) = arg.find('=') {
                (arg[2..eq].to_string(), Some(arg[eq + 1..].to_string()))
            } else {
                (arg[2..].to_string(), None)
            };

            let next_val = |idx: &mut usize| -> String {
                *idx += 1;
                match args.get(*idx) {
                    None => {
                        // C lets getopt_long reject this; silently continuing
                        // with an empty value used to mask typos (#67).
                        eprintln!("Option '--{opt}' requires an argument.");
                        eprintln!("Type '{argv0} -h' for a list of available options.");
                        process::exit(1);
                    }
                    Some(value) => match value.to_str() {
                        Some(value) => value.to_string(),
                        None => {
                            eprintln!(
                                "Option argument '{}' is not valid UTF-8.",
                                value.to_string_lossy()
                            );
                            process::exit(1);
                        }
                    },
                }
            };

            if !long_option_is_available(&opt) {
                reject_unavailable_option(&argv0, &format!("--{opt}"));
            }

            match opt.as_str() {
                "smarthome" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(SMART_HOME);
                }
                "backup" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MAKE_BACKUP);
                }
                "backupdir" => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().backup_dir = Some(v);
                    }
                }
                "boldtext" => {
                    SET!(BOLD_TEXT);
                }
                "tabstospaces" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(TABS_TO_SPACES);
                }
                "multibuffer" => {
                    #[cfg(feature = "multibuffer")]
                    SET!(MULTIBUFFER);
                }
                "locking" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(LOCKING);
                }
                "historylog" => {
                    #[cfg(feature = "histories")]
                    SET!(HISTORYLOG);
                }
                "ignorercfiles" => {
                    #[cfg(feature = "nanorc")]
                    {
                        ignore_rcfiles = true;
                    }
                }
                "guidestripe" => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        match crate::utils::parse_num(&v) {
                            Some(n) if n > 0 => state_mut().stripe_column = n,
                            _ => {
                                eprintln!("Guide column \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                "rawsequences" => {
                    SET!(RAW_SEQUENCES);
                }
                "nonewlines" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NO_NEWLINES);
                }
                "trimblanks" => {
                    #[cfg(any(feature = "wrapping", feature = "justify"))]
                    SET!(TRIM_BLANKS);
                }
                "noconvert" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NO_CONVERT);
                }
                "bookstyle" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(BOOKSTYLE);
                }
                "positionlog" => {
                    #[cfg(feature = "histories")]
                    SET!(POSITIONLOG);
                }
                "quotestr" => {
                    #[cfg(feature = "justify")]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().quotestr = Some(v);
                    }
                }
                "restricted" => {
                    SET!(RESTRICTED);
                }
                "softwrap" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(SOFTWRAP);
                }
                "tabsize" => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        match crate::utils::parse_num(&v) {
                            Some(n) if n > 0 => state_mut().tabsize = n,
                            _ => {
                                eprintln!("Requested tab size \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                "quickblank" => {
                    SET!(QUICK_BLANK);
                }
                "version" => {
                    version();
                    process::exit(0);
                }
                "wordbounds" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(WORD_BOUNDS);
                }
                "wordchars" => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().word_chars = Some(v);
                    }
                }
                "syntax" => {
                    #[cfg(feature = "color")]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().syntaxstr = Some(v);
                    }
                }
                "zap" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(LET_THEM_ZAP);
                }
                "atblanks" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AT_BLANKS);
                }
                "breaklonglines" => {
                    #[cfg(feature = "wrapping")]
                    {
                        hardwrap = 1;
                    }
                }
                "constantshow" => {
                    SET!(CONSTANT_SHOW);
                }
                "rebinddelete" => {
                    SET!(REBIND_DELETE);
                }
                "emptyline" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(EMPTY_LINE);
                }
                "rcfile" => {
                    #[cfg(feature = "nanorc")]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().custom_nanorc = Some(v);
                    }
                }
                "showcursor" => {
                    #[cfg(any(feature = "browser", feature = "help"))]
                    SET!(SHOW_CURSOR);
                }
                "help" => {
                    usage();
                    process::exit(0);
                }
                "autoindent" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AUTOINDENT);
                }
                "jumpyscrolling" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(JUMPY_SCROLLING);
                }
                "cutfromcursor" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(CUT_FROM_CURSOR);
                }
                "linenumbers" => {
                    #[cfg(feature = "linenumbers")]
                    SET!(LINE_NUMBERS);
                }
                "mouse" => {
                    #[cfg(feature = "mouse")]
                    SET!(USE_MOUSE);
                }
                "noread" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NOREAD_MODE);
                }
                "operatingdir" => {
                    #[cfg(feature = "operatingdir")]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().operating_dir = Some(v);
                    }
                }
                "preserve" => {
                    SET!(PRESERVE);
                }
                "indicator" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(INDICATOR);
                }
                "fill" => {
                    #[cfg(any(feature = "wrapping", feature = "justify"))]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        match crate::utils::parse_num(&v) {
                            Some(n) => {
                                state_mut().fill = n;
                                #[cfg(feature = "nanorc")]
                                {
                                    fill_used = true;
                                }
                            }
                            None => {
                                eprintln!("Requested fill size \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                "speller" => {
                    #[cfg(feature = "speller")]
                    {
                        let v = val.unwrap_or_else(|| next_val(&mut idx));
                        state_mut().alt_speller = Some(v);
                    }
                }
                "saveonexit" => {
                    SET!(SAVE_ON_EXIT);
                }
                "unix" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MAKE_IT_UNIX);
                }
                "view" => {
                    SET!(VIEW_MODE);
                }
                "nowrap" => {
                    #[cfg(feature = "wrapping")]
                    {
                        hardwrap = 0;
                    }
                }
                "nohelp" => {
                    SET!(NO_HELP);
                }
                "afterends" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AFTER_ENDS);
                }
                "listsyntaxes" => {
                    #[cfg(feature = "color")]
                    {
                        #[cfg(feature = "nanorc")]
                        if !ignore_rcfiles {
                            rcfile::do_rcfiles();
                        }
                        #[cfg(not(feature = "nanorc"))]
                        {
                            let _ = ();
                        }
                        list_syntax_names();
                        process::exit(0);
                    }
                }
                "magic" => {
                    #[cfg(feature = "libmagic")]
                    SET!(USE_MAGIC);
                }
                "whitespacedisplay" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(WHITESPACE_DISPLAY);
                }
                "colonparsing" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(COLON_PARSING);
                }
                "stateflags" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(STATEFLAGS);
                }
                "minibar" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MINIBAR);
                }
                "zero" => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(ZERO);
                }
                "solosidescroll" => {
                    with_state_mut(|s| {
                        s.flags[crate::global::flag_index(SOLO_SIDESCROLL)] |=
                            crate::global::flag_mask(SOLO_SIDESCROLL);
                    });
                }
                "install" | "update" | "check" => {
                    // Deferred: run only after the whole command line has
                    // been validated (see the post-loop dispatch below).
                    scan.note_option(arg);
                }
                "force" => {
                    scan.note_option("force");
                }
                "modernbindings" => {
                    SET!(MODERN_BINDINGS);
                }
                _other => {
                    eprintln!("Type '{} -h' for a list of available options.", argv0);
                    process::exit(1);
                }
            }
            idx += 1;
            continue;
        }

        // Short options: iterate the characters in the flag string.
        let chars: Vec<char> = arg[1..].chars().collect();
        let mut ci = 0;
        while ci < chars.len() {
            let c = chars[ci];
            if !short_option_is_available(c) {
                reject_unavailable_option(&argv0, &format!("-{c}"));
            }
            // Helper: get next argument (either rest of this arg or next argv).
            let next_arg = |ci: &mut usize,
                            chars: &Vec<char>,
                            idx: &mut usize,
                            args: &Vec<std::ffi::OsString>|
             -> String {
                *ci += 1;
                if *ci < chars.len() {
                    // Rest of current arg.
                    chars[*ci..]
                        .iter()
                        .collect::<String>()
                        .also(|_| *ci = chars.len())
                } else {
                    *idx += 1;
                    match args.get(*idx) {
                        None => {
                            // C lets getopt reject this; silently continuing
                            // with an empty value used to mask typos (#67).
                            eprintln!("Option '-{c}' requires an argument.");
                            eprintln!("Type '{argv0} -h' for a list of available options.");
                            process::exit(1);
                        }
                        Some(value) => match value.to_str() {
                            Some(value) => value.to_string(),
                            None => {
                                eprintln!(
                                    "Option argument '{}' is not valid UTF-8.",
                                    value.to_string_lossy()
                                );
                                process::exit(1);
                            }
                        },
                    }
                }
            };

            match c {
                'A' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(SMART_HOME);
                }
                'B' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MAKE_BACKUP);
                }
                'C' => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().backup_dir = Some(v);
                    }
                }
                'D' => {
                    SET!(BOLD_TEXT);
                }
                'E' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(TABS_TO_SPACES);
                }
                'F' => {
                    #[cfg(feature = "multibuffer")]
                    SET!(MULTIBUFFER);
                }
                'G' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(LOCKING);
                }
                'H' => {
                    #[cfg(feature = "histories")]
                    SET!(HISTORYLOG);
                }
                'I' => {
                    #[cfg(feature = "nanorc")]
                    {
                        ignore_rcfiles = true;
                    }
                }
                'J' => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        match crate::utils::parse_num(&v) {
                            Some(n) if n > 0 => state_mut().stripe_column = n,
                            _ => {
                                eprintln!("Guide column \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                'K' => {
                    SET!(RAW_SEQUENCES);
                }
                'L' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NO_NEWLINES);
                }
                'M' => {
                    #[cfg(any(feature = "wrapping", feature = "justify"))]
                    SET!(TRIM_BLANKS);
                }
                'N' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NO_CONVERT);
                }
                'O' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(BOOKSTYLE);
                }
                'P' => {
                    #[cfg(feature = "histories")]
                    SET!(POSITIONLOG);
                }
                'Q' => {
                    #[cfg(feature = "justify")]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().quotestr = Some(v);
                    }
                }
                'R' => {
                    SET!(RESTRICTED);
                }
                'S' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(SOFTWRAP);
                }
                'T' => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        match crate::utils::parse_num(&v) {
                            Some(n) if n > 0 => state_mut().tabsize = n,
                            _ => {
                                eprintln!("Requested tab size \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                'U' => {
                    SET!(QUICK_BLANK);
                }
                'V' => {
                    version();
                    process::exit(0);
                }
                'W' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(WORD_BOUNDS);
                }
                'X' => {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().word_chars = Some(v);
                    }
                }
                'Y' => {
                    #[cfg(feature = "color")]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().syntaxstr = Some(v);
                    }
                }
                'Z' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(LET_THEM_ZAP);
                }
                'a' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AT_BLANKS);
                }
                'b' => {
                    #[cfg(feature = "wrapping")]
                    {
                        hardwrap = 1;
                    }
                }
                'c' => {
                    SET!(CONSTANT_SHOW);
                }
                'd' => {
                    SET!(REBIND_DELETE);
                }
                'e' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(EMPTY_LINE);
                }
                'f' => {
                    #[cfg(feature = "nanorc")]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().custom_nanorc = Some(v);
                    }
                }
                'g' => {
                    #[cfg(any(feature = "browser", feature = "help"))]
                    SET!(SHOW_CURSOR);
                }
                'h' => {
                    usage();
                    process::exit(0);
                }
                'i' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AUTOINDENT);
                }
                'j' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(JUMPY_SCROLLING);
                }
                'k' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(CUT_FROM_CURSOR);
                }
                'l' => {
                    #[cfg(feature = "linenumbers")]
                    SET!(LINE_NUMBERS);
                }
                'm' => {
                    #[cfg(feature = "mouse")]
                    SET!(USE_MOUSE);
                }
                'n' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(NOREAD_MODE);
                }
                'o' => {
                    #[cfg(feature = "operatingdir")]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().operating_dir = Some(v);
                    }
                }
                'p' => {
                    SET!(PRESERVE);
                }
                'q' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(INDICATOR);
                }
                'r' => {
                    #[cfg(any(feature = "wrapping", feature = "justify"))]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        match crate::utils::parse_num(&v) {
                            Some(n) => {
                                state_mut().fill = n;
                                #[cfg(feature = "nanorc")]
                                {
                                    fill_used = true;
                                }
                            }
                            None => {
                                eprintln!("Requested fill size \"{}\" is invalid", v);
                                process::exit(1);
                            }
                        }
                    }
                }
                's' => {
                    #[cfg(feature = "speller")]
                    {
                        let v = next_arg(&mut ci, &chars, &mut idx, &args);
                        state_mut().alt_speller = Some(v);
                    }
                }
                't' => {
                    SET!(SAVE_ON_EXIT);
                }
                'u' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MAKE_IT_UNIX);
                }
                'v' => {
                    SET!(VIEW_MODE);
                }
                'w' => {
                    #[cfg(feature = "wrapping")]
                    {
                        hardwrap = 0;
                    }
                }
                'x' => {
                    SET!(NO_HELP);
                }
                'y' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(AFTER_ENDS);
                }
                'z' => {
                    #[cfg(feature = "color")]
                    {
                        #[cfg(feature = "nanorc")]
                        if !ignore_rcfiles {
                            rcfile::do_rcfiles();
                        }
                        list_syntax_names();
                        process::exit(0);
                    }
                }
                '!' => {
                    #[cfg(feature = "libmagic")]
                    SET!(USE_MAGIC);
                }
                '@' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(COLON_PARSING);
                }
                '%' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(STATEFLAGS);
                }
                '_' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(MINIBAR);
                }
                '0' => {
                    #[cfg(not(feature = "tiny"))]
                    SET!(ZERO);
                }
                '1' => {
                    with_state_mut(|s| {
                        s.flags[crate::global::flag_index(SOLO_SIDESCROLL)] |=
                            crate::global::flag_mask(SOLO_SIDESCROLL);
                    });
                }
                '/' => {
                    SET!(MODERN_BINDINGS);
                }
                _ => {
                    eprintln!("Type '{} -h' for a list of available options.", argv0);
                    process::exit(1);
                }
            }
            ci += 1;
        }
        idx += 1;
    }

    // Deferred installer actions: the whole command line has now been
    // validated, and --force only counted before the first FILE argument.
    match scan.action {
        DeferredAction::Install => match crate::installer::install_to_path(scan.force) {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("nano: install failed: {}", e);
                process::exit(1);
            }
        },
        DeferredAction::Update => match crate::installer::update_from_github(scan.force) {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("nano: update failed: {}", e);
                process::exit(1);
            }
        },
        DeferredAction::Check => match crate::installer::report_available_update() {
            Ok(()) => process::exit(0),
            Err(e) => {
                eprintln!("nano: check failed: {}", e);
                process::exit(1);
            }
        },
        DeferredAction::None => {}
    }

    // ----------------------------------------------------------------
    // Post-option processing
    // ----------------------------------------------------------------

    #[cfg(not(feature = "tiny"))]
    set_up_sigwinch_handler();

    // Ensure TERM is set.
    if std::env::var("TERM").is_err() {
        // Safe: single-threaded startup, before any threads are spawned.
        unsafe {
            std::env::set_var("TERM", "vt220");
        }
    }

    #[cfg(feature = "color")]
    {
        // Match GNU nano: the mere presence of NO_COLOR suppresses default
        // interface colors.  An explicit nanorc color combination may re-enable
        // them later in color::set_interface_colorpairs().
        state_mut().rescind_colors = std::env::var_os("NO_COLOR").is_some();
    }

    // Set up keybinding and function tables.
    crate::global::shortcut_init();

    // ----------------------------------------------------------------
    // Process nanorc files
    // ----------------------------------------------------------------
    #[cfg(feature = "nanorc")]
    if !ignore_rcfiles {
        // Back up command-line options that were explicitly set.
        #[cfg(any(feature = "wrapping", feature = "justify"))]
        let fill_cmdline = state().fill;
        #[cfg(not(feature = "tiny"))]
        let backup_dir_cmdline = state().backup_dir.clone();
        #[cfg(not(feature = "tiny"))]
        let word_chars_cmdline = state().word_chars.clone();
        #[cfg(not(feature = "tiny"))]
        let stripeclm_cmdline = state().stripe_column;
        #[cfg(not(feature = "tiny"))]
        let tabsize_cmdline = state().tabsize;
        #[cfg(feature = "operatingdir")]
        let operating_dir_cmdline = state().operating_dir.clone();
        #[cfg(feature = "justify")]
        let quotestr_cmdline = state().quotestr.clone();
        #[cfg(feature = "speller")]
        let alt_speller_cmdline = state().alt_speller.clone();
        let flags_cmdline = state().flags;

        // Clear string options to avoid overwriting command-line ones.
        #[cfg(not(feature = "tiny"))]
        with_state_mut(|s| {
            s.backup_dir = None;
            s.word_chars = None;
        });
        #[cfg(feature = "operatingdir")]
        with_state_mut(|s| s.operating_dir = None);
        #[cfg(feature = "justify")]
        with_state_mut(|s| s.quotestr = None);
        #[cfg(feature = "speller")]
        with_state_mut(|s| s.alt_speller = None);

        rcfile::do_rcfiles();

        // Restore command-line options if they were set.
        #[cfg(any(feature = "wrapping", feature = "justify"))]
        if fill_used {
            state_mut().fill = fill_cmdline;
        }
        #[cfg(not(feature = "tiny"))]
        {
            if backup_dir_cmdline.is_some() {
                state_mut().backup_dir = backup_dir_cmdline;
            }
            if word_chars_cmdline.is_some() {
                state_mut().word_chars = word_chars_cmdline;
            }
            if stripeclm_cmdline > 0 {
                state_mut().stripe_column = stripeclm_cmdline;
            }
            if tabsize_cmdline != -1 {
                state_mut().tabsize = tabsize_cmdline;
            }
        }
        #[cfg(feature = "operatingdir")]
        if operating_dir_cmdline.is_some() || ISSET!(RESTRICTED) {
            state_mut().operating_dir = operating_dir_cmdline;
        }
        #[cfg(feature = "justify")]
        if quotestr_cmdline.is_some() {
            state_mut().quotestr = quotestr_cmdline;
        }
        #[cfg(feature = "speller")]
        if alt_speller_cmdline.is_some() {
            state_mut().alt_speller = alt_speller_cmdline;
        }

        // If rcfile undid the default NO_WRAP, set BREAK_LONG_LINES.
        if !ISSET!(NO_WRAP) {
            SET!(BREAK_LONG_LINES);
        }

        // OR boolean flags from rcfile and command line.
        with_state_mut(|s| {
            for i in 0..s.flags.len() {
                s.flags[i] |= flags_cmdline[i];
            }
        });
    }

    // Apply wrapping flags.
    #[cfg(feature = "wrapping")]
    {
        if hardwrap == 0 {
            UNSET!(BREAK_LONG_LINES);
        } else if hardwrap == 1 {
            SET!(BREAK_LONG_LINES);
        }
    }

    // Bold instead of reverse video.
    if ISSET!(BOLD_TEXT) {
        state_mut().hilite_attribute = 0x0020_0000 /* A_BOLD */;
    }

    // Restricted mode: disable backups and history files.
    if ISSET!(RESTRICTED) {
        UNSET!(MAKE_BACKUP);
        #[cfg(feature = "nanorc")]
        {
            UNSET!(HISTORYLOG);
            UNSET!(POSITIONLOG);
        }
    }

    // Raw sequences: mouse cannot be used.
    if ISSET!(RAW_SEQUENCES) {
        UNSET!(USE_MOUSE);
    }

    // Modern bindings: ^Q and ^S must work.
    if ISSET!(MODERN_BINDINGS) {
        UNSET!(PRESERVE);
    }

    // Zero mode: suppress help lines.
    if ISSET!(ZERO) {
        SET!(NO_HELP);
    }

    // ----------------------------------------------------------------
    // History initialization
    // ----------------------------------------------------------------
    #[cfg(feature = "histories")]
    {
        history::history_init();

        if (ISSET!(HISTORYLOG) || ISSET!(POSITIONLOG)) && !history::have_statedir() {
            UNSET!(HISTORYLOG);
            UNSET!(POSITIONLOG);
        }

        if ISSET!(HISTORYLOG) {
            history::load_history();
        }
        if ISSET!(POSITIONLOG) {
            history::load_positions_register();
        }
    }

    // Backup directory.
    #[cfg(not(feature = "tiny"))]
    {
        let has_backup_dir = state().backup_dir.is_some();
        if has_backup_dir && !ISSET!(RESTRICTED) {
            files::init_backup_dir();
        }
    }

    // Operating directory.
    #[cfg(feature = "operatingdir")]
    {
        let has_opdir = state().operating_dir.is_some();
        if has_opdir {
            files::init_operating_dir();
        }
    }

    // ----------------------------------------------------------------
    // Justify defaults
    // ----------------------------------------------------------------
    #[cfg(feature = "justify")]
    {
        with_state_mut(|s| {
            if s.punct.is_none() {
                s.punct = Some("!.?".to_string());
            }
            if s.brackets.is_none() {
                s.brackets = Some("\"')>]}".to_string());
            }
            if s.quotestr.is_none() {
                s.quotestr = Some("^([ \t]*([!#%:;>|}]|/{2}))+".to_string());
            }
        });

        // Compile quoting regex.
        let quotestr = state().quotestr.clone().unwrap_or_default();
        match regex::bytes::RegexBuilder::new(&quotestr)
            .unicode(crate::chars::using_utf8())
            .dot_matches_new_line(true)
            .build()
        {
            Ok(re) => state_mut().quotereg = Some(re),
            Err(e) => {
                die(&format!("Bad quoting regex \"{}\": {}", quotestr, e));
            }
        }
        state_mut().quotestr = None;
    }

    // ----------------------------------------------------------------
    // Speller: check $SPELL environment variable
    // ----------------------------------------------------------------
    #[cfg(feature = "speller")]
    {
        let has_speller = state().alt_speller.is_some();
        if !has_speller && !ISSET!(RESTRICTED) {
            if let Ok(spellenv) = std::env::var("SPELL") {
                state_mut().alt_speller = Some(spellenv);
            }
        }
        // Strip leading blanks from alt_speller.
        with_state_mut(|s| {
            if let Some(ref mut sp) = s.alt_speller {
                crate::chars::strip_leading_blanks_from(sp);
            }
        });
    }

    // ----------------------------------------------------------------
    // Other defaults
    // ----------------------------------------------------------------
    #[cfg(not(feature = "tiny"))]
    {
        with_state_mut(|s| {
            if s.matchbrackets.is_none() {
                s.matchbrackets = Some("(<[{)>]}".to_string());
            }
            if s.whitespace.is_none() {
                #[cfg(feature = "utf8")]
                {
                    if s.using_utf8 {
                        // U+00BB >> (right-pointing double angle quotation mark)
                        // U+00B7 · (middle dot)
                        // U+00BB RIGHT-POINTING DOUBLE ANGLE QUOTATION MARK, U+00B7 MIDDLE DOT
                        s.whitespace = Some("\u{BB}\u{B7}".to_string());
                        s.whitelen[0] = 2;
                        s.whitelen[1] = 2;
                    } else {
                        s.whitespace = Some(">.".to_string());
                        s.whitelen[0] = 1;
                        s.whitelen[1] = 1;
                    }
                }
                #[cfg(not(feature = "utf8"))]
                {
                    s.whitespace = Some(">.".to_string());
                    s.whitelen[0] = 1;
                    s.whitelen[1] = 1;
                }
            }
            // Initialize the foretext.
            s.foretext = Some(String::new());
        });
    }

    // Initialize search string.
    with_state_mut(|s| {
        s.last_search = String::new();
    });
    UNSET!(BACKWARDS_SEARCH);

    // Default tabsize.
    let tabsize = state().tabsize;
    if tabsize == -1 {
        state_mut().tabsize = WIDTH_OF_TAB as isize;
    }

    // ----------------------------------------------------------------
    // Terminal and window setup
    // ----------------------------------------------------------------
    #[cfg(windows)]
    let mut redirected_stdin =
        prepare_windows_terminal_input(file_args.iter().any(|argument| argument == "-"));
    terminal_init();
    window_init();

    // ----------------------------------------------------------------
    // Interface color pairs
    // ----------------------------------------------------------------
    #[cfg(feature = "color")]
    {
        color::set_interface_colorpairs();
    }

    // Sidebar / bardata init.
    #[cfg(not(feature = "tiny"))]
    {
        let (cols, lines) = winio::terminal_size();
        with_state_mut(|s| {
            s.sidebar = if s.flag_isset(INDICATOR) && lines > 5 && cols > 9 {
                1
            } else {
                0
            };
            let needed = lines as usize;
            s.bardata.resize(needed, 0);
        });
    }
    let sidebar = state().sidebar;
    let margin = state().margin;
    let cols = winio::terminal_size().0;
    state_mut().editwincols = cols as i32 - margin - sidebar;

    // ----------------------------------------------------------------
    // Signal handlers
    // ----------------------------------------------------------------
    set_up_signal_handlers();

    #[cfg(feature = "mouse")]
    mouse_init();

    // ----------------------------------------------------------------
    // Key code assignments for modified keys
    // ----------------------------------------------------------------
    with_state_mut(|s| {
        s.controlleft = get_keycode("kLFT5", CONTROL_LEFT as i32);
        s.controlright = get_keycode("kRIT5", CONTROL_RIGHT as i32);
        s.controlup = get_keycode("kUP5", CONTROL_UP as i32);
        s.controldown = get_keycode("kDN5", CONTROL_DOWN as i32);
        s.controlhome = get_keycode("kHOM5", CONTROL_HOME as i32);
        s.controlend = get_keycode("kEND5", CONTROL_END as i32);
        #[cfg(not(feature = "tiny"))]
        {
            s.controldelete = get_keycode("kDC5", CONTROL_DELETE as i32);
            s.controlshiftdelete = get_keycode("kDC6", CONTROL_SHIFT_DELETE as i32);
            s.shiftup = get_keycode("kUP", SHIFT_UP as i32);
            s.shiftdown = get_keycode("kDN", SHIFT_DOWN as i32);
            s.shiftcontrolleft = get_keycode("kLFT6", SHIFT_CONTROL_LEFT as i32);
            s.shiftcontrolright = get_keycode("kRIT6", SHIFT_CONTROL_RIGHT as i32);
            s.shiftcontrolup = get_keycode("kUP6", SHIFT_CONTROL_UP as i32);
            s.shiftcontroldown = get_keycode("kDN6", SHIFT_CONTROL_DOWN as i32);
            s.shiftcontrolhome = get_keycode("kHOM6", SHIFT_CONTROL_HOME as i32);
            s.shiftcontrolend = get_keycode("kEND6", SHIFT_CONTROL_END as i32);
            s.altleft = get_keycode("kLFT3", ALT_LEFT as i32);
            s.altright = get_keycode("kRIT3", ALT_RIGHT as i32);
            s.altup = get_keycode("kUP3", ALT_UP as i32);
            s.altdown = get_keycode("kDN3", ALT_DOWN as i32);
            s.althome = get_keycode("kHOM3", ALT_HOME as i32);
            s.altend = get_keycode("kEND3", ALT_END as i32);
            s.altpageup = get_keycode("kPRV3", ALT_PAGEUP as i32);
            s.altpagedown = get_keycode("kNXT3", ALT_PAGEDOWN as i32);
            s.altinsert = get_keycode("kIC3", ALT_INSERT as i32);
            s.altdelete = get_keycode("kDC3", ALT_DELETE as i32);
            s.shiftaltleft = get_keycode("kLFT4", SHIFT_ALT_LEFT as i32);
            s.shiftaltright = get_keycode("kRIT4", SHIFT_ALT_RIGHT as i32);
            s.shiftaltup = get_keycode("kUP4", SHIFT_ALT_UP as i32);
            s.shiftaltdown = get_keycode("kDN4", SHIFT_ALT_DOWN as i32);
        }
        s.mousefocusin = get_keycode("kxIN", FOCUS_IN as i32);
        s.mousefocusout = get_keycode("kxOUT", FOCUS_OUT as i32);
    });

    // ----------------------------------------------------------------
    // Read files from command line
    // ----------------------------------------------------------------
    let mut file_idx = 0usize;
    let file_args_count = file_args.len();

    // Check whether we need to open multiple buffers: since nano 2.7.1,
    // every file named on the command line gets its own buffer.
    let read_them_all = cfg!(feature = "multibuffer");

    // Whether '-' (redirected standard input) was consumed as a buffer.
    let mut stdin_consumed = false;

    while file_idx < file_args_count {
        // Check we should keep reading.
        let has_openfile = state().openfile.is_some();
        if has_openfile && !read_them_all {
            break;
        }

        let mut givenline: isize = 0;
        let mut givencol: isize = 0;
        #[cfg(not(feature = "tiny"))]
        let mut searchstring: Option<String> = None;

        // If there's a +LINE[,COLUMN] argument, consume it.  ('+' is ASCII, so
        // the lossy form is a faithful test; the argument itself is a line/
        // column spec or search string, not a filename.)
        if file_idx + 1 < file_args_count && file_args[file_idx].to_string_lossy().starts_with('+')
        {
            let plus_arg = file_args[file_idx].to_string_lossy().into_owned();
            let rest = &plus_arg[1..];

            #[cfg(not(feature = "tiny"))]
            {
                let mut n = 0usize;
                let rbytes: Vec<char> = rest.chars().collect();
                while n < rbytes.len() && rbytes[n].is_ascii_alphabetic() {
                    match rbytes[n] {
                        'c' => {
                            SET!(CASE_SENSITIVE);
                        }
                        'C' => {
                            UNSET!(CASE_SENSITIVE);
                        }
                        'r' => {
                            SET!(USE_REGEXP);
                        }
                        'R' => {
                            UNSET!(USE_REGEXP);
                        }
                        _ => {
                            winio::statusline(
                                MessageType::Alert,
                                &format!("Invalid search modifier '{}'", rbytes[n]),
                            );
                        }
                    }
                    n += 1;
                }

                if n < rbytes.len() && (rbytes[n] == '/' || rbytes[n] == '?') {
                    if n + 1 < rbytes.len() {
                        let sstr: String = rbytes[n + 1..].iter().collect();
                        if rbytes[n] == '?' {
                            SET!(BACKWARDS_SEARCH);
                        }
                        searchstring = Some(sstr);
                    } else {
                        winio::statusline(MessageType::Alert, "Empty search string");
                    }
                    file_idx += 1;
                    // Don't fall through to number parsing.
                } else {
                    if rest.is_empty() {
                        givenline = -1; // EOF
                    } else {
                        match parse_position_numbers(rest) {
                            Ok((line, column)) => {
                                givenline = line;
                                givencol = column;
                            }
                            Err(()) => {
                                winio::statusline(
                                    MessageType::Alert,
                                    "Invalid line or column number",
                                );
                            }
                        }
                    }
                    file_idx += 1;
                }
            }
            #[cfg(feature = "tiny")]
            {
                if rest.is_empty() {
                    givenline = -1;
                } else {
                    if let Ok((line, column)) = parse_position_numbers(rest) {
                        givenline = line;
                        givencol = column;
                    } else {
                        winio::statusline(MessageType::Alert, "Invalid line or column number");
                    }
                }
                file_idx += 1;
            }
        }

        if file_idx >= file_args_count {
            break;
        }

        // The filename keeps its exact OS bytes all the way into the buffer.
        let mut filename = std::path::PathBuf::from(file_args[file_idx].clone());
        file_idx += 1;

        // Handle '-' (stdin).
        #[cfg(not(feature = "tiny"))]
        if filename.as_os_str() == "-" {
            #[cfg(windows)]
            let read_succeeded = if let Some(reader) = redirected_stdin.take() {
                scoop_stdin_from(reader, false)
            } else {
                scoop_stdin()
            };
            #[cfg(not(windows))]
            let read_succeeded = scoop_stdin();
            if !read_succeeded {
                continue;
            }
            stdin_consumed = true;
        } else {
            // Colon notation is only attempted for valid-UTF-8 names; a
            // non-UTF-8 name is opened exactly as given.
            #[cfg(not(feature = "tiny"))]
            if ISSET!(COLON_PARSING) && givenline == 0 && givencol == 0 {
                if let Some(name_str) = filename.to_str() {
                    if let Some((parsed_filename, line, column)) = parse_colon_notation(name_str) {
                        filename = std::path::PathBuf::from(parsed_filename);
                        givenline = line;
                        givencol = column;
                    }
                }
            }

            if !files::open_buffer_impl(&filename, true) {
                continue;
            }
        }

        #[cfg(feature = "tiny")]
        {
            if !files::open_buffer_impl(&filename, true) {
                continue;
            }
        }

        // Restore cursor position from history.
        #[cfg(feature = "histories")]
        {
            let has_filename = with_state(|s| {
                s.openfile
                    .as_ref()
                    .map(|of| !of.filename.is_empty())
                    .unwrap_or(false)
            });
            if ISSET!(POSITIONLOG) && has_filename {
                history::restore_cursor_position_if_any();
            }
        }

        // Apply given line/column position or search string.
        if givenline != 0 || givencol != 0 {
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.current = of.filetop.clone();
                    of.placewewant = 0;
                }
            });
            search::goto_line_and_column(givenline, givencol, true);
        }
        #[cfg(not(feature = "tiny"))]
        {
            if givenline == 0 && givencol == 0 {
                if let Some(ref sstr) = searchstring {
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.current = of.filetop.clone();
                            of.current_x = 0;
                        }
                    });
                    if ISSET!(USE_REGEXP) {
                        search::regexp_init(sstr);
                    }
                    let ss_clone = sstr.clone();
                    let filetop =
                        with_state(|s| s.openfile.as_ref().and_then(|of| of.filetop.clone()));
                    if let Some(ft) = filetop {
                        let backwards = ISSET!(BACKWARDS_SEARCH);
                        let mut match_len: usize = 0;
                        let found = search::findnextstr(
                            &ss_clone,
                            false,
                            JUSTFIND,
                            &mut match_len,
                            backwards,
                            Some(&ft),
                            0,
                        );
                        if found == 0 {
                            search::not_found_msg(&ss_clone);
                        } else {
                            let lastmsg = state().lastmessage;
                            if lastmsg <= MessageType::Remark {
                                winio::wipe_statusbar();
                            }
                        }
                    }
                    let pw = crate::utils::xplustabs();
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.placewewant = pw;
                        }
                    });
                    if ISSET!(USE_REGEXP) {
                        search::tidy_up_after_search();
                    }
                    let ss_owned = ss_clone.clone();
                    state_mut().last_search = ss_owned;
                }
            }
        }
    }

    #[cfg(not(feature = "multibuffer"))]
    if file_idx < file_args_count {
        die(crate::tr!("Can open just one file\n"));
    }

    // After handling command-line files, allow inserting files.
    UNSET!(NOREAD_MODE);

    // Nano needs a keyboard: when standard input is not a terminal
    // (e.g. `seq 9 | nano -`), reattach it to /dev/tty like C does.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        // Without '-', a redirected stdin means there is no keyboard to
        // drive the editor at all: die like GNU nano instead of limping on.
        if !stdin_consumed {
            die(crate::tr!("Standard input is not a terminal\n"));
        }
        reconnect_and_store_state();
    }

    // If no files were given, open a blank buffer.
    let has_openfile = state().openfile.is_some();
    if !has_openfile {
        files::open_buffer_impl("", true);
        UNSET!(VIEW_MODE);
    } else {
        #[cfg(feature = "multibuffer")]
        {
            // Switch from the last opened file to the first.
            // (C: `openfile = openfile->next` — a raw rotation, without
            // redecorate_after_switch and its possible status message.)
            with_state_mut(|s| {
                if let Some(next) = s.buffer_ring.pop_front() {
                    if let Some(cur) = s.openfile.replace(next) {
                        s.buffer_ring.push_back(cur);
                    }
                }
            });
            let more_than_one = state().more_than_one;
            if more_than_one {
                files::mention_name_and_linecount();
            }
            if ISSET!(VIEW_MODE) {
                SET!(MULTIBUFFER);
            }
        }
    }

    files::prepare_for_display();

    // Display any startup problem.
    #[cfg(any(feature = "nanorc", feature = "histories"))]
    {
        let prob = state().startup_problem.clone();
        if let Some(ref msg) = prob {
            let m = msg.clone();
            winio::statusline(MessageType::Alert, &m);
        }
    }

    // Welcome message for an empty new buffer.
    #[cfg(feature = "help")]
    {
        let show_welcome = with_state(|s| {
            s.openfile
                .as_ref()
                .map(|of| of.filename.is_empty() && of.totsize == 0 && !s.flag_isset(NO_HELP))
                .unwrap_or(false)
        });
        // Check NOTREBOUND: help function must still be bound to ^G (0x07).
        #[cfg(feature = "nanorc")]
        let notrebound = crate::global::first_sc_for(MMAIN, crate::global::do_help as FuncPtr)
            .map(|(kc, _)| kc == 0x07)
            .unwrap_or(false);
        #[cfg(not(feature = "nanorc"))]
        let notrebound = true;
        if show_welcome && notrebound {
            winio::statusbar("Welcome to nano.  For basic help, type Ctrl+G.");
        }
    }

    // Force re-evaluation of margin.
    #[cfg(feature = "linenumbers")]
    with_state_mut(|s| s.margin = 12345);

    state_mut().we_are_running = true;

    // Kick off a background check for a newer release (set NANO_NO_UPDATE_CHECK
    // to disable). Best-effort and silent on failure; never blocks startup.
    // Surface an already-staged update on the titlebar immediately.
    if let Some(version) = crate::installer::pending_update_version() {
        with_state_mut(|s| s.pending_update = Some(version));
    }

    let update_rx = crate::installer::spawn_update_check();

    // ----------------------------------------------------------------
    // Main input loop
    // ----------------------------------------------------------------
    loop {
        process_pending_signal_requests();

        // Surface a completed background update, if any.
        if let Ok(status) = update_rx.try_recv() {
            if let crate::installer::UpdateStatus::Downloaded { version, in_place } = status {
                let tail = if in_place {
                    "restart nano to apply."
                } else {
                    "the installed copy was refreshed."
                };
                winio::statusline(
                    MessageType::Notice,
                    &format!("Update v{} downloaded \u{2014} {}", version, tail),
                );
                with_state_mut(|s| s.pending_update = Some(version.clone()));
                winio::refresh_titlebar();
            }
        }

        #[cfg(feature = "linenumbers")]
        confirm_margin();

        // On a VT, mute modifiers when no keys are waiting.
        #[cfg(target_os = "linux")]
        {
            let on_vt = state().on_a_vt;
            if on_vt && winio::waiting_keycodes() == 0 {
                state_mut().mute_modifiers = false;
            }
        }

        // Show the bottom bars when not in MMAIN.
        let currmenu = state().currmenu;
        if currmenu != MMAIN {
            winio::bottombars(MMAIN);
        }

        // Sidescroll computation.
        #[cfg(not(feature = "tiny"))]
        {
            let (softwrap, solo_sidescroll, editwincols) = with_state(|s| {
                (
                    s.flag_isset(SOFTWRAP),
                    s.flag_isset(SOLO_SIDESCROLL),
                    s.editwincols,
                )
            });
            let want_united =
                !solo_sidescroll && !softwrap && editwincols > (2 * CUSHION + 2) as i32;
            let united = state().united_sidescroll;
            if united != want_united {
                with_state_mut(|s| {
                    s.united_sidescroll = want_united;
                    s.refresh_needed = true;
                });
            }

            // Minibar display.
            let (minibar, zero, lines, lastmsg) = with_state(|s| {
                let lines = s.editwinrows + s.midwin.y as i32;
                (
                    s.flag_isset(MINIBAR),
                    s.flag_isset(ZERO),
                    lines,
                    s.lastmessage,
                )
            });
            if minibar && !zero && lines > 1 && lastmsg < MessageType::Remark {
                winio::minibar();
            } else {
                // Constant cursor position display.
                let (constant_show, lastmsg2, lines2, zero2, waiting) = with_state(|s| {
                    let ln = s.editwinrows + s.midwin.y as i32;
                    (
                        s.flag_isset(CONSTANT_SHOW),
                        s.lastmessage,
                        ln,
                        s.flag_isset(ZERO),
                        winio::waiting_keycodes(),
                    )
                });
                if constant_show
                    && lastmsg2 == MessageType::Vacuum
                    && lines2 > 1
                    && !zero2
                    && waiting == 0
                {
                    winio::report_cursor_position();
                }
            }
        }
        #[cfg(feature = "tiny")]
        {
            let (constant_show, lastmsg, lines, zero, waiting) = with_state(|s| {
                let ln = s.editwinrows;
                (
                    s.flag_isset(CONSTANT_SHOW),
                    s.lastmessage,
                    ln,
                    s.flag_isset(ZERO),
                    winio::waiting_keycodes(),
                )
            });
            if constant_show && lastmsg == MessageType::Vacuum && lines > 1 && !zero && waiting == 0
            {
                winio::report_cursor_position();
            }
        }

        state_mut().as_an_at = true;

        // BOM detection.
        #[cfg(all(feature = "utf8", not(feature = "tiny")))]
        {
            let using_utf8 = state().using_utf8;
            let at_bom = with_state(|s| {
                s.openfile
                    .as_ref()
                    .and_then(|of| of.current.as_ref())
                    .map(|cur| {
                        let b = cur.borrow();
                        let d = b.data.as_bytes();
                        s.openfile
                            .as_ref()
                            .map(|of| of.current_x == 0)
                            .unwrap_or(false)
                            && d.get(0) == Some(&0xEF)
                            && d.get(1) == Some(&0xBB)
                            && d.get(2) == Some(&0xBF)
                    })
                    .unwrap_or(false)
            });
            if at_bom && using_utf8 {
                winio::statusline(MessageType::Notice, "Byte Order Mark");
                winio::set_blankdelay_to_one();
            }
        }

        // Refresh display.
        let (refresh_needed, lines, lastmsg) = with_state(|s| {
            let ln = s.editwinrows;
            (s.refresh_needed, ln, s.lastmessage)
        });
        if (refresh_needed && lines > 1) || (lines == 1 && lastmsg <= MessageType::Hush) {
            winio::edit_refresh();
        } else {
            winio::place_the_cursor();
        }

        // Barless mode: ZERO flag handling.
        #[cfg(not(feature = "tiny"))]
        {
            let (zero, lastmsg, editwinrows, lines) = with_state(|s| {
                let ln = s.editwinrows + s.midwin.y as i32;
                (s.flag_isset(ZERO), s.lastmessage, s.editwinrows, ln)
            });
            if zero && lastmsg > MessageType::Hush {
                let cursor_row =
                    with_state(|s| s.openfile.as_ref().map(|of| of.cursor_row).unwrap_or(0));
                if cursor_row == (editwinrows - 1) as isize && lines > 1 {
                    winio::edit_scroll(FORWARD);
                    // wnoutrefresh(midwin) — flush happens in edit_refresh
                }
                // wredrawln(footwin, 0, 1) — simplified; refresh screen.
                winio::place_the_cursor();
            } else if zero && lastmsg > MessageType::Vacuum {
                // wredrawln(midwin, editwinrows-1, 1) — simplified.
            }
        }

        with_state_mut(|s| {
            s.final_status = 0;
            s.focusing = true;
        });

        prompt::put_cursor_at_end_of_answer();

        // Handle window resize.
        if winio::consume_resize_request(None) {
            continue;
        }

        process_a_keystroke();
    }
}

// ---------------------------------------------------------------------------
// Trait extension for method chaining (used in short-option parsing)
// ---------------------------------------------------------------------------

trait Also: Sized {
    fn also<F: FnOnce(&Self)>(self, f: F) -> Self {
        f(&self);
        self
    }
}
impl<T> Also for T {}

#[cfg(all(test, unix))]
mod signal_tests {
    use super::*;

    #[test]
    fn asynchronous_handlers_only_publish_atomic_requests() {
        TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
        handle_hupterm(libc::SIGTERM);
        assert_eq!(TERMINATION_SIGNAL.swap(0, Ordering::SeqCst), libc::SIGTERM);

        THE_WINDOW_RESIZED.store(false, Ordering::SeqCst);
        #[cfg(not(feature = "tiny"))]
        handle_sigwinch(libc::SIGWINCH);
        #[cfg(not(feature = "tiny"))]
        assert!(THE_WINDOW_RESIZED.swap(false, Ordering::SeqCst));

        #[cfg(not(feature = "tiny"))]
        {
            SUSPEND_REQUESTED.store(false, Ordering::SeqCst);
            request_suspend(libc::SIGTSTP);
            assert!(SUSPEND_REQUESTED.swap(false, Ordering::SeqCst));
        }
    }

    #[test]
    fn emergency_save_preserves_non_utf8_path_and_document_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let directory = tempfile::tempdir().unwrap();
        let mut filename_bytes = directory.path().as_os_str().as_bytes().to_vec();
        filename_bytes.extend_from_slice(b"/buffer-\xFF.bin");
        let filename = std::path::PathBuf::from(OsString::from_vec(filename_bytes));

        let line = make_new_node(None);
        let document = [b'a', 0xFF, 0, b'z'];
        line.borrow_mut().data = LineData::from_external(&document);

        let mut buffer = Box::new(OpenFileStruct::default());
        buffer.filename = filename.to_string_lossy().into_owned();
        buffer.filename_path = filename.clone();
        buffer.filetop = Some(line.clone());
        buffer.filebot = Some(line.clone());
        buffer.edittop = Some(line.clone());
        buffer.current = Some(line);
        buffer.modified = true;
        state_mut().openfile = Some(buffer);

        emergency_save(&filename);

        let mut save_name = filename.as_os_str().to_os_string();
        save_name.push(".save");
        let save_path = std::path::PathBuf::from(save_name);
        assert_eq!(std::fs::read(&save_path).unwrap(), document);
        assert!(!directory.path().join("buffer-�.bin.save").exists());

        state_mut().openfile = None;
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn position_parser_accepts_column_only_and_rejects_partial_numbers() {
        assert_eq!(parse_position_numbers("12"), Ok((12, 0)));
        assert_eq!(parse_position_numbers("12,3"), Ok((12, 3)));
        assert_eq!(parse_position_numbers(",3"), Ok((0, 3)));
        assert_eq!(parse_position_numbers("12:3"), Ok((12, 3)));
        assert_eq!(parse_position_numbers("12,bad"), Err(()));
        assert_eq!(parse_position_numbers("12,"), Err(()));
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn colon_parser_only_strips_numeric_suffixes_from_existing_paths() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("sample.txt");
        std::fs::write(&file, b"one\ntwo\n").unwrap();
        let base = file.to_string_lossy();

        assert_eq!(
            parse_colon_notation(&format!("{base}:2")),
            Some((base.to_string(), 2, 0))
        );
        assert_eq!(
            parse_colon_notation(&format!("{base}:2:4")),
            Some((base.to_string(), 2, 4))
        );
        assert_eq!(parse_colon_notation(&format!("{base}:bad")), None);
    }

    #[test]
    fn feature_availability_matches_the_compiled_build() {
        assert_eq!(
            long_option_is_available("multibuffer"),
            cfg!(feature = "multibuffer")
        );
        assert_eq!(short_option_is_available('s'), cfg!(feature = "speller"));
        assert_eq!(
            long_option_is_available("magic"),
            cfg!(feature = "libmagic")
        );
    }
}

#[cfg(all(test, feature = "linenumbers"))]
mod margin_tests {
    use crate::global::state;

    // Regression: confirm_margin used to call ensure_firstcolumn_is_aligned
    // inside its with_state_mut closure, so any margin change (turning line
    // numbers on or off) panicked with "RefCell already mutably borrowed".
    #[test]
    fn margin_changes_do_not_reborrow_state() {
        crate::global::state_mut().flags = [0; 4];
        crate::files::make_new_buffer();

        crate::SET!(crate::definitions::LINE_NUMBERS);
        super::confirm_margin();
        assert!(state().margin > 0);

        crate::UNSET!(crate::definitions::LINE_NUMBERS);
        super::confirm_margin();
        assert_eq!(state().margin, 0);
    }
}

#[cfg(test)]
mod cli_scan_tests {
    use super::{ArgScan, DeferredAction};

    /// Walk args the way the real parse loop feeds the tracker for
    /// option-shaped tokens and positionals.  Option-value consumption
    /// (--tabsize 4) happens in the loop itself, so those cases are covered
    /// by construction there rather than duplicated here.
    fn scan(args: &[&str]) -> ArgScan {
        let mut s = ArgScan::default();
        for arg in args {
            if *arg == "--" || !arg.starts_with('-') || *arg == "-" {
                s.note_positional();
            } else {
                s.note_option(arg.trim_start_matches('-'));
            }
        }
        s
    }

    #[test]
    fn force_counts_only_before_the_first_file() {
        assert!(scan(&["--force", "--install"]).force);
        assert!(scan(&["--install", "--force"]).force);
        // The bug being fixed: a FILE named --force must not trigger it.
        assert!(!scan(&["file.txt", "--force", "--install"]).force);
        assert!(!scan(&["-", "--force", "--install"]).force);
        assert!(!scan(&["--", "file.txt", "--force", "--install"]).force);
        // Bare --force stays a silent no-op action.
        let bare = scan(&["--force"]);
        assert!(bare.force);
        assert_eq!(bare.action, DeferredAction::None);
    }

    #[test]
    fn installer_actions_are_recorded() {
        assert_eq!(scan(&["--install"]).action, DeferredAction::Install);
        assert_eq!(scan(&["--update"]).action, DeferredAction::Update);
        assert_eq!(
            scan(&["--check"]).action,
            DeferredAction::Check,
            "--check must be recognized as a deferred action"
        );
        assert_eq!(scan(&[]).action, DeferredAction::None);
    }
}
