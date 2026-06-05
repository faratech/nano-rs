#![allow(unused, non_snake_case, dead_code, non_camel_case_types)]
// Port of src/files.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2015-2022, 2025 Benno Schulenberg

use crate::definitions::*;
use crate::global::{STATE, with_state, with_state_mut};
use crate::{ISSET, SET, UNSET, TOGGLE};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// Re-export stubs for winio/text/search/nano functions referenced here.
// These will be replaced by real implementations when those modules are ported.

macro_rules! tr {
    ($s:literal) => { $s };
    ($s:literal, $($a:expr),*) => { &format!($s, $($a),*) };
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const LOCKSIZE: usize = 1024;
const LUMPSIZE: usize = 120;

// ---------------------------------------------------------------------------
// Stub calls to other modules (forward declarations)
// ---------------------------------------------------------------------------

// ---- Delegating wrappers: call the real implementations ----

#[inline] fn statusline(level: MessageType, msg: &str) { crate::winio::statusline(level, msg); }
#[inline] fn statusbar(msg: &str) { crate::winio::statusbar(msg); }
#[inline] fn titlebar(extra: Option<&str>) { crate::winio::titlebar(extra); }
fn blank_bottombars() { crate::winio::blank_bottombars(); }
fn wipe_statusbar() { crate::winio::wipe_statusbar(); }
fn beep() {}  // stub: no winio::beep exposed as pub yet
fn napms(_ms: i32) {}  // stub: no direct equivalent

fn make_new_node(prev: Option<LinePtr>) -> LinePtr {
    use std::rc::Rc;
    use std::cell::RefCell;
    Rc::new(RefCell::new(LineNode {
        data: String::new(),
        lineno: 0,
        next: None,
        prev: prev.as_ref().map(|p| Rc::downgrade(p)),
        #[cfg(feature = "color")]
        multidata: Vec::new(),
        #[cfg(not(feature = "tiny"))]
        has_anchor: false,
    }))
}

#[inline] fn ingraft_buffer(topline: LinePtr) { crate::cut::ingraft_buffer(topline); }
#[inline] fn xplustabs() -> usize { crate::utils::xplustabs() }

#[cfg(feature = "color")]
fn precalc_multicolorinfo() { crate::color::precalc_multicolorinfo(); }
#[cfg(not(feature = "color"))]
fn precalc_multicolorinfo() {}

#[cfg(feature = "color")]
fn find_and_prime_applicable_syntax() { crate::color::find_and_prime_applicable_syntax(); }
#[cfg(not(feature = "color"))]
fn find_and_prime_applicable_syntax() {}

// Signature adapter: call site passes isize, real fn takes usize.
fn less_than_a_screenful(was_lineno: isize, was_leftedge: usize) -> bool {
    crate::winio::less_than_a_screenful(was_lineno as usize, was_leftedge)
}

// Signature adapter: call site passes Option<&LinePtr>, real fn takes &str.
fn leftedge_for(xpt: usize, current: Option<&LinePtr>) -> usize {
    let data = current.map(|l| l.borrow().data.clone()).unwrap_or_default();
    crate::winio::leftedge_for(xpt, &data)
}

#[inline] fn free_lines(line: Option<LinePtr>) { crate::nano::free_lines(line); }

#[cfg(not(feature = "tiny"))]
fn ensure_firstcolumn_is_aligned() { crate::winio::ensure_firstcolumn_is_aligned(); }
#[cfg(feature = "tiny")]
fn ensure_firstcolumn_is_aligned() {}

#[inline] fn warn_and_briefly_pause(msg: &str) { crate::winio::warn_and_briefly_pause(msg); }
fn ask_user(_yesno: bool, _msg: &str) -> i32 { YES }  // stub: real impl TBD
fn in_restricted_mode() -> bool { false }  // stub: real impl is in nano.rs but with different sig
fn breadth(_s: &str) -> usize { _s.len() }
fn display_string(_s: &str, _from: usize, _room: usize, _a: bool, _b: bool) -> String {
    _s.to_string()
}
fn mbstrcasecmp(_a: &str, _b: &str) -> i32 { _a.cmp(_b) as i32 }

// Stub for restoring terminal state — no real equivalent exists yet.
fn reconnect_and_store_state() {}  // stub: function not yet ported
fn terminal_init() { let _ = crate::winio::terminal_init(); }
fn doupdate() {}  // stub: no crate::winio::doupdate exists
fn isendwin() -> bool { false }  // stub: no crate::winio::isendwin exists

#[inline] fn install_handler_for_Ctrl_C() { crate::nano::install_handler_for_Ctrl_C(); }
#[inline] fn restore_handler_for_Ctrl_C() { crate::nano::restore_handler_for_Ctrl_C(); }
#[inline] fn block_sigwinch(block: bool) { crate::nano::block_sigwinch(block); }
fn enable_kb_interrupt() { crate::nano::enable_kb_interrupt(); }
fn close_and_go() {}
fn finish() {}

// Undo record delegation
#[inline] fn add_undo(utype: UndoType, msg: Option<&str>) { crate::text::add_undo(utype, msg); }
#[inline] fn update_undo(utype: UndoType) { crate::text::update_undo(utype); }
fn discard_until(_target: *mut crate::definitions::UndoStruct) {}
fn copy_marked_region() {}
fn do_snip(_a: bool, _b: bool, _c: bool) {}
fn get_region(
    _topline: &mut Option<LinePtr>, _top_x: &mut usize,
    _botline: &mut Option<LinePtr>, _bot_x: &mut usize,
) {}

// Signature adapter: call site passes LinePtr by value, real fn takes &LinePtr.
fn delete_node(node: LinePtr) { crate::nano::delete_node(&node); }

fn add_or_remove_pipe_symbol_from_answer() {}
fn restore_cursor_position_if_any() {}
fn do_undo() {}
fn goto_line_posx(_lineno: isize, _x: usize) {}
fn cancel_the_command_signal(_sig: i32) {}
fn do_credits() {}
#[inline]
fn do_prompt(
    menu: u32,
    given: &str,
    _history: Option<&mut Option<LinePtr>>,
    refresh: fn(),
    msg: &str,
    _extra: &str,
) -> i32 {
    crate::prompt::do_prompt(menu, Some(given), None, Some(refresh), msg)
}
#[inline] fn edit_refresh() { crate::winio::edit_refresh(); }
fn browse_in(_path: &str) -> Option<String> { None }
#[inline] fn func_from_key(response: i32) -> Option<FuncPtr> { crate::global::func_from_key(response) }
fn update_history(_history: &mut Option<LinePtr>, _s: &str, _prune: bool) {}
fn open_buffer(_filename: &str, _new_one: bool) -> bool { true }

// Stub display helper
fn COLS() -> usize {
    with_state(|s| s.midwin.cols as usize)
}
fn LINES() -> usize {
    with_state(|s| s.midwin.rows as usize)
}
fn editwinrows() -> i32 {
    with_state(|s| s.editwinrows)
}

// ---------------------------------------------------------------------------
// crop_to_fit — return the given file name cropped to fit within `room` cols
// C: char *crop_to_fit(const char *name, int room)
// ---------------------------------------------------------------------------
pub fn crop_to_fit(name: &str, room: usize) -> String {
    if breadth(name) <= room {
        return display_string(name, 0, room, false, false);
    }

    if room < 4 {
        return "_".to_string();
    }

    let mut clipped = display_string(name, breadth(name) - room + 3, room, false, false);
    clipped.insert_str(0, "...");
    clipped
}

// ---------------------------------------------------------------------------
// LOCK FILE support (!NANO_TINY)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tiny"))]
pub const LOCKING_PREFIX: &str = ".";
#[cfg(not(feature = "tiny"))]
pub const LOCKING_SUFFIX: &str = ".swp";

/* C: bool delete_lockfile(const char *lockfilename)
 * Delete the lock file.  Return TRUE on success, and FALSE otherwise. */
#[cfg(not(feature = "tiny"))]
pub fn delete_lockfile(lockfilename: &str) -> bool {
    match std::fs::remove_file(lockfilename) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            statusline(MessageType::Mild,
                &format!("Error deleting lock file {}: {}", lockfilename, e));
            false
        }
    }
}

/* C: bool write_lockfile(const char *lockfilename, const char *filename, bool modified)
 * Write a lock file under the given lockfilename.  Always annihilates an
 * existing version of that file.  Return TRUE on success; FALSE otherwise. */
#[cfg(not(feature = "tiny"))]
pub fn write_lockfile(lockfilename: &str, filename: &str, modified: bool) -> bool {
    use std::ffi::CString;

    // First remove any existing lock file.
    if !delete_lockfile(lockfilename) {
        return false;
    }

    let pid = unsafe { libc::getpid() } as u32;

    // Get username
    let username: String = {
        #[cfg(unix)]
        unsafe {
            let uid = libc::geteuid();
            let pw = libc::getpwuid(uid);
            if pw.is_null() {
                statusline(MessageType::Mild, "Couldn't determine my identity for lock file");
                return false;
            }
            let name = std::ffi::CStr::from_ptr((*pw).pw_name);
            name.to_string_lossy().into_owned()
        }
        #[cfg(not(unix))]
        String::from("unknown")
    };

    // Get hostname
    let hostname: String = {
        let mut buf = [0u8; 32];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, 31) };
        if ret < 0 {
            // errno check: if not ENAMETOOLONG, return false
            let errno = unsafe { *libc::__errno_location() };
            if errno != libc::ENAMETOOLONG {
                statusline(MessageType::Mild,
                    &format!("Couldn't determine hostname: {}", io::Error::last_os_error()));
                return false;
            }
        }
        buf[31] = 0;
        let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
        cstr.to_string_lossy().into_owned()
    };

    // Build lock data (1024 bytes)
    let mut lockdata = vec![0u8; LOCKSIZE];

    lockdata[0] = 0x62;
    lockdata[1] = 0x30;

    // bytes 2-11: program name "nano VERSION" (truncated to 10 bytes)
    let progname = format!("nano {}", env!("CARGO_PKG_VERSION"));
    let progname_bytes = progname.as_bytes();
    let plen = progname_bytes.len().min(10);
    lockdata[2..2+plen].copy_from_slice(&progname_bytes[..plen]);

    // bytes 24-27: PID, little endian
    lockdata[24] = (pid % 256) as u8;
    lockdata[25] = ((pid / 256) % 256) as u8;
    lockdata[26] = ((pid / (256 * 256)) % 256) as u8;
    lockdata[27] = (pid / (256 * 256 * 256)) as u8;

    // bytes 28-43: username (up to 16 bytes)
    let uname_bytes = username.as_bytes();
    let ulen = uname_bytes.len().min(16);
    lockdata[28..28+ulen].copy_from_slice(&uname_bytes[..ulen]);

    // bytes 68-99: hostname (up to 32 bytes)
    let hname_bytes = hostname.as_bytes();
    let hlen = hname_bytes.len().min(32);
    lockdata[68..68+hlen].copy_from_slice(&hname_bytes[..hlen]);

    // bytes 108-875: filename (up to 768 bytes)
    let fname_bytes = filename.as_bytes();
    let flen = fname_bytes.len().min(768);
    lockdata[108..108+flen].copy_from_slice(&fname_bytes[..flen]);

    // byte 1007: modified flag
    lockdata[1007] = if modified { 0x55 } else { 0x00 };

    // Create file exclusively
    use std::os::unix::fs::OpenOptionsExt;
    let file_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o666) // RW_FOR_ALL
        .open(lockfilename);

    match file_result {
        Err(e) => {
            statusline(MessageType::Mild,
                &format!("Error writing lock file {}: {}", lockfilename, e));
            false
        }
        Ok(mut f) => {
            match f.write_all(&lockdata) {
                Ok(_) => {
                    match f.flush() {
                        Ok(_) => true,
                        Err(e) => {
                            statusline(MessageType::Mild,
                                &format!("Error writing lock file {}: {}", lockfilename, e));
                            false
                        }
                    }
                }
                Err(e) => {
                    statusline(MessageType::Mild,
                        &format!("Error writing lock file {}: {}", lockfilename, e));
                    false
                }
            }
        }
    }
}

/// Sentinel indicating user chose not to open a locked file.
#[cfg(not(feature = "tiny"))]
pub const SKIPTHISFILE: i32 = -2;

/* C: char *do_lockfile(const char *filename, bool ask_the_user)
 * First check if a lock file already exists.  If so, and ask_the_user is TRUE,
 * ask whether to open the corresponding file anyway.  Return SKIPTHISFILE when
 * the user answers "No", return the lock filename on success, and return None on
 * failure.  Rust version returns Option<String> (None = failure, Some(path) = success)
 * and a special Err(()) means SKIPTHISFILE. */
#[cfg(not(feature = "tiny"))]
pub fn do_lockfile(filename: &str, ask_the_user: bool) -> Result<Option<String>, ()> {
    // Build lock filename: <dir>/.<basename>.swp
    let path = Path::new(filename);
    let dirname = path.parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let basename = path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let lockfilename = format!("{}/{}{}{}", dirname, LOCKING_PREFIX, basename, LOCKING_SUFFIX);

    let lock_exists = Path::new(&lockfilename).exists();

    if lock_exists && !ask_the_user {
        blank_bottombars();
        statusline(MessageType::Alert, "Someone else is also editing this file");
        napms(1200);
    } else if lock_exists {
        // Read and parse the lock file
        match File::open(&lockfilename) {
            Err(e) => {
                statusline(MessageType::Alert,
                    &format!("Error opening lock file {}: {}", lockfilename, e));
                return Ok(None);
            }
            Ok(mut f) => {
                let mut lockbuf = vec![0u8; LOCKSIZE];
                let readamt = match f.read(&mut lockbuf) {
                    Ok(n) => n,
                    Err(e) => {
                        statusline(MessageType::Alert,
                            &format!("Error reading lock file {}: {}", lockfilename, e));
                        return Ok(None);
                    }
                };

                // Validate magic bytes and minimum size
                if readamt < 68 || lockbuf[0] != 0x62 || lockbuf[1] != 0x30 {
                    statusline(MessageType::Alert,
                        &format!("Bad lock file is ignored: {}", lockfilename));
                    return Ok(None);
                }

                // Extract program name (bytes 2-11)
                let lockprog: String = {
                    let bytes = &lockbuf[2..12];
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(10);
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                };

                // Extract PID (bytes 24-27, little endian)
                let lockpid = ((lockbuf[27] as u32) * 256 * 256 * 256)
                    + ((lockbuf[26] as u32) * 256 * 256)
                    + ((lockbuf[25] as u32) * 256)
                    + (lockbuf[24] as u32);

                // Extract username (bytes 28-43)
                let lockuser: String = {
                    let bytes = &lockbuf[28..44];
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(16);
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                };

                let pidstring = format!("{}", lockpid);

                // Display newlines in filenames as ^J
                with_state_mut(|s| s.as_an_at = false);

                let question = "File %s is being edited by %s (with %s, PID %s); open anyway?";
                let total_fixed = breadth(question)
                    + breadth(&lockuser)
                    + breadth(&lockprog)
                    + breadth(&pidstring);
                let room = if COLS() > total_fixed + 7 { COLS() - total_fixed + 7 } else { 4 };
                let postedname = crop_to_fit(filename, room);
                let promptstr = format!("{}", question
                    .replace("%s", &postedname).replacen("%s", &lockuser, 1)
                    .replacen("%s", &lockprog, 1).replacen("%s", &pidstring, 1));

                let choice = ask_user(YESORNO, &promptstr);

                // When the user cancelled while we're still starting up, quit.
                let we_are_running = with_state(|s| s.we_are_running);
                if choice == CANCEL && !we_are_running {
                    finish();
                }

                if choice != YES {
                    wipe_statusbar();
                    return Err(());
                }
            }
        }
    }

    if write_lockfile(&lockfilename, filename, false) {
        Ok(Some(lockfilename))
    } else {
        Ok(None)
    }
}

/* C: void stat_with_alloc(const char *filename, struct stat **pstat)
 * Perform a stat call on the given filename.  On success, *pstat points to
 * the stat's result.  On failure, *pstat is freed and made NULL. */
#[cfg(not(feature = "tiny"))]
pub fn stat_with_alloc(filename: &str) -> Option<Box<libc::stat>> {
    let cname = match std::ffi::CString::new(filename) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::stat(cname.as_ptr(), &mut st) };
    if ret == 0 {
        Some(Box::new(st))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// has_valid_path — verify that the containing directory exists
// C: bool has_valid_path(const char *filename)
// ---------------------------------------------------------------------------
pub fn has_valid_path(filename: &str) -> bool {
    let path = Path::new(filename);
    let parentdir = path.parent().unwrap_or(Path::new("."));
    let parentdir_str = parentdir.to_string_lossy();

    // Check if it's the current directory
    if parentdir_str == "." {
        let current = std::env::current_dir();
        let gone = current.is_err();
        if gone {
            statusline(MessageType::Alert, "The working directory has disappeared");
            return false;
        }
    }

    match std::fs::metadata(parentdir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            statusline(MessageType::Alert,
                &format!("Directory '{}' does not exist", parentdir_str));
            false
        }
        Err(e) => {
            statusline(MessageType::Alert,
                &format!("Path '{}': {}", parentdir_str, e));
            false
        }
        Ok(meta) => {
            if !meta.is_dir() {
                statusline(MessageType::Alert,
                    &format!("Path '{}' is not a directory", parentdir_str));
                return false;
            }

            // Check for execute access
            let cpath = match std::ffi::CString::new(parentdir_str.as_ref()) {
                Ok(s) => s,
                Err(_) => return false,
            };
            let x_ok = unsafe { libc::access(cpath.as_ptr(), libc::X_OK) };
            if x_ok < 0 {
                statusline(MessageType::Alert,
                    &format!("Path '{}' is not accessible", parentdir_str));
                return false;
            }

            // Check for write access if locking is enabled
            #[cfg(not(feature = "tiny"))]
            {
                let locking = ISSET!(LOCKING);
                let view_mode = ISSET!(VIEW_MODE);
                if locking && !view_mode {
                    let w_ok = unsafe { libc::access(cpath.as_ptr(), libc::W_OK) };
                    if w_ok < 0 {
                        statusline(MessageType::Mild,
                            &format!("Directory '{}' is not writable", parentdir_str));
                    }
                }
            }

            true
        }
    }
}

// ---------------------------------------------------------------------------
// make_new_buffer — add an item to the circular list of openfile structs
// C: void make_new_buffer(void)
// ---------------------------------------------------------------------------
pub fn make_new_buffer() {
    with_state_mut(|s| {
        let mut newnode = Box::new(OpenFileStruct::default());

        #[cfg(feature = "multibuffer")]
        {
            // Link into circular list
            if s.openfile.is_none() {
                // first buffer — set startfile pointer
                s.startfile = Some(newnode.as_mut() as *mut OpenFileStruct);
            } else {
                // More than one buffer: show "Close" in help lines
                if let Some(idx) = s.exitfunc {
                    s.allfuncs[idx].tag = s.close_tag;
                }
                let not_in_help = !s.inhelp;
                if not_in_help || s.more_than_one {
                    s.more_than_one = true;
                }
            }
        }

        // Initialize fields
        newnode.filename = String::new();

        let filetop = make_new_node(None);
        filetop.borrow_mut().data = String::new();
        filetop.borrow_mut().lineno = 1;

        let filetop_clone = filetop.clone();
        newnode.filetop = Some(filetop);
        newnode.filebot = Some(filetop_clone.clone());
        newnode.current = Some(filetop_clone.clone());
        newnode.current_x = 0;
        newnode.placewewant = 0;
        newnode.brink = 0;
        newnode.cursor_row = 0;
        newnode.edittop = Some(filetop_clone);
        newnode.firstcolumn = 0;
        newnode.totsize = 0;
        newnode.modified = false;

        #[cfg(feature = "wrapping")]
        { newnode.spillage_line = None; }

        #[cfg(not(feature = "tiny"))]
        {
            newnode.mark = None;
            newnode.softmark = false;
            newnode.fmt = FormatType::Unspecified;
            newnode.undotop = None;
            newnode.current_undo = std::ptr::null_mut();
            newnode.last_saved = std::ptr::null_mut();
            newnode.last_action = UndoType::Other;
            newnode.statinfo = None;
            newnode.lock_filename = None;
        }

        #[cfg(feature = "multibuffer")]
        { newnode.errormessage = None; }

        #[cfg(feature = "color")]
        { newnode.syntax = None; }

        s.openfile = Some(newnode);
    });
}

// ---------------------------------------------------------------------------
// open_buffer — create or populate a buffer from a file
// C: bool open_buffer(const char *filename, bool new_one)
// ---------------------------------------------------------------------------
pub fn open_buffer_impl(filename: &str, new_one: bool) -> bool {
    // Display newlines in filenames as ^J
    with_state_mut(|s| s.as_an_at = false);

    #[cfg(feature = "operatingdir")]
    {
        let confined = with_state(|s| {
            s.operating_dir.as_deref()
                .map(|od| outside_of_confinement(filename, false))
                .unwrap_or(false)
        });
        if confined {
            let od = with_state(|s| s.operating_dir.clone().unwrap_or_default());
            statusline(MessageType::Alert,
                &format!("Can't read file from outside of {}", od));
            return false;
        }
    }

    let realname = expand_leading_tilde(filename);

    // Don't try to open directories, character files, or block files.
    if !filename.is_empty() {
        if let Ok(meta) = std::fs::metadata(&realname) {
            if meta.is_dir() {
                statusline(MessageType::Alert, &format!("\"{}\" is a directory", realname));
                return false;
            }
            // Check for block/char device (requires libc)
            use std::os::unix::fs::FileTypeExt;
            let ft = meta.file_type();
            if ft.is_char_device() || ft.is_block_device() {
                statusline(MessageType::Alert, &format!("\"{}\" is a device file", realname));
                return false;
            }
            #[cfg(feature = "tiny")]
            if ft.is_fifo() {
                statusline(MessageType::Alert, &format!("\"{}\" is a FIFO", realname));
                return false;
            }
            #[cfg(all(not(feature = "tiny"), unix))]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode();
                let euid = unsafe { libc::geteuid() };
                if new_one && (mode & 0o222) == 0 && euid == ROOT_UID {
                    statusline(MessageType::Alert,
                        &format!("{} is meant to be read-only", realname));
                }
            }
        }
    }

    if new_one {
        make_new_buffer();

        if has_valid_path(&realname) {
            #[cfg(not(feature = "tiny"))]
            {
                let do_locking = ISSET!(LOCKING);
                let view_mode = ISSET!(VIEW_MODE);
                if do_locking && !view_mode && !filename.is_empty() {
                    match do_lockfile(&realname, true) {
                        Err(()) => {
                            // SKIPTHISFILE
                            #[cfg(feature = "multibuffer")]
                            close_buffer_impl();
                            return false;
                        }
                        Ok(lock_fname) => {
                            with_state_mut(|s| {
                                if let Some(ref mut of) = s.openfile {
                                    of.lock_filename = lock_fname;
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    // If we have a filename and are not in NOREAD mode, open the file.
    let noread = ISSET!(NOREAD_MODE);
    let mut descriptor: i32 = 0;
    let mut file_handle: Option<File> = None;

    if !filename.is_empty() && !noread {
        descriptor = open_file_impl(&realname, new_one, &mut file_handle);
    }

    // If successfully opened an existing file, read it in.
    if descriptor > 0 {
        if let Some(f) = file_handle {
            install_handler_for_Ctrl_C();
            read_file_impl(f, descriptor, &realname, !new_one);
            restore_handler_for_Ctrl_C();

            #[cfg(not(feature = "tiny"))]
            {
                let has_stat = with_state(|s| {
                    s.openfile.as_ref().map(|of| of.statinfo.is_some()).unwrap_or(false)
                });
                if !has_stat {
                    let st = stat_with_alloc(&realname);
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.statinfo = st;
                        }
                    });
                }
            }
        }
    }

    // For a new buffer, store filename and put cursor at start of buffer.
    if descriptor >= 0 && new_one {
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.filename = realname.clone();
                let filetop = of.filetop.clone();
                of.current = filetop;
                of.current_x = 0;
                of.placewewant = 0;
            }
        });
    }

    #[cfg(feature = "color")]
    if new_one {
        find_and_prime_applicable_syntax();
    }

    true
}

// ---------------------------------------------------------------------------
// set_modified — mark the current buffer as modified
// C: void set_modified(void)
// ---------------------------------------------------------------------------
pub fn set_modified() {
    let already = with_state(|s| s.openfile.as_ref().map(|of| of.modified).unwrap_or(false));
    if already {
        return;
    }
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.modified = true;
        }
    });
    titlebar(None);

    #[cfg(not(feature = "tiny"))]
    {
        let (has_lock, lock_fname, buf_filename) = with_state(|s| {
            if let Some(ref of) = s.openfile {
                (
                    of.lock_filename.is_some(),
                    of.lock_filename.clone(),
                    of.filename.clone(),
                )
            } else {
                (false, None, String::new())
            }
        });
        if has_lock {
            if let (Some(lf), fname) = (lock_fname, buf_filename) {
                write_lockfile(&lf, &fname, true);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// prepare_for_display — update title bar and multiline cache
// C: void prepare_for_display(void)
// ---------------------------------------------------------------------------
pub fn prepare_for_display() {
    let inhelp = with_state(|s| s.inhelp);
    if !inhelp {
        titlebar(None);
    }

    #[cfg(feature = "color")]
    {
        let needs_precalc = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.filetop.as_ref())
                .map(|ft| ft.borrow().multidata.is_empty())
                .unwrap_or(false)
        });
        if needs_precalc {
            precalc_multicolorinfo();
        }
        with_state_mut(|s| s.have_palette = false);
    }
    with_state_mut(|s| s.refresh_needed = true);
}

// ---------------------------------------------------------------------------
// MULTIBUFFER functions
// ---------------------------------------------------------------------------

/* C: void mention_name_and_linecount(void)
 * Show name of current buffer and its number of lines on the status bar. */
#[cfg(feature = "multibuffer")]
pub fn mention_name_and_linecount() {
    let (count, filename) = with_state(|s| {
        if let Some(ref of) = s.openfile {
            let bot_lineno = of.filebot.as_ref().map(|b| b.borrow().lineno).unwrap_or(0);
            let bot_empty = of.filebot.as_ref()
                .map(|b| b.borrow().data.is_empty())
                .unwrap_or(true);
            let count = bot_lineno - (if bot_empty { 1 } else { 0 });
            (count as usize, of.filename.clone())
        } else {
            (0, String::new())
        }
    });

    #[cfg(not(feature = "tiny"))]
    {
        let minibar = ISSET!(MINIBAR);
        let zero = ISSET!(ZERO);
        if minibar {
            with_state_mut(|s| s.report_size = true);
            return;
        }
        if zero {
            return;
        }
        let fmt = with_state(|s| {
            s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
        });
        if fmt == FormatType::DosFile {
            let name = if filename.is_empty() { "New Buffer" } else { &filename };
            let msg = if count == 1 {
                format!("{} -- {} line ({})", name, count, "DOS")
            } else {
                format!("{} -- {} lines ({})", name, count, "DOS")
            };
            statusline(MessageType::Hush, &msg);
            return;
        }
    }

    let name = if filename.is_empty() { "New Buffer" } else { &filename };
    let msg = if count == 1 {
        format!("{} -- {} line", name, count)
    } else {
        format!("{} -- {} lines", name, count)
    };
    statusline(MessageType::Hush, &msg);
}

/* C: void redecorate_after_switch(void)
 * Update title bar and such after switching to another buffer. */
#[cfg(feature = "multibuffer")]
pub fn redecorate_after_switch() {
    // Check if only one file buffer is open (points to itself)
    // In Rust we track this differently via the openfile linked structure.
    // For now, we just proceed with the refresh.
    #[cfg(not(feature = "tiny"))]
    ensure_firstcolumn_is_aligned();

    prepare_for_display();

    with_state_mut(|s| {
        s.currmenu = MMOST;
        s.shift_held = true;
    });

    let (has_error, error_msg) = with_state(|s| {
        if let Some(ref of) = s.openfile {
            (of.errormessage.is_some(), of.errormessage.clone())
        } else {
            (false, None)
        }
    });

    if has_error {
        if let Some(msg) = error_msg {
            statusline(MessageType::Alert, &msg);
        }
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.errormessage = None;
            }
        });
    } else {
        mention_name_and_linecount();
    }
}

/* C: void switch_to_prev_buffer(void)
 * Switch to the previous entry in the circular list of buffers. */
#[cfg(feature = "multibuffer")]
pub fn switch_to_prev_buffer() {
    // NOTE: full circular list navigation requires Box<OpenFileStruct> with prev pointers.
    // This is a simplified stub that calls redecorate.
    redecorate_after_switch();
}

/* C: void switch_to_next_buffer(void)
 * Switch to the next entry in the circular list of buffers. */
#[cfg(feature = "multibuffer")]
pub fn switch_to_next_buffer() {
    redecorate_after_switch();
}

/* C: void close_buffer(void)
 * Remove the current buffer from the circular list of buffers. */
#[cfg(feature = "multibuffer")]
pub fn close_buffer_impl() {
    // Free resources of current buffer and switch to prev.
    with_state_mut(|s| {
        // Drop the current openfile.
        // In a full implementation this would navigate the circular list.
        s.openfile = None;
    });
}

// ---------------------------------------------------------------------------
// encode_data — encode NUL bytes in a buffer as LF bytes
// C: char *encode_data(char *text, size_t length)
// ---------------------------------------------------------------------------
pub fn encode_data(buf: &[u8]) -> String {
    // Replace NUL bytes with LF (0x0A) as in the C version's recode_NUL_to_LF
    let recoded: Vec<u8> = buf.iter().map(|&b| if b == 0 { b'\n' } else { b }).collect();
    String::from_utf8_lossy(&recoded).into_owned()
}

// ---------------------------------------------------------------------------
// read_file_impl — read an open file into the current buffer
// C: void read_file(FILE *f, int fd, const char *filename, bool undoable)
// ---------------------------------------------------------------------------
pub fn read_file_impl(mut f: File, fd: i32, filename: &str, undoable: bool) {
    let was_lineno = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|of| of.current.as_ref())
            .map(|c| c.borrow().lineno)
            .unwrap_or(1)
    });

    let was_leftedge: usize;
    #[cfg(not(feature = "tiny"))]
    {
        was_leftedge = if ISSET!(SOFTWRAP) {
            let cur = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.current.clone())
            });
            leftedge_for(xplustabs(), cur.as_ref())
        } else {
            0
        };
        if undoable {
            add_undo(UndoType::Insert, None);
        }
    }
    #[cfg(feature = "tiny")]
    { let was_leftedge: usize = 0; }

    let topline = make_new_node(None);
    topline.borrow_mut().lineno = 1;
    let mut bottomline = topline.clone();
    let mut num_lines: usize = 0;

    // Read the file byte by byte
    let mut buf: Vec<u8> = Vec::with_capacity(LUMPSIZE);
    let mut error_occurred = false;
    let mut error_msg = String::new();
    let mut interrupted = false;

    #[cfg(not(feature = "tiny"))]
    block_sigwinch(true);

    with_state_mut(|s| s.control_C_was_pressed = false);

    // Read the entire file contents
    let mut content = Vec::new();
    match f.read_to_end(&mut content) {
        Ok(_) => {}
        Err(e) => {
            error_occurred = true;
            error_msg = e.to_string();
        }
    }

    #[cfg(not(feature = "tiny"))]
    block_sigwinch(false);

    #[cfg(not(feature = "tiny"))]
    {
        if isendwin() {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                reconnect_and_store_state();
            }
            terminal_init();
            doupdate();
        }
    }

    if error_occurred {
        statusline(MessageType::Alert, &error_msg);
    }

    let ctrl_c = with_state(|s| s.control_C_was_pressed);
    if ctrl_c {
        statusline(MessageType::Alert, "Interrupted");
    }

    // Check writability
    let writable = if fd > 0 && !undoable && !ISSET!(VIEW_MODE) {
        let cname = std::ffi::CString::new(filename).unwrap_or_default();
        let access_ret = unsafe { libc::access(cname.as_ptr(), libc::W_OK) };
        access_ret == 0
    } else {
        true
    };

    // Parse the content into lines
    let mut line_buf: Vec<u8> = Vec::with_capacity(LUMPSIZE);
    #[cfg(not(feature = "tiny"))]
    let mut format = FormatType::NixFile;

    for &byte in &content {
        if with_state(|s| s.control_C_was_pressed) {
            break;
        }

        if byte == b'\n' {
            #[cfg(not(feature = "tiny"))]
            {
                // Strip CR before LF when not in NO_CONVERT mode
                if !line_buf.is_empty() && *line_buf.last().unwrap() == b'\r' && !ISSET!(NO_CONVERT) {
                    if num_lines == 0 {
                        format = FormatType::DosFile;
                    }
                    line_buf.pop();
                }
            }
            // Store the line
            let linedata = encode_data(&line_buf);
            bottomline.borrow_mut().data = linedata;

            // Make a new node for next line
            let newline = make_new_node(Some(bottomline.clone()));
            newline.borrow_mut().lineno = (num_lines + 2) as isize;
            bottomline.borrow_mut().next = Some(newline.clone());
            bottomline = newline;
            num_lines += 1;
            line_buf.clear();
        } else {
            line_buf.push(byte);
        }
    }

    // Handle last line (which may or may not end with newline)
    if line_buf.is_empty() {
        bottomline.borrow_mut().data = String::new();
    } else {
        bottomline.borrow_mut().data = encode_data(&line_buf);
        num_lines += 1;
    }

    // Insert the read buffer into the current buffer
    ingraft_buffer(topline);

    // Set the desired x position at the end of what was inserted
    let xpt = xplustabs();
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.placewewant = xpt;
        }
    });

    if !writable {
        statusline(MessageType::Alert, &format!("File '{}' is unwritable", filename));
    } else {
        let zero = ISSET!(ZERO);
        let minibar = ISSET!(MINIBAR);
        let we_are_running = with_state(|s| s.we_are_running);
        if (zero || minibar) && !(we_are_running && undoable) {
            // No blurb for new buffers with --zero or --mini
        } else {
            #[cfg(not(feature = "tiny"))]
            if format == FormatType::DosFile {
                let msg = if num_lines == 1 {
                    format!("Read {} line (converted from DOS format)", num_lines)
                } else {
                    format!("Read {} lines (converted from DOS format)", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            } else {
                let msg = if num_lines == 1 {
                    format!("Read {} line", num_lines)
                } else {
                    format!("Read {} lines", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            }
            #[cfg(feature = "tiny")]
            {
                let msg = if num_lines == 1 {
                    format!("Read {} line", num_lines)
                } else {
                    format!("Read {} lines", num_lines)
                };
                statusline(MessageType::Remark, &msg);
            }
        }
    }

    with_state_mut(|s| s.report_size = true);

    // If we inserted less than a screenful, don't center the cursor.
    if undoable && less_than_a_screenful(was_lineno, was_leftedge) {
        with_state_mut(|s| s.focusing = false);
        #[cfg(feature = "color")]
        with_state_mut(|s| s.perturbed = true);
    } else if undoable {
        #[cfg(feature = "color")]
        with_state_mut(|s| s.recook = true);
    }

    #[cfg(not(feature = "tiny"))]
    {
        if undoable {
            update_undo(UndoType::Insert);
        }
        let make_it_unix = ISSET!(MAKE_IT_UNIX);
        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                if make_it_unix {
                    of.fmt = FormatType::NixFile;
                } else if of.fmt == FormatType::Unspecified {
                    of.fmt = format;
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// open_file_impl — open the file with the given name
// C: int open_file(const char *filename, bool new_one, FILE **f)
// Returns 0=new file, -1=failure, or the file descriptor on success.
// ---------------------------------------------------------------------------
pub fn open_file_impl(filename: &str, new_one: bool, out_file: &mut Option<File>) -> i32 {
    let full_filename = get_full_path(filename);

    // Resolve path, falling back to given path if realpath fails
    let resolved = {
        let candidate = full_filename.as_deref().unwrap_or(filename);
        if std::fs::metadata(candidate).is_ok() {
            candidate.to_string()
        } else {
            filename.to_string()
        }
    };

    if std::fs::metadata(&resolved).is_err() {
        if new_one {
            statusline(MessageType::Remark, "New File");
            return 0;
        } else {
            statusline(MessageType::Alert, &format!("File \"{}\" not found", filename));
            return -1;
        }
    }

    // Check if it's a FIFO
    #[cfg(not(feature = "tiny"))]
    {
        use std::os::unix::fs::FileTypeExt;
        if let Ok(meta) = std::fs::metadata(&resolved) {
            if meta.file_type().is_fifo() {
                statusbar("Reading from FIFO...");
            }
        }
        block_sigwinch(true);
        install_handler_for_Ctrl_C();
    }

    // Open the file
    let open_result = File::open(&resolved);

    #[cfg(not(feature = "tiny"))]
    {
        restore_handler_for_Ctrl_C();
        block_sigwinch(false);
    }

    match open_result {
        Err(e) => {
            let err_kind = e.kind();
            if err_kind == io::ErrorKind::Interrupted {
                statusline(MessageType::Alert, "Interrupted");
            } else {
                statusline(MessageType::Alert,
                    &format!("Error reading {}: {}", filename, e));
            }
            -1
        }
        Ok(f) => {
            // Get the file descriptor
            use std::os::unix::io::AsRawFd;
            let fd = f.as_raw_fd();

            let zero = ISSET!(ZERO);
            let we_are_running = with_state(|s| s.we_are_running);
            if !zero || we_are_running {
                statusbar("Reading...");
            }

            // Store file handle; we use fd 1 to signal success (actual fd varies)
            *out_file = Some(f);
            1  // signal success; caller uses the file handle
        }
    }
}

// ---------------------------------------------------------------------------
// get_next_filename — return the first available extension of a filename
// C: char *get_next_filename(const char *name, const char *suffix)
// ---------------------------------------------------------------------------
pub fn get_next_filename(name: &str, suffix: &str) -> String {
    let base = format!("{}{}", name, suffix);

    if !Path::new(&base).exists() {
        return base;
    }

    for i in 1u64..100_000 {
        let candidate = format!("{}.{}", base, i);
        if !Path::new(&candidate).exists() {
            return candidate;
        }
    }

    // No possible save file
    String::new()
}

// ---------------------------------------------------------------------------
// Process / fork / pipe support (!NANO_TINY)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "tiny"))]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(not(feature = "tiny"))]
static PID_OF_COMMAND: AtomicI32 = AtomicI32::new(-1);
#[cfg(not(feature = "tiny"))]
static PID_OF_SENDER: AtomicI32 = AtomicI32::new(-1);
#[cfg(not(feature = "tiny"))]
static SHOULD_PIPE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/* C: void cancel_the_command(int signal)
 * Send an unconditional kill signal to the running external command. */
#[cfg(not(feature = "tiny"))]
pub fn cancel_the_command(signal: i32) {
    let pid_cmd = PID_OF_COMMAND.load(Ordering::SeqCst);
    let pid_snd = PID_OF_SENDER.load(Ordering::SeqCst);
    let piping = SHOULD_PIPE.load(Ordering::SeqCst);

    if pid_cmd > 0 {
        unsafe { libc::kill(pid_cmd, libc::SIGKILL); }
    }
    if piping && pid_snd > 0 {
        unsafe { libc::kill(pid_snd, libc::SIGKILL); }
    }
}

/* C: void send_data(const linestruct *line, int fd)
 * Send the text that starts at the given line to file descriptor fd. */
#[cfg(not(feature = "tiny"))]
pub fn send_data(line: Option<LinePtr>, fd: i32) {
    use std::os::unix::io::FromRawFd;
    let mut tube = unsafe { File::from_raw_fd(fd) };

    let mut current = line;
    while let Some(node) = current {
        let (data, has_next) = {
            let b = node.borrow();
            (b.data.clone(), b.next.is_some())
        };

        // Don't write a final empty line
        let next_node = node.borrow().next.clone();
        if next_node.is_none() && data.is_empty() {
            break;
        }

        // Recode LF as NUL before writing
        let recoded: Vec<u8> = data.bytes().map(|b| if b == b'\n' { 0 } else { b }).collect();
        if tube.write_all(&recoded).is_err() {
            std::process::exit(5);
        }

        if has_next {
            if tube.write_all(b"\n").is_err() {
                std::process::exit(6);
            }
        }

        current = node.borrow().next.clone();
    }
    // Don't close here — the fd will be closed by the OS when the process exits
    std::mem::forget(tube);
}

/* C: void execute_command(const char *command)
 * Execute the given command in a shell. */
#[cfg(not(feature = "tiny"))]
pub fn execute_command(command: &str) {
    use std::process::{Command, Stdio};
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let should_pipe = command.starts_with('|');
    let capture_output = !(should_pipe && command.len() > 1 && command.chars().nth(1) == Some('|'));

    SHOULD_PIPE.store(should_pipe, Ordering::SeqCst);

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    // The actual command string passed to the shell
    let cmd_str = if should_pipe {
        if capture_output { &command[1..] } else { &command[2..] }
    } else {
        command
    };

    // Create from_fd pipe (output from command)
    let (from_read_fd, from_write_fd) = {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            statusline(MessageType::Alert,
                &format!("Could not create pipe: {}", io::Error::last_os_error()));
            return;
        }
        (fds[0], fds[1])
    };

    // Create to_fd pipe (input to command) if piping
    let (to_read_fd, to_write_fd) = if should_pipe {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            statusline(MessageType::Alert,
                &format!("Could not create pipe: {}", io::Error::last_os_error()));
            unsafe { libc::close(from_read_fd); libc::close(from_write_fd); }
            return;
        }
        (fds[0], fds[1])
    } else {
        (-1, -1)
    };

    // Fork the child process
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Child process
        unsafe {
            libc::close(from_read_fd);
            if capture_output {
                libc::dup2(from_write_fd, libc::STDOUT_FILENO);
            }
            libc::dup2(from_write_fd, libc::STDERR_FILENO);
            libc::close(from_write_fd);

            if should_pipe {
                libc::dup2(to_read_fd, libc::STDIN_FILENO);
                libc::close(to_read_fd);
                libc::close(to_write_fd);
            }

            let shell_cstr = std::ffi::CString::new(shell.as_str()).unwrap();
            let shell_tail = std::ffi::CString::new(
                crate::utils::tail(&shell)).unwrap();
            let minus_c = std::ffi::CString::new("-c").unwrap();
            let cmd_cstr = std::ffi::CString::new(cmd_str).unwrap();

            libc::execl(
                shell_cstr.as_ptr(),
                shell_tail.as_ptr(),
                minus_c.as_ptr(),
                cmd_cstr.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
            libc::_exit(6);
        }
    }

    // Parent
    unsafe { libc::close(from_write_fd); }

    if pid < 0 {
        statusline(MessageType::Alert,
            &format!("Could not fork: {}", io::Error::last_os_error()));
        unsafe { libc::close(from_read_fd); }
        if should_pipe {
            unsafe { libc::close(to_read_fd); libc::close(to_write_fd); }
        }
        return;
    }

    PID_OF_COMMAND.store(pid, Ordering::SeqCst);
    statusbar("Executing...");

    // If the command starts with "|", pipe buffer or region to the command.
    let pid_sender;
    if should_pipe {
        // Get the lines to pipe
        let lines_to_send = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.filetop.clone())
        });

        pid_sender = unsafe { libc::fork() };
        if pid_sender == 0 {
            // Child sender process
            unsafe { libc::close(to_read_fd); }
            send_data(lines_to_send, to_write_fd);
            unsafe { libc::_exit(0); }
        }

        if pid_sender < 0 {
            statusline(MessageType::Alert,
                &format!("Could not fork: {}", io::Error::last_os_error()));
        }

        PID_OF_SENDER.store(pid_sender, Ordering::SeqCst);
        unsafe { libc::close(to_read_fd); libc::close(to_write_fd); }
    } else {
        pid_sender = -1;
    }

    // Set up signal handler
    enable_kb_interrupt();

    // Read command output
    let stream = unsafe { File::from_raw_fd(from_read_fd) };
    read_file_impl(stream, 0, "pipe", true);

    // Wait for processes
    let mut command_status: i32 = 0;
    unsafe { libc::waitpid(pid, &mut command_status, 0); }

    let mut sender_status: i32 = 0;
    if should_pipe && pid_sender > 0 {
        unsafe { libc::waitpid(pid_sender, &mut sender_status, 0); }
    }

    // Check exit status
    let cmd_ok = unsafe {
        libc::WIFEXITED(command_status) && libc::WEXITSTATUS(command_status) == 0
    };
    let cmd_signaled = unsafe { libc::WIFSIGNALED(command_status) };

    if !cmd_ok {
        if cmd_signaled {
            statusline(MessageType::Alert, "Cancelled");
        } else {
            // Try to extract error message from last line
            let err_detail = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .and_then(|c| {
                        let b = c.borrow();
                        b.prev.as_ref()
                            .and_then(|pw| pw.upgrade())
                            .map(|prev| {
                                let pb = prev.borrow();
                                if let Some(pos) = pb.data.find(": ") {
                                    pb.data[pos + 2..].to_string()
                                } else {
                                    "---".to_string()
                                }
                            })
                    })
                    .unwrap_or_else(|| "---".to_string())
            });
            statusline(MessageType::Alert, &format!("Error: {}", err_detail));
        }
    } else if should_pipe && pid_sender > 0 {
        let sender_ok = unsafe {
            libc::WIFEXITED(sender_status) && libc::WEXITSTATUS(sender_status) == 0
        };
        if !sender_ok {
            statusline(MessageType::Alert, "Piping failed");
        }
    }

    // If there was an error, undo and discard what the command did.
    let last_msg = with_state(|s| s.lastmessage);
    if last_msg == MessageType::Alert {
        do_undo();
        let current_undo = with_state(|s| {
            s.openfile.as_ref().map(|of| of.current_undo).unwrap_or(std::ptr::null_mut())
        });
        discard_until(current_undo);
    }

    terminal_init();
}

// ---------------------------------------------------------------------------
// insert_a_file_or — insert a file or execute a command
// C: void insert_a_file_or(bool execute)
// ---------------------------------------------------------------------------
pub fn insert_a_file_or(execute: bool) {
    let mut execute = execute;
    let mut response: i32;
    let mut given = String::new();

    #[cfg(feature = "multibuffer")]
    let was_multibuffer = ISSET!(MULTIBUFFER);

    with_state_mut(|s| s.as_an_at = false);
    with_state_mut(|s| s.ran_a_tool = false);

    #[cfg(not(feature = "tiny"))]
    {
        if execute {
            let foretext = with_state(|s| s.foretext.clone().unwrap_or_default());
            if !foretext.is_empty() {
                given = foretext;
            }
        }
    }

    loop {
        let msg: &str;

        #[cfg(not(feature = "tiny"))]
        if execute {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                msg = "Command to execute in new buffer";
            } else {
                msg = "Command to execute";
            }
            #[cfg(not(feature = "multibuffer"))]
            { msg = "Command to execute"; }
        } else {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to read unconverted into new buffer [from %s]";
                } else {
                    msg = "File to read into new buffer [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to read into new buffer [from %s]"; }
            } else {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to insert unconverted [from %s]";
                } else {
                    msg = "File to insert [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to insert [from %s]"; }
            }
            #[cfg(not(feature = "multibuffer"))]
            {
                #[cfg(not(feature = "tiny"))]
                if ISSET!(NO_CONVERT) {
                    msg = "File to insert unconverted [from %s]";
                } else {
                    msg = "File to insert [from %s]";
                }
                #[cfg(feature = "tiny")]
                { msg = "File to insert [from %s]"; }
            }
        }
        #[cfg(feature = "tiny")]
        { msg = "File to insert [from %s]"; }

        with_state_mut(|s| s.present_path = Some("./".to_string()));

        let menu = if execute { MEXECUTE } else { MINSERTFILE };
        let operating_dir_str = with_state(|s| {
            #[cfg(feature = "operatingdir")]
            { s.operating_dir.clone() }
            #[cfg(not(feature = "operatingdir"))]
            { None::<String> }
        });

        let prompt_default = operating_dir_str.as_deref().unwrap_or("./");
        let mut exec_hist = with_state(|s| s.execute_history.clone());

        response = do_prompt(
            menu,
            &given,
            if execute { Some(&mut exec_hist) } else { None },
            edit_refresh,
            msg,
            prompt_default,
        );

        let ran = with_state(|s| s.ran_a_tool);
        let multibuf = ISSET!(MULTIBUFFER);

        if response == -1 || (response == -2 && !multibuf) {
            statusbar("Cancelled");
            break;
        }

        let was_lineno = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|of| of.current.as_ref())
                .map(|c| c.borrow().lineno)
                .unwrap_or(0)
        });
        let was_x = with_state(|s| {
            s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
        });

        let answer = with_state(|s| s.answer.clone());
        given = answer.clone();

        if ran {
            break;
        }

        let function = func_from_key(response);

        // Handle function keys
        #[cfg(feature = "multibuffer")]
        if function == Some(crate::global::flip_newbuffer as FuncPtr) {
            if !ISSET!(VIEW_MODE) {
                TOGGLE!(MULTIBUFFER);
            } else {
                beep();
            }
            continue;
        }

        #[cfg(not(feature = "tiny"))]
        {
            if function == Some(crate::global::flip_convert as FuncPtr) {
                TOGGLE!(NO_CONVERT);
                continue;
            }
            if function == Some(crate::global::flip_execute as FuncPtr) {
                execute = !execute;
                continue;
            }
            if function == Some(crate::global::flip_pipe as FuncPtr) {
                add_or_remove_pipe_symbol_from_answer();
                let new_answer = with_state(|s| s.answer.clone());
                given = new_answer;
                continue;
            }
        }

        #[cfg(feature = "browser")]
        {
            if function == Some(crate::global::to_files as FuncPtr) {
                if let Some(chosen) = browse_in(&answer) {
                    with_state_mut(|s| s.answer = chosen.clone());
                    response = 0;
                } else {
                    continue;
                }
            }
        }

        // If we don't have a file yet, go back to the prompt.
        let multibuf2 = ISSET!(MULTIBUFFER);
        if response != 0 && (!multibuf2 || response != -2) {
            continue;
        }

        let final_answer = with_state(|s| s.answer.clone());

        #[cfg(not(feature = "tiny"))]
        if execute {
            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                open_buffer_impl("", true);
            }

            if !final_answer.is_empty() {
                execute_command(&final_answer);
                #[cfg(feature = "histories")]
                {
                    let mut exec_hist_local = with_state(|s| s.execute_history.clone());
                    update_history(&mut exec_hist_local, &final_answer, PRUNE_DUPLICATE);
                    with_state_mut(|s| s.execute_history = exec_hist_local);
                }
            }

            #[cfg(feature = "multibuffer")]
            if ISSET!(MULTIBUFFER) {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        let filetop = of.filetop.clone();
                        of.current = filetop;
                        of.current_x = 0;
                        of.placewewant = 0;
                    }
                });
                set_modified();
            }
        } else {
            open_buffer_impl(&final_answer, ISSET!(MULTIBUFFER));
        }

        #[cfg(feature = "multibuffer")]
        if ISSET!(MULTIBUFFER) {
            #[cfg(feature = "histories")]
            if ISSET!(POSITIONLOG) {
                #[cfg(not(feature = "tiny"))]
                if !execute {
                    restore_cursor_position_if_any();
                }
                #[cfg(feature = "tiny")]
                restore_cursor_position_if_any();
            }
            prepare_for_display();
        } else {
            let cur_lineno = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| of.current.as_ref())
                    .map(|c| c.borrow().lineno)
                    .unwrap_or(0)
            });
            let cur_x = with_state(|s| {
                s.openfile.as_ref().map(|of| of.current_x).unwrap_or(0)
            });
            if cur_lineno != was_lineno || cur_x != was_x {
                set_modified();
            }
            with_state_mut(|s| s.refresh_needed = true);
        }

        break;
    }

    #[cfg(feature = "multibuffer")]
    if was_multibuffer { SET!(MULTIBUFFER); } else { UNSET!(MULTIBUFFER); }
}

/* C: void do_insertfile(void)
 * If the current mode of operation allows it, go insert a file. */
pub fn do_insertfile() {
    if !in_restricted_mode() {
        insert_a_file_or(false);
    }
}

/* C: void do_execute(void)
 * If the current mode of operation allows it, go prompt for a command. */
#[cfg(not(feature = "tiny"))]
pub fn do_execute() {
    if !in_restricted_mode() {
        insert_a_file_or(true);
    }
}

// ---------------------------------------------------------------------------
// get_full_path — return the canonical absolute path
// C: char *get_full_path(const char *origpath)
// ---------------------------------------------------------------------------
pub fn get_full_path(origpath: &str) -> Option<String> {
    if origpath.is_empty() {
        return None;
    }

    let untilded = expand_leading_tilde(origpath);
    let path = Path::new(&untilded);

    // Try canonicalize
    match std::fs::canonicalize(path) {
        Ok(canonical) => {
            let mut s = canonical.to_string_lossy().into_owned();
            // Ensure non-apex directory paths end with slash
            if let Ok(meta) = std::fs::metadata(&s) {
                if meta.is_dir() && !s.ends_with('/') && s.len() > 1 {
                    s.push('/');
                }
            }
            Some(s)
        }
        Err(_) => {
            // Try without the last component (file may not exist yet)
            let parent = path.parent()?;
            let filename = path.file_name()?;

            if filename.is_empty() {
                return None;
            }

            match std::fs::canonicalize(parent) {
                Ok(canonical_parent) => {
                    let mut s = canonical_parent.to_string_lossy().into_owned();
                    if !s.ends_with('/') {
                        s.push('/');
                    }
                    s.push_str(&filename.to_string_lossy());
                    Some(s)
                }
                Err(_) => None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// check_writable_directory — verify path is a writable directory
// C: char *check_writable_directory(const char *path)
// ---------------------------------------------------------------------------
pub fn check_writable_directory(path: &str) -> Option<String> {
    let full_path = get_full_path(path)?;

    if !full_path.ends_with('/') {
        return None;
    }

    let cpath = std::ffi::CString::new(full_path.as_str()).ok()?;
    let ret = unsafe { libc::access(cpath.as_ptr(), libc::W_OK) };
    if ret != 0 {
        return None;
    }

    Some(full_path)
}

// ---------------------------------------------------------------------------
// safe_tempfile — create a temporary file
// C: char *safe_tempfile(FILE **stream)
// Returns (path, File) on success, None on failure.
// ---------------------------------------------------------------------------
pub fn safe_tempfile() -> Option<(String, File)> {
    let env_dir = std::env::var("TMPDIR").ok();

    // P_tmpdir is typically "/tmp" on Linux but is a C macro, not in libc crate.
    // Use the standard fallback directly.
    let tempdir = env_dir.as_deref()
        .and_then(check_writable_directory)
        .or_else(|| check_writable_directory("/tmp"))
        .unwrap_or_else(|| "/tmp/".to_string());

    // Get extension from current filename
    let extension = with_state(|s| {
        s.openfile.as_ref().map(|of| {
            let fname = &of.filename;
            if let Some(dot_pos) = fname.rfind('.') {
                // Only use the extension if there's no slash after the dot
                let ext = &fname[dot_pos..];
                if !ext.contains('/') {
                    ext.to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }).unwrap_or_default()
    });

    // Build template: <tempdir>nano.XXXXXX<ext>
    let template = format!("{}nano.XXXXXX{}", tempdir, extension);

    // Use mkstemp equivalent via tempfile crate pattern (manual approach)
    let template_cstr = std::ffi::CString::new(template.clone()).ok()?;
    let mut template_bytes = template_cstr.into_bytes_with_nul();

    let fd = unsafe {
        libc::mkstemps(
            template_bytes.as_mut_ptr() as *mut libc::c_char,
            extension.len() as libc::c_int,
        )
    };

    if fd < 0 {
        return None;
    }

    // Get the actual filename
    let tempfile_name = {
        let end = template_bytes.iter().position(|&b| b == 0).unwrap_or(template_bytes.len());
        String::from_utf8_lossy(&template_bytes[..end]).into_owned()
    };

    let file = unsafe {
        use std::os::unix::io::FromRawFd;
        File::from_raw_fd(fd)
    };

    Some((tempfile_name, file))
}

// ---------------------------------------------------------------------------
// init_operating_dir — change to the operating directory
// C: void init_operating_dir(void)
// ---------------------------------------------------------------------------
#[cfg(feature = "operatingdir")]
pub fn init_operating_dir() {
    let od = with_state(|s| s.operating_dir.clone()).unwrap_or_default();
    let target = get_full_path(&od);

    match target {
        None => {
            eprintln!("Invalid operating directory: {}", od);
            std::process::exit(1);
        }
        Some(t) => {
            if std::env::set_current_dir(&t).is_err() {
                eprintln!("Invalid operating directory: {}", od);
                std::process::exit(1);
            }
            with_state_mut(|s| s.operating_dir = Some(t));
        }
    }
}

/* C: bool outside_of_confinement(const char *somepath, bool tabbing)
 * Check whether the given path is outside of the operating directory. */
#[cfg(feature = "operatingdir")]
pub fn outside_of_confinement(somepath: &str, tabbing: bool) -> bool {
    let fullpath = match get_full_path(somepath) {
        None => return tabbing,
        Some(p) => p,
    };

    let operating_dir = with_state(|s| s.operating_dir.clone().unwrap_or_default());

    let is_inside = fullpath.starts_with(&operating_dir);
    let begins_to_be = tabbing && operating_dir.starts_with(&fullpath);

    !is_inside && !begins_to_be
}

// ---------------------------------------------------------------------------
// init_backup_dir (!NANO_TINY)
// C: void init_backup_dir(void)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
pub fn init_backup_dir() {
    let bd = with_state(|s| s.backup_dir.clone()).unwrap_or_default();
    let target = get_full_path(&bd);

    match target {
        None => {
            eprintln!("Invalid backup directory: {}", bd);
            std::process::exit(1);
        }
        Some(t) => {
            if !t.ends_with('/') {
                eprintln!("Invalid backup directory: {}", bd);
                std::process::exit(1);
            }
            with_state_mut(|s| s.backup_dir = Some(t));
        }
    }
}

// ---------------------------------------------------------------------------
// copy_file — copy all data from `inn` to `out`
// C: int copy_file(FILE *inn, FILE *out, bool close_out)
// Returns 0 on success, negative on read error, positive on write error.
// ---------------------------------------------------------------------------
pub fn copy_file(mut inn: File, mut out: File, close_out: bool) -> i32 {
    let mut buf = [0u8; 8192];
    let mut retval = 0i32;

    loop {
        let n = match inn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => { retval = -1; break; }
        };
        match out.write_all(&buf[..n]) {
            Ok(_) => {}
            Err(_) => { retval = 2; break; }
        }
    }

    // Always close inn
    drop(inn);

    if retval != 0 {
        if !close_out { let _ = out.flush(); }
        drop(out);
        return retval;
    }

    if close_out {
        if out.flush().is_err() { retval = 4; }
        drop(out);
    } else {
        if out.flush().is_err() { retval = 4; }
    }

    retval
}

// ---------------------------------------------------------------------------
// make_backup_of (!NANO_TINY) — create a backup copy of the file
// C: bool make_backup_of(char *realname, struct stat fileinfo)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
pub fn make_backup_of(realname: &str, fileinfo: &libc::stat) -> bool {
    statusbar("Making backup...");

    let backup_dir = with_state(|s| s.backup_dir.clone());

    let backupname: String = if backup_dir.is_none() {
        format!("{}~", realname)
    } else {
        let bd = backup_dir.as_ref().unwrap();
        let thename = match get_full_path(realname) {
            Some(full) => full.replace('/', "!"),
            None => crate::utils::tail(realname).to_string(),
        };
        let base = format!("{}{}", bd, thename);
        let next = get_next_filename(&base, "~");
        if next.is_empty() {
            statusline(MessageType::Alert, "Too many existing backup files");
            return false;
        }
        next
    };

    // Remove existing backup
    let _ = std::fs::remove_file(&backupname);

    // Create backup file
    let create_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&backupname);

    let backup_file = match create_result {
        Err(e) => {
            warn_and_briefly_pause("Cannot make backup");
            warn_and_briefly_pause(&e.to_string());
            let err_msg = e.to_string();
            if ask_user(YESORNO,
                &format!("Cannot make backup; continue and save actual file? ")) == YES
            {
                return true;
            }
            statusline(MessageType::Hush,
                &format!("Cannot make backup: {}", err_msg));
            return false;
        }
        Ok(f) => f,
    };

    // Set permissions to match original
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = backup_file.as_raw_fd();
        unsafe {
            libc::fchown(fd, fileinfo.st_uid, fileinfo.st_gid);
            libc::fchmod(fd, fileinfo.st_mode);
        }
    }

    // Open original and copy to backup
    let original = match File::open(realname) {
        Err(_) => {
            warn_and_briefly_pause("Cannot read original file");
            drop(backup_file);
            let _ = std::fs::remove_file(&backupname);
            if ask_user(YESORNO,
                "Cannot make backup; continue and save actual file? ") == YES
            {
                return true;
            }
            statusline(MessageType::Hush, "Cannot make backup");
            return false;
        }
        Ok(f) => f,
    };

    let verdict = copy_file(original, backup_file, false);
    if verdict < 0 {
        warn_and_briefly_pause("Cannot read original file");
        let _ = std::fs::remove_file(&backupname);
        if ask_user(YESORNO,
            "Cannot make backup; continue and save actual file? ") == YES
        {
            return true;
        }
        return false;
    } else if verdict > 0 {
        warn_and_briefly_pause("Cannot write backup file");
        let _ = std::fs::remove_file(&backupname);
        if ask_user(YESORNO,
            "Cannot make backup; continue and save actual file? ") == YES
        {
            return true;
        }
        return false;
    }

    // Set timestamps to match original
    #[cfg(unix)]
    unsafe {
        let times = [
            libc::timespec { tv_sec: fileinfo.st_atime, tv_nsec: fileinfo.st_atime_nsec },
            libc::timespec { tv_sec: fileinfo.st_mtime, tv_nsec: fileinfo.st_mtime_nsec },
        ];
        // Reopen backup to set times via futimens
        if let Ok(bf) = File::open(&backupname) {
            use std::os::unix::io::AsRawFd;
            libc::futimens(bf.as_raw_fd(), times.as_ptr());
        }
    }

    true
}

// ---------------------------------------------------------------------------
// write_file — write the current buffer to disk
// C: bool write_file(const char *name, FILE *thefile, bool normal,
//                    kind_of_writing_type method, bool annotate)
// ---------------------------------------------------------------------------
pub fn write_file(
    name: &str,
    thefile: Option<File>,
    normal: bool,
    method: KindOfWritingType,
    annotate: bool,
) -> bool {
    let realname = expand_leading_tilde(name);

    #[cfg(feature = "operatingdir")]
    if normal {
        let confined = with_state(|s| {
            s.operating_dir.as_deref()
                .map(|_od| outside_of_confinement(&realname, false))
                .unwrap_or(false)
        });
        if confined {
            let od = with_state(|s| s.operating_dir.clone().unwrap_or_default());
            statusline(MessageType::Alert, &format!("Can't write outside of {}", od));
            return false;
        }
    }

    let mut tempname: Option<String> = None;
    let mut lineswritten: usize = 0;

    #[cfg(not(feature = "tiny"))]
    let is_existing_file = {
        normal && std::fs::metadata(&realname).is_ok()
    };

    // Make backup if needed
    #[cfg(not(feature = "tiny"))]
    if ISSET!(MAKE_BACKUP) && is_existing_file {
        // Check it's not a FIFO
        use std::os::unix::fs::FileTypeExt;
        let is_fifo = std::fs::metadata(&realname)
            .map(|m| m.file_type().is_fifo())
            .unwrap_or(false);
        if !is_fifo {
            if let Some(st) = stat_with_alloc(&realname) {
                if !make_backup_of(&realname, &st) {
                    return false;
                }
            }
        }
    }

    // When prepending, first copy existing file to a temporary file
    #[cfg(not(feature = "tiny"))]
    if method == KindOfWritingType::Prepend {
        use std::os::unix::fs::FileTypeExt;
        let is_fifo = std::fs::metadata(&realname)
            .map(|m| m.file_type().is_fifo())
            .unwrap_or(false);
        if is_fifo {
            statusline(MessageType::Alert, &format!("Error writing {}: FIFO", realname));
            return false;
        }

        let source = match File::open(&realname) {
            Err(e) => {
                statusline(MessageType::Alert,
                    &format!("Error reading {}: {}", realname, e));
                return false;
            }
            Ok(f) => f,
        };

        match safe_tempfile() {
            None => {
                statusline(MessageType::Alert,
                    &format!("Error writing temp file: {}", io::Error::last_os_error()));
                drop(source);
                return false;
            }
            Some((tname, target)) => {
                let verdict = copy_file(source, target, true);
                if verdict < 0 {
                    statusline(MessageType::Alert,
                        &format!("Error reading {}: {}", realname, io::Error::last_os_error()));
                    let _ = std::fs::remove_file(&tname);
                    return false;
                } else if verdict > 0 {
                    statusline(MessageType::Alert,
                        &format!("Error writing temp file: {}", io::Error::last_os_error()));
                    let _ = std::fs::remove_file(&tname);
                    return false;
                }
                tempname = Some(tname);
            }
        }
    }

    #[cfg(not(feature = "tiny"))]
    {
        use std::os::unix::fs::FileTypeExt;
        let is_fifo = std::fs::metadata(&realname)
            .map(|m| m.file_type().is_fifo())
            .unwrap_or(false);
        if is_existing_file && is_fifo {
            statusbar("Writing to FIFO...");
        }
    }

    // Open / create the file when not writing to a temp file
    let mut the_file: File = match thefile {
        Some(f) => f,
        None => {
            let permissions: u32 = if normal { 0o666 } else { 0o600 };

            #[cfg(not(feature = "tiny"))]
            block_sigwinch(true);
            #[cfg(not(feature = "tiny"))]
            if normal { install_handler_for_Ctrl_C(); }

            use std::os::unix::fs::OpenOptionsExt;
            let open_result = match method {
                KindOfWritingType::Append => {
                    OpenOptions::new()
                        .write(true).create(true).append(true)
                        .mode(permissions)
                        .open(&realname)
                }
                KindOfWritingType::Emergency => {
                    // O_EXCL — fail if exists
                    OpenOptions::new()
                        .write(true).create_new(true)
                        .mode(permissions)
                        .open(&realname)
                }
                _ => {
                    OpenOptions::new()
                        .write(true).create(true).truncate(true)
                        .mode(permissions)
                        .open(&realname)
                }
            };

            #[cfg(not(feature = "tiny"))]
            if normal { restore_handler_for_Ctrl_C(); }
            #[cfg(not(feature = "tiny"))]
            block_sigwinch(false);

            match open_result {
                Err(e) => {
                    let kind = e.kind();
                    if kind == io::ErrorKind::Interrupted {
                        statusline(MessageType::Alert, "Interrupted");
                    } else {
                        statusline(MessageType::Alert,
                            &format!("Error writing {}: {}", realname, e));
                    }
                    if let Some(ref tn) = tempname {
                        let _ = std::fs::remove_file(tn);
                    }
                    return false;
                }
                Ok(f) => f,
            }
        }
    };

    if normal {
        statusbar("Writing...");
    }

    // Write the buffer line by line
    let filetop = with_state(|s| s.openfile.as_ref().and_then(|of| of.filetop.clone()));
    let mut line = filetop;

    loop {
        let (data, has_next) = match &line {
            None => break,
            Some(node) => {
                let b = node.borrow();
                (b.data.clone(), b.next.is_some())
            }
        };

        // Recode LF as NUL for writing (inverse of encode_data)
        let recoded: Vec<u8> = data.bytes().map(|b| if b == b'\n' { 0 } else { b }).collect();

        if the_file.write_all(&recoded).is_err() {
            let e = io::Error::last_os_error();
            statusline(MessageType::Alert, &format!("Error writing {}: {}", realname, e));
            drop(the_file);
            if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
            return false;
        }

        // If we've reached the last line, don't write a trailing newline.
        if !has_next {
            if !data.is_empty() {
                lineswritten += 1;
            }
            break;
        }

        // Write newline (preceded by CR for DOS format)
        #[cfg(not(feature = "tiny"))]
        {
            let fmt = with_state(|s| {
                s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
            });
            if fmt == FormatType::DosFile {
                if the_file.write_all(b"\r").is_err() {
                    let e = io::Error::last_os_error();
                    statusline(MessageType::Alert, &format!("Error writing {}: {}", realname, e));
                    drop(the_file);
                    if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
                    return false;
                }
            }
        }

        if the_file.write_all(b"\n").is_err() {
            let e = io::Error::last_os_error();
            statusline(MessageType::Alert, &format!("Error writing {}: {}", realname, e));
            drop(the_file);
            if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
            return false;
        }

        lineswritten += 1;
        line = line.and_then(|n| n.borrow().next.clone());
    }

    // When prepending, append the temporary file to what we wrote above.
    #[cfg(not(feature = "tiny"))]
    if method == KindOfWritingType::Prepend {
        if let Some(ref tn) = tempname {
            let source = match File::open(tn) {
                Err(e) => {
                    statusline(MessageType::Alert,
                        &format!("Error reading temp file: {}", e));
                    drop(the_file);
                    if let Some(ref tn2) = tempname { let _ = std::fs::remove_file(tn2); }
                    return false;
                }
                Ok(f) => f,
            };

            // copy_file closes source; we need to keep the_file open
            // We'll do a manual copy here
            let mut buf = [0u8; 8192];
            let mut src = source;
            loop {
                let n = match src.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        statusline(MessageType::Alert,
                            &format!("Error reading temp file: {}", e));
                        drop(the_file);
                        return false;
                    }
                };
                if the_file.write_all(&buf[..n]).is_err() {
                    let e = io::Error::last_os_error();
                    statusline(MessageType::Alert,
                        &format!("Error writing {}: {}", realname, e));
                    drop(the_file);
                    return false;
                }
            }
            let _ = std::fs::remove_file(tn);
        }
    }

    // Flush and sync (not for FIFOs)
    #[cfg(not(feature = "tiny"))]
    {
        use std::os::unix::fs::FileTypeExt;
        let is_fifo = std::fs::metadata(&realname)
            .map(|m| m.file_type().is_fifo())
            .unwrap_or(false);
        if !is_fifo {
            use std::os::unix::io::AsRawFd;
            let fd = the_file.as_raw_fd();
            if the_file.flush().is_err() || unsafe { libc::fsync(fd) } != 0 {
                let e = io::Error::last_os_error();
                statusline(MessageType::Alert, &format!("Error writing {}: {}", realname, e));
                drop(the_file);
                if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
                return false;
            }
        }
    }

    // Close the file
    if the_file.flush().is_err() {
        let e = io::Error::last_os_error();
        statusline(MessageType::Alert, &format!("Error writing {}: {}", realname, e));
        if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }

        // Check for ENOSPC
        #[cfg(not(feature = "tiny"))]
        if e.raw_os_error() == Some(libc::ENOSPC) && normal {
            napms(3200);
            with_state_mut(|s| s.lastmessage = MessageType::Vacuum);
            statusline(MessageType::Alert, "File on disk has been truncated!");
            napms(3200);
            with_state_mut(|s| s.lastmessage = MessageType::Vacuum);
            statusline(MessageType::Alert,
                "Maybe ^T^Z, make room on disk, resume, then ^S^X");
            let st = stat_with_alloc(&realname);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.statinfo = st;
                }
            });
        }

        return false;
    }
    drop(the_file);

    // When having written an entire buffer, update administrivia.
    if annotate && method == KindOfWritingType::Overwrite {
        let old_filename = with_state(|s| {
            s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
        });

        if old_filename != realname {
            #[cfg(not(feature = "tiny"))]
            {
                let (has_lock, lock_fname) = with_state(|s| {
                    if let Some(ref of) = s.openfile {
                        (of.lock_filename.is_some(), of.lock_filename.clone())
                    } else {
                        (false, None)
                    }
                });
                if has_lock {
                    if let Some(lf) = lock_fname {
                        delete_lockfile(&lf);
                    }
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.lock_filename = None;
                        }
                    });
                }
                if ISSET!(LOCKING) {
                    let lock_result = do_lockfile(&realname, false);
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            of.lock_filename = lock_result.ok().flatten();
                        }
                    });
                }
            }

            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.filename = realname.clone();
                }
            });

            #[cfg(feature = "color")]
            {
                let was_syntax = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.syntax).map(|p| p as usize)
                });
                find_and_prime_applicable_syntax();
                let new_syntax = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.syntax).map(|p| p as usize)
                });
                if was_syntax != new_syntax {
                    // Clear multidata and recompute
                    with_state_mut(|s| {
                        if let Some(ref mut of) = s.openfile {
                            let mut ln = of.filetop.clone();
                            while let Some(node) = ln {
                                node.borrow_mut().multidata.clear();
                                let next = node.borrow().next.clone();
                                ln = next;
                            }
                        }
                        s.have_palette = false;
                        s.refresh_needed = true;
                    });
                    precalc_multicolorinfo();
                }
            }
        }

        #[cfg(not(feature = "tiny"))]
        {
            let st = stat_with_alloc(&realname);
            with_state_mut(|s| {
                if let Some(ref mut of) = s.openfile {
                    of.statinfo = st;
                    of.last_saved = of.current_undo;
                    of.last_action = UndoType::Other;
                }
            });
        }

        with_state_mut(|s| {
            if let Some(ref mut of) = s.openfile {
                of.modified = false;
            }
        });
        titlebar(None);
    }

    // Report the number of lines written
    #[cfg(not(feature = "tiny"))]
    {
        let minibar = ISSET!(MINIBAR);
        let zero = ISSET!(ZERO);
        let lines = LINES();
        if minibar && !zero && lines > 1 && annotate {
            with_state_mut(|s| s.report_size = true);
            if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
            return true;
        }
    }

    if normal {
        let msg = if lineswritten == 1 {
            format!("Wrote {} line", lineswritten)
        } else {
            format!("Wrote {} lines", lineswritten)
        };
        statusline(MessageType::Remark, &msg);
    }

    if let Some(ref tn) = tempname { let _ = std::fs::remove_file(tn); }
    true
}

// ---------------------------------------------------------------------------
// write_region_to_file (!NANO_TINY)
// C: bool write_region_to_file(const char *name, FILE *stream, bool normal,
//                               kind_of_writing_type method)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tiny"))]
pub fn write_region_to_file(
    name: &str,
    stream: Option<File>,
    normal: bool,
    method: KindOfWritingType,
) -> bool {
    let mut topline: Option<LinePtr> = None;
    let mut botline: Option<LinePtr> = None;
    let mut top_x: usize = 0;
    let mut bot_x: usize = 0;

    get_region(&mut topline, &mut top_x, &mut botline, &mut bot_x);

    // When needed, prepare a magic end line
    let stopper: Option<LinePtr> = if normal && bot_x > 0 && !ISSET!(NO_NEWLINES) {
        let new_node = make_new_node(botline.clone());
        new_node.borrow_mut().data = String::new();
        if let Some(ref bot) = botline {
            bot.borrow_mut().next = Some(new_node.clone());
        }
        Some(new_node)
    } else {
        None
    };

    // Save and modify the buffer to look like a region
    let birthline = with_state(|s| s.openfile.as_ref().and_then(|of| of.filetop.clone()));
    let after_line = botline.as_ref().and_then(|b| b.borrow().next.clone());

    // Truncate botline at bot_x
    let saved_byte = botline.as_ref().map(|b| {
        let b_ref = b.borrow();
        b_ref.data.as_bytes().get(bot_x).copied().unwrap_or(0)
    });

    if let Some(ref bot) = botline {
        let mut b = bot.borrow_mut();
        if bot_x <= b.data.len() {
            b.data.truncate(bot_x);
        }
        if let Some(ref stopper_node) = stopper {
            b.next = Some(stopper_node.clone());
        } else {
            b.next = None;
        }
    }

    // Adjust topline start
    if let Some(ref top) = topline {
        let mut t = top.borrow_mut();
        if top_x <= t.data.len() {
            // slice from top_x — we store as an offset
            // This is simplified; the C code adjusts the data pointer directly
        }
    }

    // Set filetop to topline
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.filetop = topline.clone();
        }
    });

    let retval = write_file(name, stream, normal, method, NONOTES);

    // Restore buffer state
    with_state_mut(|s| {
        if let Some(ref mut of) = s.openfile {
            of.filetop = birthline;
        }
    });

    if let Some(ref bot) = botline {
        let mut b = bot.borrow_mut();
        // Restore the saved byte
        if let Some(sb) = saved_byte {
            if bot_x <= b.data.len() {
                // Push back the saved character
                b.data.push(sb as char);
            }
        }
        b.next = after_line;
    }

    if let Some(stopper_node) = stopper {
        delete_node(stopper_node);
    }

    retval
}

// ---------------------------------------------------------------------------
// write_it_out — write current buffer (or marked region) to disk
// C: int write_it_out(bool exiting, bool withprompt)
// Returns 0 on error, 1 on success, 2 when buffer is to be discarded.
// ---------------------------------------------------------------------------
pub fn write_it_out(exiting: bool, withprompt: bool) -> i32 {
    let given = {
        #[cfg(not(feature = "tiny"))]
        {
            let mark_on = with_state(|s| {
                s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
            });
            if mark_on && !exiting {
                String::new()
            } else {
                with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
            }
        }
        #[cfg(feature = "tiny")]
        {
            with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
        }
    };

    let mut given = given;
    let maychange_initial = with_state(|s| {
        s.openfile.as_ref().map(|of| of.filename.is_empty()).unwrap_or(true)
    });
    let mut maychange = maychange_initial;
    let mut method = KindOfWritingType::Overwrite;

    with_state_mut(|s| s.as_an_at = false);

    #[cfg(feature = "extra")]
    let mut did_credits = false;

    loop {
        let response: i32;
        let mut choice = NO;

        #[cfg(not(feature = "tiny"))]
        let formatstr = {
            let fmt = with_state(|s| {
                s.openfile.as_ref().map(|of| of.fmt).unwrap_or(FormatType::Unspecified)
            });
            if fmt == FormatType::DosFile { " [DOS Format]" } else { "" }
        };
        #[cfg(feature = "tiny")]
        let formatstr = "";

        #[cfg(not(feature = "tiny"))]
        let backupstr = if ISSET!(MAKE_BACKUP) { " [Backup]" } else { "" };
        #[cfg(feature = "tiny")]
        let backupstr = "";

        let msg = {
            #[cfg(not(feature = "tiny"))]
            {
                let mark_on = with_state(|s| {
                    s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
                });
                let restricted = ISSET!(RESTRICTED);
                if mark_on && !exiting && !restricted {
                    match method {
                        KindOfWritingType::Prepend => "Prepend Selection to File",
                        KindOfWritingType::Append  => "Append Selection to File",
                        _                          => "Write Selection to File",
                    }
                } else if method != KindOfWritingType::Overwrite {
                    match method {
                        KindOfWritingType::Prepend => "Prepend to File",
                        _                          => "Append to File",
                    }
                } else {
                    "Write to File"
                }
            }
            #[cfg(feature = "tiny")]
            "Write to File"
        };

        with_state_mut(|s| s.present_path = Some("./".to_string()));

        let save_on_exit = ISSET!(SAVE_ON_EXIT);
        let has_filename = with_state(|s| {
            s.openfile.as_ref().map(|of| !of.filename.is_empty()).unwrap_or(false)
        });

        let skip_prompt = (!withprompt || (save_on_exit && exiting)) && has_filename;

        if skip_prompt {
            let fname = with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default());
            with_state_mut(|s| s.answer = fname);
            response = 0;
        } else {
            let prompt_str = format!("{}{}{}", msg, formatstr, backupstr);
            response = do_prompt(
                MWRITEFILE,
                &given,
                None,
                edit_refresh,
                &prompt_str,
                "",
            );
        }

        if response < 0 {
            statusbar("Cancelled");
            return 0;
        }

        let function = func_from_key(response);

        // Handle discard
        if function == Some(crate::global::discard_buffer as FuncPtr) {
            with_state_mut(|s| s.final_status = 2);
            return 2;
        }

        let answer = with_state(|s| s.answer.clone());
        given = answer.clone();

        #[cfg(feature = "browser")]
        {
            let restricted = ISSET!(RESTRICTED);
            if function == Some(crate::global::to_files as FuncPtr) && !restricted {
                if let Some(chosen) = browse_in(&answer) {
                    with_state_mut(|s| s.answer = chosen);
                } else {
                    continue;
                }
            }
        }

        let answer2 = with_state(|s| s.answer.clone());

        #[cfg(not(feature = "tiny"))]
        {
            if function == Some(crate::global::dos_format as FuncPtr) {
                with_state_mut(|s| {
                    if let Some(ref mut of) = s.openfile {
                        of.fmt = if of.fmt == FormatType::DosFile {
                            FormatType::NixFile
                        } else {
                            FormatType::DosFile
                        };
                    }
                });
                continue;
            }
            let restricted = ISSET!(RESTRICTED);
            if function == Some(crate::global::back_it_up as FuncPtr) && !restricted {
                TOGGLE!(MAKE_BACKUP);
                continue;
            }
            if (function == Some(crate::global::prepend_it as FuncPtr)
                || function == Some(crate::global::append_it as FuncPtr))
                && !restricted
            {
                if function == Some(crate::global::prepend_it as FuncPtr) {
                    method = if method == KindOfWritingType::Prepend {
                        KindOfWritingType::Overwrite
                    } else {
                        KindOfWritingType::Prepend
                    };
                } else {
                    method = if method == KindOfWritingType::Append {
                        KindOfWritingType::Overwrite
                    } else {
                        KindOfWritingType::Append
                    };
                }
                let of_fname = with_state(|s| {
                    s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
                });
                if answer2 == of_fname {
                    given.clear();
                }
                continue;
            }
        }

        if function == Some(crate::global::do_help as FuncPtr) {
            continue;
        }

        // Easter egg
        #[cfg(feature = "extra")]
        {
            let of_fname = with_state(|s| {
                s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
            });
            if exiting && !ISSET!(SAVE_ON_EXIT) && of_fname.is_empty()
                && answer2 == "zzy" && !did_credits
            {
                let lines = LINES();
                let cols = COLS();
                if lines > 5 && cols > 31 {
                    do_credits();
                    did_credits = true;
                } else {
                    statusline(MessageType::Ahem, "Too tiny");
                }
                return 0;
            }
        }

        if method == KindOfWritingType::Overwrite {
            let full_answer = get_full_path(&answer2);
            let full_filename = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|of| get_full_path(&of.filename))
            });
            let of_filename_empty = with_state(|s| {
                s.openfile.as_ref().map(|of| of.filename.is_empty()).unwrap_or(true)
            });
            let answer_path = full_answer.as_deref().unwrap_or(&answer2);
            let of_filename_string: String = full_filename.clone().unwrap_or_else(|| {
                with_state(|s| s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default())
            });
            let of_path: &str = &of_filename_string;
            let name_exists = std::fs::metadata(answer_path).is_ok();
            let do_warning = if of_filename_empty {
                name_exists
            } else {
                answer_path != of_path
            };

            if do_warning {
                let restricted = ISSET!(RESTRICTED);
                if restricted {
                    warn_and_briefly_pause("File exists -- cannot overwrite");
                    continue;
                }

                if !maychange {
                    #[cfg(not(feature = "tiny"))]
                    {
                        let mark_on = with_state(|s| {
                            s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
                        });
                        if exiting || !mark_on {
                            if ask_user(YESORNO, "Save file under DIFFERENT NAME? ") != YES {
                                continue;
                            }
                            maychange = true;
                        }
                    }
                    #[cfg(feature = "tiny")]
                    {
                        if ask_user(YESORNO, "Save file under DIFFERENT NAME? ") != YES {
                            continue;
                        }
                        maychange = true;
                    }
                }

                if name_exists {
                    let question = "File \"%s\" exists; OVERWRITE? ";
                    let room = if COLS() > breadth(question) + 1 { COLS() - breadth(question) + 1 } else { 4 };
                    let name = crop_to_fit(&answer2, room);
                    let message = format!("File \"{}\" exists; OVERWRITE? ", name);
                    choice = ask_user(YESORNO, &message);
                    if choice != YES {
                        continue;
                    }
                }
            } else {
                #[cfg(not(feature = "tiny"))]
                {
                    if name_exists {
                        let (stat_changed, stat_mtime, stat_dev, stat_ino) = with_state(|s| {
                            if let Some(ref of) = s.openfile {
                                if let Some(ref si) = of.statinfo {
                                    (true, si.st_mtime, si.st_dev, si.st_ino)
                                } else {
                                    (false, 0, 0, 0)
                                }
                            } else {
                                (false, 0, 0, 0)
                            }
                        });

                        if stat_changed {
                            let new_st = stat_with_alloc(&answer2);
                            let changed = new_st.as_ref().map(|ns| {
                                ns.st_mtime > stat_mtime
                                || ns.st_dev != stat_dev
                                || ns.st_ino != stat_ino
                            }).unwrap_or(false);

                            if changed {
                                warn_and_briefly_pause("File on disk has changed");
                                choice = ask_user(YESORNO,
                                    "File was modified since you opened it; continue saving? ");
                                wipe_statusbar();

                                if ISSET!(SAVE_ON_EXIT) && withprompt {
                                    let fname = with_state(|s| {
                                        s.openfile.as_ref().map(|of| of.filename.clone()).unwrap_or_default()
                                    });
                                    if choice == YES {
                                        return write_file(&fname, None, NORMAL, KindOfWritingType::Overwrite, NONOTES) as i32;
                                    } else if choice == NO {
                                        return 2; // Discard
                                    } else {
                                        return 0;
                                    }
                                } else if choice == CANCEL && exiting {
                                    continue;
                                } else if choice != YES {
                                    return 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        break;
    }

    // Write the file
    let final_answer = with_state(|s| s.answer.clone());

    #[cfg(not(feature = "tiny"))]
    {
        let mark_on = with_state(|s| {
            s.openfile.as_ref().and_then(|of| of.mark.as_ref()).is_some()
        });
        if mark_on && withprompt && !exiting && !ISSET!(RESTRICTED) {
            return write_region_to_file(&final_answer, None, NORMAL, method) as i32;
        }
    }

    write_file(&final_answer, None, NORMAL, method, ANNOTATE) as i32
}

/* C: void do_writeout(void)
 * Write the current buffer to disk, or discard it. */
pub fn do_writeout() {
    if write_it_out(false, true) == 2 {
        close_and_go();
    }
}

/* C: void do_savefile(void)
 * Write the current buffer to disk without prompting (if it has a name). */
pub fn do_savefile() {
    if write_it_out(false, false) == 2 {
        close_and_go();
    }
}

// ---------------------------------------------------------------------------
// expand_leading_tilde — expand ~ in paths
// C: char *expand_leading_tilde(const char *path)
// ---------------------------------------------------------------------------
pub fn expand_leading_tilde(path: &str) -> String {
    if !path.starts_with('~') || path.len() == 1 {
        return path.to_string();
    }

    // Find the end of the username part (~user or ~/)
    let slash_pos = path[1..].find('/').map(|p| p + 1).unwrap_or(path.len());
    let username_part = &path[1..slash_pos];
    let rest = &path[slash_pos..];

    let tilded: String = if username_part.is_empty() {
        // Just ~, use $HOME
        crate::utils::get_homedir();
        with_state(|s| s.homedir.clone().unwrap_or_default())
    } else {
        // ~user — look up in passwd
        #[cfg(unix)]
        {
            let cname = match std::ffi::CString::new(username_part) {
                Ok(s) => s,
                Err(_) => return path.to_string(),
            };
            let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
            if pw.is_null() {
                String::new()
            } else {
                unsafe {
                    std::ffi::CStr::from_ptr((*pw).pw_dir)
                        .to_string_lossy()
                        .into_owned()
                }
            }
        }
        #[cfg(not(unix))]
        String::new()
    };

    if tilded.is_empty() {
        path.to_string()
    } else {
        format!("{}{}", tilded, rest)
    }
}

// ---------------------------------------------------------------------------
// diralphasort — sort file listings alphabetically with dirs first
// C: int diralphasort(const void *va, const void *vb)
// ---------------------------------------------------------------------------
#[cfg(any(feature = "tabcomp", feature = "browser"))]
pub fn diralphasort(a: &str, b: &str) -> std::cmp::Ordering {
    let a_is_dir = std::fs::metadata(a).map(|m| m.is_dir()).unwrap_or(false);
    let b_is_dir = std::fs::metadata(b).map(|m| m.is_dir()).unwrap_or(false);

    if a_is_dir && !b_is_dir {
        return std::cmp::Ordering::Less;
    }
    if !a_is_dir && b_is_dir {
        return std::cmp::Ordering::Greater;
    }

    // Case-insensitive compare
    let diff = mbstrcasecmp(a, b);
    if diff != 0 {
        if diff < 0 { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
    } else {
        a.cmp(b)
    }
}

// ---------------------------------------------------------------------------
// is_dir — return TRUE when the given path is a directory
// C: bool is_dir(const char *path)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn is_dir(path: &str) -> bool {
    let expanded = expand_leading_tilde(path);
    std::fs::metadata(&expanded).map(|m| m.is_dir()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// username_completion — complete a username fragment
// C: char **username_completion(const char *morsel, size_t length, size_t *num_matches)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn username_completion(morsel: &str, length: usize) -> Vec<String> {
    let mut matches: Vec<String> = Vec::new();

    #[cfg(unix)]
    unsafe {
        libc::setpwent();
        loop {
            let pw = libc::getpwent();
            if pw.is_null() {
                break;
            }
            let name = std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .into_owned();
            // morsel starts with ~, so morsel[1..length-1] is the fragment
            let fragment = if morsel.len() >= 1 { &morsel[1..length.saturating_sub(0)] } else { "" };
            // Actually compare against morsel+1 .. length-1
            let frag = if morsel.len() > 1 { &morsel[1..] } else { "" };
            if name.starts_with(frag) {
                #[cfg(feature = "operatingdir")]
                {
                    let dir = std::ffi::CStr::from_ptr((*pw).pw_dir)
                        .to_string_lossy()
                        .into_owned();
                    let od = with_state(|s| s.operating_dir.clone());
                    if let Some(ref od_str) = od {
                        if outside_of_confinement(&dir, true) {
                            continue;
                        }
                    }
                }
                matches.push(format!("~{}", name));
            }
        }
        libc::endpwent();
    }

    matches
}

// ---------------------------------------------------------------------------
// filename_completion — complete a filename fragment
// C: char **filename_completion(const char *morsel, size_t *num_matches)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn filename_completion(morsel: &str) -> Vec<String> {
    let mut matches: Vec<String> = Vec::new();

    let present_path = with_state(|s| s.present_path.clone().unwrap_or_else(|| "./".to_string()));

    // Split morsel into dirname and filename parts
    let (dirname, filename) = if let Some(slash_pos) = morsel.rfind('/') {
        let dir_part = &morsel[..=slash_pos];
        let file_part = &morsel[slash_pos + 1..];
        let expanded = expand_leading_tilde(dir_part);
        let full_dir = if expanded.starts_with('/') {
            expanded
        } else {
            format!("{}{}", present_path, dir_part)
        };
        (full_dir, file_part.to_string())
    } else {
        (present_path.clone(), morsel.to_string())
    };

    let dir = match std::fs::read_dir(&dirname) {
        Err(_) => {
            beep();
            return matches;
        }
        Ok(d) => d,
    };

    let filenamelen = filename.len();

    for entry in dir {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let entry_name = entry.file_name().to_string_lossy().into_owned();

        if entry_name == "." || entry_name == ".." {
            continue;
        }

        if entry_name.starts_with(&filename) {
            let fullname = format!("{}{}", dirname, entry_name);

            #[cfg(feature = "operatingdir")]
            {
                if with_state(|s| s.operating_dir.is_some()) {
                    if outside_of_confinement(&fullname, true) {
                        continue;
                    }
                }
            }

            let currmenu = with_state(|s| s.currmenu);
            if currmenu == MGOTODIR && !is_dir(&fullname) {
                continue;
            }

            matches.push(entry_name);
        }
    }

    matches
}

// ---------------------------------------------------------------------------
// input_tab — do tab completion
// C: char *input_tab(char *morsel, size_t *place, void (*refresh_func)(void), bool *listed)
// ---------------------------------------------------------------------------
#[cfg(feature = "tabcomp")]
pub fn input_tab(
    morsel: &str,
    place: &mut usize,
    refresh_func: fn(),
    listed: &mut bool,
) -> String {
    // If the cursor is not at the end of the fragment, do nothing.
    if *place < morsel.len() {
        beep();
        return morsel.to_string();
    }

    let mut matches: Vec<String>;

    // Try username completion if starts with ~ and no slash
    if morsel.starts_with('~') && !morsel.contains('/') {
        matches = username_completion(morsel, *place);
    } else {
        matches = Vec::new();
    }

    // If no matches yet, try filename completion
    if matches.is_empty() {
        matches = filename_completion(morsel);
    }

    // If completions were listed before but none will be listed now...
    if *listed && matches.len() < 2 {
        refresh_func();
        *listed = false;
    }

    if matches.is_empty() {
        beep();
        return morsel.to_string();
    }

    // Find last slash in morsel
    let length_of_path = morsel.rfind('/').map(|p| p + 1).unwrap_or(0);

    // Determine common prefix length
    let mut common_len = 0;
    if !matches.is_empty() {
        let first = &matches[0];
        'outer: loop {
            if common_len >= first.len() {
                break;
            }
            // Get next char boundary
            let ch1 = &first[common_len..];
            let ch1_char = ch1.chars().next();
            let ch1_len = match ch1_char {
                Some(c) => c.len_utf8(),
                None => break,
            };

            for m in &matches[1..] {
                if common_len + ch1_len > m.len() {
                    break 'outer;
                }
                if &first[common_len..common_len + ch1_len] != &m[common_len..common_len + ch1_len] {
                    break 'outer;
                }
            }
            common_len += ch1_len;
        }
    }

    // Build shared prefix: path_prefix + common portion of matches
    let mut shared = format!("{}{}", &morsel[..length_of_path], &matches[0][..common_len]);
    let total_common_len = shared.len();

    // Append slash if single match that is a directory
    let present_path = with_state(|s| s.present_path.clone().unwrap_or_else(|| "./".to_string()));
    let glued = format!("{}{}", present_path, shared);

    let is_single_dir = matches.len() == 1 && (is_dir(&shared) || is_dir(&glued));
    if is_single_dir {
        shared.push('/');
    }

    // Update morsel if common part is longer than current position
    let new_morsel = if total_common_len != *place {
        *place = shared.len();
        shared.clone()
    } else {
        if matches.len() == 1 {
            beep();
        }
        morsel.to_string()
    };

    // Show list if more than one possible completion
    if matches.len() > 1 {
        // Sort matches
        matches.sort_by(|a, b| diralphasort(a, b));

        let editwinrows_val = editwinrows();
        let zero = ISSET!(ZERO);
        let lines = LINES();
        let lastrow = editwinrows_val - 1 - (if zero && lines > 1 { 1 } else { 0 });
        let cols = COLS();

        // Find the longest match name
        let longest_name = matches.iter().map(|m| breadth(m)).max().unwrap_or(0);
        let longest_name = longest_name.min(cols.saturating_sub(1));

        // Calculate columns and rows
        let ncols = if longest_name + 2 > 0 { (cols + 1) / (longest_name + 2) } else { 1 };
        let nrows = (matches.len() + ncols - 1) / ncols;

        if !*listed {
            beep();
        }

        // In the real implementation, we'd draw to the edit window via winio.
        // Here we just set listed = true as a stub.
        *listed = true;
    }

    new_morsel
}
