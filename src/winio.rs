#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/winio.c from GNU nano.
// C original: Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2014-2026 Benno Schulenberg
//
// This module replaces ncurses WINDOW* with crossterm terminal control.
// The key-input architecture retains the integer key_buffer model so that
// put_back, macros, implant/plantation, verbatim and unicode assembly all
// work identically to the C code.  Only read_keys_from() is fundamentally
// changed: it calls crossterm event::read() and translates each Event into
// the integer code(s) that ncurses would have put in the buffer.

#[allow(unused_imports)] // some of these are used only under feature gates
use crossterm::{
    execute, queue,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen, Clear, ClearType,
               ScrollUp, ScrollDown},
    cursor::{MoveTo, Hide, Show},
    style::{
        Print, SetForegroundColor, SetBackgroundColor, SetAttribute, Attribute, Color, ResetColor,
    },
    event::{
        self, Event, KeyEvent, KeyCode, KeyModifiers, KeyEventKind,
        MouseEvent, MouseEventKind, MouseButton,
    },
};
use std::io::{self, BufWriter, Stdout, Write, stdout};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Once;
use std::time::Duration;
use std::cell::{Cell, RefCell};
use crate::definitions::*;
use crate::global::{
    with_state, with_state_mut, state, state_mut, NanoWindow,
    KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_HOME, KEY_END,
    KEY_PPAGE, KEY_NPAGE, KEY_DC, KEY_IC, KEY_BACKSPACE,
    key_f, A_REVERSE, shown_entries_for,
    flag_index, flag_mask,
};
#[cfg(not(feature = "tiny"))]
use crate::global::{KEY_ENTER, KEY_F0};
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::chars::{
    is_cntrl_char, control_mbrep, char_length,
    step_left, step_right, advance_over, is_blank_char,
};
#[cfg(feature = "utf8")]
use crate::chars::{is_doublewidth, is_zerowidth, mbtowide};
#[cfg(not(feature = "utf8"))]
fn is_doublewidth(_s: &str) -> bool { false }
#[cfg(not(feature = "utf8"))]
fn is_zerowidth(s: &str) -> bool { false }
use crate::utils::{
    actual_x, wideness, breadth, get_page_start, xplustabs, digits,
};

// ---------------------------------------------------------------------------
// Shared buffered terminal writer.
//
// Every byte we paint goes through this one BufWriter instead of a fresh
// `std::io::out()` handle per call. `out()`'s LineWriter has only a ~1KB
// buffer and acquires the process-wide stdout lock on every write, so a full
// repaint or a scroll used to issue several write() syscalls mid-frame. A
// single 64KB BufWriter collapses a frame's worth of escape sequences into one
// write at flush time.
//
// IMPORTANT: because nothing reaches the terminal until an explicit flush, any
// code that blocks waiting for input MUST flush first (see read_keys_from),
// otherwise the last painted frame would sit buffered and the screen would look
// stale until the next keypress — the very symptom the poll fix removed.
//
// Re-entrancy: nano's draw functions call one another while a local output
// handle is alive.  The handle therefore cannot itself be a reference into
// the shared writer.  `TerminalOutput` is an owned proxy that borrows the
// thread-local writer for one `Write` operation at a time, so nested painters
// never create aliased mutable references and still share one frame buffer.
// ---------------------------------------------------------------------------
/// An owned handle to nano's shared terminal output.
///
/// The `Rc` marker keeps the handle on the editor thread.  It contains no
/// reference to the writer, so several handles may safely coexist while each
/// individual write is serialized through the thread-local `RefCell`.
pub struct TerminalOutput(PhantomData<Rc<()>>);

impl Write for TerminalOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        with_out(|writer| writer.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        with_out(Write::flush)
    }

    fn write_vectored(&mut self, buffers: &[io::IoSlice<'_>]) -> io::Result<usize> {
        with_out(|writer| writer.write_vectored(buffers))
    }
}

/// Run one operation with the shared buffered writer.
///
/// The reference cannot escape the callback.  Most rendering code uses
/// `out()` instead so that it cannot accidentally hold this borrow while
/// calling another painter.
#[inline]
pub fn with_out<R>(operation: impl FnOnce(&mut BufWriter<Stdout>) -> R) -> R {
    OUT.with(|slot| operation(&mut slot.borrow_mut()))
}

/// Return an owned output proxy.  No reference to the shared writer escapes.
#[inline]
pub fn out() -> TerminalOutput {
    TerminalOutput(PhantomData)
}

/// Flush the shared buffered writer to the real terminal.
#[inline]
pub fn flush_out() {
    let _ = with_out(Write::flush);
}

// ---------------------------------------------------------------------------
// Module-level statics (equivalent to file-scope C statics in winio.c)
// ---------------------------------------------------------------------------

thread_local! {
    /// The one buffered terminal writer used by every output proxy.
    static OUT: RefCell<BufWriter<Stdout>> =
        RefCell::new(BufWriter::with_capacity(64 * 1024, stdout()));

    /// Only the thread that installs the process-wide panic hook owns the UI
    /// terminal.  A recoverable panic on a worker thread must not tear down
    /// the editor's raw mode or alternate screen.
    static IS_TERMINAL_UI_THREAD: Cell<bool> = const { Cell::new(false) };

    /// A buffer for keystrokes that haven't been handled yet.
    static KEY_BUFFER: RefCell<Vec<i32>> = RefCell::new(Vec::with_capacity(32));
    /// Index into KEY_BUFFER of the next code to consume.
    static NEXTCODES_IDX: RefCell<usize> = RefCell::new(0);
    /// The number of key codes waiting in the keystroke buffer.
    static WAITING_CODES: RefCell<usize> = RefCell::new(0);

    /// Whether the cursor should be shown when waiting for input.
    static REVEAL_CURSOR: RefCell<bool> = RefCell::new(false);
    /// Whether to give the terminal some extra time to deliver the next code after ESC.
    static LINGER_AFTER_ESCAPE: RefCell<bool> = RefCell::new(false);
    /// The number of keystrokes left before we blank the status bar.
    static COUNTDOWN: RefCell<i32> = RefCell::new(0);

    /// From where in the relevant line the current row is drawn.
    static FROM_X: RefCell<usize> = RefCell::new(0);
    /// Until where in the relevant line the current row is drawn.
    static TILL_X: RefCell<usize> = RefCell::new(0);
    /// Whether the current line has more text after the displayed part.
    static HAS_MORE: RefCell<bool> = RefCell::new(false);
    /// Whether a row's text is narrower than the screen's width.
    static IS_SHORTER: RefCell<bool> = RefCell::new(true);

    /// The starting column of the next softwrap chunk.
    #[cfg(not(feature = "tiny"))]
    static SEQUEL_COLUMN: RefCell<usize> = RefCell::new(0);

    /// Whether we are recording a macro.
    #[cfg(not(feature = "tiny"))]
    static RECORDING: RefCell<bool> = RefCell::new(false);
    /// The buffer where recorded key codes are stored.
    #[cfg(not(feature = "tiny"))]
    static MACRO_BUFFER: RefCell<Vec<i32>> = RefCell::new(Vec::new());
    /// The macro that was active before a new recording began.
    ///
    /// Keeping this separate makes starting a recording transactional: when
    /// the user immediately stops again, the previous macro can be restored.
    #[cfg(not(feature = "tiny"))]
    static PREVIOUS_MACRO: RefCell<Option<Vec<i32>>> = RefCell::new(None);
    /// Where the last burst of recorded keystrokes started.
    #[cfg(not(feature = "tiny"))]
    static MILESTONE: RefCell<usize> = RefCell::new(0);

    /// Points into the expansion string for the current implantation.
    #[cfg(feature = "nanorc")]
    static PLANTS_POINTER: RefCell<Option<(String, usize)>> = RefCell::new(None);

    /// How many digits of a three-digit character code we've eaten.
    static DIGIT_COUNT: RefCell<i32> = RefCell::new(0);

    // Static locals from parse_kbinput
    static ESCAPES: RefCell<i32> = RefCell::new(0);
    static FIRST_ESCAPE_WAS_ALONE: RefCell<bool> = RefCell::new(false);
    static LAST_ESCAPE_WAS_ALONE: RefCell<bool> = RefCell::new(false);

    // Static locals from assemble_byte_code
    static BYTE_ACC: RefCell<i32> = RefCell::new(0);

    // Static locals from assemble_unicode
    #[cfg(feature = "utf8")]
    static UNICODE_ACC: RefCell<u32> = RefCell::new(0);
    #[cfg(feature = "utf8")]
    static UNICODE_DIGITS: RefCell<i32> = RefCell::new(0);

    // Static local from statusline
    static STATUSLINE_START_COL: RefCell<usize> = RefCell::new(0);

    // get_softwrap_breakpoint static state (byte offset + column)
    static SWB_TEXT_OFFSET: RefCell<usize> = RefCell::new(0);
    static SWB_COLUMN: RefCell<usize> = RefCell::new(0);
}

// Convenience helpers to read/write thread-locals without noise.
macro_rules! tl_get {
    ($var:ident) => { $var.with(|v| *v.borrow()) };
}
macro_rules! tl_set {
    ($var:ident, $val:expr) => { $var.with(|v| *v.borrow_mut() = $val) };
}

// ---------------------------------------------------------------------------
// ESC_CODE as i32 (definitions.rs has it as u32)
// ---------------------------------------------------------------------------
const ESC: i32 = 0x1B;
const DEL: i32 = 0x7F;
const ERR_CODE: i32 = -1;

// Crossterm exposes Shift+Left/Right as modifiers on the base key.  Keep them
// in the same private range as nano's other dedicated shifted key codes so the
// modifier remains attached while several events wait in KEY_BUFFER.
const SHIFT_LEFT_CODE: i32 = 0x451;
const SHIFT_RIGHT_CODE: i32 = 0x452;

// A sentinel used in assemble_byte_code / assemble_unicode
const PROCEED: i64 = -44;
const INVALID_DIGIT: i64 = -77;

const RAW_MODE_ACTIVE: u8 = 1 << 0;
const ALTERNATE_SCREEN_ACTIVE: u8 = 1 << 1;
const CURSOR_HIDDEN: u8 = 1 << 2;
const TERMINAL_READY: u8 = RAW_MODE_ACTIVE | ALTERNATE_SCREEN_ACTIVE | CURSOR_HIDDEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalCleanupPlan {
    show_cursor: bool,
    leave_alternate_screen: bool,
    disable_raw_mode: bool,
}

fn cleanup_plan(active_state: u8) -> TerminalCleanupPlan {
    TerminalCleanupPlan {
        show_cursor: active_state & CURSOR_HIDDEN != 0,
        leave_alternate_screen: active_state & ALTERNATE_SCREEN_ACTIVE != 0,
        disable_raw_mode: active_state & RAW_MODE_ACTIVE != 0,
    }
}

/// A bitset instead of a single "active" boolean lets teardown unwind an
/// initialization that failed after changing only part of the terminal state.
static TERMINAL_STATE: AtomicU8 = AtomicU8::new(0);
static RESIZE_GENERATION: AtomicUsize = AtomicUsize::new(0);
static INSTALL_PANIC_HOOK: Once = Once::new();

#[cfg(unix)]
static SAVED_TERMIOS_VALID: AtomicU8 = AtomicU8::new(0);
#[cfg(unix)]
static SAVED_TERMIOS: [AtomicU8; std::mem::size_of::<libc::termios>()] =
    [const { AtomicU8::new(0) }; std::mem::size_of::<libc::termios>()];
#[cfg(windows)]
static SAVED_CONSOLE_MODE_VALID: AtomicU8 = AtomicU8::new(0);
#[cfg(windows)]
static SAVED_CONSOLE_MODE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Retain an exact, lock-free copy of the pre-raw terminal attributes.  The
/// bytewise atomic representation is intentional: a fatal signal cannot lock
/// crossterm's internal saved-mode mutex, but it can safely reconstruct this
/// snapshot for `tcsetattr`.
#[cfg(unix)]
fn remember_pre_raw_termios() {
    const TTY: &[u8] = b"/dev/tty\0";
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    let mut fd = libc::STDIN_FILENO;
    let mut close_fd = false;
    let mut succeeded = unsafe { libc::tcgetattr(fd, &mut settings) == 0 };
    if !succeeded {
        fd = unsafe { libc::open(TTY.as_ptr().cast(), libc::O_RDWR | libc::O_NOCTTY) };
        if fd >= 0 {
            close_fd = true;
            succeeded = unsafe { libc::tcgetattr(fd, &mut settings) == 0 };
        }
    }
    if close_fd {
        unsafe { libc::close(fd) };
    }
    if !succeeded {
        SAVED_TERMIOS_VALID.store(0, Ordering::Release);
        return;
    }

    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&settings as *const libc::termios).cast::<u8>(),
            std::mem::size_of::<libc::termios>(),
        )
    };
    for (slot, byte) in SAVED_TERMIOS.iter().zip(bytes) {
        slot.store(*byte, Ordering::Relaxed);
    }
    SAVED_TERMIOS_VALID.store(1, Ordering::Release);
}

#[cfg(unix)]
fn forget_pre_raw_termios() {
    SAVED_TERMIOS_VALID.store(0, Ordering::Release);
}

#[cfg(windows)]
fn remember_pre_raw_console_mode() {
    use windows::Win32::System::Console::{
        CONSOLE_MODE, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
    };

    let Ok(input) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
        SAVED_CONSOLE_MODE_VALID.store(0, Ordering::Release);
        return;
    };
    let mut mode = CONSOLE_MODE(0);
    if unsafe { GetConsoleMode(input, &mut mode) }.is_ok() {
        SAVED_CONSOLE_MODE.store(mode.0, Ordering::Relaxed);
        SAVED_CONSOLE_MODE_VALID.store(1, Ordering::Release);
    } else {
        SAVED_CONSOLE_MODE_VALID.store(0, Ordering::Release);
    }
}

#[cfg(windows)]
fn restore_pre_raw_console_mode() -> bool {
    use windows::Win32::System::Console::{
        CONSOLE_MODE, GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE,
    };

    if SAVED_CONSOLE_MODE_VALID.swap(0, Ordering::AcqRel) == 0 {
        return false;
    }
    let Ok(input) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
        return false;
    };
    let mode = SAVED_CONSOLE_MODE.load(Ordering::Relaxed);
    unsafe { SetConsoleMode(input, CONSOLE_MODE(mode)) }.is_ok()
}

#[cfg(windows)]
fn forget_pre_raw_console_mode() {
    SAVED_CONSOLE_MODE_VALID.store(0, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Terminal initialisation / teardown
// ---------------------------------------------------------------------------

/* C: (new in Rust port) terminal_init: replaces initscr() + refresh() */
pub fn terminal_init() -> io::Result<()> {
    if TERMINAL_STATE.load(Ordering::SeqCst) == TERMINAL_READY {
        return Ok(());
    }

    // A previous partial initialization should have cleaned itself up.  Be
    // defensive if a backend returned an error while doing so.
    if TERMINAL_STATE.load(Ordering::SeqCst) != 0 {
        terminal_exit()?;
    }

    #[cfg(unix)]
    remember_pre_raw_termios();
    #[cfg(windows)]
    remember_pre_raw_console_mode();
    // Publish the intended transition before calling into the backend.  A
    // fatal signal or panic immediately after the kernel switches modes must
    // already know that raw-mode restoration is required.
    TERMINAL_STATE.fetch_or(RAW_MODE_ACTIVE, Ordering::SeqCst);
    if let Err(error) = terminal::enable_raw_mode() {
        #[cfg(unix)]
        signal_safe_terminal_restore();
        #[cfg(not(unix))]
        emergency_terminal_restore();
        return Err(error);
    }

    // Mark each transition before issuing it.  A write or flush can report an
    // error after the terminal consumed the escape sequence, so cleanup must
    // conservatively issue the inverse operation on every error path.
    TERMINAL_STATE.fetch_or(ALTERNATE_SCREEN_ACTIVE, Ordering::SeqCst);
    if let Err(e) = execute!(out(), EnterAlternateScreen) {
        let _ = terminal_exit();
        return Err(e);
    }

    TERMINAL_STATE.fetch_or(CURSOR_HIDDEN, Ordering::SeqCst);
    if let Err(e) = execute!(out(), Hide) {
        let _ = terminal_exit();
        return Err(e);
    }

    Ok(())
}

/* C: (new in Rust port) terminal_exit: replaces endwin() */
pub fn terminal_exit() -> io::Result<()> {
    // Keep the bits published until teardown finishes.  If any operation
    // panics unexpectedly, the panic hook still sees the original state and
    // can run the direct-output fallback.
    let active_state = TERMINAL_STATE.load(Ordering::SeqCst);
    if active_state == 0 {
        return Ok(());
    }
    let plan = cleanup_plan(active_state);
    let mut remaining_state = active_state;

    let mut first_error: Option<io::Error> = None;

    // Do not combine these commands: if showing the cursor fails, leaving the
    // alternate screen must still be attempted.
    if plan.show_cursor {
        if let Err(e) = execute!(out(), Show) {
            first_error = Some(e);
        } else {
            remaining_state &= !CURSOR_HIDDEN;
        }
    }

    if plan.leave_alternate_screen {
        if let Err(e) = execute!(out(), LeaveAlternateScreen) {
            if first_error.is_none() {
                first_error = Some(e);
            }
        } else {
            remaining_state &= !ALTERNATE_SCREEN_ACTIVE;
        }
    }

    if plan.disable_raw_mode {
        if let Err(e) = terminal::disable_raw_mode() {
            if first_error.is_none() {
                first_error = Some(e);
            }
        } else {
            remaining_state &= !RAW_MODE_ACTIVE;
            #[cfg(unix)]
            forget_pre_raw_termios();
            #[cfg(windows)]
            forget_pre_raw_console_mode();
        }
    }

    TERMINAL_STATE.store(remaining_state, Ordering::SeqCst);

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Whether terminal initialization changed any process-visible terminal state.
/// This includes partial initialization, which callers must treat as active so
/// that bracketed paste and other modes are unwound conservatively.
pub fn terminal_is_active() -> bool {
    TERMINAL_STATE.load(Ordering::SeqCst) != 0
}

/// Best-effort terminal restoration that does not borrow the shared painter.
///
/// A panic can occur while `OUT` is mutably borrowed, so the ordinary teardown
/// path could itself panic with a `RefCell` borrow error.  This fallback writes
/// through a fresh stdout handle, then restores raw mode.  All operations are
/// deliberately fallible and ignored: a panic hook must never panic again.
pub fn emergency_terminal_restore() {
    let active_state = TERMINAL_STATE.swap(0, Ordering::SeqCst);
    if active_state == 0 {
        return;
    }
    let plan = cleanup_plan(active_state);

    let mut output = stdout();
    // Bracketed paste is enabled by nano after winio initialization.  Sending
    // its disable sequence is harmless if initialization failed before then.
    let _ = output.write_all(b"\x1B[?2004l");

    if plan.show_cursor {
        let _ = execute!(output, Show);
    }
    if plan.leave_alternate_screen {
        let _ = execute!(output, LeaveAlternateScreen);
    }
    let _ = output.flush();

    if plan.disable_raw_mode {
        #[cfg(windows)]
        let restored_directly = restore_pre_raw_console_mode();
        #[cfg(not(windows))]
        let restored_directly = false;

        if !restored_directly {
            let _ = terminal::disable_raw_mode();
        }
        #[cfg(unix)]
        forget_pre_raw_termios();
    }
}

/// Restore terminal state from a fatal Unix signal without allocation, locks,
/// buffered Rust I/O, or access to editor state.
///
/// `write`, `open`, `tcgetattr`, `tcsetattr`, and `close` are specified as
/// async-signal-safe by POSIX.  The exact pre-raw termios snapshot is published
/// through atomics before signal handlers are installed.
#[cfg(unix)]
pub fn signal_safe_terminal_restore() {
    let active_state = TERMINAL_STATE.swap(0, Ordering::SeqCst);
    if active_state == 0 {
        return;
    }

    const RESTORE_DISPLAY: &[u8] = b"\x1B[?2004l\x1B[?25h\x1B[?1049l";
    unsafe {
        let _ = libc::write(
            libc::STDOUT_FILENO,
            RESTORE_DISPLAY.as_ptr().cast(),
            RESTORE_DISPLAY.len(),
        );
    }

    if active_state & RAW_MODE_ACTIVE == 0 {
        return;
    }

    const TTY: &[u8] = b"/dev/tty\0";
    let mut fd = libc::STDIN_FILENO;
    let mut close_fd = false;
    let mut current: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut current) != 0 } {
        fd = unsafe { libc::open(TTY.as_ptr().cast(), libc::O_RDWR | libc::O_NOCTTY) };
        if fd < 0 {
            return;
        }
        close_fd = true;
        if unsafe { libc::tcgetattr(fd, &mut current) != 0 } {
            unsafe { libc::close(fd) };
            return;
        }
    }

    if SAVED_TERMIOS_VALID.swap(0, Ordering::AcqRel) != 0 {
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut settings as *mut libc::termios).cast::<u8>(),
                std::mem::size_of::<libc::termios>(),
            )
        };
        for (byte, slot) in bytes.iter_mut().zip(SAVED_TERMIOS.iter()) {
            *byte = slot.load(Ordering::Relaxed);
        }
        unsafe {
            let _ = libc::tcsetattr(fd, libc::TCSANOW, &settings);
        }
    } else {
        // If the exact snapshot could not be captured, reverse the flags
        // crossterm clears for raw mode so the user still gets a usable tty.
        current.c_iflag |= libc::BRKINT | libc::ICRNL | libc::IXON;
        current.c_oflag |= libc::OPOST;
        current.c_lflag |= libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG;
        unsafe {
            let _ = libc::tcsetattr(fd, libc::TCSANOW, &current);
        }
    }

    if close_fd {
        unsafe {
            libc::close(fd);
        }
    }
}

/// Install one process-wide panic fallback while preserving Rust's existing
/// panic reporter.  Calling this repeatedly is safe.
pub fn install_terminal_panic_hook() {
    INSTALL_PANIC_HOOK.call_once(|| {
        IS_TERMINAL_UI_THREAD.with(|is_ui_thread| is_ui_thread.set(true));
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let is_ui_thread = IS_TERMINAL_UI_THREAD.with(Cell::get);
            if panic_should_restore_terminal(is_ui_thread) {
                #[cfg(unix)]
                signal_safe_terminal_restore();
                #[cfg(not(unix))]
                emergency_terminal_restore();
            }
            previous_hook(panic_info);
        }));
    });
}

#[inline]
fn panic_should_restore_terminal(is_ui_thread: bool) -> bool {
    // In abort builds every panic terminates the process, regardless of which
    // thread panicked, so leaving the user's terminal altered is never safe.
    is_ui_thread || cfg!(panic = "abort")
}

/* C: (new in Rust port) terminal_size() -> (cols, rows) */
pub fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/* C: regenerate_screen() / recalculate_screensize() */
pub fn recalculate_screensize() {
    let (cols, rows) = terminal_size();
    with_state_mut(|s| {
        let layout = calculate_window_layout(
            cols,
            rows,
            s.flag_isset(NO_HELP),
            s.flag_isset(ZERO),
            s.flag_isset(MINIBAR),
            s.flag_isset(EMPTY_LINE),
        );

        s.topwin = NanoWindow {
            rows: layout.top_rows,
            cols,
            y: 0,
            x: 0,
        };
        s.midwin = NanoWindow {
            rows: layout.mid_rows,
            cols,
            y: layout.mid_y,
            x: 0,
        };
        s.footwin = NanoWindow {
            rows: layout.foot_rows,
            cols,
            y: layout.foot_y,
            x: 0,
        };
        s.editwinrows = layout.mid_rows as i32;

        let decorations = s.margin.max(0) as u16 + s.sidebar.max(0) as u16;
        s.editwincols = cols.saturating_sub(decorations).max(1) as i32;
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowLayout {
    top_rows: u16,
    mid_rows: u16,
    mid_y: u16,
    foot_rows: u16,
    foot_y: u16,
}

/// Compute nano's three-window geometry without touching the terminal.
///
/// This mirrors GNU nano's `window_init()`, including the overlapping
/// one-line layout and the ZERO/MINIBAR/EMPTY_LINE height rules.  Keeping the
/// arithmetic in `u16` with saturating operations also makes one-column and
/// very short synthetic terminals safe to exercise in tests.
fn calculate_window_layout(
    _cols: u16,
    rows: u16,
    no_help: bool,
    zero: bool,
    minibar: bool,
    empty_line: bool,
) -> WindowLayout {
    if rows < 3 {
        let mid_rows = if zero { rows } else { rows.min(1) };
        return WindowLayout {
            top_rows: 0,
            mid_rows,
            mid_y: 0,
            foot_rows: rows.min(1),
            foot_y: rows.saturating_sub(1),
        };
    }

    let minimum = if zero { 3 } else if minibar { 4 } else { 5 };
    let mut top_rows = if empty_line && rows > minimum { 2 } else { 1 };
    let foot_rows = if no_help || rows < minimum { 1 } else { 3 };

    if minibar || zero {
        top_rows = 0;
    }

    let mid_rows = rows
        .saturating_sub(top_rows)
        .saturating_sub(foot_rows)
        .saturating_add(u16::from(zero));

    WindowLayout {
        top_rows,
        mid_rows,
        mid_y: top_rows,
        foot_rows,
        foot_y: rows.saturating_sub(foot_rows),
    }
}

/* C: window_init() in nano.c — set up topwin/midwin/footwin from terminal size */
pub fn window_init() {
    recalculate_screensize();
}

/// Return the physical screen height represented by the current windows.
/// ZERO mode intentionally overlaps the edit window and status bar by one
/// row, so summing window heights would overcount.
pub fn screen_rows() -> u16 {
    with_state(|s| {
        let top = s.topwin.y.saturating_add(s.topwin.rows);
        let middle = s.midwin.y.saturating_add(s.midwin.rows);
        let footer = s.footwin.y.saturating_add(s.footwin.rows);
        top.max(middle).max(footer)
    })
}

/// Complete the terminal-wide part of a resize at an input-loop safe point.
///
/// The signal/event handlers only publish a request.  Every interactive view
/// calls this coordinator after it regains control, so terminal dimensions and
/// window geometry are rebuilt exactly once before that view recalculates its
/// own derived layout (prompt width, help wrapping, or browser piles).
pub fn consume_resize_request(input: Option<i32>) -> bool {
    let pending = crate::nano::THE_WINDOW_RESIZED.load(Ordering::SeqCst);
    if !resize_was_requested(input, pending) {
        return false;
    }

    // `regenerate_screen()` clears the request before rebuilding.  A second
    // SIGWINCH that arrives during the rebuild therefore remains pending for
    // the next safe point instead of being lost.
    crate::nano::regenerate_screen();
    RESIZE_GENERATION.fetch_add(1, Ordering::SeqCst);

    true
}

/// Monotonic token used by an outer view to notice a resize consumed by a
/// nested prompt or help screen.
pub fn resize_generation() -> usize {
    RESIZE_GENERATION.load(Ordering::SeqCst)
}

#[inline]
fn resize_was_requested(input: Option<i32>, pending: bool) -> bool {
    match input {
        // Never discard a real keystroke merely because a signal became
        // pending at the same time.  Its resize event (or the next explicit
        // safe-point check) will consume the request without losing input.
        Some(keycode) => keycode == THE_WINDOW_RESIZED as i32,
        None => pending,
    }
}

// ---------------------------------------------------------------------------
// Macro recording (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void record_macro(void) */
#[cfg(not(feature = "tiny"))]
pub fn record_macro() {
    let outcome = toggle_macro_recording();
    statusline(MessageType::Remark, match outcome {
        MacroRecordingOutcome::Started => "Recording a macro...",
        MacroRecordingOutcome::Cancelled => "Cancelled",
        MacroRecordingOutcome::Stopped => "Stopped recording",
    });

    if state().flag_isset(STATEFLAGS) {
        titlebar(None);
    }
}

#[cfg(not(feature = "tiny"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacroRecordingOutcome {
    Started,
    Cancelled,
    Stopped,
}

#[cfg(not(feature = "tiny"))]
fn toggle_macro_recording() -> MacroRecordingOutcome {
    if !tl_get!(RECORDING) {
        let old_macro = MACRO_BUFFER.with(|mb| std::mem::take(&mut *mb.borrow_mut()));
        PREVIOUS_MACRO.with(|previous| *previous.borrow_mut() = Some(old_macro));
        tl_set!(MILESTONE, 0);
        tl_set!(RECORDING, true);
        return MacroRecordingOutcome::Started;
    }

    tl_set!(RECORDING, false);
    let milestone = tl_get!(MILESTONE);

    if milestone == 0 {
        let previous = PREVIOUS_MACRO
            .with(|saved| saved.borrow_mut().take())
            .unwrap_or_default();
        MACRO_BUFFER.with(|mb| *mb.borrow_mut() = previous);
        MacroRecordingOutcome::Cancelled
    } else {
        MACRO_BUFFER.with(|mb| mb.borrow_mut().truncate(milestone));
        PREVIOUS_MACRO.with(|saved| {
            saved.borrow_mut().take();
        });
        MacroRecordingOutcome::Stopped
    }
}

/* C: void add_to_macrobuffer(int code) */
#[cfg(not(feature = "tiny"))]
pub fn add_to_macrobuffer(code: i32) {
    MACRO_BUFFER.with(|mb| mb.borrow_mut().push(code));
}

/* C: void run_macro(void) */
#[cfg(not(feature = "tiny"))]
pub fn run_macro() {
    let is_rec = tl_get!(RECORDING);
    if is_rec {
        statusline(MessageType::Ahem, "Cannot run macro while recording");
        let milestone = tl_get!(MILESTONE);
        MACRO_BUFFER.with(|mb| mb.borrow_mut().truncate(milestone));
        return;
    }
    let mac_len = MACRO_BUFFER.with(|mb| mb.borrow().len());
    if mac_len == 0 {
        statusline(MessageType::Ahem, "Macro is empty");
        return;
    }
    let codes: Vec<i32> = MACRO_BUFFER.with(|mb| mb.borrow().clone());
    for &code in codes.iter().rev() {
        put_back(code);
    }
    state_mut().mute_modifiers = true;
}

// ---------------------------------------------------------------------------
// Key buffer management
// ---------------------------------------------------------------------------

/// Ensure the key buffer has at least `newsize` capacity.
/* C: void reserve_space_for(size_t newsize) */
#[allow(dead_code)] // parity: C calls this from implant()/macro recording — not yet wired
fn reserve_space_for(newsize: usize) {
    KEY_BUFFER.with(|kb| {
        let mut buf = kb.borrow_mut();
        let current_cap = buf.capacity();
        if current_cap < newsize {
            let additional = newsize - current_cap;
            buf.reserve(additional);
        }
    });
}

/* C: size_t waiting_keycodes(void) */
pub fn waiting_keycodes() -> usize {
    tl_get!(WAITING_CODES)
}

/* C: void put_back(int keycode) */
pub fn put_back(keycode: i32) {
    KEY_BUFFER.with(|kb| {
        NEXTCODES_IDX.with(|ni| {
            WAITING_CODES.with(|wc| {
                let mut buf = kb.borrow_mut();
                let mut idx = ni.borrow_mut();
                let mut waiting = wc.borrow_mut();

                if *idx == 0 {
                    // No room at the head; shift everything right by one
                    buf.insert(0, keycode);
                    // idx stays 0
                } else {
                    *idx -= 1;
                    buf[*idx] = keycode;
                }
                *waiting += 1;
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Implantation (ENABLE_NANORC)
// ---------------------------------------------------------------------------

/* C: void implant(const char *string) */
#[cfg(feature = "nanorc")]
pub fn implant(string: &str) {
    PLANTS_POINTER.with(|pp| {
        *pp.borrow_mut() = Some((string.to_string(), 0));
    });
    put_back(MORE_PLANTS as i32);
    state_mut().mute_modifiers = true;
}

/* C: int get_code_from_plantation(void) */
#[cfg(feature = "nanorc")]
pub fn get_code_from_plantation() -> i32 {
    PLANTS_POINTER.with(|pp| {
        let mut opt = pp.borrow_mut();
        if let Some((ref s, ref mut pos)) = *opt {
            let bytes = s.as_bytes();
            if *pos >= bytes.len() {
                return ERR_CODE;
            }

            if bytes[*pos] == b'{' {
                // Handle the documented literal-brace spellings first.  In
                // particular, `{}}` has an empty substring before the first
                // closing brace, so deriving this from `inner` would miss it.
                if matches!(bytes.get(*pos + 1), Some(b'{') | Some(b'}')) {
                    if bytes.get(*pos + 2) != Some(&b'}') {
                        return MISSING_BRACE as i32;
                    }
                    let ch = bytes[*pos + 1] as i32;
                    *pos += 3;
                    if *pos < bytes.len() {
                        put_back(MORE_PLANTS as i32);
                    }
                    return ch;
                }

                // Find closing brace
                let rest = &s[*pos + 1..];
                if let Some(close_idx) = rest.find('}') {
                    let inner = &rest[..close_idx];
                    // It's a command name
                    let cmd = inner.to_string();
                    *pos += 2 + close_idx; // skip {inner}
                    if *pos < bytes.len() {
                        put_back(MORE_PLANTS as i32);
                    }

                    // Resolve the command name exactly as a normal nanorc
                    // function bind does.  Key labels ("^B", "Left", ...)
                    // are display strings, not function identifiers, and
                    // matching against them made expansions such as `{left}`
                    // fail at runtime.
                    let planted = crate::rcfile::strtosc(&cmd);
                    with_state_mut(|st| {
                        st.commandname = Some(cmd.clone());
                        st.planted_shortcut = planted.map(|mut shortcut| {
                            // Keep one hidden transient entry so the existing
                            // command dispatcher can retrieve both the function
                            // and toggle metadata for PLANTED_A_COMMAND.
                            shortcut.keystr = "";
                            shortcut.keycode = PLANTED_A_COMMAND as i32;
                            shortcut.menus = st.currmenu as i32;

                            if let Some(index) = st.sclist.iter().position(|entry| {
                                entry.keystr.is_empty()
                                    && entry.keycode == PLANTED_A_COMMAND as i32
                            }) {
                                st.sclist[index] = shortcut;
                                index
                            } else {
                                st.sclist.push(shortcut);
                                st.sclist.len() - 1
                            }
                        });
                    });
                    if with_state(|st| st.planted_shortcut.is_none()) {
                        return NO_SUCH_FUNCTION as i32;
                    }
                    return PLANTED_A_COMMAND as i32;
                } else {
                    return MISSING_BRACE as i32;
                }
            } else {
                // Plain character run
                let start = *pos;
                let next_brace = s[start..].find('{').map(|i| start + i);
                let end = next_brace.unwrap_or(bytes.len());
                let chunk = &bytes[start..end];

                if chunk.is_empty() {
                    *pos = end;
                    return ERR_CODE;
                }

                let first = chunk[0] as i32;
                // Queue remaining bytes in reverse
                for i in (1..chunk.len()).rev() {
                    put_back(chunk[i] as i32);
                }
                *pos = end;

                if next_brace.is_some() {
                    put_back(MORE_PLANTS as i32);
                }

                return if first == 0 { ERR_CODE } else { first };
            }
        }
        ERR_CODE
    })
}

// ---------------------------------------------------------------------------
// Core input: get_input reads one code from the buffer
// ---------------------------------------------------------------------------

/* C: int get_input(WINDOW *frame) — frame==NULL means don't block for more */
pub fn get_input(frame: Option<()>) -> i32 {
    let waiting = tl_get!(WAITING_CODES);

    if waiting > 0 {
        state_mut().spotlighted = false;
    } else if frame.is_some() {
        read_keys_from();
    }

    let waiting = tl_get!(WAITING_CODES);
    if waiting > 0 {
        let code = KEY_BUFFER.with(|kb| {
            NEXTCODES_IDX.with(|ni| {
                WAITING_CODES.with(|wc| {
                    let buf = kb.borrow();
                    let mut idx = ni.borrow_mut();
                    let mut w = wc.borrow_mut();
                    *w -= 1;
                    let code = buf[*idx];
                    *idx += 1;
                    code
                })
            })
        });

        // Expanding a plantation can put additional bytes back into the key
        // buffer.  Do it only after the buffer/index/count borrow guards above
        // have been dropped.
        #[cfg(feature = "nanorc")]
        if code == MORE_PLANTS as i32 {
            return get_code_from_plantation();
        }

        code
    } else {
        ERR_CODE
    }
}

// ---------------------------------------------------------------------------
// read_keys_from: the crossterm-based replacement for ncurses read_keys_from()
// This is the ONLY function that fundamentally changes from the C code.
// It translates crossterm Events into the int codes ncurses would buffer.
// ---------------------------------------------------------------------------

/* C: void read_keys_from(WINDOW *frame) */
pub fn read_keys_from() {
    let mut stdout = out();

    // Flush any pending output before blocking
    let _ = stdout.flush();

    // Show cursor if appropriate
    let reveal = tl_get!(REVEAL_CURSOR);
    let spotlight = state().spotlighted;
    let show_cursor_flag = state().flag_isset(SHOW_CURSOR);
    let currmenu = state().currmenu;
    let lastmessage = state().lastmessage;
    let lines = screen_rows();

    if reveal && (!spotlight || show_cursor_flag || currmenu == MSPELL)
        && (lines > 1 || lastmessage <= MessageType::Hush)
    {
        let _ = execute!(stdout, Show);
    }

    // Wait for the first event.  A HUP/TERM or suspend published by a signal
    // handler must not sit until the next keystroke (GNU nano dies promptly
    // on SIGTERM), so block in short poll slices and surface deferred signal
    // work between them.  Poll errors are EINTR from those same signals.
    let first_event = loop {
        crate::nano::process_pending_signal_requests();
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(ev) => break ev,
                Err(_) => {
                    // Treat unrecoverable read failure as resize
                    break Event::Resize(80, 24);
                }
            },
            _ => continue,
        }
    };

    let _ = execute!(stdout, Hide);

    // Initialise buffer
    KEY_BUFFER.with(|kb| kb.borrow_mut().clear());
    NEXTCODES_IDX.with(|ni| *ni.borrow_mut() = 0);
    WAITING_CODES.with(|wc| *wc.borrow_mut() = 0);

    #[cfg(not(feature = "tiny"))]
    {
        // Remember where this terminal burst began.  If it contains the key
        // that stops recording, record_macro() truncates back to this point.
        let macro_len = MACRO_BUFFER.with(|mb| mb.borrow().len());
        tl_set!(MILESTONE, macro_len);
    }

    // Translate the first event and push codes.
    translate_event(first_event);

    // Drain only the events that are already buffered; do NOT wait for new
    // input. A non-zero timeout here is a "wait for the next keystroke" that
    // stalls the whole loop during fast typing or held keys (each repeat
    // arrives within the window, so we never return to repaint until you
    // pause). ZERO grabs same-write bursts (paste, escape sequences) that have
    // already arrived, then returns immediately — matching the idiom used
    // elsewhere in this file (see the non-blocking drains below).
    loop {
        match event::poll(Duration::ZERO) {
            Ok(true) => {
                match event::read() {
                    Ok(ev) => translate_event(ev),
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }
}

/// Push one i32 keycode into the key buffer.
fn push_keycode(code: i32) {
    push_keycode_impl(code, true);
}

fn push_keycode_impl(code: i32, _record: bool) {
    KEY_BUFFER.with(|kb| {
        WAITING_CODES.with(|wc| {
            let mut buf = kb.borrow_mut();
            let idx = NEXTCODES_IDX.with(|ni| *ni.borrow());
            // If the buffer has been partially consumed, we need to append at the live end
            let insert_pos = idx + *wc.borrow();
            if insert_pos < buf.len() {
                buf[insert_pos] = code;
            } else {
                buf.push(code);
            }
            *wc.borrow_mut() += 1;
        });
    });

    #[cfg(not(feature = "tiny"))]
    if _record && tl_get!(RECORDING) {
        add_to_macrobuffer(code);
    }
}

/// Push a synthetic editor event that must not become part of a macro.
fn push_unrecorded_keycode(code: i32) {
    push_keycode_impl(code, false);
}

/// Translate a crossterm Event into nano key code(s) and push them.
fn translate_event(ev: Event) {
    match ev {
        Event::Key(ke) if ke.kind == KeyEventKind::Press || ke.kind == KeyEventKind::Repeat => {
            translate_key_event(ke);
        }
        #[cfg(feature = "mouse")]
        Event::Mouse(me) => {
            translate_mouse_event(me);
        }
        Event::Paste(text) => {
            push_keycode(START_OF_PASTE as i32);
            for byte in text.bytes() {
                push_keycode(byte as i32);
            }
            push_keycode(END_OF_PASTE as i32);
        }
        Event::Resize(_w, _h) => {
            crate::nano::THE_WINDOW_RESIZED.store(true, std::sync::atomic::Ordering::SeqCst);
            push_unrecorded_keycode(THE_WINDOW_RESIZED as i32);
        }
        Event::FocusGained => {
            push_keycode(FOCUS_IN as i32);
        }
        Event::FocusLost => {
            push_keycode(FOCUS_OUT as i32);
        }
        _ => {}
    }
}

/// Map a crossterm KeyEvent into nano's integer key code(s).
fn translate_key_event(ke: KeyEvent) {
    let ctrl = ke.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = ke.modifiers.contains(KeyModifiers::ALT);
    let shift = ke.modifiers.contains(KeyModifiers::SHIFT);

    match ke.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Use nano's canonical mapping for letters, digits, space,
                // slash, brackets, underscore and question mark.
                if c.is_ascii() {
                    push_keycode(convert_to_control(c as i32));
                } else {
                    let mut buf = [0u8; 4];
                    for &byte in c.encode_utf8(&mut buf).as_bytes() {
                        push_keycode(byte as i32);
                    }
                }
            } else if alt {
                // Push ESC + character (standard nano escape-sequence convention)
                // For lowercase letters with shift, or uppercase without shift-metas,
                // we push the lowercase form.
                let ch = if shift && c.is_ascii_alphabetic() && !state().shifted_metas {
                    c.to_ascii_lowercase()
                } else {
                    c
                };
                push_keycode(ESC);
                push_keycode(ch as i32);
            } else if c.is_ascii() {
                // Plain ASCII character — use the byte value directly.
                push_keycode(c as i32);
            } else {
                // Non-ASCII Unicode character (multi-byte UTF-8).
                // Push each UTF-8 byte as a separate keycode so they accumulate
                // in the PUDDLE together and are injected as one valid sequence.
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                for &b in s.as_bytes() {
                    push_keycode(b as i32);
                }
            }
        }
        KeyCode::Enter => {
            if alt {
                push_keycode(ESC);
                push_keycode('\r' as i32);
            } else {
                push_keycode('\r' as i32);
            }
        }
        KeyCode::Tab => {
            if shift || alt {
                push_keycode(SHIFT_TAB as i32);
            } else {
                push_keycode('\t' as i32);
            }
        }
        KeyCode::Backspace => {
            if alt {
                push_keycode(ESC);
                push_keycode(KEY_BACKSPACE);
            } else {
                push_keycode(KEY_BACKSPACE);
            }
        }
        KeyCode::Delete => {
            if ctrl && shift {
                push_keycode(CONTROL_SHIFT_DELETE as i32);
            } else if ctrl {
                push_keycode(CONTROL_DELETE as i32);
            } else if shift {
                push_keycode(SHIFT_DELETE as i32);
            } else if alt {
                push_keycode(ALT_DELETE as i32);
            } else {
                push_keycode(KEY_DC);
            }
        }
        KeyCode::Insert => {
            if alt {
                push_keycode(ALT_INSERT as i32);
            } else {
                push_keycode(KEY_IC);
            }
        }
        KeyCode::Up => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_UP as i32);
            } else if ctrl {
                push_keycode(CONTROL_UP as i32);
            } else if shift && alt {
                push_keycode(SHIFT_ALT_UP as i32);
            } else if shift {
                push_keycode(SHIFT_UP as i32);
            } else if alt {
                push_keycode(ALT_UP as i32);
            } else {
                push_keycode(KEY_UP);
            }
        }
        KeyCode::Down => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_DOWN as i32);
            } else if ctrl {
                push_keycode(CONTROL_DOWN as i32);
            } else if shift && alt {
                push_keycode(SHIFT_ALT_DOWN as i32);
            } else if shift {
                push_keycode(SHIFT_DOWN as i32);
            } else if alt {
                push_keycode(ALT_DOWN as i32);
            } else {
                push_keycode(KEY_DOWN);
            }
        }
        KeyCode::Left => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_LEFT as i32);
            } else if ctrl {
                push_keycode(CONTROL_LEFT as i32);
            } else if shift && alt {
                push_keycode(SHIFT_ALT_LEFT as i32);
            } else if shift {
                push_keycode(SHIFT_LEFT_CODE);
            } else if alt {
                push_keycode(ALT_LEFT as i32);
            } else {
                push_keycode(KEY_LEFT);
            }
        }
        KeyCode::Right => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_RIGHT as i32);
            } else if ctrl {
                push_keycode(CONTROL_RIGHT as i32);
            } else if shift && alt {
                push_keycode(SHIFT_ALT_RIGHT as i32);
            } else if shift {
                push_keycode(SHIFT_RIGHT_CODE);
            } else if alt {
                push_keycode(ALT_RIGHT as i32);
            } else {
                push_keycode(KEY_RIGHT);
            }
        }
        KeyCode::Home => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_HOME as i32);
            } else if ctrl {
                push_keycode(CONTROL_HOME as i32);
            } else if shift && alt {
                push_keycode(SHIFT_HOME as i32);
            } else if shift {
                push_keycode(SHIFT_HOME as i32);
            } else if alt {
                push_keycode(ALT_HOME as i32);
            } else {
                push_keycode(KEY_HOME);
            }
        }
        KeyCode::End => {
            if ctrl && shift {
                push_keycode(SHIFT_CONTROL_END as i32);
            } else if ctrl {
                push_keycode(CONTROL_END as i32);
            } else if shift && alt {
                push_keycode(SHIFT_END as i32);
            } else if shift {
                push_keycode(SHIFT_END as i32);
            } else if alt {
                push_keycode(ALT_END as i32);
            } else {
                push_keycode(KEY_END);
            }
        }
        KeyCode::PageUp => {
            if shift && alt {
                push_keycode(SHIFT_PAGEUP as i32);
            } else if shift {
                push_keycode(SHIFT_PAGEUP as i32);
            } else if alt {
                push_keycode(ALT_PAGEUP as i32);
            } else {
                push_keycode(KEY_PPAGE);
            }
        }
        KeyCode::PageDown => {
            if shift && alt {
                push_keycode(SHIFT_PAGEDOWN as i32);
            } else if shift {
                push_keycode(SHIFT_PAGEDOWN as i32);
            } else if alt {
                push_keycode(ALT_PAGEDOWN as i32);
            } else {
                push_keycode(KEY_NPAGE);
            }
        }
        KeyCode::F(n) => {
            if alt {
                push_keycode(ESC);
                push_keycode(key_f(n as i32));
            } else {
                push_keycode(key_f(n as i32));
            }
        }
        KeyCode::Esc => {
            push_keycode(ESC);
        }
        KeyCode::Null => {
            push_keycode(0);
        }
        _ => {
            // Unknown key; ignore
        }
    }
}

// Thread-local storage for the last mouse event (replaces C's global MEVENT).
#[cfg(feature = "mouse")]
thread_local! {
    static LAST_MOUSE_EVENT: RefCell<Option<NanoMouseEvent>> = RefCell::new(None);
}

/// Translate a crossterm mouse event into a nano mouse event code.
#[cfg(feature = "mouse")]
fn translate_mouse_event(me: MouseEvent) {
    let ev = NanoMouseEvent {
        y: me.row,
        x: me.column,
        bstate: match me.kind {
            MouseEventKind::Down(MouseButton::Left)   => BUTTON1_CLICKED,
            MouseEventKind::Up(MouseButton::Left)     => BUTTON1_RELEASED,
            MouseEventKind::ScrollUp                  => BUTTON4_PRESSED,
            MouseEventKind::ScrollDown                => BUTTON5_PRESSED,
            _ => 0,
        },
    };
    LAST_MOUSE_EVENT.with(|m| *m.borrow_mut() = Some(ev));
    push_keycode(KEY_MOUSE_CODE);
}

// Mouse event codes — these parallel ncurses BUTTON constants
#[cfg(feature = "mouse")]
pub const BUTTON1_RELEASED: u32 = 0x0001;
#[cfg(feature = "mouse")]
pub const BUTTON1_CLICKED:  u32 = 0x0004;
#[cfg(feature = "mouse")]
pub const BUTTON4_PRESSED:  u32 = 0x0800;
#[cfg(feature = "mouse")]
pub const BUTTON5_PRESSED:  u32 = 0x8000;
#[cfg(feature = "mouse")]
pub const KEY_MOUSE_CODE:   i32 = 0x199; // ncurses KEY_MOUSE

#[cfg(feature = "mouse")]
#[derive(Debug, Clone, Default)]
pub struct NanoMouseEvent {
    pub y: u16,
    pub x: u16,
    pub bstate: u32,
}

// ---------------------------------------------------------------------------
// Escape sequence parsers (kept for completeness; rarely fired since
// crossterm pre-decodes most sequences)
// ---------------------------------------------------------------------------

/* C: int arrow_from_ABCD(int letter) */
pub fn arrow_from_ABCD(letter: i32) -> i32 {
    if letter < 'C' as i32 {
        if letter == 'A' as i32 { KEY_UP } else { KEY_DOWN }
    } else {
        if letter == 'D' as i32 { KEY_LEFT } else { KEY_RIGHT }
    }
}

/* C: int convert_SS3_sequence(const int *seq, size_t length, int *consumed) */
pub fn convert_SS3_sequence(seq: &[i32], length: usize, consumed: &mut i32) -> i32 {
    if seq.is_empty() {
        return FOREIGN_SEQUENCE as i32;
    }
    match seq[0] as u8 as char {
        '1' => {
            if length > 3 && seq[1] == ';' as i32 {
                *consumed = 4;
                #[cfg(not(feature = "tiny"))]
                if length > 3 {
                    match seq[2] as u8 as char {
                        '2' if length > 3 && seq[3] >= 'A' as i32 && seq[3] <= 'D' as i32 => {
                            state_mut().shift_held = true;
                            return arrow_from_ABCD(seq[3]);
                        }
                        '5' if length > 3 => {
                            match seq[3] as u8 as char {
                                'A' => return CONTROL_UP as i32,
                                'B' => return CONTROL_DOWN as i32,
                                'C' => return CONTROL_RIGHT as i32,
                                'D' => return CONTROL_LEFT as i32,
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        '2'..='8' => {
            if length > 1 {
                *consumed = 2;
                if seq[0] == '4' as i32 || seq[0] > '5' as i32 {
                    return FOREIGN_SEQUENCE as i32;
                }
                #[cfg(not(feature = "tiny"))]
                match seq[1] as u8 as char {
                    'A' => return CONTROL_UP as i32,
                    'B' => return CONTROL_DOWN as i32,
                    'C' => return CONTROL_RIGHT as i32,
                    'D' => return CONTROL_LEFT as i32,
                    _ => {}
                }
                return seq[1] - 0x40;
            }
        }
        'A' | 'B' | 'C' | 'D' => return arrow_from_ABCD(seq[0]),
        #[cfg(not(feature = "tiny"))]
        'F' => return KEY_END,
        #[cfg(not(feature = "tiny"))]
        'H' => return KEY_HOME,
        #[cfg(not(feature = "tiny"))]
        'M' => return KEY_ENTER,
        'P' | 'Q' | 'R' | 'S' => return key_f(seq[0] - 'O' as i32),
        #[cfg(not(feature = "tiny"))]
        'T'..='Y' => return key_f(seq[0] - 'O' as i32),
        #[cfg(not(feature = "tiny"))]
        'a' => return CONTROL_UP as i32,
        #[cfg(not(feature = "tiny"))]
        'b' => return CONTROL_DOWN as i32,
        #[cfg(not(feature = "tiny"))]
        'c' => return CONTROL_RIGHT as i32,
        #[cfg(not(feature = "tiny"))]
        'd' => return CONTROL_LEFT as i32,
        #[cfg(not(feature = "tiny"))]
        'j' => return '*' as i32,
        #[cfg(not(feature = "tiny"))]
        'k' => return '+' as i32,
        #[cfg(not(feature = "tiny"))]
        'l' => return ',' as i32,
        #[cfg(not(feature = "tiny"))]
        'm' => return '-' as i32,
        #[cfg(not(feature = "tiny"))]
        'n' => return KEY_DC,
        #[cfg(not(feature = "tiny"))]
        'o' => return '/' as i32,
        #[cfg(not(feature = "tiny"))]
        'p' => return KEY_IC,
        #[cfg(not(feature = "tiny"))]
        'q' => return KEY_END,
        #[cfg(not(feature = "tiny"))]
        'r' => return KEY_DOWN,
        #[cfg(not(feature = "tiny"))]
        's' => return KEY_NPAGE,
        #[cfg(not(feature = "tiny"))]
        't' => return KEY_LEFT,
        #[cfg(not(feature = "tiny"))]
        'v' => return KEY_RIGHT,
        #[cfg(not(feature = "tiny"))]
        'w' => return KEY_HOME,
        #[cfg(not(feature = "tiny"))]
        'x' => return KEY_UP,
        #[cfg(not(feature = "tiny"))]
        'y' => return KEY_PPAGE,
        _ => {}
    }
    FOREIGN_SEQUENCE as i32
}

/* C: int convert_CSI_sequence(const int *seq, size_t length, int *consumed) */
pub fn convert_CSI_sequence(seq: &[i32], length: usize, consumed: &mut i32) -> i32 {
    if seq.is_empty() {
        return FOREIGN_SEQUENCE as i32;
    }

    if seq[0] < '9' as i32 && length > 1 {
        *consumed = 2;
    }

    match seq[0] as u8 as char {
        '1' => {
            if length > 1 && seq[1] == '~' as i32 {
                return KEY_HOME;
            } else if length > 2 && seq[2] == '~' as i32 {
                *consumed = 3;
                match seq[1] as u8 as char {
                    #[cfg(not(feature = "tiny"))]
                    '1' | '2' | '3' | '4' => return key_f(seq[1] - '0' as i32),
                    '5' => return key_f(seq[1] - '0' as i32),
                    '7' | '8' | '9' => return key_f(seq[1] - '1' as i32),
                    _ => {}
                }
            } else if length > 3 && seq[1] == ';' as i32 {
                *consumed = 4;
                #[cfg(not(feature = "tiny"))]
                match seq[2] as u8 as char {
                    '2' => match seq[3] as u8 as char {
                        'A' | 'B' | 'C' | 'D' => {
                            state_mut().shift_held = true;
                            return arrow_from_ABCD(seq[3]);
                        }
                        'F' => return SHIFT_END as i32,
                        'H' => return SHIFT_HOME as i32,
                        _ => {}
                    },
                    '3' | '9' => match seq[3] as u8 as char {
                        'A' => return ALT_UP as i32,
                        'B' => return ALT_DOWN as i32,
                        'C' => return ALT_RIGHT as i32,
                        'D' => return ALT_LEFT as i32,
                        'F' => return ALT_END as i32,
                        'H' => return ALT_HOME as i32,
                        _ => {}
                    },
                    '4' => match seq[3] as u8 as char {
                        'A' => return SHIFT_PAGEUP as i32,
                        'B' => return SHIFT_PAGEDOWN as i32,
                        'C' => return SHIFT_END as i32,
                        'D' => return SHIFT_HOME as i32,
                        _ => {}
                    },
                    '5' => match seq[3] as u8 as char {
                        'A' => return CONTROL_UP as i32,
                        'B' => return CONTROL_DOWN as i32,
                        'C' => return CONTROL_RIGHT as i32,
                        'D' => return CONTROL_LEFT as i32,
                        'E' => return KEY_CENTER as i32,
                        'F' => return CONTROL_END as i32,
                        'H' => return CONTROL_HOME as i32,
                        _ => {}
                    },
                    '6' => {
                        let sc = with_state(|s| (
                            s.shiftcontrolup, s.shiftcontroldown,
                            s.shiftcontrolright, s.shiftcontrolleft,
                            s.shiftcontrolend, s.shiftcontrolhome,
                        ));
                        match seq[3] as u8 as char {
                            'A' => return sc.0,
                            'B' => return sc.1,
                            'C' => return sc.2,
                            'D' => return sc.3,
                            'F' => return sc.4,
                            'H' => return sc.5,
                            _ => {}
                        }
                    },
                    _ => {}
                }
            }
        }
        '2' => {
            if length > 2 && seq[2] == '~' as i32 {
                *consumed = 3;
                match seq[1] as u8 as char {
                    '0' => return key_f(9),
                    '1' => return key_f(10),
                    '3' => return key_f(11),
                    '4' => return key_f(12),
                    #[cfg(feature = "nanorc")]
                    '5' => return key_f(13),
                    #[cfg(feature = "nanorc")]
                    '6' => return key_f(14),
                    #[cfg(feature = "nanorc")]
                    '8' => return key_f(15),
                    #[cfg(feature = "nanorc")]
                    '9' => return key_f(16),
                    _ => {}
                }
            } else if length > 1 && seq[1] == '~' as i32 {
                return KEY_IC;
            } else if length > 3 && seq[1] == ';' as i32 && seq[3] == '~' as i32 {
                *consumed = 4;
                #[cfg(not(feature = "tiny"))]
                if seq[2] == '3' as i32 {
                    return ALT_INSERT as i32;
                }
            }
            #[cfg(not(feature = "tiny"))]
            if length > 3 && seq[1] == '0' as i32 && seq[3] == '~' as i32 {
                *consumed = 4;
                return if seq[2] == '0' as i32 { START_OF_PASTE as i32 } else { END_OF_PASTE as i32 };
            }
        }
        '3' => {
            if length > 1 && seq[1] == '~' as i32 {
                return KEY_DC;
            }
            if length > 3 && seq[1] == ';' as i32 && seq[3] == '~' as i32 {
                *consumed = 4;
                #[cfg(not(feature = "tiny"))]
                {
                    let sc_shiftdelete = SHIFT_DELETE as i32;
                    let sc_altdelete = ALT_DELETE as i32;
                    let sc_ctrldelete = CONTROL_DELETE as i32;
                    let sc_csd = state().controlshiftdelete;
                    match seq[2] as u8 as char {
                        '2' => return sc_shiftdelete,
                        '3' => return sc_altdelete,
                        '5' => return sc_ctrldelete,
                        '6' => return sc_csd,
                        _ => {}
                    }
                }
            }
            #[cfg(not(feature = "tiny"))]
            {
                if length > 1 && seq[1] == '$' as i32 { return SHIFT_DELETE as i32; }
                if length > 1 && seq[1] == '^' as i32 { return CONTROL_DELETE as i32; }
                if length > 1 && seq[1] == '@' as i32 {
                    return state().controlshiftdelete;
                }
            }
        }
        '4' => {
            if length > 1 && seq[1] == '~' as i32 { return KEY_END; }
        }
        '5' => {
            if length > 1 && seq[1] == '~' as i32 { return KEY_PPAGE; }
            #[cfg(not(feature = "tiny"))]
            if length > 3 && seq[1] == ';' as i32 && seq[3] == '~' as i32 {
                *consumed = 4;
                match seq[2] as u8 as char {
                    '2' => return state().shiftaltup,
                    '3' => return ALT_PAGEUP as i32,
                    _ => {}
                }
            }
        }
        '6' => {
            if length > 1 && seq[1] == '~' as i32 { return KEY_NPAGE; }
            #[cfg(not(feature = "tiny"))]
            if length > 3 && seq[1] == ';' as i32 && seq[3] == '~' as i32 {
                *consumed = 4;
                match seq[2] as u8 as char {
                    '2' => return state().shiftaltdown,
                    '3' => return ALT_PAGEDOWN as i32,
                    _ => {}
                }
            }
        }
        '7' => {
            if length > 1 {
                match seq[1] as u8 as char {
                    '~' => return KEY_HOME,
                    '$' => return SHIFT_HOME as i32,
                    '^' => return CONTROL_HOME as i32,
                    #[cfg(not(feature = "tiny"))]
                    '@' => return state().shiftcontrolhome,
                    _ => {}
                }
            }
        }
        '8' => {
            if length > 1 {
                match seq[1] as u8 as char {
                    '~' => return KEY_END,
                    '$' => return SHIFT_END as i32,
                    '^' => return CONTROL_END as i32,
                    #[cfg(not(feature = "tiny"))]
                    '@' => return state().shiftcontrolend,
                    _ => {}
                }
            }
        }
        '9' => return KEY_DC,
        '@' => return KEY_IC,
        'A' | 'B' | 'C' | 'D' => return arrow_from_ABCD(seq[0]),
        'F' => return KEY_END,
        'G' => return KEY_NPAGE,
        'H' => return KEY_HOME,
        'I' => return KEY_PPAGE,
        'L' => return KEY_IC,
        #[cfg(not(feature = "tiny"))]
        'M'..='T' => return key_f(seq[0] - 'L' as i32),
        'U' => return KEY_NPAGE,
        'V' => return KEY_PPAGE,
        #[cfg(not(feature = "tiny"))]
        'W' => return key_f(11),
        #[cfg(not(feature = "tiny"))]
        'X' => return key_f(12),
        'Y' => return KEY_END,
        'Z' => return SHIFT_TAB as i32,
        #[cfg(not(feature = "tiny"))]
        'a' | 'b' | 'c' | 'd' => {
            state_mut().shift_held = true;
            return arrow_from_ABCD(seq[0] - 0x20);
        }
        '[' => {
            if length > 1 {
                *consumed = 2;
                if seq[1] > '@' as i32 && seq[1] < 'F' as i32 {
                    return key_f(seq[1] - '@' as i32);
                }
            }
        }
        _ => {}
    }
    FOREIGN_SEQUENCE as i32
}

/* C: int parse_escape_sequence(int starter) */
pub fn parse_escape_sequence(starter: i32) -> i32 {
    let mut consumed = 1i32;
    let keycode;

    let seq_snapshot: Vec<i32> = KEY_BUFFER.with(|kb| {
        NEXTCODES_IDX.with(|ni| {
            WAITING_CODES.with(|wc| {
                let buf = kb.borrow();
                let idx = *ni.borrow();
                let w = *wc.borrow();
                buf[idx..idx + w].to_vec()
            })
        })
    });

    let length = seq_snapshot.len();

    if starter == 'O' as i32 {
        keycode = convert_SS3_sequence(&seq_snapshot, length, &mut consumed);
    } else if starter == '[' as i32 {
        keycode = convert_CSI_sequence(&seq_snapshot, length, &mut consumed);
    } else {
        keycode = FOREIGN_SEQUENCE as i32;
    }

    // Skip consumed elements
    KEY_BUFFER.with(|_| {
        NEXTCODES_IDX.with(|ni| {
            WAITING_CODES.with(|wc| {
                let mut idx = ni.borrow_mut();
                let mut w = wc.borrow_mut();
                let skip = consumed as usize;
                if skip <= *w {
                    *idx += skip;
                    *w -= skip;
                }
            });
        });
    });

    keycode
}

/* C: int assemble_byte_code(int keycode) */
pub fn assemble_byte_code(keycode: i32) -> i32 {
    let count = DIGIT_COUNT.with(|dc| {
        let mut d = dc.borrow_mut();
        *d += 1;
        *d
    });

    let byte_val = BYTE_ACC.with(|b| *b.borrow());

    if count == 1 {
        BYTE_ACC.with(|b| *b.borrow_mut() = (keycode - '0' as i32) * 100);
        return PROCEED as i32;
    }

    if count == 2 {
        if byte_val < 200 || keycode <= '5' as i32 {
            BYTE_ACC.with(|b| *b.borrow_mut() += (keycode - '0' as i32) * 10);
            return PROCEED as i32;
        } else {
            return keycode;
        }
    }

    // count == 3
    if byte_val < 250 || keycode <= '5' as i32 {
        let result = byte_val + keycode - '0' as i32;
        DIGIT_COUNT.with(|dc| *dc.borrow_mut() = 0);
        BYTE_ACC.with(|b| *b.borrow_mut() = 0);
        result
    } else {
        DIGIT_COUNT.with(|dc| *dc.borrow_mut() = 0);
        BYTE_ACC.with(|b| *b.borrow_mut() = 0);
        keycode
    }
}

/* C: int convert_to_control(int kbinput) */
pub fn convert_to_control(kbinput: i32) -> i32 {
    let k = kbinput as u8;
    match k {
        b'@'..=b'_' => kbinput - '@' as i32,
        b'`'..=b'~' => kbinput - '`' as i32,
        b'3'..=b'7' => kbinput - 24,
        b'?' | b'8' => DEL as i32,
        b' ' | b'2' => 0,
        b'/' => 31,
        _ => kbinput,
    }
}

/* C: long assemble_unicode(int symbol) — #ifdef ENABLE_UTF8 */
#[cfg(feature = "utf8")]
pub fn assemble_unicode(symbol: i32) -> i64 {
    let digits = UNICODE_DIGITS.with(|d| {
        let mut d = d.borrow_mut();
        *d += 1;
        *d
    });

    let uni = UNICODE_ACC.with(|u| *u.borrow());

    let outcome;

    if (b'0'..=b'9').contains(&(symbol as u8)) {
        UNICODE_ACC.with(|u| *u.borrow_mut() = (uni << 4) + (symbol as u32 - b'0' as u32));
        outcome = PROCEED;
    } else if (symbol | 0x20) >= 'a' as i32 && (symbol | 0x20) <= 'f' as i32 {
        let digit = ((symbol | 0x20) - 'a' as i32 + 10) as u32;
        UNICODE_ACC.with(|u| *u.borrow_mut() = (uni << 4) + digit);
        outcome = PROCEED;
    } else if symbol == '\r' as i32 || symbol == ' ' as i32 {
        outcome = uni as i64;
    } else {
        outcome = INVALID_DIGIT;
    }

    if digits == 6 && outcome == PROCEED {
        let final_uni = UNICODE_ACC.with(|u| *u.borrow());
        let out = if final_uni < 0x110000 { final_uni as i64 } else { INVALID_DIGIT };
        UNICODE_DIGITS.with(|d| *d.borrow_mut() = 0);
        UNICODE_ACC.with(|u| *u.borrow_mut() = 0);
        return out;
    }

    let currmenu = state().currmenu;
    if outcome == PROCEED && currmenu == MMAIN {
        let cur_digits = digits;
        let cur_uni = UNICODE_ACC.with(|u| *u.borrow());
        let mut partial = String::from("      ");
        let hex = format!("{:0>width$X}", cur_uni, width = cur_digits as usize);
        let start = 6 - cur_digits as usize;
        partial.replace_range(start.., &hex);
        statusline(MessageType::Info, &format!("Unicode Input: {}", partial));
    }

    if outcome != PROCEED {
        UNICODE_DIGITS.with(|d| *d.borrow_mut() = 0);
        UNICODE_ACC.with(|u| *u.borrow_mut() = 0);
    }

    outcome
}

/* C: int *parse_verbatim_kbinput(WINDOW *frame, size_t *count) */
pub fn parse_verbatim_kbinput(count: &mut usize) -> Vec<i32> {
    tl_set!(REVEAL_CURSOR, true);

    let keycode = get_input(Some(()));

    if consume_resize_request(Some(keycode)) {
        *count = 999;
        return Vec::new();
    }

    let mut yield_buf: Vec<i32> = vec![0; 6];

    #[cfg(feature = "utf8")]
    {
        let using_utf8 = state().using_utf8;
        if using_utf8 && (keycode as u8).is_ascii_hexdigit() {
            let mut unicode = assemble_unicode(keycode);
            tl_set!(REVEAL_CURSOR, false);

            let mut last_keycode = keycode;
            while unicode == PROCEED {
                let k = get_input(Some(()));
                last_keycode = k;
                unicode = assemble_unicode(k);
            }

            if consume_resize_request(Some(last_keycode)) {
                *count = 999;
                return Vec::new();
            }

            if unicode == INVALID_DIGIT {
                // For an invalid keystroke, discard its possible continuation bytes,
                // exactly as C does (winio.c:1480-1491).  nextcodes[0] is the front
                // of the waiting buffer: KEY_BUFFER[NEXTCODES_IDX].
                let peek_front = || -> Option<i32> {
                    if tl_get!(WAITING_CODES) > 0 {
                        KEY_BUFFER.with(|kb| NEXTCODES_IDX.with(|ni| kb.borrow().get(*ni.borrow()).copied()))
                    } else {
                        None
                    }
                };
                if last_keycode == ESC_CODE as i32 && tl_get!(WAITING_CODES) > 0 {
                    let _ = get_input(None);
                    while peek_front().map_or(false, |c| 0x1F < c && c < 0x40) {
                        let _ = get_input(None);
                    }
                    if peek_front().map_or(false, |c| 0x3F < c && c < 0x7F) {
                        let _ = get_input(None);
                    }
                } else if (0xC0..=0xFF).contains(&last_keycode) {
                    while peek_front().map_or(false, |c| 0x7F < c && c < 0xC0) {
                        let _ = get_input(None);
                    }
                }
                *count = 0;
                return Vec::new();
            }

            // Convert unicode codepoint to UTF-8 bytes
            if let Some(ch) = char::from_u32(unicode as u32) {
                let mut buf = [0u8; 4];
                let s = ch.encode_utf8(&mut buf);
                *count = s.len();
                let result: Vec<i32> = s.bytes().map(|b| b as i32).collect();
                return result;
            } else {
                *count = 0;
                return Vec::new();
            }
        }
    }

    yield_buf[0] = keycode;

    if keycode == ESC && tl_get!(WAITING_CODES) > 0 {
        yield_buf[1] = get_input(None);
        *count = 2;
    } else {
        *count = 1;
    }

    yield_buf
}

/* C: char *get_verbatim_kbinput(WINDOW *frame, size_t *count) */
pub fn get_verbatim_kbinput(count: &mut usize) -> String {
    let _preserve = state().flag_isset(PRESERVE);
    let _raw_sequences = state().flag_isset(RAW_SEQUENCES);

    // Disable bracketed paste
    #[cfg(not(feature = "tiny"))]
    {
        let _ = print!("\x1B[?2004l");
        let _ = out().flush();
    }

    tl_set!(LINGER_AFTER_ESCAPE, true);

    let input = parse_verbatim_kbinput(count);

    // Handle invalid or incomplete sequences
    if !input.is_empty() && *count > 0 {
        if input[0] >= 0x80 && *count == 1 {
            put_back(input[0]);
            *count = 999;
        } else {
            let as_at = state().as_an_at;
            if (input[0] == '\n' as i32 && as_at) || (input[0] == 0 && !as_at) {
                *count = 0;
            }
        }
    }

    tl_set!(LINGER_AFTER_ESCAPE, false);

    // Re-enable bracketed paste
    #[cfg(not(feature = "tiny"))]
    {
        let _ = print!("\x1B[?2004h");
        let _ = out().flush();
    }

    if *count < 999 {
        let bytes: Vec<u8> = input[..*count].iter().map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// parse_kbinput — the main keystroke FSM
// ---------------------------------------------------------------------------

/* C: int parse_kbinput(WINDOW *frame) */
pub fn parse_kbinput() -> i32 {
    with_state_mut(|s| {
        s.meta_key = false;
        s.shift_held = false;
    });

    let keycode = get_input(Some(()));

    let escapes = tl_get!(ESCAPES);
    let _digit_count = tl_get!(DIGIT_COUNT);
    let waiting = tl_get!(WAITING_CODES);

    if keycode == ESC {
        let _prev_first = tl_get!(FIRST_ESCAPE_WAS_ALONE);
        let prev_last = tl_get!(LAST_ESCAPE_WAS_ALONE);
        tl_set!(FIRST_ESCAPE_WAS_ALONE, prev_last);
        let alone = waiting == 0;
        tl_set!(LAST_ESCAPE_WAS_ALONE, alone);
        let dc = tl_get!(DIGIT_COUNT);
        if dc > 0 {
            tl_set!(DIGIT_COUNT, 0);
            tl_set!(ESCAPES, 1);
        } else {
            let new_escapes = if escapes + 1 > 2 {
                if alone { 0 } else { 1 }
            } else {
                escapes + 1
            };
            tl_set!(ESCAPES, new_escapes);
        }
        return ERR_CODE;
    } else if keycode == ERR_CODE {
        return ERR_CODE;
    }

    let escapes = tl_get!(ESCAPES);

    if escapes == 0 {
        if keycode < 0xFF && keycode != '\t' as i32 && keycode != DEL {
            return keycode;
        }
    } else if escapes == 1 {
        tl_set!(ESCAPES, 0);
        let last_alone = tl_get!(LAST_ESCAPE_WAS_ALONE);

        if keycode < 0x20 || keycode > 0x7E {
            if keycode == '\t' as i32 {
                return SHIFT_TAB as i32;
            }
            #[cfg(not(feature = "tiny"))]
            if keycode == KEY_BACKSPACE || keycode == '\x08' as i32 || keycode == DEL {
                return CONTROL_SHIFT_DELETE as i32;
            }
            #[cfg(feature = "utf8")]
            {
                let using_utf8 = state().using_utf8;
                if keycode >= 0xC0 && keycode <= 0xFF && using_utf8 {
                    // Skip continuation bytes
                    while tl_get!(WAITING_CODES) > 0 {
                        let next = KEY_BUFFER.with(|kb| {
                            NEXTCODES_IDX.with(|ni| kb.borrow()[*ni.borrow()])
                        });
                        if next >= 0x80 && next <= 0xBF {
                            get_input(None);
                        } else {
                            break;
                        }
                    }
                    return FOREIGN_SEQUENCE as i32;
                }
            }
            if keycode < 0x20 && !last_alone {
                state_mut().meta_key = true;
            }
        } else {
            // ASCII printable range
            let waiting = tl_get!(WAITING_CODES);
            let next_is_esc = if waiting > 0 {
                KEY_BUFFER.with(|kb| {
                    NEXTCODES_IDX.with(|ni| kb.borrow()[*ni.borrow()] == ESC)
                })
            } else {
                false
            };

            if waiting == 0 || next_is_esc
               || (keycode != 'O' as i32 && keycode != '[' as i32)
            {
                let shifted = state().shifted_metas;
                let kc = if keycode >= 'A' as i32 && keycode <= 'Z' as i32 && !shifted {
                    keycode | 0x20
                } else {
                    keycode
                };
                state_mut().meta_key = true;
                return kc;
            } else {
                return parse_escape_sequence(keycode);
            }
        }
    } else {
        // escapes == 2
        tl_set!(ESCAPES, 0);

        let waiting = tl_get!(WAITING_CODES);
        let next = if waiting > 0 {
            KEY_BUFFER.with(|kb| NEXTCODES_IDX.with(|ni| kb.borrow()[*ni.borrow()]))
        } else {
            -1
        };

        if keycode == '[' as i32 && waiting > 0
            && ((next >= 'A' as i32 && next <= 'D' as i32)
                || (next >= 'a' as i32 && next <= 'd' as i32))
        {
            let inner = get_input(None);
            return match inner as u8 as char {
                'A' => KEY_HOME,
                'B' => KEY_END,
                'C' => CONTROL_RIGHT as i32,
                'D' => CONTROL_LEFT as i32,
                #[cfg(not(feature = "tiny"))]
                'a' => { state_mut().shift_held = true; KEY_PPAGE }
                #[cfg(not(feature = "tiny"))]
                'b' => { state_mut().shift_held = true; KEY_NPAGE }
                #[cfg(not(feature = "tiny"))]
                'c' => { state_mut().shift_held = true; KEY_HOME }
                #[cfg(not(feature = "tiny"))]
                'd' => { state_mut().shift_held = true; KEY_END }
                _ => ERR_CODE,
            };
        } else if waiting > 0 && next != ESC && (keycode == '[' as i32 || keycode == 'O' as i32) {
            let result = parse_escape_sequence(keycode);
            state_mut().meta_key = true;
            return result;
        } else if keycode >= '0' as i32 && (keycode <= '2' as i32
            || (keycode <= '9' as i32 && tl_get!(DIGIT_COUNT) > 0))
        {
            let byte = assemble_byte_code(keycode);
            if byte == PROCEED as i32 {
                tl_set!(ESCAPES, 2);
                return ERR_CODE;
            }
            #[cfg(feature = "utf8")]
            {
                let using_utf8 = state().using_utf8;
                if byte > 0x7F && using_utf8 {
                    if (byte as u8) < 0xC0 {
                        put_back(byte as u8 as i32);
                        return 0xC2;
                    } else {
                        put_back((byte as u8).wrapping_sub(0x40) as i32);
                        return 0xC3;
                    }
                }
            }
            if byte == '\t' as i32 || byte == DEL {
                return byte;
            } else {
                return byte;
            }
        } else if tl_get!(DIGIT_COUNT) == 0 {
            let first_alone = tl_get!(FIRST_ESCAPE_WAS_ALONE);
            let last_alone = tl_get!(LAST_ESCAPE_WAS_ALONE);
            if first_alone && !last_alone {
                let shifted = state().shifted_metas;
                let kc = if keycode >= 'A' as i32 && keycode <= 'Z' as i32 && !shifted {
                    keycode | 0x20
                } else {
                    keycode
                };
                state_mut().meta_key = true;
                return kc;
            } else {
                return convert_to_control(keycode);
            }
        }
    }

    // Apply custom key mappings from rcfile
    let kc = apply_custom_keycode(keycode);
    kc
}

/// Apply configured key remappings (controlleft, controlright, etc.)
fn apply_custom_keycode(keycode: i32) -> i32 {
    // Crossterm can deliver many events in one burst.  Decode dedicated
    // shifted codes only when each event is consumed so `shift_held` cannot be
    // reset or leaked by a neighboring queued event.
    let shifted_navigation = match keycode {
            SHIFT_LEFT_CODE => Some(KEY_LEFT),
            SHIFT_RIGHT_CODE => Some(KEY_RIGHT),
            k if k == SHIFT_UP as i32 => Some(KEY_UP),
            k if k == SHIFT_DOWN as i32 => Some(KEY_DOWN),
            k if k == SHIFT_HOME as i32 => Some(KEY_HOME),
            k if k == SHIFT_END as i32 => Some(KEY_END),
            k if k == SHIFT_PAGEUP as i32 => Some(KEY_PPAGE),
            k if k == SHIFT_PAGEDOWN as i32 => Some(KEY_NPAGE),
            k if k == SHIFT_CONTROL_LEFT as i32 => Some(CONTROL_LEFT as i32),
            k if k == SHIFT_CONTROL_RIGHT as i32 => Some(CONTROL_RIGHT as i32),
            k if k == SHIFT_CONTROL_UP as i32 => Some(CONTROL_UP as i32),
            k if k == SHIFT_CONTROL_DOWN as i32 => Some(CONTROL_DOWN as i32),
            k if k == SHIFT_CONTROL_HOME as i32 => Some(CONTROL_HOME as i32),
            k if k == SHIFT_CONTROL_END as i32 => Some(CONTROL_END as i32),
            k if k == SHIFT_ALT_LEFT as i32 => Some(KEY_HOME),
            k if k == SHIFT_ALT_RIGHT as i32 => Some(KEY_END),
            k if k == SHIFT_ALT_UP as i32 => Some(KEY_PPAGE),
            k if k == SHIFT_ALT_DOWN as i32 => Some(KEY_NPAGE),
            _ => None,
    };

    if let Some(navigation) = shifted_navigation {
        state_mut().shift_held = true;
        return navigation;
    }

    let (cl, cr, cu, cd, ch, ce) = with_state(|s| (
        s.controlleft, s.controlright, s.controlup, s.controldown,
        s.controlhome, s.controlend,
    ));

    if keycode == cl { return CONTROL_LEFT as i32; }
    if keycode == cr { return CONTROL_RIGHT as i32; }
    if keycode == cu { return CONTROL_UP as i32; }
    if keycode == cd { return CONTROL_DOWN as i32; }
    if keycode == ch { return CONTROL_HOME as i32; }
    if keycode == ce { return CONTROL_END as i32; }

    #[cfg(not(feature = "tiny"))]
    {
        let (cdelete, cshdelete, sup, sdown, scl, scr, scu, scd, sch, sce,
             al, ar, au, ad, ahome, aend, apgup, apgdn, ains, adel,
             sal, sar, sau, sad) = with_state(|s| (
            s.controldelete, s.controlshiftdelete,
            s.shiftup, s.shiftdown,
            s.shiftcontrolleft, s.shiftcontrolright,
            s.shiftcontrolup, s.shiftcontroldown,
            s.shiftcontrolhome, s.shiftcontrolend,
            s.altleft, s.altright, s.altup, s.altdown,
            s.althome, s.altend, s.altpageup, s.altpagedown,
            s.altinsert, s.altdelete,
            s.shiftaltleft, s.shiftaltright, s.shiftaltup, s.shiftaltdown,
        ));

        if keycode == cdelete { return CONTROL_DELETE as i32; }
        if keycode == cshdelete { return CONTROL_SHIFT_DELETE as i32; }
        if keycode == sup { state_mut().shift_held = true; return KEY_UP; }
        if keycode == sdown { state_mut().shift_held = true; return KEY_DOWN; }
        if keycode == scl { state_mut().shift_held = true; return CONTROL_LEFT as i32; }
        if keycode == scr { state_mut().shift_held = true; return CONTROL_RIGHT as i32; }
        if keycode == scu { state_mut().shift_held = true; return CONTROL_UP as i32; }
        if keycode == scd { state_mut().shift_held = true; return CONTROL_DOWN as i32; }
        if keycode == sch { state_mut().shift_held = true; return CONTROL_HOME as i32; }
        if keycode == sce { state_mut().shift_held = true; return CONTROL_END as i32; }
        if keycode == al { return ALT_LEFT as i32; }
        if keycode == ar { return ALT_RIGHT as i32; }
        if keycode == au { return ALT_UP as i32; }
        if keycode == ad { return ALT_DOWN as i32; }
        if keycode == ahome { return ALT_HOME as i32; }
        if keycode == aend { return ALT_END as i32; }
        if keycode == apgup { return ALT_PAGEUP as i32; }
        if keycode == apgdn { return ALT_PAGEDOWN as i32; }
        if keycode == ains { return ALT_INSERT as i32; }
        if keycode == adel { return ALT_DELETE as i32; }
        if keycode == sal { state_mut().shift_held = true; return KEY_HOME; }
        if keycode == sar { state_mut().shift_held = true; return KEY_END; }
        if keycode == sau { state_mut().shift_held = true; return KEY_PPAGE; }
        if keycode == sad { state_mut().shift_held = true; return KEY_NPAGE; }

        // Out-of-range function keys
        if keycode > (KEY_F0 + 24) && keycode < (KEY_F0 + 64) {
            return FOREIGN_SEQUENCE as i32;
        }
    }

    // Spurious VTE focus codes
    let (mfi, mfo) = with_state(|s| (s.mousefocusin, s.mousefocusout));
    if keycode == mfi || keycode == mfo {
        return ERR_CODE;
    }

    // Shift+arrow variants
    match keycode {
        k if k == KEY_DC => {
            return if state().flag_isset(REBIND_DELETE) { KEY_BACKSPACE } else { KEY_DC };
        }
        k if k == KEY_BACKSPACE => {
            return if state().flag_isset(REBIND_DELETE) { KEY_DC } else { KEY_BACKSPACE };
        }
        _ => {}
    }

    // KEY_BTAB (shift-tab from ncurses)
    if keycode == 0x161 { return SHIFT_TAB as i32; } // KEY_BTAB

    keycode
}

/* C: int get_kbinput(WINDOW *frame, bool showcursor) */
pub fn get_kbinput(showcursor: bool) -> i32 {
    tl_set!(REVEAL_CURSOR, showcursor);

    let mut kbinput = ERR_CODE;
    while kbinput == ERR_CODE {
        kbinput = parse_kbinput();
    }

    let currmenu = state().currmenu;
    // When reading from the edit window, blank status bar when countdown expires
    // (midwin is the "frame" in C terms)
    if currmenu == MMAIN || currmenu == 0 {
        blank_it_when_expired();
    }

    kbinput
}

// ---------------------------------------------------------------------------
// Mouse input (ENABLE_MOUSE)
// ---------------------------------------------------------------------------

/* C: int get_mouseinput(int *mouse_y, int *mouse_x) */
#[cfg(feature = "mouse")]
pub fn get_mouseinput(mouse_y: &mut i32, mouse_x: &mut i32) -> i32 {
    let event = LAST_MOUSE_EVENT.with(|m| m.borrow().clone());
    let event = match event {
        Some(e) => e,
        None => return -1,
    };

    let (mid_y, mid_rows, mid_x, mid_cols) = with_state(|s| (
        s.midwin.y, s.midwin.rows, s.midwin.x, s.midwin.cols,
    ));
    let (foot_y, foot_rows) = with_state(|s| (s.footwin.y, s.footwin.rows));
    let cols = with_state(|s| s.midwin.cols + s.midwin.x);

    let in_middle = event.y >= mid_y && event.y < mid_y + mid_rows
        && event.x >= mid_x && event.x < mid_x + mid_cols;
    let in_footer = event.y >= foot_y && event.y < foot_y + foot_rows;

    let (margin, currmenu) = with_state(|s| (s.margin, s.currmenu));
    // Only the main editor's middle window is expressed relative to the text
    // area after the line-number margin.  Browser/help layouts start at the
    // window origin and must receive the raw x coordinate.
    *mouse_x = event.x as i32 - if in_middle && currmenu == MMAIN { margin } else { 0 };
    *mouse_y = event.y as i32;

    let bstate = event.bstate;

    if bstate & (BUTTON1_RELEASED | BUTTON1_CLICKED) != 0 {
        let sidebar = state().sidebar;
        if in_middle && sidebar != 0 && event.x == cols - 1 && currmenu == MMAIN {
            // Clicking in the "scrollbar" goes to the roughly corresponding line.
            let editwinrows = state().editwinrows as isize;
            let (total_lines, placewewant) = with_state(|s| {
                let f = s.openfile.as_ref();
                (
                    f.and_then(|f| f.filebot.as_ref()).map(|b| b.borrow().lineno).unwrap_or(1),
                    f.map(|f| f.placewewant).unwrap_or(0) as isize,
                )
            });
            let mut click_row = (*mouse_y - mid_y as i32).max(0) as isize;
            if click_row != 0 { click_row += 1; }
            crate::search::goto_line_and_column(
                total_lines * click_row / editwinrows.max(1) + 1,
                placewewant + 1,
                true,
            );
            state_mut().refresh_needed = true;
            // Fall out as "handled" like C (the click must not also be
            // treated as an edit-window positioning click).
            return 2;
        }

        if in_footer && !state().flag_isset(NO_HELP) && currmenu != MYESNO {
            let lines_val = with_state(|s| s.footwin.rows + s.midwin.rows + s.topwin.rows);
            if *mouse_y == lines_val as i32 - 3 {
                return 0;
            }

            let foot_rel_y = (*mouse_y - foot_y as i32).max(0) as usize;
            let foot_rel_x = (*mouse_x).max(0) as usize;

            let currmenu = state().currmenu;
            let number = shown_entries_for(currmenu);
            if number == 0 { return 2; }

            let cols_usize = cols as usize;
            let width = if number < 5 { cols_usize / 2 } else { cols_usize / ((number + 1) / 2) };
            if width == 0 { return 2; }

            let mut index = (foot_rel_x / width) * 2 + foot_rel_y + 1;

            if index > number && foot_rel_x % width < cols_usize % width {
                index -= 2;
            }

            if index > number { return 2; }

            // Find the index-th shortcut in current menu
            let mut count = 0usize;
            let result = with_state(|s| {
                for sc in &s.sclist {
                    if (sc.menus as u32 & currmenu) == 0 { continue; }
                    if sc.keystr.is_empty() { continue; }
                    count += 1;
                    if count == index {
                        return Some((sc.keycode, sc.keystr.len() > 1));
                    }
                }
                None
            });

            if let Some((kc, _is_special)) = result {
                put_back(kc);
                if kc >= 0x20 && kc <= 0x7E {
                    put_back(ESC);
                }
                return 1;
            }
            return 2;
        }
        return 0;
    }

    if bstate & (BUTTON4_PRESSED | BUTTON5_PRESSED) != 0 {
        if in_middle || (in_footer && *mouse_y == foot_y as i32) {
            let keycode = if bstate & BUTTON4_PRESSED != 0 {
                ALT_UP as i32
            } else {
                ALT_DOWN as i32
            };
            put_back(keycode);
            put_back(keycode);
            return 1;
        }
        return 2;
    }

    2
}

// ---------------------------------------------------------------------------
// Blank helpers
// ---------------------------------------------------------------------------

/* C: void blank_row(WINDOW *window, int row) */
pub fn blank_row(win: &NanoWindow, row: u16) {
    let mut stdout = out();
    let _cols = win.cols;
    let abs_y = win.y + row;
    let _ = queue!(stdout,
        MoveTo(win.x, abs_y),
        Clear(ClearType::UntilNewLine),
    );
}

/* C: void blank_titlebar(void) */
pub fn blank_titlebar() {
    let (topwin_x, topwin_y, cols) = with_state(|s| (s.topwin.x, s.topwin.y, s.topwin.cols));
    let spaces = " ".repeat(cols as usize);
    // NOTE: intentionally uses the global stdout so the fill inherits whatever
    // attributes are already queued onto stdout by the caller (e.g. Reverse in titlebar).
    let _ = queue!(out(), MoveTo(topwin_x, topwin_y), Print(&spaces));
}

/* C: void blank_edit(void) */
pub fn blank_edit() {
    let (editwinrows, midwin_y, midwin_x, _midwin_cols) = with_state(|s| {
        (s.editwinrows, s.midwin.y, s.midwin.x, s.midwin.cols)
    });
    let mut stdout = out();
    for row in 0..editwinrows {
        let _ = queue!(stdout,
            MoveTo(midwin_x, midwin_y + row as u16),
            Clear(ClearType::UntilNewLine),
        );
    }
}

// ---------------------------------------------------------------------------
// Filename-completion candidate grid
// ---------------------------------------------------------------------------

/// One cell in the filename-completion grid.  A `None` index is the overflow
/// marker that replaces the final visible candidate when more names exist.
#[cfg(feature = "tabcomp")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletionCell {
    row: usize,
    column: usize,
    match_index: Option<usize>,
}

/// Terminal-independent geometry for the filename-completion list.
///
/// Keeping this calculation separate from painting makes the boundary cases
/// (one-column terminals, wide Unicode names, and lists taller than the edit
/// window) testable without a PTY.
#[cfg(feature = "tabcomp")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletionGrid {
    name_width: usize,
    cells: Vec<CompletionCell>,
}

#[cfg(feature = "tabcomp")]
fn completion_grid(
    matches: &[String],
    cols: usize,
    edit_rows: usize,
    reserve_bottom_row: bool,
) -> CompletionGrid {
    if matches.len() < 2 || cols == 0 || edit_rows == 0 {
        return CompletionGrid { name_width: 0, cells: Vec::new() };
    }

    let available_rows = edit_rows.saturating_sub(usize::from(reserve_bottom_row));
    if available_rows == 0 {
        return CompletionGrid { name_width: 0, cells: Vec::new() };
    }

    // GNU nano leaves one terminal column unused when possible.  On a truly
    // one-column terminal, however, retaining a width of one is more useful
    // than producing an entirely invisible list.
    let width_limit = cols.saturating_sub(1).max(1);
    let name_width = matches
        .iter()
        .map(|name| breadth(name))
        .max()
        .unwrap_or(0)
        .min(width_limit);
    let column_span = name_width.saturating_add(2).max(1);
    let ncols = (cols.saturating_add(1) / column_span).max(1);
    let nrows = matches.len().div_ceil(ncols);
    let last_row = available_rows - 1;

    // Match nano's established placement: keep one blank row between a short
    // list and the prompt, while a tall list starts at the top and uses its
    // final row for an overflow marker.
    let first_row = if nrows < last_row { last_row - nrows } else { 0 };

    let mut cells = Vec::new();
    for match_index in 0..matches.len() {
        let row = first_row + match_index / ncols;
        if row > last_row {
            break;
        }
        let column_in_grid = match_index % ncols;
        let column = column_span.saturating_mul(column_in_grid);

        // When another complete row would not fit, reserve the bottom-right
        // cell for the same `(more)` indicator used by GNU nano.
        if row == last_row
            && column_in_grid + 1 == ncols
            && match_index + 1 < matches.len()
        {
            cells.push(CompletionCell { row, column, match_index: None });
            break;
        }

        cells.push(CompletionCell {
            row,
            column,
            match_index: Some(match_index),
        });
    }

    CompletionGrid { name_width, cells }
}

/// Blank the edit area and paint a sorted set of filename completions there.
/// The caller owns list lifetime: its existing refresh callback redraws the
/// edit view when completion ends or the resize coordinator rebuilds windows.
#[cfg(feature = "tabcomp")]
pub fn show_completion_candidates(matches: &[String]) {
    let (cols, edit_rows, reserve_bottom_row, midwin_x, midwin_y) = with_state(|s| {
        (
            s.midwin.cols as usize,
            s.editwinrows.max(0) as usize,
            s.flag_isset(ZERO) && screen_rows() > 1,
            s.midwin.x,
            s.midwin.y,
        )
    });
    let grid = completion_grid(matches, cols, edit_rows, reserve_bottom_row);
    if grid.cells.is_empty() {
        return;
    }

    blank_edit();
    let mut stdout = out();
    let _ = queue!(stdout, Hide);

    for cell in grid.cells {
        let text = match cell.match_index {
            Some(index) => display_string(&matches[index], 0, grid.name_width, false, false),
            None => display_string("(more)", 0, grid.name_width, false, false),
        };
        let remaining = cols.saturating_sub(cell.column);
        let visible = display_string(&text, 0, remaining, false, false);
        let _ = queue!(
            stdout,
            MoveTo(
                midwin_x.saturating_add(cell.column as u16),
                midwin_y.saturating_add(cell.row as u16),
            ),
            Print(visible),
        );
    }

    let _ = stdout.flush();
}

/* C: void blank_statusbar(void) */
pub fn blank_statusbar() {
    let (footwin_x, footwin_y) = with_state(|s| (s.footwin.x, s.footwin.y));
    let mut stdout = out();
    let _ = queue!(stdout, MoveTo(footwin_x, footwin_y), Clear(ClearType::UntilNewLine));
}

/* C: void wipe_statusbar(void) */
pub fn wipe_statusbar() {
    state_mut().lastmessage = MessageType::Vacuum;

    let (zero, minibar, currmenu) = with_state(|s| (
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
        s.currmenu,
    ));
    let lines = screen_rows();

    if (zero || minibar || lines == 1) && currmenu == MMAIN {
        return;
    }

    blank_statusbar();
    let _ = out().flush();
}

/* C: void blank_bottombars(void) */
pub fn blank_bottombars() {
    let (no_help, footwin_y, footwin_x) = with_state(|s| (
        s.flag_isset(NO_HELP),
        s.footwin.y,
        s.footwin.x,
    ));
    let lines = screen_rows();

    if !no_help && lines > 5 {
        let mut stdout = out();
        let _ = queue!(stdout,
            MoveTo(footwin_x, footwin_y + 1),
            Clear(ClearType::UntilNewLine),
            MoveTo(footwin_x, footwin_y + 2),
            Clear(ClearType::UntilNewLine),
        );
    }
}

/* C: void blank_it_when_expired(void) */
pub fn blank_it_when_expired() {
    let countdown = tl_get!(COUNTDOWN);
    if countdown == 0 {
        return;
    }

    tl_set!(COUNTDOWN, countdown - 1);

    if countdown - 1 == 0 {
        wipe_statusbar();
    }

    let (currmenu, zero) = with_state(|s| (s.currmenu, s.flag_isset(ZERO)));
    let lines = screen_rows();
    if currmenu == MMAIN && (zero || lines == 1) {
        // Redraw last row of edit window
        let _ = out().flush();
    }
}

/* C: void set_blankdelay_to_one(void) */
pub fn set_blankdelay_to_one() {
    tl_set!(COUNTDOWN, 1);
}

// ---------------------------------------------------------------------------
// display_string — convert text to a displayable form
// ---------------------------------------------------------------------------

/* C: char *display_string(const char *text, size_t column, size_t span, bool isdata, bool isprompt) */
pub fn display_string(text: &str, column: usize, span: usize, isdata: bool, isprompt: bool) -> String {
    if span == 0 {
        return String::new();
    }

    // Hoist all the per-call state out of the per-character loop below.
    let (cols, tabsize, softwrap) = with_state(|s| (
        s.midwin.cols as usize,
        s.tabsize as usize,
        s.flag_isset(SOFTWRAP),
    ));
    let tabsize = if tabsize == 0 { 8 } else { tabsize };

    #[cfg(not(feature = "tiny"))]
    let (ws_display, whitespace, wlen0, wlen1) = with_state(|s| (
        s.flag_isset(WHITESPACE_DISPLAY),
        if s.flag_isset(WHITESPACE_DISPLAY) { s.whitespace.clone() } else { None },
        s.whitelen[0] as usize,
        s.whitelen[1] as usize,
    ));

    let start_x = actual_x(text, column);
    let start_col = wideness(text, start_x);
    let beyond = column + span;

    let mut converted = String::with_capacity((cols + 20) * MAXCHARLEN);
    let bytes = text.as_bytes();
    let mut pos = start_x;
    let mut cur_col = start_col;

    let from_x_val = start_x;
    tl_set!(FROM_X, from_x_val);

    // Handle case where the first character starts before the left edge, or would
    // be overwritten by a "<" token (C: start_col < column ||
    // (start_col > 0 && isdata && !SOFTWRAP)) — show placeholders instead.
    if (start_col < column || (start_col > 0 && isdata && !softwrap))
        && pos < text.len() && bytes[pos] != b'\t' {
        let ch = &text[pos..];
        if is_cntrl_char(ch) {
            if start_col < column {
                converted.push(control_mbrep(ch, isdata));
                cur_col += 1;
                pos += char_length(ch);
            }
        } else {
            #[cfg(feature = "utf8")]
            {
                if is_doublewidth(ch) {
                    if start_col == column {
                        converted.push(' ');
                        cur_col += 1;
                    }
                    converted.push(']');
                    cur_col += 1;
                    pos += char_length(ch);
                }
            }
        }
    }

    // C loop guard: while (*text && (column < beyond || ZEROWIDTH_CHAR)).  Keep
    // consuming trailing zero-width characters at the right edge instead of
    // dropping them, so the inner break below is the real terminator.
    while pos < text.len() {
        let ch = &text[pos..];
        let b = bytes[pos];

        // Zero-width check first so we don't break out of loop
        #[cfg(feature = "utf8")]
        let zerowidth = is_zerowidth(ch);
        #[cfg(not(feature = "utf8"))]
        let zerowidth = false;

        if !zerowidth && cur_col >= beyond {
            break;
        }

        // Plain printable ASCII (fast path)
        if b > 0x20 && b != DEL as u8 {
            #[cfg(not(feature = "utf8"))]
            {
                converted.push(b as char);
                cur_col += 1;
                pos += 1;
                continue;
            }
            #[cfg(feature = "utf8")]
            {
                // ASCII or multi-byte
                let cl = char_length(ch);
                converted.push_str(&text[pos..pos + cl]);
                let width = if is_doublewidth(ch) { 2 } else if zerowidth { 0 } else { 1 };
                cur_col += width;
                pos += cl;
                continue;
            }
        }

        // ISO 8859 characters (non-UTF8 mode)
        #[cfg(not(feature = "utf8"))]
        if b > 0x9F {
            converted.push(b as char);
            cur_col += 1;
            pos += 1;
            continue;
        }

        // Space
        if b == b' ' {
            #[cfg(not(feature = "tiny"))]
            if ws_display {
                if let Some(ref ws) = whitespace {
                    converted.push_str(&ws[wlen0..wlen0 + wlen1]);
                    cur_col += 1;
                    pos += 1;
                    continue;
                }
            }
            converted.push(' ');
            cur_col += 1;
            pos += 1;
            continue;
        }

        // Tab
        if b == b'\t' {
            #[cfg(not(feature = "tiny"))]
            if ws_display {
                let can_show_tab = converted.len() > 0 || !isdata
                    || !softwrap
                    || cur_col % tabsize == 0
                    || cur_col == start_col;
                if can_show_tab {
                    if let Some(ref ws) = whitespace {
                        converted.push_str(&ws[..wlen0]);
                        cur_col += 1;
                        pos += 1;
                        // Pad to tab stop
                        while cur_col % tabsize != 0 && cur_col < beyond {
                            converted.push(' ');
                            cur_col += 1;
                        }
                        continue;
                    }
                }
            }
            converted.push(' ');
            cur_col += 1;
            pos += 1;
            while cur_col % tabsize != 0 && cur_col < beyond {
                converted.push(' ');
                cur_col += 1;
            }
            continue;
        }

        // Control character
        if is_cntrl_char(ch) {
            converted.push('^');
            converted.push(control_mbrep(ch, isdata));
            pos += char_length(ch);
            cur_col += 2;
            continue;
        }

        // Multi-byte / UTF-8
        #[cfg(feature = "utf8")]
        {
            let cl = char_length(ch);
            if cl == 0 {
                // Invalid byte
                converted.push('\u{FFFD}');
                pos += 1;
                cur_col += 1;
                continue;
            }

            if let Ok((wc, _)) = mbtowide(ch) {
                let charwidth = unicode_width::UnicodeWidthChar::width(wc).unwrap_or(1);
                if zerowidth {
                    // Copy zero-width char without incrementing column
                    converted.push_str(&text[pos..pos + cl]);
                    pos += cl;
                    continue;
                }
                converted.push_str(&text[pos..pos + cl]);
                cur_col += charwidth;
                pos += cl;
            } else {
                // Invalid multibyte sequence
                converted.push('\u{FFFD}');
                pos += 1;
                cur_col += 1;
            }
            continue;
        }
        #[cfg(not(feature = "utf8"))]
        {
            pos += 1;
            cur_col += 1;
        }
    }

    tl_set!(TILL_X, pos);

    // Trim if we went past the right edge
    if cur_col > beyond || (pos < text.len() && (isprompt || (isdata && !softwrap))) {
        #[cfg(feature = "utf8")]
        {
            let mut trim_pos = converted.len();
            while trim_pos > 0 {
                trim_pos = step_left(&converted, trim_pos);
                if !is_zerowidth(&converted[trim_pos..]) {
                    break;
                }
            }
            if is_doublewidth(&converted[trim_pos..]) {
                converted.truncate(trim_pos);
                converted.push('[');
            } else {
                converted.truncate(trim_pos);
            }
        }
        #[cfg(not(feature = "utf8"))]
        if !converted.is_empty() {
            converted.pop();
        }
        tl_set!(HAS_MORE, true);
    } else {
        tl_set!(HAS_MORE, false);
    }

    tl_set!(IS_SHORTER, cur_col < beyond);

    converted
}

// ---------------------------------------------------------------------------
// buffer_number (ENABLE_MULTIBUFFER)
// ---------------------------------------------------------------------------

/* C: int buffer_number(openfilestruct *buffer) — #ifdef ENABLE_MULTIBUFFER
 * Position of the current buffer, counted from the oldest surviving
 * buffer (C's startfile) along the circular order. */
#[cfg(feature = "multibuffer")]
pub fn buffer_number() -> i32 {
    with_state(|s| {
        let Some(ref cur) = s.openfile else { return 1 };
        // Cyclic order is [current, ring[0], ring[1], ...].
        let mut min_pos = 0usize;
        let mut min_seq = cur.seq;
        for (i, b) in s.buffer_ring.iter().enumerate() {
            if b.seq < min_seq {
                min_seq = b.seq;
                min_pos = i + 1;
            }
        }
        let n = s.buffer_ring.len() + 1;
        ((n - min_pos) % n + 1) as i32
    })
}

/// The total number of open buffers (C: buffer_number(startfile->prev)).
#[cfg(feature = "multibuffer")]
pub fn buffer_count() -> i32 {
    with_state(|s| {
        if s.openfile.is_none() { 0 } else { s.buffer_ring.len() as i32 + 1 }
    })
}

// ---------------------------------------------------------------------------
// show_states_at — show editor state flags in a window
// ---------------------------------------------------------------------------

/* C: void show_states_at(WINDOW *window) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn show_states_at_win(win: &NanoWindow, cur_y: u16, cur_x: u16) {
    let mut stdout = out();
    let (autoindent, has_mark, break_long, _recording, softwrap) = with_state(|s| (
        s.flag_isset(AUTOINDENT),
        s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some(),
        s.flag_isset(BREAK_LONG_LINES),
        false, // recording is in thread_local, not AppState
        s.flag_isset(SOFTWRAP),
    ));
    let rec = tl_get!(RECORDING);

    let _ = queue!(stdout,
        MoveTo(win.x + cur_x, win.y + cur_y),
        Print(if autoindent { "I" } else { " " }),
        Print(if has_mark    { "M" } else { " " }),
        Print(if break_long  { "L" } else { " " }),
        Print(if rec         { "R" } else { " " }),
        Print(if softwrap    { "S" } else { " " }),
    );
}

// ---------------------------------------------------------------------------
// Color pair helpers
// ---------------------------------------------------------------------------

/// Write the color/attribute commands for `pair` onto `w`.
/// Called by apply_interface_color and by callers that already hold a stdout handle.
pub fn queue_interface_color<W: Write>(w: &mut W, pair: i32) {
    // Emit the configured foreground/background colours.  The low 8 bits of `pair`
    // hold the 1-based interface-pair index (0 means attributes only); look up the
    // decoded (fg, bg) and emit them just like set_color does for syntax colours.
    #[cfg(feature = "color")]
    {
        let pair_index = (pair & 0xFF) as usize;
        if pair_index > 0 {
            let (fg, bg) = with_state(|s| {
                *s.interface_color_rgb.get(pair_index).unwrap_or(&(-1, -1))
            });
            if fg >= 0 {
                let _ = queue!(w, SetForegroundColor(ncurses_color_to_crossterm(fg)));
            }
            if bg >= 0 {
                let _ = queue!(w, SetBackgroundColor(ncurses_color_to_crossterm(bg)));
            }
        }
    }
    let reverse = (pair & A_REVERSE) != 0;
    if reverse {
        let _ = queue!(w, SetAttribute(Attribute::Reverse));
    }
    let bold = (pair & crate::color::A_BOLD) != 0;
    if bold {
        let _ = queue!(w, SetAttribute(Attribute::Bold));
    }
    let italic = (pair & crate::color::A_ITALIC) != 0;
    if italic {
        let _ = queue!(w, SetAttribute(Attribute::Italic));
    }
}

/// Apply interface color pair by writing to the global stdout immediately.
/// Use queue_interface_color when you already hold a stdout handle.
pub fn apply_interface_color(pair: i32) {
    let mut out = out();
    queue_interface_color(&mut out, pair);
}

/// Reset colors/attributes onto the given writer.
pub fn queue_reset_color<W: Write>(w: &mut W) {
    let _ = queue!(w, ResetColor, SetAttribute(Attribute::Reset));
}

/// Reset colors/attributes on global stdout.
pub fn reset_color() {
    let mut out = out();
    queue_reset_color(&mut out);
}

/* C: void set_color(const colortype *varnish) — for syntax highlighting */
#[cfg(feature = "color")]
pub fn set_color(varnish: &ColorType) {
    let mut stdout = out();
    // Apply foreground color
    if varnish.fg >= 0 {
        let color = ncurses_color_to_crossterm(varnish.fg);
        let _ = queue!(stdout, SetForegroundColor(color));
    }
    // Apply background color
    if varnish.bg >= 0 {
        let color = ncurses_color_to_crossterm(varnish.bg);
        let _ = queue!(stdout, SetBackgroundColor(color));
    }
    if varnish.attributes & A_REVERSE != 0 {
        let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
    }
    let bold = 0x0200_0000i32; // ncurses A_BOLD
    if varnish.attributes & bold != 0 {
        let _ = queue!(stdout, SetAttribute(Attribute::Bold));
    }
    let italic = 0x0008_0000i32; // ncurses A_ITALIC
    if varnish.attributes & italic != 0 {
        let _ = queue!(stdout, SetAttribute(Attribute::Italic));
    }
}

/* C: void unset_color(const colortype *varnish) */
#[cfg(feature = "color")]
pub fn unset_color(_varnish: &ColorType) {
    reset_color();
}

/// Convert a nano/ncurses color index to a crossterm Color.
#[cfg(feature = "color")]
pub fn ncurses_color_to_crossterm(nc: i16) -> Color {
    match nc {
        0 => Color::Black,
        1 => Color::DarkRed,
        2 => Color::DarkGreen,
        3 => Color::DarkYellow,
        4 => Color::DarkBlue,
        5 => Color::DarkMagenta,
        6 => Color::DarkCyan,
        7 => Color::Grey,
        8 => Color::DarkGrey,
        9 => Color::Red,
        10 => Color::Green,
        11 => Color::Yellow,
        12 => Color::Blue,
        13 => Color::Magenta,
        14 => Color::Cyan,
        15 => Color::White,
        n if n >= 16 => Color::AnsiValue(n as u8),
        _ => Color::Reset,  // THE_DEFAULT = -1
    }
}

// ---------------------------------------------------------------------------
// Titlebar
// ---------------------------------------------------------------------------

/* C: void titlebar(const char *path) */
pub fn titlebar(path: Option<&str>) {
    let (topwin_rows, topwin_y, topwin_x, cols, title_pair, currmenu, inhelp) = with_state(|s| (
        s.topwin.rows, s.topwin.y, s.topwin.x, s.topwin.cols,
        s.interface_color_pair[TITLE_BAR], s.currmenu, s.inhelp,
    ));

    if topwin_rows == 0 {
        return;
    }

    let mut stdout = out();

    // Apply title bar color — all on the SAME stdout handle so
    // the attribute is guaranteed to precede the fill in the output stream.
    queue_interface_color(&mut stdout, title_pair);

    // Fill the entire top row with the title-bar background.
    let spaces = " ".repeat(cols as usize);
    let _ = queue!(stdout, MoveTo(topwin_x, topwin_y), Print(&spaces));
    // Move back to start to overprint with actual title text.
    let _ = queue!(stdout, MoveTo(topwin_x, topwin_y));

    state_mut().as_an_at = false;

    let (upperleft, prefix, state, caption) = compute_titlebar_strings(path, currmenu, inhelp);

    let cols = cols as usize;
    let reserve_modified = !inhelp && path.is_none() && currmenu != MLINTER && with_state(|s| {
        let file = s.openfile.as_ref();
        !s.flag_isset(VIEW_MODE)
            && !s.flag_isset(STATEFLAGS)
            && !s.flag_isset(RESTRICTED)
            && !file.map(|f| f.modified).unwrap_or(false)
    });
    let layout = calculate_titlebar_layout(
        &upperleft,
        &prefix,
        &state,
        &caption,
        reserve_modified,
        cols,
    );

    // Print version / buffer ranking
    if layout.show_upperleft {
        let _ = queue!(stdout, MoveTo(topwin_x + 2, topwin_y));
        let _ = queue!(stdout, Print(&upperleft));
    }

    // Print prefix
    if layout.show_prefix && !prefix.is_empty() {
        let _ = queue!(stdout, MoveTo(topwin_x + layout.offset as u16, topwin_y));
        let _ = queue!(stdout, Print(&prefix));
        let _ = queue!(stdout, Print(" "));
    } else {
        let _ = queue!(stdout, MoveTo(topwin_x + layout.offset as u16, topwin_y));
    }

    // Print path / title
    if layout.pathlen + layout.pluglen + layout.statelen <= cols {
        let disp = display_string(&caption, 0, layout.pathlen, false, false);
        let _ = queue!(stdout, Print(&disp));
    } else if 5 + layout.statelen <= cols {
        let _ = queue!(stdout, Print("..."));
        let disp = display_string(&caption,
            3 + layout.pathlen.saturating_sub(cols.saturating_sub(layout.statelen)),
            cols.saturating_sub(layout.statelen),
            false, false);
        let _ = queue!(stdout, Print(&disp));
    }

    // Print state flags or state word
    #[cfg(not(feature = "tiny"))]
    {
        let (stateflags, view_mode, modified) = with_state(|s| (
            s.flag_isset(STATEFLAGS),
            s.flag_isset(VIEW_MODE),
            s.openfile.as_ref().map(|f| f.modified).unwrap_or(false),
        ));
        if !state.is_empty() && stateflags && !view_mode {
            if modified && cols > 1 {
                let _ = queue!(stdout, Print(" *"));
            }
            if layout.statelen < cols {
                let state_col = (cols + 2).saturating_sub(layout.statelen);
                let _ = queue!(stdout, MoveTo(topwin_x + state_col as u16, topwin_y));
                show_states_at_win(&crate::global::state().topwin.clone(), 0, state_col as u16);
            }
        } else {
            print_state_word(&state, layout.statelen, cols, topwin_x, topwin_y);
        }
    }
    #[cfg(feature = "tiny")]
    {
        print_state_word(&state, layout.statelen, cols, topwin_x, topwin_y);
    }

    queue_reset_color(&mut stdout);
    let _ = stdout.flush();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TitlebarLayout {
    pathlen: usize,
    statelen: usize,
    pluglen: usize,
    offset: usize,
    show_upperleft: bool,
    show_prefix: bool,
}

fn calculate_titlebar_layout(
    upperleft: &str,
    prefix: &str,
    state_word: &str,
    caption: &str,
    reserve_modified: bool,
    cols: usize,
) -> TitlebarLayout {
    let mut verlen = breadth(upperleft) + 3;
    let prefixlen = if prefix.is_empty() { 0 } else { breadth(prefix) + 1 };
    let mut pathlen = breadth(caption);
    // GNU nano always reserves the two side cells initially.  They are eaten
    // only after the version and the Modified placeholder have been
    // sacrificed on a narrow terminal.
    let mut statelen = breadth(state_word) + 2;
    if statelen > 2 {
        pathlen += 1;
    }
    let mut pluglen = if reserve_modified {
        breadth("Modified") + 1
    } else {
        0
    };

    let fits = |v: usize, p: usize, path: usize, plug: usize, state: usize| {
        v.saturating_add(p)
            .saturating_add(path)
            .saturating_add(plug)
            .saturating_add(state)
            <= cols
    };

    let show_upperleft = fits(verlen, prefixlen, pathlen, pluglen, statelen);
    if !show_upperleft {
        verlen = 2;
        if !fits(verlen, prefixlen, pathlen, pluglen, statelen) {
            pluglen = 0;
        }
        if !fits(verlen, prefixlen, pathlen, pluglen, statelen) {
            verlen = 0;
            statelen = statelen.saturating_sub(2);
        }
    }

    let show_prefix = fits(verlen, prefixlen, pathlen, pluglen, statelen);
    let offset = if verlen > 0 {
        verlen + cols
            .saturating_sub(verlen + pluglen + statelen + prefixlen + pathlen)
            / 2
    } else {
        0
    };

    TitlebarLayout {
        pathlen,
        statelen,
        pluglen,
        offset,
        show_upperleft,
        show_prefix,
    }
}

fn print_state_word(state: &str, statelen: usize, cols: usize, x: u16, y: u16) {
    if statelen > 0 {
        let mut stdout = out();
        if statelen <= cols {
            let col = (cols - statelen) as u16;
            let _ = queue!(stdout, MoveTo(x + col, y), Print(state));
        } else {
            let truncated = &state[..actual_x(state, cols)];
            let _ = queue!(stdout, MoveTo(x, y), Print(truncated));
        }
    }
}

/// Compute the title bar string components.
fn compute_titlebar_strings(
    path: Option<&str>,
    currmenu: u32,
    inhelp: bool,
) -> (String, String, String, String) {
    let upperleft: String;
    let prefix: String;
    let state: String;
    let caption: String;

    #[cfg(feature = "color")]
    if currmenu == MLINTER {
        prefix = "Linting --".to_string();
        let fname = with_state(|s| s.openfile.as_ref().map(|f| f.filename.clone()).unwrap_or_default());
        caption = fname;
        upperleft = String::new();
        state = String::new();
        return (upperleft, prefix, state, caption);
    }

    #[cfg(feature = "browser")]
    if !inhelp && path.is_some() {
        prefix = "DIR:".to_string();
        caption = path.unwrap_or("").to_string();
        #[cfg(feature = "multibuffer")]
        {
            upperleft = format!("[{}/{}]", buffer_number(), buffer_count());
        }
        #[cfg(not(feature = "multibuffer"))]
        {
            upperleft = "GNU nano".to_string();
        }
        state = String::new();
        return (upperleft, prefix, state, caption);
    }

    if !inhelp {
        #[cfg(feature = "multibuffer")]
        {
            let more_than_one = crate::global::state().more_than_one;
            if more_than_one {
                upperleft = format!("[{}/{}]", buffer_number(), buffer_count());
            } else {
                upperleft = "GNU nano".to_string();
            }
        }
        #[cfg(not(feature = "multibuffer"))]
        {
            upperleft = "GNU nano".to_string();
        }

        let (filename, modified, view_mode, stateflags, restricted) = with_state(|s| {
            let f = s.openfile.as_ref();
            let fname = f.map(|f| f.filename.clone()).unwrap_or_default();
            let modif = f.map(|f| f.modified).unwrap_or(false);
            let view = s.flag_isset(VIEW_MODE);
            let stateflags = s.flag_isset(STATEFLAGS);
            let rest = s.flag_isset(RESTRICTED);
            (fname, modif, view, stateflags, rest)
        });

        if filename.is_empty() {
            caption = "New Buffer".to_string();
        } else {
            caption = filename;
        }

        if view_mode {
            state = "View".to_string();
        } else if stateflags {
            state = "+.xxxxx".to_string();
        } else if modified {
            state = "Modified".to_string();
        } else if restricted {
            state = "Restricted".to_string();
        } else {
            state = String::new();
        }

        prefix = String::new();
    } else {
        // In help viewer
        upperleft = "GNU nano".to_string();
        prefix = String::new();
        caption = path.map(|p| p.to_string()).unwrap_or_else(|| "Help".to_string());
        state = String::new();
    }

    (upperleft, prefix, state, caption)
}

// ---------------------------------------------------------------------------
// minibar (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void minibar(void) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn minibar() {
    let (footwin_x, footwin_y, cols, mini_pair,
         filename, modified, current_lineno, filebot_lineno, _totsize,
         constant_show, stateflags, has_anchor, using_utf8) = with_state(|s| {
        let f = s.openfile.as_ref();
        (
            s.footwin.x, s.footwin.y, s.footwin.cols as usize,
            s.interface_color_pair[MINI_INFOBAR],
            f.map(|f| f.filename.clone()).unwrap_or_default(),
            f.map(|f| f.modified).unwrap_or(false),
            f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1),
            f.and_then(|f| f.filebot.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1),
            f.map(|f| f.totsize).unwrap_or(0),
            s.flag_isset(CONSTANT_SHOW),
            s.flag_isset(STATEFLAGS),
            f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().has_anchor).unwrap_or(false),
            s.using_utf8,
        )
    });

    if cols == 0 { return; }

    let mut stdout = out();

    // Draw colored bar
    apply_interface_color(mini_pair);

    let spaces = " ".repeat(cols);
    let _ = queue!(stdout, MoveTo(footwin_x, footwin_y), Print(&spaces));

    state_mut().as_an_at = false;

    let thename = if !filename.is_empty() {
        display_string(&filename, 0, cols, false, false)
    } else {
        "(nameless)".to_string()
    };

    let col_pos = xplustabs() + 1;
    let location = format!("{},{}", current_lineno, col_pos);
    let placewidth = location.len();
    let namewidth = breadth(&thename);
    let padding: usize = if namewidth + 19 > cols { 0 } else { 2 };

    // Display filename (possibly truncated)
    if cols > 4 {
        if namewidth > cols - 2 {
            let shortname = display_string(&thename, namewidth.saturating_sub(cols - 5), cols - 5, false, false);
            let _ = queue!(stdout, MoveTo(footwin_x, footwin_y), Print("..."), Print(&shortname));
        } else {
            let _ = queue!(stdout, MoveTo(footwin_x + padding as u16, footwin_y), Print(&thename));
        }
        let _ = queue!(stdout, Print(if modified { " *" } else { "  " }));
    }

    // Display buffer ranking or line count
    #[cfg(feature = "multibuffer")]
    {
        // C: openfile != openfile->next — i.e. more than one buffer is open.
        let multiple = with_state(|s| !s.buffer_ring.is_empty());
        if multiple && cols > 35 {
            let ranking = format!(" [{}/{}]", buffer_number(), buffer_count());
            if namewidth + placewidth + breadth(&ranking) + 32 < cols {
                let _ = queue!(stdout, Print(&ranking));
            }
        }
    }

    // Display cursor position.  C guard: namewidth + tallywidth + placewidth + 32
    // < COLS (tallywidth is 0 here, as the minibar shows no line-count tally).
    // Including placewidth is what stops `cols - 27 - placewidth` from underflowing
    // (and panicking) on a narrow terminal.
    if constant_show && namewidth + placewidth + 32 < cols {
        let loc_col = (cols - 27 - placewidth) as u16;
        let _ = queue!(stdout, MoveTo(footwin_x + loc_col, footwin_y), Print(&location));
    }

    // Display the hex code of the character under the cursor, plus the codes of
    // up to two succeeding zero-width characters (C winio.c:2232-2269).
    let mut had_successor = false;
    if constant_show && namewidth + 28 < cols {
        let mut hex_str = compute_cursor_hex();
        let (succ, has_succ) = compute_cursor_hex_successors();
        hex_str.push_str(&succ);
        had_successor = has_succ;
        let hex_col = (cols - 23) as u16;
        let _ = queue!(stdout, MoveTo(footwin_x + hex_col, footwin_y), Print(&hex_str));
    }

    // Display the state flags — but not when a succeeding zero-width code was
    // shown (C gates this on !successor).
    if stateflags && !had_successor && namewidth + 14 + 2 * padding < cols {
        let state_col = (cols - 11 - padding) as u16;
        show_states_at_win(&state().footwin.clone(), 0, state_col);
    }

    // Display anchor indicator
    if has_anchor && namewidth + 7 < cols {
        let anchor_col = (cols - 5 - padding) as u16;
        let dagger = if using_utf8 { "\u{2020}" } else { "+" };
        let _ = queue!(stdout, MoveTo(footwin_x + anchor_col, footwin_y), Print(dagger));
    }

    // Display percentage
    if namewidth + 6 < cols {
        let pct = 100 * current_lineno / filebot_lineno.max(1);
        let pct_str = format!("{:3}%", pct);
        let pct_col = (cols - 4 - padding) as u16;
        let _ = queue!(stdout, MoveTo(footwin_x + pct_col, footwin_y), Print(&pct_str));
    }

    reset_color();
    let _ = stdout.flush();
}

/// Compute hexadecimal representation of character under cursor.
#[cfg(not(feature = "tiny"))]
fn compute_cursor_hex() -> String {
    let (data, current_x, using_utf8, has_next) = with_state(|s| {
        let f = s.openfile.as_ref();
        let data = f.and_then(|f| f.current.as_ref())
            .map(|l| l.borrow().data.clone())
            .unwrap_or_default();
        let cx = f.map(|f| f.current_x).unwrap_or(0);
        let utf8 = s.using_utf8;
        let next = f.and_then(|f| f.current.as_ref())
            .and_then(|l| l.borrow().next.clone())
            .is_some();
        (data, cx, utf8, next)
    });

    if current_x >= data.len() {
        if has_next {
            return if using_utf8 { "U+000A".to_string() } else { "  0x0A".to_string() };
        } else {
            return "  ----".to_string();
        }
    }

    let ch = &data[current_x..];
    let b = ch.as_bytes()[0];

    if b == b'\n' {
        return "  0x00".to_string();
    }

    #[cfg(feature = "utf8")]
    if using_utf8 {
        if b < 0x80 {
            return format!("U+{:04X}", b);
        }
        if let Ok((wc, _)) = mbtowide(ch) {
            return format!("U+{:04X}", wc as u32);
        }
    }

    format!("  0x{:02X}", b)
}

/// Compute the "|XXXX" codes of up to two zero-width characters that immediately
/// follow the character under the cursor (C winio.c:2253-2269).  Returns the
/// appended string and whether any successor code was produced.
#[cfg(not(feature = "tiny"))]
fn compute_cursor_hex_successors() -> (String, bool) {
    #[cfg(not(feature = "utf8"))]
    {
        (String::new(), false)
    }
    #[cfg(feature = "utf8")]
    {
        let (data, current_x, using_utf8) = with_state(|s| {
            let f = s.openfile.as_ref();
            let data = f.and_then(|f| f.current.as_ref())
                .map(|l| l.borrow().data.clone())
                .unwrap_or_default();
            let cx = f.map(|f| f.current_x).unwrap_or(0);
            (data, cx, s.using_utf8)
        });
        if !using_utf8 || current_x >= data.len() {
            return (String::new(), false);
        }
        // successor starts just after the character under the cursor.
        let this_pos = &data[current_x..];
        let mut succ_start = current_x + char_length(this_pos);
        let mut out = String::new();
        let mut had = false;
        if succ_start < data.len() {
            let succ = &data[succ_start..];
            if is_zerowidth(succ) {
                if let Ok((wc, _)) = mbtowide(succ) {
                    out.push_str(&format!("|{:04X}", wc as u32));
                    had = true;
                    succ_start += char_length(succ);
                    if succ_start < data.len() {
                        let succ2 = &data[succ_start..];
                        if is_zerowidth(succ2) {
                            if let Ok((wc2, _)) = mbtowide(succ2) {
                                out.push_str(&format!("|{:04X}", wc2 as u32));
                            }
                        }
                    }
                }
            }
        }
        (out, had)
    }
}

// ---------------------------------------------------------------------------
// statusline
// ---------------------------------------------------------------------------

/* C: void statusline(message_type importance, const char *msg, ...) */
pub fn statusline(importance: MessageType, msg: &str) {
    // Drop all waiting keystrokes upon any kind of "error" (C: importance >= AHEM).
    // This fires only for AHEM/MILD/ALERT, never for ordinary INFO/HUSH updates,
    // so it cannot destroy in-flight UTF-8 continuation bytes.
    if importance >= MessageType::Ahem {
        WAITING_CODES.with(|wc| *wc.borrow_mut() = 0);
        NEXTCODES_IDX.with(|ni| *ni.borrow_mut() = 0);
    }

    let lastmessage = state().lastmessage;

    // Ignore lower-importance messages
    if importance < lastmessage && lastmessage > MessageType::Notice {
        return;
    }

    let (cols, footwin_x, footwin_y, _zero, _minibar_on, _currmenu) = with_state(|s| (
        s.footwin.cols as usize,
        s.footwin.x,
        s.footwin.y,
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
        s.currmenu,
    ));

    let mut stdout = out();

    // If multiple ALERT messages, add trailing dots
    if lastmessage == MessageType::Alert {
        let start_col = STATUSLINE_START_COL.with(|sc| *sc.borrow());
        if start_col > 4 {
            let alert_pair = state().interface_color_pair[ERROR_MESSAGE];
            apply_interface_color(alert_pair);
            let col = (cols + 2).saturating_sub(start_col) as u16;
            let _ = queue!(stdout, MoveTo(footwin_x + col, footwin_y), Print("..."));
            reset_color();
            let _ = stdout.flush();
            STATUSLINE_START_COL.with(|sc| *sc.borrow_mut() = 0);
        }
        return;
    }

    // Determine color pair
    let colorpair = if importance > MessageType::Notice {
        state().interface_color_pair[ERROR_MESSAGE]
    } else if importance == MessageType::Notice {
        state().interface_color_pair[SELECTED_TEXT]
    } else {
        state().interface_color_pair[STATUS_BAR]
    };

    if importance == MessageType::Alert {
        // beep equivalent — terminal bell
        let _ = print!("\x07");
    }

    state_mut().lastmessage = importance;

    blank_statusbar();

    // Temporarily disable WHITESPACE_DISPLAY for the message
    let showed_whitespace = state().flag_isset(WHITESPACE_DISPLAY);
    if showed_whitespace {
        with_state_mut(|s| {
            s.flags[flag_index(WHITESPACE_DISPLAY)] &= !flag_mask(WHITESPACE_DISPLAY);
        });
    }

    let message = display_string(msg, 0, cols, false, false);

    if showed_whitespace {
        with_state_mut(|s| {
            s.flags[flag_index(WHITESPACE_DISPLAY)] |= flag_mask(WHITESPACE_DISPLAY);
        });
    }

    let msg_width = breadth(&message);
    let start_col = if msg_width < cols { (cols - msg_width) / 2 } else { 0 };
    let bracketed = start_col > 1;

    STATUSLINE_START_COL.with(|sc| *sc.borrow_mut() = start_col);

    apply_interface_color(colorpair);

    let col = if bracketed { start_col.saturating_sub(2) } else { start_col };
    let _ = queue!(stdout, MoveTo(footwin_x + col as u16, footwin_y));
    if bracketed { let _ = queue!(stdout, Print("[ ")); }
    let _ = queue!(stdout, Print(&message));
    if bracketed { let _ = queue!(stdout, Print(" ]")); }

    reset_color();
    let _ = stdout.flush();

    // Set countdown for auto-blank
    let quick_blank = state().flag_isset(QUICK_BLANK);
    tl_set!(COUNTDOWN, if quick_blank { 1 } else { 20 });
}

/* C: void statusbar(const char *msg) */
pub fn statusbar(msg: &str) {
    statusline(MessageType::Hush, msg);
}

/* C: void warn_and_briefly_pause(const char *msg) */
pub fn warn_and_briefly_pause(msg: &str) {
    blank_bottombars();
    statusline(MessageType::Alert, msg);
    state_mut().lastmessage = MessageType::Vacuum;
    // Sleep 1500ms equivalent
    std::thread::sleep(std::time::Duration::from_millis(1500));
}

// ---------------------------------------------------------------------------
// post_one_key and bottombars
// ---------------------------------------------------------------------------

/* C: void post_one_key(const char *keystroke, const char *tag, int width) */
/// Draws one key+description pair at the *current* cursor position.
/// The caller is responsible for positioning the cursor beforehand.
pub fn post_one_key(keystroke: &str, tag: &str, width: i32) {
    let width = width as usize;
    let mut stdout = out();

    // Key name in KEY_COMBO color (reverse video by default).
    let key_pair = state().interface_color_pair[KEY_COMBO];
    queue_interface_color(&mut stdout, key_pair);
    let ks_len = actual_x(keystroke, width);
    let ks_display = &keystroke[..ks_len];
    let _ = queue!(stdout, Print(ks_display));
    queue_reset_color(&mut stdout);

    let ks_width = breadth(keystroke);
    let remaining = width.saturating_sub(ks_width);
    if remaining < 2 { return; }

    let _ = queue!(stdout, Print(" "));

    // Function name in FUNCTION_TAG color (normal by default).
    let func_pair = state().interface_color_pair[FUNCTION_TAG];
    queue_interface_color(&mut stdout, func_pair);
    let tag_len = actual_x(tag, remaining - 1);
    let tag_display = &tag[..tag_len];
    let _ = queue!(stdout, Print(tag_display));
    queue_reset_color(&mut stdout);
}

/// Internal: post_one_key with explicit row/col positioning (used by bottombars).
fn post_one_key_at(keystroke: &str, tag: &str, width: usize, row: u16, col: u16) {
    let (footwin_x, footwin_y) = with_state(|s| (s.footwin.x, s.footwin.y));
    let mut stdout = out();
    let _ = queue!(stdout, MoveTo(footwin_x + col, footwin_y + row));
    post_one_key(keystroke, tag, width as i32);
}

/* C: void bottombars(int menu) */
pub fn bottombars(menu: u32) {
    state_mut().currmenu = menu;

    let (no_help, zero, minibar_on) = with_state(|s| (
        s.flag_isset(NO_HELP),
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
    ));
    let lines = screen_rows();

    let min_lines = if zero { 3 } else if minibar_on { 4 } else { 5 };
    if no_help || (lines as i32) < min_lines { return; }

    let number = shown_entries_for(menu);
    if number == 0 { return; }

    let cols = state().footwin.cols as usize;
    let itemwidth = if number == 0 { return } else { cols / ((number + 1) / 2) };
    if itemwidth == 0 { return; }

    blank_bottombars();

    let entries: Vec<(i32, &'static str, &'static str)> = with_state(|s| {
        let mut v = Vec::new();
        let mut count = 0usize;
        for f in &s.allfuncs {
            if count >= number { break; }
            if (f.menus as u32 & menu) == 0 { continue; }
            // Find first shortcut for this function
            if let Some(func) = f.func {
                for sc in &s.sclist {
                    if (sc.menus as u32 & menu) != 0 && sc.func == Some(func) && !sc.keystr.is_empty() {
                        v.push((sc.keycode, sc.keystr, f.tag));
                        break;
                    }
                }
            }
            if v.len() > count { count += 1; }
        }
        v
    });

    for (index, (_keycode, keystr, tag)) in entries.iter().enumerate() {
        let row = (index % 2) as u16;
        let col_pos = ((index / 2) * itemwidth) as u16;
        let this_width = if (number % 2) == 1 && index + 2 == number {
            itemwidth * 2
        } else if index + 2 >= number {
            itemwidth + cols % itemwidth
        } else {
            itemwidth
        };
        post_one_key_at(keystr, tag, this_width, 1 + row, col_pos);
    }

    let _ = out().flush();
}

// ---------------------------------------------------------------------------
// place_the_cursor
// ---------------------------------------------------------------------------

/* C: void place_the_cursor(void) */
pub fn place_the_cursor() {
    let (editwinrows, midwin_x, midwin_y, margin) = with_state(|s| {
        (s.editwinrows, s.midwin.x, s.midwin.y, s.margin)
    });

    let column = xplustabs();
    let mut row: isize;

    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let (edittop, firstcolumn, current) = with_state(|s| {
            let f = s.openfile.as_ref();
            (
                f.and_then(|f| f.edittop.clone()),
                f.map(|f| f.firstcolumn).unwrap_or(0),
                f.and_then(|f| f.current.clone()),
            )
        });
        let (Some(edittop), Some(current)) = (edittop, current) else { return };

        row = -({ let b = edittop.borrow(); chunk_for(firstcolumn, &b.data) } as isize);

        // Calculate how many rows the lines from edittop to current use.
        let mut line = Some(edittop);
        while let Some(l) = line {
            if LinePtr::ptr_eq(&l, &current) { break; }
            row += 1 + { let b = l.borrow(); extra_chunks_in(&b.data) as isize };
            line = l.borrow().next.clone();
        }

        // Add the number of wraps in the current line before the cursor.
        let (chunk_row, leftedge) = { let b = current.borrow(); get_chunk_and_edge_for(&b.data, column) };
        row += chunk_row as isize;
        let col = column - leftedge;

        if row >= 0 && row < editwinrows as isize {
            let _ = queue!(out(), MoveTo(midwin_x + margin as u16 + col as u16,
                midwin_y + row as u16));
            with_state_mut(|s| s.openfile.as_mut().map(|f| f.cursor_row = row));
        } else {
            statusline(MessageType::Alert, "Misplaced cursor -- please report a bug");
        }
        let _ = out().flush();
        return;
    }

    #[cfg(feature = "tiny")]
    let _ = ();

    // Non-softwrap path
    let (edittop_lineno, current_lineno, _current_x) = with_state(|s| {
        let f = s.openfile.as_ref();
        let et = f.and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        let cl = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        let cx = f.map(|f| f.current_x).unwrap_or(0);
        (et, cl, cx)
    });

    row = (current_lineno - edittop_lineno) as isize;
    let page_col = get_page_start(column);
    let display_col = column - page_col;

    // Clamp row to the visible edit window — cursor may temporarily appear out
    // of range if the viewport hasn't caught up with a buffer modification.
    let clamped_row = row.min(editwinrows as isize - 1).max(0);
    let _ = queue!(out(), MoveTo(
        midwin_x + margin as u16 + display_col as u16,
        midwin_y + clamped_row as u16
    ));
    with_state_mut(|s| { s.openfile.as_mut().map(|f| f.cursor_row = clamped_row); });
    let _ = out().flush();
}

// ---------------------------------------------------------------------------
// Softwrap helpers (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: size_t get_softwrap_breakpoint(const char *linedata, size_t leftedge,
                                      bool *kickoff, bool *end_of_line) */
pub fn get_softwrap_breakpoint(
    linedata: &str,
    leftedge: usize,
    kickoff: &mut bool,
    end_of_line: &mut bool,
) -> usize {
    let editwincols = state().editwincols as usize;
    let at_blanks = state().flag_isset(AT_BLANKS);
    let tabsize = state().tabsize as usize;
    let _tabsize = if tabsize == 0 { 8 } else { tabsize };

    let rightside = leftedge + editwincols;

    // Initialize or continue from static state
    let (mut text_offset, mut column) = if *kickoff {
        SWB_TEXT_OFFSET.with(|o| *o.borrow_mut() = 0);
        SWB_COLUMN.with(|c| *c.borrow_mut() = 0);
        *kickoff = false;
        (0usize, 0usize)
    } else {
        (
            SWB_TEXT_OFFSET.with(|o| *o.borrow()),
            SWB_COLUMN.with(|c| *c.borrow()),
        )
    };

    let _bytes = linedata.as_bytes();
    let len = linedata.len();

    // Find where the current chunk starts
    while text_offset < len && column < leftedge {
        let consumed = advance_over(&linedata[text_offset..], &mut column);
        text_offset += consumed;
    }

    let mut breaking_col = rightside;
    let mut last_blank_offset: Option<usize> = None;
    let mut last_blank_col = 0usize;

    // Find where this chunk ends
    while text_offset < len && column <= rightside {
        let ch = &linedata[text_offset..];
        if at_blanks && is_blank_char(ch) && column < rightside {
            last_blank_offset = Some(text_offset);
            last_blank_col = column;
        }
        breaking_col = if ch.starts_with('\t') { rightside } else { column };
        let consumed = advance_over(ch, &mut column);
        text_offset += consumed;
    }

    // Save state for next call
    SWB_TEXT_OFFSET.with(|o| *o.borrow_mut() = text_offset);
    SWB_COLUMN.with(|c| *c.borrow_mut() = column);

    if column <= rightside {
        *end_of_line = column < rightside;
        return column;
    }

    // Softwrap-at-blanks
    if let Some(blank_off) = last_blank_offset {
        let blank_ch = &linedata[blank_off..];
        let mut after = last_blank_col;
        let step = advance_over(blank_ch, &mut after);
        if after <= rightside {
            SWB_TEXT_OFFSET.with(|o| *o.borrow_mut() = blank_off + step);
            SWB_COLUMN.with(|c| *c.borrow_mut() = after);
            return after;
        }
        if blank_ch.starts_with('\t') {
            breaking_col = rightside;
        }
    }

    if editwincols > 1 { breaking_col } else { column.saturating_sub(1) }
}

/* C: size_t get_chunk_and_edge(size_t column, linestruct *line, size_t *leftedge) */
pub fn get_chunk_and_edge_for(linedata: &str, column: usize) -> (usize, usize) {
    let mut current_chunk = 0usize;
    let mut end_of_line = false;
    let mut kickoff = true;
    let mut start_col = 0usize;

    loop {
        let end_col = get_softwrap_breakpoint(linedata, start_col, &mut kickoff, &mut end_of_line);
        if end_of_line || (start_col <= column && column < end_col) {
            return (current_chunk, start_col);
        }
        start_col = end_col;
        current_chunk += 1;
    }
}

/* C: size_t extra_chunks_in(linestruct *line) */
pub fn extra_chunks_in(linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, usize::MAX).0
}

/* C: size_t chunk_for(size_t column, linestruct *line) */
pub fn chunk_for(column: usize, linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, column).0
}

/* C: size_t leftedge_for(size_t column, linestruct *line) */
pub fn leftedge_for(column: usize, linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, column).1
}

/* C: void ensure_firstcolumn_is_aligned(void) */
pub fn ensure_firstcolumn_is_aligned() {
    let softwrap = state().flag_isset(SOFTWRAP);
    if softwrap {
        let (firstcolumn, edittop_data) = with_state(|s| (
            s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0),
            s.openfile.as_ref().and_then(|f| f.edittop.as_ref())
                .map(|l| l.borrow().data.clone()).unwrap_or_default(),
        ));
        let new_fc = leftedge_for(firstcolumn, &edittop_data);
        with_state_mut(|s| { s.openfile.as_mut().map(|f| f.firstcolumn = new_fc); });
    } else {
        with_state_mut(|s| { s.openfile.as_mut().map(|f| f.firstcolumn = 0); });
    }
    state_mut().focusing = false;
}

/* C: size_t actual_last_column(size_t leftedge, size_t column) */
pub fn actual_last_column(leftedge: usize, column: usize) -> usize {
    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let linedata = with_state(|s| {
            s.openfile.as_ref()
                .and_then(|f| f.current.as_ref())
                .map(|l| l.borrow().data.clone())
                .unwrap_or_default()
        });
        let mut kickoff = true;
        let mut last_chunk = false;
        let end_col = get_softwrap_breakpoint(&linedata, leftedge, &mut kickoff, &mut last_chunk);
        let end_col = end_col - leftedge;
        let end_col = if !last_chunk { end_col.saturating_sub(1) } else { end_col };
        return leftedge + column.min(end_col);
    }
    leftedge + column
}

// ---------------------------------------------------------------------------
// Viewport / offscreen tests
// ---------------------------------------------------------------------------

/* C: bool current_is_above_screen(void) */
pub fn current_is_above_screen() -> bool {
    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let (cur_lineno, et_lineno, _cur_col, firstcol) = with_state(|s| {
            let f = s.openfile.as_ref();
            let cl = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
            let et = f.and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
            let cc = xplustabs();
            let fc = f.map(|f| f.firstcolumn).unwrap_or(0);
            (cl, et, cc, fc)
        });
        return cur_lineno < et_lineno
            || (cur_lineno == et_lineno && xplustabs() < firstcol);
    }

    let (cur_lineno, et_lineno) = with_state(|s| {
        let f = s.openfile.as_ref();
        let cl = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        let et = f.and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        (cl, et)
    });
    cur_lineno < et_lineno
}

/* C: bool current_is_below_screen(void) */
pub fn current_is_below_screen() -> bool {
    let (editwinrows, shim) = with_state(|s| {
        let shim = if s.flag_isset(ZERO) && (s.currmenu == MREPLACEWITH || s.currmenu == MYESNO) { 1 } else { 0 };
        (s.editwinrows, shim)
    });

    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let (edittop, firstcol, current) = with_state(|s| {
            let f = s.openfile.as_ref();
            (
                f.and_then(|f| f.edittop.clone()),
                f.map(|f| f.firstcolumn).unwrap_or(0),
                f.and_then(|f| f.current.clone()),
            )
        });
        let (Some(mut line), Some(current)) = (edittop, current) else { return false };
        let mut leftedge = firstcol;

        // If current[current_x] is more than a screen's worth of lines after
        // edittop at column firstcolumn, it's below the screen.
        let exhausted = go_forward_chunks(editwinrows - 1 - shim, &mut line, &mut leftedge) == 0;
        let line_lineno = line.borrow().lineno;
        let cur_lineno = current.borrow().lineno;
        return exhausted
            && (line_lineno < cur_lineno
                || (line_lineno == cur_lineno
                    && leftedge < { let b = current.borrow(); leftedge_for(xplustabs(), &b.data) }));
    }

    let (cur_lineno, et_lineno) = with_state(|s| {
        let f = s.openfile.as_ref();
        let cl = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        let et = f.and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0);
        (cl, et)
    });
    cur_lineno >= et_lineno + editwinrows as isize - shim as isize
}

/* C: bool current_is_offscreen(void) */
pub fn current_is_offscreen() -> bool {
    current_is_above_screen() || current_is_below_screen()
}

// ---------------------------------------------------------------------------
// go_back_chunks / go_forward_chunks
// ---------------------------------------------------------------------------

/* C: int go_back_chunks(int nrows, linestruct **line, size_t *leftedge) */
pub fn go_back_chunks(nrows: i32, line: &mut LinePtr, leftedge: &mut usize) -> i32 {
    let mut i = nrows;

    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        // Recede through the requested number of chunks.
        while i > 0 {
            let chunk = { let b = line.borrow(); chunk_for(*leftedge, &b.data) };
            *leftedge = 0;

            if chunk >= i as usize {
                return go_forward_chunks(chunk as i32 - i, line, leftedge);
            }

            let prev = line.borrow().prev.as_ref().and_then(|w| w.upgrade());
            match prev {
                None => break, // C: *line == openfile->filetop
                Some(p) => {
                    i -= chunk as i32;
                    *line = p;
                    *leftedge = HIGHEST_POSITIVE;
                }
            }
            i -= 1; // the C for-loop's own i--
        }

        if *leftedge == HIGHEST_POSITIVE {
            let b = line.borrow();
            *leftedge = leftedge_for(HIGHEST_POSITIVE, &b.data);
        }
        return i;
    }

    // Non-softwrap path
    while i > 0 {
        let prev = line.borrow().prev.as_ref().and_then(|w| w.upgrade());
        match prev {
            None => break,
            Some(p) => {
                *line = p;
                i -= 1;
            }
        }
    }
    i
}

/* C: int go_forward_chunks(int nrows, linestruct **line, size_t *leftedge) */
pub fn go_forward_chunks(nrows: i32, line: &mut LinePtr, leftedge: &mut usize) -> i32 {
    let mut i = nrows;

    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let mut current_leftedge = *leftedge;
        let mut kickoff = true;

        // Advance through the requested number of chunks.
        while i > 0 {
            let mut end_of_line = false;
            current_leftedge = {
                let b = line.borrow();
                get_softwrap_breakpoint(&b.data, current_leftedge, &mut kickoff, &mut end_of_line)
            };

            if !end_of_line { i -= 1; continue; }

            let next = line.borrow().next.clone();
            match next {
                None => break, // C: *line == openfile->filebot (no i-- on break)
                Some(n) => {
                    *line = n;
                    current_leftedge = 0;
                    kickoff = true;
                }
            }
            i -= 1; // the C for-loop's own i--
        }

        // Only change leftedge when we actually could move.
        if i < nrows { *leftedge = current_leftedge; }
        return i;
    }

    // Non-softwrap path
    while i > 0 {
        let next = line.borrow().next.clone();
        match next {
            None => break,
            Some(n) => {
                *line = n;
                i -= 1;
            }
        }
    }
    i
}

/* C: bool less_than_a_screenful(size_t was_lineno, size_t was_leftedge) */
pub fn less_than_a_screenful(was_lineno: usize, was_leftedge: usize) -> bool {
    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        let (editwinrows, current) = with_state(|s| (
            s.editwinrows,
            s.openfile.as_ref().and_then(|f| f.current.clone()),
        ));
        let Some(mut line) = current else { return true };
        let col = xplustabs();
        let mut leftedge = { let b = line.borrow(); leftedge_for(col, &b.data) };
        let rows_left = go_back_chunks(editwinrows - 1, &mut line, &mut leftedge);
        let back_lineno = line.borrow().lineno;
        return rows_left > 0 || back_lineno < was_lineno as isize
            || (back_lineno == was_lineno as isize && leftedge <= was_leftedge);
    }

    let (cur_lineno, editwinrows) = with_state(|s| (
        s.openfile.as_ref().and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0),
        s.editwinrows,
    ));
    (cur_lineno - was_lineno as isize) < editwinrows as isize
}

// ---------------------------------------------------------------------------
// draw_row — draw one row of the edit window
// ---------------------------------------------------------------------------

/* C: void draw_row(int row, const char *converted, linestruct *line, size_t from_col) */
pub fn draw_row(row: i32, converted: &str, line: &LinePtr, from_col: usize)
{
    // Keep node borrows scoped: apply_syntax_highlighting below needs to
    // borrow_mut the node to update its multidata.
    let (line_lineno, has_anchor) = {
        let node = line.borrow();
        (node.lineno, node.has_anchor)
    };

    let (midwin_x, midwin_y, margin, cols, _sidebar, editwincols) = with_state(|s| {
        (s.midwin.x, s.midwin.y, s.margin, s.midwin.cols as usize, s.sidebar, s.editwincols as usize)
    });
    let mut stdout = out();

    let abs_y = midwin_y + row as u16;

    // Line numbers
    #[cfg(feature = "linenumbers")]
    if margin > 0 {
        let ln_pair = state().interface_color_pair[LINE_NUMBER];
        apply_interface_color(ln_pair);

        #[cfg(not(feature = "tiny"))]
        let softwrap = state().flag_isset(SOFTWRAP);
        #[cfg(feature = "tiny")]
        let softwrap = false;

        if softwrap && from_col != 0 {
            let spaces = " ".repeat((margin - 1) as usize);
            let _ = queue!(stdout, MoveTo(midwin_x, abs_y), Print(&spaces));
        } else {
            let num_str = format!("{:>width$}", line_lineno, width = (margin - 1) as usize);
            let _ = queue!(stdout, MoveTo(midwin_x, abs_y), Print(&num_str));
        }
        reset_color();

        // Anchor indicator
        #[cfg(not(feature = "tiny"))]
        if has_anchor && (from_col == 0 || !softwrap) {
            let using_utf8 = state().using_utf8;
            let dagger = if using_utf8 { "\u{2020}" } else { "+" };
            let _ = queue!(stdout, Print(dagger));
        } else {
            let _ = queue!(stdout, Print(" "));
        }
        #[cfg(feature = "tiny")]
        let _ = queue!(stdout, Print(" "));
    }

    // Write the converted line text
    let _ = queue!(stdout, MoveTo(midwin_x + margin as u16, abs_y), Print(converted));

    // Clear to end of line if needed
    let is_shorter = tl_get!(IS_SHORTER);
    let softwrap_on = {
        #[cfg(not(feature = "tiny"))]
        { state().flag_isset(SOFTWRAP) }
        #[cfg(feature = "tiny")]
        { false }
    };

    if is_shorter || softwrap_on {
        let _ = queue!(stdout, Clear(ClearType::UntilNewLine));
    }

    // Scrollbar character
    #[cfg(not(feature = "tiny"))]
    {
        let sidebar_val = state().sidebar;
        if sidebar_val != 0 {
            let bardata = state().bardata.get(row as usize).copied().unwrap_or(b' ' as i32);
            let bar_char = (bardata & 0xFF) as u8 as char;
            let bar_reverse = (bardata & A_REVERSE) != 0;
            if bar_reverse {
                let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
            }
            let _ = queue!(stdout, MoveTo(midwin_x + cols as u16 - 1, abs_y), Print(bar_char));
            if bar_reverse {
                let _ = queue!(stdout, SetAttribute(Attribute::NormalIntensity));
                reset_color();
            }
        }
    }

    // Syntax highlighting
    #[cfg(feature = "color")]
    apply_syntax_highlighting(row, converted, line, from_col, abs_y, midwin_x, margin);

    // Guide stripe
    #[cfg(not(feature = "tiny"))]
    {
        let stripe_col = state().stripe_column;
        let sequel = tl_get!(SEQUEL_COLUMN);
        let inhelp = state().inhelp;
        let from_col_usize = from_col;
        if stripe_col > from_col_usize as isize && !inhelp
            && (sequel == 0 || stripe_col as usize <= sequel)
            && stripe_col as usize <= from_col_usize + editwincols
        {
            let target_column = (stripe_col as usize - from_col_usize).saturating_sub(1);
            let target_x = actual_x(converted, target_column);
            let striped_char = if target_x < converted.len() {
                let cl = char_length(&converted[target_x..]);
                converted[target_x..target_x + cl].to_string()
            } else {
                " ".to_string()
            };

            let guide_pair = state().interface_color_pair[GUIDE_STRIPE];
            apply_interface_color(guide_pair);
            let _ = queue!(stdout, MoveTo(midwin_x + margin as u16 + target_column as u16, abs_y),
                Print(&striped_char));
            reset_color();
        }
    }

    // Mark highlighting
    #[cfg(not(feature = "tiny"))]
    {
        let node = line.borrow();
        apply_mark_highlighting(row, converted, line_lineno, &node.data, from_col, abs_y, midwin_x, margin);
    }
}

/// Apply syntax color rules to a drawn row. (ENABLE_COLOR)
/* C: the syntax-painting part of edit_draw() — winio.c. */
#[cfg(feature = "color")]
fn apply_syntax_highlighting(
    _row: i32,
    converted: &str,
    line: &LinePtr,
    from_col: usize,
    abs_y: u16,
    midwin_x: u16,
    margin: i32,
) {
    let syntax = with_state(|s| {
        s.openfile.as_ref()
            .and_then(|f| f.syntax)
            .map(|p| unsafe { &*p })
    });

    let no_syntax = state().flag_isset(NO_SYNTAX);
    if no_syntax { return; }
    let Some(syntax) = syntax else { return };

    let from_x = tl_get!(FROM_X);
    let till_x = tl_get!(TILL_X);
    const PAINT_LIMIT: usize = 2000;

    // Paint `piece` (a slice of `converted`) at the given display column,
    // in the rule's colors and attributes.
    let paint = |v: &ColorType, start_col: usize, piece: &str| {
        if piece.is_empty() {
            return;
        }
        set_color(v);
        let mut stdout = out();
        let _ = queue!(stdout,
            MoveTo(midwin_x + margin as u16 + start_col as u16, abs_y),
            Print(piece),
        );
        reset_color();
    };

    let priorline = line.borrow().prev.as_ref().and_then(|w| w.upgrade());

    let mut varnish: Option<&ColorType> = syntax.color.as_deref();
    while let Some(v) = varnish {
        // First case: varnish is a single-line expression.
        if v.end.is_none() {
            if let Some(regex) = &v.start {
                let node = line.borrow();
                let line_data: &str = &node.data;
                let mut search_from = from_x;

                while search_from < PAINT_LIMIT && search_from < till_x {
                    // find_at gives REG_NOTBOL semantics: ^ only matches
                    // at the real start of the line.
                    let Some(m) = regex.find_at(line_data, search_from) else { break };
                    let match_so = m.start();
                    let match_eo = m.end();

                    if match_so >= till_x { break; }
                    if match_so == match_eo {
                        if match_eo >= line_data.len() { break; }
                        search_from = step_right(line_data, match_eo);
                        continue;
                    }
                    if match_eo <= from_x {
                        search_from = match_eo;
                        continue;
                    }

                    let start_col = if match_so > from_x {
                        wideness(line_data, match_so).saturating_sub(from_col)
                    } else { 0 };

                    let thetext_x = actual_x(converted, start_col);
                    let end_col = wideness(line_data, match_eo).saturating_sub(from_col);
                    let paintlen = actual_x(&converted[thetext_x..], end_col.saturating_sub(start_col));

                    paint(v, start_col, &converted[thetext_x..thetext_x + paintlen]);

                    search_from = match_eo;
                }
            }
            varnish = v.next.as_deref();
            continue;
        }

        // Second case: varnish is a multiline expression.
        let (Some(start_re), Some(end_re)) = (&v.start, &v.end) else {
            varnish = v.next.as_deref();
            continue;
        };
        let id = v.id as usize;

        // Assume nothing gets painted until proven otherwise below;
        // collect the new multidata state and write it once afterwards
        // (the node must not be borrowed when we borrow_mut it).
        let new_state: i16 = {
            let node = line.borrow();
            let line_data: &str = &node.data;
            let mut state: i16 = NOTHING;
            let mut index: usize = 0;
            let mut painted_whole = false;

            let prior_state: i16 = match priorline {
                Some(ref p) => {
                    let pb = p.borrow();
                    if pb.multidata.is_empty() {
                        statusline(MessageType::Alert,
                            "Missing multidata -- please report a bug");
                        NOTHING
                    } else if id < pb.multidata.len() {
                        pb.multidata[id]
                    } else {
                        NOTHING
                    }
                }
                None => NOTHING,
            };

            // If there is an unterminated start match before the current
            // line, we need to look for an end match first.
            if prior_state == WHOLELINE || prior_state == STARTSHERE {
                match end_re.find(line_data) {
                    None => {
                        // No end on this line: paint the whole line.
                        paint(v, 0, converted);
                        state = WHOLELINE;
                        painted_whole = true;
                    }
                    Some(em) => {
                        // Only if it is visible, paint the part to be coloured.
                        if em.end() > from_x {
                            let paintlen = actual_x(converted,
                                wideness(line_data, em.end()).saturating_sub(from_col));
                            paint(v, 0, &converted[..paintlen]);
                        }
                        state = ENDSHERE;
                        index = em.end();
                    }
                }
            }

            // Now look for start matches on this line.
            while !painted_whole && index < PAINT_LIMIT {
                let Some(sm) = start_re.find_at(line_data, index) else { break };
                let (start_so, start_eo) = (sm.start(), sm.end());

                let start_col = if start_so > from_x {
                    wideness(line_data, start_so).saturating_sub(from_col)
                } else { 0 };
                let thetext_x = actual_x(converted, start_col);

                match end_re.find_at(line_data, start_eo) {
                    Some(em) => {
                        let (end_so, end_eo) = (em.start(), em.end());
                        // Only paint the match when it is visible on screen
                        // and more than zero characters long.
                        if end_eo > from_x && end_eo > start_so {
                            let paintlen = actual_x(&converted[thetext_x..],
                                wideness(line_data, end_eo)
                                    .saturating_sub(from_col)
                                    .saturating_sub(start_col));
                            paint(v, start_col, &converted[thetext_x..thetext_x + paintlen]);
                            state = JUSTONTHIS;
                        }
                        index = end_eo;
                        // If both start and end match are anchors, advance.
                        if start_so == start_eo && end_so == end_eo {
                            if index >= line_data.len() { break; }
                            index = step_right(line_data, index);
                        }
                    }
                    None => {
                        // Paint the rest of the line, and we're done.
                        paint(v, start_col, &converted[thetext_x..]);
                        state = STARTSHERE;
                        break;
                    }
                }
            }

            state
        };

        {
            let mut node = line.borrow_mut();
            if id < node.multidata.len() {
                node.multidata[id] = new_state;
            }
        }

        varnish = v.next.as_deref();
    }
}

/// Apply mark (selection) highlighting.
#[cfg(not(feature = "tiny"))]
fn apply_mark_highlighting(
    _row: i32,
    converted: &str,
    line_lineno: isize,
    line_data: &str,
    from_col: usize,
    abs_y: u16,
    midwin_x: u16,
    margin: i32,
) {
    let (has_mark, mark_lineno, current_lineno) = with_state(|s| {
        let f = s.openfile.as_ref();
        let mark_ln = f.and_then(|f| f.mark.as_ref()).map(|l| l.borrow().lineno).unwrap_or(-1);
        let cur_ln = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(-1);
        (mark_ln != -1, mark_ln, cur_ln)
    });

    if !has_mark { return; }

    let in_region = (line_lineno >= mark_lineno && line_lineno <= current_lineno)
        || (line_lineno <= mark_lineno && line_lineno >= current_lineno);

    if !in_region { return; }

    let (top_x, bot_x, top_lineno, bot_lineno) = with_state(|s| {
        if let Some(_f) = s.openfile.as_ref() {
            let (tl, tx, bl, bx) = s.get_region_coords();
            (tx, bx, tl as isize, bl as isize)
        } else {
            (0, 0, 0, 0)
        }
    });

    let from_x = tl_get!(FROM_X);
    let till_x = tl_get!(TILL_X);

    let effective_top_x = if top_lineno < line_lineno || top_x < from_x { from_x } else { top_x };
    let effective_bot_x = if bot_lineno > line_lineno || bot_x > till_x { till_x } else { bot_x };

    if effective_top_x < till_x && effective_bot_x > from_x {
        let start_col = wideness(line_data, effective_top_x).saturating_sub(from_col);
        let start_col = start_col as i32;
        let start_col = start_col.max(0) as usize;
        let thetext_x = actual_x(converted, start_col);

        let paintlen: Option<usize> = if effective_bot_x < till_x {
            let end_col = wideness(line_data, effective_bot_x).saturating_sub(from_col);
            Some(actual_x(&converted[thetext_x..], end_col.saturating_sub(start_col)))
        } else {
            None // paint all
        };

        let selected_pair = state().interface_color_pair[SELECTED_TEXT];
        apply_interface_color(selected_pair);

        let mut stdout = out();
        let _ = queue!(stdout, MoveTo(midwin_x + margin as u16 + start_col as u16, abs_y));
        match paintlen {
            Some(n) => { let _ = queue!(stdout, Print(&converted[thetext_x..thetext_x + n])); }
            None    => { let _ = queue!(stdout, Print(&converted[thetext_x..])); }
        }
        reset_color();
    }
}

// ---------------------------------------------------------------------------
// update_line / update_softwrapped_line
// ---------------------------------------------------------------------------

/* C: int update_line(linestruct *line, size_t index) */
pub fn update_line(line: &LinePtr, index: usize) -> i32 {
    #[cfg(not(feature = "tiny"))]
    if state().flag_isset(SOFTWRAP) {
        return update_softwrapped_line(line);
    }

    #[cfg(not(feature = "tiny"))]
    { tl_set!(SEQUEL_COLUMN, 0); }

    let line_lineno = line.borrow().lineno;

    let from_col = {
        #[cfg(not(feature = "tiny"))]
        if state().united_sidescroll {
            with_state(|s| s.openfile.as_ref().map(|f| f.brink).unwrap_or(0))
        } else {
            let b = line.borrow();
            get_page_start(wideness(&b.data, index))
        }
        #[cfg(feature = "tiny")]
        {
            let b = line.borrow();
            get_page_start(wideness(&b.data, index))
        }
    };

    let (edittop_lineno, editwincols) = with_state(|s| (
        s.openfile.as_ref().and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(0),
        s.editwincols as usize,
    ));
    let row = (line_lineno - edittop_lineno) as i32;

    // Expand the piece to be drawn to its representable form, and draw it.
    let converted = {
        let b = line.borrow();
        display_string(&b.data, from_col, editwincols, true, false)
    };
    draw_row(row, &converted, line, from_col);

    let (midwin_x, midwin_y, margin, _sidebar, _hilite) = with_state(|s| (
        s.midwin.x, s.midwin.y, s.margin, s.sidebar, s.hilite_attribute,
    ));
    let mut stdout = out();

    // Left-scroll indicator
    if from_col > 0 && !converted.is_empty() {
        let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
        let _ = queue!(stdout, MoveTo(midwin_x + margin as u16, midwin_y + row as u16), Print("<"));
        reset_color();
    }

    // Right-scroll indicator
    let has_more = tl_get!(HAS_MORE);
    if has_more {
        let (cols, sidebar) = with_state(|s| (s.midwin.cols, s.sidebar));
        let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
        let _ = queue!(stdout, MoveTo(midwin_x + cols - 1 - sidebar as u16, midwin_y + row as u16), Print(">"));
        reset_color();
    }

    // Spotlight (search match highlight)
    let (spotlighted, light_from, light_to, current) = with_state(|s| (
        s.spotlighted,
        s.light_from_col,
        s.light_to_col,
        s.openfile.as_ref().and_then(|f| f.current.clone()),
    ));
    if spotlighted && current.as_ref().is_some_and(|c| LinePtr::ptr_eq(line, c)) {
        spotlight(light_from, light_to);
    }

    1
}

/* C: int update_softwrapped_line(linestruct *line) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn update_softwrapped_line(line: &LinePtr) -> i32 {
    let (edittop, edittop_firstcol, editwinrows) = with_state(|s| (
        s.openfile.as_ref().and_then(|f| f.edittop.clone()),
        s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0),
        s.editwinrows,
    ));
    let Some(edittop) = edittop else { return 0 };

    let mut row = 0i32;
    let mut from_col = if LinePtr::ptr_eq(line, &edittop) {
        edittop_firstcol
    } else {
        let b = edittop.borrow();
        row -= chunk_for(edittop_firstcol, &b.data) as i32;
        0
    };

    // Find out on which screen row the target line should be shown.
    let mut someline = Some(edittop);
    while let Some(sl) = someline {
        if LinePtr::ptr_eq(&sl, line) { break; }
        row += 1 + { let b = sl.borrow(); extra_chunks_in(&b.data) as i32 };
        someline = sl.borrow().next.clone();
    }

    // If the first chunk is offscreen, don't even try to display it.
    if row < 0 || row >= editwinrows { return 0; }

    let starting_row = row;
    let mut kickoff = true;
    let mut end_of_line = false;

    while !end_of_line && row < editwinrows {
        let node = line.borrow();
        let to_col = get_softwrap_breakpoint(&node.data, from_col, &mut kickoff, &mut end_of_line);
        tl_set!(SEQUEL_COLUMN, if end_of_line { 0 } else { to_col });

        // Convert the chunk to its displayable form and draw it.
        let converted = display_string(&node.data, from_col, to_col - from_col, true, false);
        drop(node);
        draw_row(row, &converted, line, from_col);
        row += 1;
        from_col = to_col;
    }

    // Spotlight
    let (spotlighted, light_from, light_to, current) = with_state(|s| (
        s.spotlighted, s.light_from_col, s.light_to_col,
        s.openfile.as_ref().and_then(|f| f.current.clone()),
    ));
    if spotlighted && current.as_ref().is_some_and(|c| LinePtr::ptr_eq(line, c)) {
        spotlight_softwrapped(light_from, light_to);
    }

    row - starting_row
}

// ---------------------------------------------------------------------------
// line_needs_update
// ---------------------------------------------------------------------------

/* C: bool line_needs_update(const size_t old_column, const size_t new_column) */
pub fn line_needs_update(old_column: usize, new_column: usize) -> bool {
    #[cfg(not(feature = "tiny"))]
    if with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some()) {
        return true;
    }

    if get_page_start(old_column) == get_page_start(new_column) {
        return false;
    }

    #[cfg(not(feature = "tiny"))]
    {
        let united = state().united_sidescroll;
        if united {
            state_mut().refresh_needed = true;
        }
    }

    !state().refresh_needed
}

// ---------------------------------------------------------------------------
// draw_scrollbar (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void draw_scrollbar(void) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn draw_scrollbar() {
    let (edittop_lineno, filebot_lineno, editwinrows, softwrap, _firstcol, _sidebar_w) = with_state(|s| {
        let f = s.openfile.as_ref();
        let et = f.and_then(|f| f.edittop.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1);
        let fb = f.and_then(|f| f.filebot.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1);
        let ewr = s.editwinrows;
        let sw = s.flag_isset(SOFTWRAP);
        let fc = f.map(|f| f.firstcolumn).unwrap_or(0);
        let sd = s.sidebar;
        (et, fb, ewr, sw, fc, sd)
    });

    let from_line = edittop_lineno - 1;
    let total_lines = filebot_lineno;

    // C: under SOFTWRAP, covered_lines is found by walking chunks forward from
    // edittop until editwinrows screen rows are covered — not simply editwinrows.
    let covered_lines = if softwrap {
        let (edittop, firstcolumn) = with_state(|s| {
            let f = s.openfile.as_ref();
            (f.and_then(|f| f.edittop.clone()), f.map(|f| f.firstcolumn).unwrap_or(0))
        });
        if let Some(top) = edittop {
            let mut line = top;
            let mut extras: isize = {
                let data = line.borrow().data.clone();
                extra_chunks_in(&data) as isize - chunk_for(firstcolumn, &data) as isize
            };
            loop {
                let lineno = line.borrow().lineno;
                let next = line.borrow().next.clone();
                if lineno + extras < from_line + editwinrows as isize {
                    if let Some(n) = next {
                        let data = n.borrow().data.clone();
                        extras += extra_chunks_in(&data) as isize;
                        line = n;
                        continue;
                    }
                }
                break;
            }
            line.borrow().lineno - from_line
        } else {
            editwinrows as isize
        }
    } else {
        editwinrows as isize
    };

    let lowest = (from_line * editwinrows as isize) / total_lines;
    let highest = lowest + (editwinrows as isize * covered_lines) / total_lines;
    let highest = if editwinrows as isize > total_lines && !softwrap { editwinrows as isize } else { highest };

    let (midwin_x, midwin_y, cols) = with_state(|s| (s.midwin.x, s.midwin.y, s.midwin.cols));
    let bar_pair = state().interface_color_pair[SCROLL_BAR];

    let mut stdout = out();
    let mut bardata = Vec::with_capacity(editwinrows as usize);

    for row in 0..editwinrows {
        let row_l = row as isize;
        let in_bar = row_l >= lowest && row_l <= highest;
        let cell = b' ' as i32 | bar_pair | (if !in_bar { 0 } else { A_REVERSE });
        bardata.push(cell);

        let bar_char = if in_bar {
            let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
            '█'
        } else {
            ' '
        };
        let _ = queue!(stdout,
            MoveTo(midwin_x + cols - 1, midwin_y + row as u16),
            Print(bar_char),
        );
        if in_bar {
            reset_color();
        }
    }

    state_mut().bardata = bardata;
}

// ---------------------------------------------------------------------------
// edit_scroll
// ---------------------------------------------------------------------------

/* C: void edit_scroll(bool direction) */
pub fn edit_scroll(direction: bool) {
    let (edittop, firstcol, current, current_x, placewewant) = with_state(|s| {
        let f = s.openfile.as_ref();
        let et = f.and_then(|f| f.edittop.clone());
        let fc = f.map(|f| f.firstcolumn).unwrap_or(0);
        let cur = f.and_then(|f| f.current.clone());
        let cx = f.map(|f| f.current_x).unwrap_or(0);
        let pw = f.map(|f| f.placewewant).unwrap_or(0);
        (et, fc, cur, cx, pw)
    });
    let Some(mut line) = edittop else { return };

    // Move the top line of the edit window one row up or down.
    let mut leftedge = firstcol;
    if direction == BACKWARD {
        go_back_chunks(1, &mut line, &mut leftedge);
    } else {
        go_forward_chunks(1, &mut line, &mut leftedge);
    }

    with_state_mut(|s| {
        if let Some(f) = s.openfile.as_mut() {
            f.edittop = Some(line.clone());
            f.firstcolumn = leftedge;
        }
    });

    // Actually scroll the text of the edit window one row up or down.
    let (midwin_y, editwinrows) = with_state(|s| (s.midwin.y, s.editwinrows));
    {
        let mut stdout = out();
        if direction == BACKWARD {
            let _ = queue!(stdout, MoveTo(0, midwin_y), ScrollDown(1));
        } else {
            let _ = queue!(stdout, MoveTo(0, midwin_y), ScrollUp(1));
        }
    }

    // If we're not on the first "page" (when not softwrapping), or the mark
    // is on, the row next to the scrolled region needs to be redrawn too.
    let mut nrows = 1i32;
    if line_needs_update(placewewant, 0) && nrows < editwinrows {
        nrows += 1;
    }

    // If we scrolled backward, the top row needs to be redrawn;
    // if forward, the bottom row.
    let edittop = line.clone();
    let mut draw_line = line;
    let mut draw_leftedge = leftedge;

    if direction == FORWARD {
        go_forward_chunks(editwinrows - nrows, &mut draw_line, &mut draw_leftedge);
    }

    #[cfg(not(feature = "tiny"))]
    {
        let sidebar = state().sidebar;
        if sidebar != 0 { draw_scrollbar(); }

        let softwrap = state().flag_isset(SOFTWRAP);
        if softwrap {
            // Compensate for the earlier chunks of a softwrapped line.
            nrows += { let b = draw_line.borrow(); chunk_for(draw_leftedge, &b.data) as i32 };

            // Don't compensate for the chunks that are offscreen.
            if LinePtr::ptr_eq(&draw_line, &edittop) {
                nrows -= { let b = draw_line.borrow(); chunk_for(leftedge, &b.data) as i32 };
            }
        }
    }

    // Draw new content on the blank row (and on the bordering row too
    // when it was deemed necessary).
    let mut walker = Some(draw_line);
    while nrows > 0 {
        let Some(l) = walker else { break };
        let ix = if current.as_ref().is_some_and(|c| LinePtr::ptr_eq(&l, c)) { current_x } else { 0 };
        nrows -= update_line(&l, ix);
        walker = l.borrow().next.clone();
    }
}

// ---------------------------------------------------------------------------
// edit_redraw / edit_refresh / adjust_viewport
// ---------------------------------------------------------------------------

/* C: void edit_redraw(linestruct *old_current, update_type manner) */
pub fn edit_redraw(old_current: &LinePtr, manner: UpdateType) {
    let was_pww = with_state(|s| s.openfile.as_ref().map(|f| f.placewewant).unwrap_or(0));
    let new_pww = xplustabs();
    with_state_mut(|s| { s.openfile.as_mut().map(|f| f.placewewant = new_pww); });

    // If the current line is offscreen, scroll until it's onscreen.
    if current_is_offscreen() {
        let jump = state().flag_isset(JUMPY_SCROLLING);
        adjust_viewport(if jump { UpdateType::Centering } else { manner });
        state_mut().refresh_needed = true;
        return;
    }

    #[cfg(not(feature = "tiny"))]
    {
        let united = state().united_sidescroll;
        let brink = with_state(|s| s.openfile.as_ref().map(|f| f.brink).unwrap_or(0));
        let page = get_page_start(new_pww);
        if united && brink != page {
            state_mut().refresh_needed = true;
            return;
        }
    }

    let Some(current) = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone())) else { return };

    let mark_is_on = {
        #[cfg(not(feature = "tiny"))]
        { with_state(|s| s.openfile.as_ref().and_then(|f| f.mark.as_ref()).is_some()) }
        #[cfg(feature = "tiny")]
        { false }
    };

    if mark_is_on {
        // If the mark is on, update all lines between old_current and current.
        #[cfg(not(feature = "tiny"))]
        {
            let current_lineno = current.borrow().lineno;
            let mut line = old_current.clone();
            while !LinePtr::ptr_eq(&line, &current) {
                update_line(&line, 0);
                let neighbour = if line.borrow().lineno > current_lineno {
                    line.borrow().prev.as_ref().and_then(|w| w.upgrade())
                } else {
                    line.borrow().next.clone()
                };
                match neighbour {
                    Some(n) => line = n,
                    None => break,
                }
            }
        }
    } else if !LinePtr::ptr_eq(old_current, &current) && get_page_start(was_pww) > 0 {
        // Otherwise, update old_current only if it differs from current
        // and was horizontally scrolled.
        update_line(old_current, 0);
    }

    // Update current if the mark is on or it has changed "page", or if it
    // differs from old_current and needs to be horizontally scrolled.
    let current_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
    if line_needs_update(was_pww, new_pww)
        || (!LinePtr::ptr_eq(old_current, &current) && get_page_start(new_pww) > 0)
    {
        update_line(&current, current_x);
    }
}

/* C: void edit_refresh(void) */
pub fn edit_refresh() {
    if current_is_offscreen() {
        let focusing = state().focusing;
        let jumpy = state().flag_isset(JUMPY_SCROLLING);
        let manner = if focusing || jumpy { UpdateType::Centering } else { UpdateType::Flowing };
        adjust_viewport(manner);
    }

    #[cfg(not(feature = "tiny"))]
    {
        let united = state().united_sidescroll;
        if united {
            let col = xplustabs();
            let page = get_page_start(col);
            with_state_mut(|s| { s.openfile.as_mut().map(|f| f.brink = page); });
        }
    }

    #[cfg(feature = "color")]
    {
        // Prepare palette if needed
        // When needed and useful, initialize the colors for the current syntax.
        let need_palette = with_state(|s| {
            s.openfile.as_ref().and_then(|f| f.syntax).is_some()
                && !s.have_palette
                && !s.flag_isset(NO_SYNTAX)
        });
        if need_palette {
            crate::color::prepare_palette();
        }

        // When the line above the viewport does not have multidata,
        // the multiline-regex cache needs recalculating.
        let recook = with_state(|s| {
            let above_lacks_multidata = s.flag_isset(SOFTWRAP)
                && s.openfile.as_ref()
                    .and_then(|f| f.edittop.as_ref())
                    .and_then(|et| et.borrow().prev.as_ref().and_then(|w| w.upgrade()))
                    .map(|prev| prev.borrow().multidata.is_empty())
                    .unwrap_or(false);
            s.recook || above_lacks_multidata
        });
        if recook {
            crate::color::precalc_multicolorinfo();
            with_state_mut(|s| {
                s.perturbed = false;
                s.recook = false;
            });
        }
    }

    #[cfg(not(feature = "tiny"))]
    {
        let sidebar = state().sidebar;
        if sidebar != 0 { draw_scrollbar(); }
    }

    let (editwinrows, edittop, current, current_x) = with_state(|s| (
        s.editwinrows,
        s.openfile.as_ref().and_then(|f| f.edittop.clone()),
        s.openfile.as_ref().and_then(|f| f.current.clone()),
        s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0),
    ));

    let mut row = 0i32;
    let mut line = edittop;

    while row < editwinrows {
        let Some(l) = line else { break };
        let index = if current.as_ref().is_some_and(|c| LinePtr::ptr_eq(&l, c)) { current_x } else { 0 };
        row += update_line(&l, index);
        line = l.borrow().next.clone();
    }

    // Blank remaining rows
    let (midwin_x, midwin_y, midwin_cols) = with_state(|s| (s.midwin.x, s.midwin.y, s.midwin.cols));
    let mut stdout = out();
    while row < editwinrows {
        let _ = queue!(stdout,
            MoveTo(midwin_x, midwin_y + row as u16),
            Clear(ClearType::UntilNewLine),
        );

        #[cfg(not(feature = "tiny"))]
        {
            let sidebar = state().sidebar;
            if sidebar != 0 {
                let bardata = state().bardata.get(row as usize).copied().unwrap_or(b' ' as i32);
                let in_bar = (bardata & A_REVERSE) != 0;
                if in_bar { let _ = queue!(stdout, SetAttribute(Attribute::Reverse)); }
                let _ = queue!(stdout, MoveTo(midwin_x + midwin_cols - 1, midwin_y + row as u16), Print(" "));
                if in_bar { reset_color(); }
            }
        }
        row += 1;
    }

    place_the_cursor();
    let _ = stdout.flush();
    state_mut().refresh_needed = false;
}

/* C: void adjust_viewport(update_type manner) */
pub fn adjust_viewport(manner: UpdateType) {
    let (editwinrows, cursor_row) = with_state(|s| (
        s.editwinrows,
        s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0),
    ));

    let goal = match manner {
        UpdateType::Stationary => cursor_row as i32,
        UpdateType::Centering  => editwinrows / 2,
        UpdateType::Flowing    => {
            if !current_is_above_screen() {
                let shim = if state().flag_isset(ZERO)
                    && (state().currmenu == MREPLACEWITH
                        || state().currmenu == MYESNO)
                { 1 } else { 0 };
                editwinrows - 1 - shim
            } else {
                0
            }
        }
    };

    let Some(current) = with_state(|s| s.openfile.as_ref().and_then(|f| f.current.clone())) else { return };

    // C: openfile->edittop = openfile->current;
    let mut edittop = current.clone();

    #[cfg(not(feature = "tiny"))]
    let softwrap = state().flag_isset(SOFTWRAP);
    #[cfg(feature = "tiny")]
    let softwrap = false;

    let mut leftedge = if softwrap {
        #[cfg(not(feature = "tiny"))]
        {
            let col = xplustabs();
            let b = current.borrow();
            leftedge_for(col, &b.data)
        }
        #[cfg(feature = "tiny")]
        { 0 }
    } else {
        with_state(|s| s.openfile.as_ref().map(|f| f.firstcolumn).unwrap_or(0))
    };

    // Move edittop back goal rows, starting at current[firstcolumn].
    go_back_chunks(goal, &mut edittop, &mut leftedge);

    with_state_mut(|s| {
        if let Some(f) = s.openfile.as_mut() {
            f.edittop = Some(edittop.clone());
            f.firstcolumn = leftedge;
        }
    });
}

// ---------------------------------------------------------------------------
// full_refresh / draw_all_subwindows
// ---------------------------------------------------------------------------

/* C: void full_refresh(void) */
pub fn full_refresh() {
    // In crossterm there is no curscr equivalent; we just redraw everything.
    draw_all_subwindows();
}

/* C: void draw_all_subwindows(void) */
pub fn draw_all_subwindows() {
    let (currmenu, inhelp, title) = with_state(|s| (
        s.currmenu,
        s.inhelp,
        s.title.clone(),
    ));

    let is_browser = (currmenu & (MBROWSER | MGOTODIR | MWHEREISFILE)) != 0;

    if !is_browser {
        titlebar(title.as_deref());
    }

    #[cfg(feature = "help")]
    if inhelp {
        // help refresh
        return;
    }

    #[cfg(feature = "browser")]
    if is_browser {
        // browser_refresh() from browser.rs
        return;
    }

    edit_refresh();
    bottombars(currmenu);
}

// ---------------------------------------------------------------------------
// report_cursor_position
// ---------------------------------------------------------------------------

/* C: void report_cursor_position(void) */
pub fn report_cursor_position() {
    let (current_data, current_x, current_lineno, filebot_lineno, totsize, _filetop_data) =
        with_state(|s| {
            let f = s.openfile.as_ref();
            let data = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().data.clone()).unwrap_or_default();
            let cx = f.map(|f| f.current_x).unwrap_or(0);
            let cl = f.and_then(|f| f.current.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1);
            let fb = f.and_then(|f| f.filebot.as_ref()).map(|l| l.borrow().lineno).unwrap_or(1);
            let ts = f.map(|f| f.totsize).unwrap_or(0);
            let ft = f.and_then(|f| f.filetop.as_ref()).map(|l| l.borrow().data.clone()).unwrap_or_default();
            (data, cx, cl, fb, ts, ft)
        });

    let fullwidth = breadth(&current_data) + 1;
    let column = xplustabs() + 1;

    // Number of characters from the top of the file up to the cursor — counted in
    // CHARACTERS (C: number_of_characters_in(filetop, current) with the current
    // line logically truncated at current_x), not as a byte offset of one line.
    let sum = {
        let (filetop, current) = with_state(|s| {
            let f = s.openfile.as_ref();
            (f.and_then(|f| f.filetop.clone()), f.and_then(|f| f.current.clone()))
        });
        let mut count = 0usize;
        if let (Some(top), Some(cur)) = (filetop, current) {
            let mut node = Some(top);
            while let Some(n) = node {
                if LinePtr::ptr_eq(&n, &cur) {
                    let data = n.borrow().data.clone();
                    let upto = data.get(..current_x).unwrap_or(&data);
                    count += crate::chars::mbstrlen(upto);
                    break;
                }
                count += crate::chars::mbstrlen(&n.borrow().data) + 1;
                node = n.borrow().next.clone();
            }
        }
        count
    };

    let linepct = (100 * current_lineno / filebot_lineno.max(1)) as i32;
    let colpct = (100 * column / fullwidth.max(1)) as i32;
    let charpct = if totsize == 0 { 0i32 } else { (100 * sum / totsize) as i32 };

    let digs = digits(filebot_lineno as isize);
    let digs_ts = digits(totsize as isize);

    let msg = format!(
        "line {:>width$}/{} ({:2}%), col {:2}/{:2} ({:3}%), char {:>wts$}/{} ({:2}%)",
        current_lineno, filebot_lineno, linepct,
        column, fullwidth, colpct,
        sum, totsize, charpct,
        width = digs as usize,
        wts = digs_ts as usize,
    );

    statusline(MessageType::Info, &msg);
}

// ---------------------------------------------------------------------------
// spotlight / spotlight_softwrapped
// ---------------------------------------------------------------------------

/* C: void spotlight(size_t from_col, size_t to_col) */
pub fn spotlight(from_col: usize, to_col: usize) {
    let (editwincols, sidebar, _margin, midwin_x, midwin_y) = with_state(|s| (
        s.editwincols as usize, s.sidebar as u16, s.margin as u16,
        s.midwin.x, s.midwin.y,
    ));

    let right_edge = get_page_start(from_col) + editwincols;
    let overshoots = to_col > right_edge;
    let to_col_eff = if overshoots { right_edge } else { to_col };

    let current_data = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.as_ref()).map(|l| l.borrow().data.clone()).unwrap_or_default()
    });

    let word = if to_col_eff == from_col {
        " ".to_string()
    } else {
        display_string(&current_data, from_col, to_col_eff - from_col, false, overshoots)
    };

    let cursor_row = with_state(|s| s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0));

    place_the_cursor();

    let spot_pair = state().interface_color_pair[SPOTLIGHTED];
    apply_interface_color(spot_pair);

    let mut stdout = out();
    let _ = queue!(stdout, Print(&word[..actual_x(&word, to_col_eff)]));

    if overshoots {
        let cols = state().midwin.cols;
        let _ = queue!(stdout,
            MoveTo(midwin_x + cols - 1 - sidebar, midwin_y + cursor_row as u16),
            Print('>'),
        );
    }

    reset_color();
}

/* C: void spotlight_softwrapped(size_t from_col, size_t to_col) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn spotlight_softwrapped(from_col: usize, to_col: usize) {
    let current_data = with_state(|s| {
        s.openfile.as_ref().and_then(|f| f.current.as_ref()).map(|l| l.borrow().data.clone()).unwrap_or_default()
    });

    let (margin, midwin_x, midwin_y, editwinrows) = with_state(|s| (
        s.margin as u16, s.midwin.x, s.midwin.y, s.editwinrows,
    ));

    let leftedge = leftedge_for(from_col, &current_data);
    place_the_cursor();
    let mut row = with_state(|s| s.openfile.as_ref().map(|f| f.cursor_row).unwrap_or(0)) as i32;
    let mut cur_from = from_col;
    let mut kickoff = true;
    let mut end_of_line = false;

    let spot_pair = state().interface_color_pair[SPOTLIGHTED];

    while row < editwinrows {
        let break_col = {
            let mut bc = get_softwrap_breakpoint(&current_data, leftedge, &mut kickoff, &mut end_of_line);
            if bc >= to_col {
                end_of_line = true;
                bc = to_col;
            }
            bc
        };

        let word = if break_col == cur_from {
            " ".to_string()
        } else {
            display_string(&current_data, cur_from, break_col - cur_from, false, false)
        };

        apply_interface_color(spot_pair);
        let mut stdout = out();
        let _ = queue!(stdout, Print(&word[..actual_x(&word, break_col)]));
        reset_color();

        if end_of_line { break; }

        row += 1;
        let _ = queue!(stdout, MoveTo(midwin_x + margin, midwin_y + row as u16));
        cur_from = break_col;
    }
}

// ---------------------------------------------------------------------------
// do_credits (ENABLE_EXTRA)
// ---------------------------------------------------------------------------

/* C: void do_credits(void) — #ifdef ENABLE_EXTRA */
pub fn do_credits() {
    let with_interface = !state().flag_isset(ZERO);
    let with_help = !state().flag_isset(NO_HELP);

    if with_interface || with_help {
        with_state_mut(|s| {
            s.flags[flag_index(ZERO)] |= flag_mask(ZERO);
            s.flags[flag_index(NO_HELP)] |= flag_mask(NO_HELP);
        });
        window_init();
    }

    let credits: &[Option<&str>] = &[
        None,  // "The nano text editor"
        None,  // "version"
        Some(GNU_NANO_VERSION),
        Some(""),
        None,  // "Brought to you by:"
        Some("Chris Allegretta"),
        Some("Benno Schulenberg"),
        Some("David Lawrence Ramsey"),
        Some("Jordi Mallach"),
        Some("David Benbennick"),
        Some("Rocco Corsi"),
        Some("Mike Frysinger"),
        Some("Adam Rogoyski"),
        Some("Rob Siemborski"),
        Some("Mark Majeres"),
        Some("Ken Tyler"),
        Some("Sven Guckes"),
        Some("Bill Soudan"),
        Some("Christian Weisgerber"),
        Some("Erik Andersen"),
        Some("Big Gaute"),
        Some("Joshua Jensen"),
        Some("Ryan Krebs"),
        Some("Albert Chin"),
        Some(""),
        None,  // "Special thanks to:"
        Some("Monique, Brielle & Joseph"),
        Some("Plattsburgh State University"),
        Some("Benet Laboratories"),
        Some("Amy Allegretta"),
        Some("Linda Young"),
        Some("Jeremy Robichaud"),
        Some("Richard Kolb II"),
        None,  // "The Free Software Foundation"
        Some("Linus Torvalds"),
        None,  // "the many translators and the TP"
        None,  // "For ncurses:"
        Some("Thomas Dickey"),
        Some("Pavel Curtis"),
        Some("Zeyd Ben-Halim"),
        Some("Eric S. Raymond"),
        None,  // "and anyone else we forgot..."
        Some(""),
        Some(""),
        None,  // "Thank you for using nano!"
        Some(""),
        Some(""),
        Some("(C) 2026"),
        Some("Free Software Foundation, Inc."),
        Some(""),
        Some(""),
        Some("https://nano-editor.org/"),
    ];

    let xlcredits = [
        "The nano text editor",
        "version",
        "Brought to you by:",
        "Special thanks to:",
        "The Free Software Foundation",
        "the many translators and the TP",
        "For ncurses:",
        "and anyone else we forgot...",
        "Thank you for using nano!",
    ];

    let editwinrows = state().editwinrows;
    let cols = state().midwin.cols as usize;

    blank_edit();
    let _ = out().flush();
    std::thread::sleep(std::time::Duration::from_millis(600));

    let mut xlpos = 0usize;

    for crpos in 0..(credits.len() as i32 + editwinrows / 2) {
        if crpos < credits.len() as i32 {
            let text = match credits[crpos as usize] {
                Some(t) => t,
                None => {
                    let t = xlcredits[xlpos];
                    xlpos += 1;
                    t
                }
            };
            let text_width = breadth(text);
            let col = if text_width < cols { (cols - text_width) / 2 } else { 0 };
            let row = editwinrows - 1;
            let (midwin_x, midwin_y) = with_state(|s| (s.midwin.x, s.midwin.y));
            let mut stdout = out();
            let _ = queue!(stdout, MoveTo(midwin_x + col as u16, midwin_y + row as u16), Print(text));
            let _ = stdout.flush();
        }

        // Check for keypress
        if event::poll(std::time::Duration::ZERO).unwrap_or(false) {
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(600));

        // Scroll up
        let (midwin_y, _midwin_x) = with_state(|s| (s.midwin.y, s.midwin.x));
        let _ = queue!(out(), MoveTo(0, midwin_y), ScrollUp(1));
        let _ = out().flush();

        if event::poll(std::time::Duration::ZERO).unwrap_or(false) {
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = queue!(out(), MoveTo(0, midwin_y), ScrollUp(1));
        let _ = out().flush();
    }

    if with_interface {
        state_mut().flags[flag_index(ZERO)] &= !flag_mask(ZERO);
    }
    if with_help {
        state_mut().flags[flag_index(NO_HELP)] &= !flag_mask(NO_HELP);
    }
    window_init();
    draw_all_subwindows();
}

// ---------------------------------------------------------------------------
// Cursor position display (do_cursorpos)
// ---------------------------------------------------------------------------

/* C: void do_cursorpos(bool force) — this is report_cursor_position in C */
pub fn do_cursorpos(_force: bool) {
    report_cursor_position();
}

// ---------------------------------------------------------------------------
// suggest_ctrlT_ctrlZ (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void suggest_ctrlT_ctrlZ(void) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn suggest_ctrlT_ctrlZ() {
    statusline(MessageType::Info, "^T = execute command,  ^Z = suspend");
}

/* C: void suck_up_input_and_paste_it(void) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn suck_up_input_and_paste_it() {
    // Read all pending input and paste it as if typed
    let mut chars = String::new();
    while let Ok(true) = event::poll(std::time::Duration::ZERO) {
        if let Ok(Event::Key(ke)) = event::read() {
            if let KeyCode::Char(c) = ke.code {
                chars.push(c);
            }
        }
    }
    // Put chars into the key buffer in reverse
    for b in chars.bytes().rev() {
        put_back(b as i32);
    }
}

// ---------------------------------------------------------------------------
// Cursor movement helper
// ---------------------------------------------------------------------------

/* C: (new) do_curses_move(y, x) */
pub fn do_curses_move(y: u16, x: u16) {
    let _ = queue!(out(), MoveTo(x, y));
}

// (All imports are at the top of the file.)

// ---------------------------------------------------------------------------
// Helper functions expected by other modules (prompt.rs, etc.)
// These wrap footwin operations so callers don't need to hold the NanoWindow.
// ---------------------------------------------------------------------------

/// Terminal bell (equivalent to ncurses beep()).
pub fn beep() {
    let _ = print!("\x07");
    let _ = out().flush();
}

/// Sleep for the given number of milliseconds (equivalent to ncurses napms()).
/// Used to let flash messages linger long enough to be read.
pub fn napms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Return the number of columns in the terminal (COLS equivalent).
pub fn get_cols() -> usize {
    state().footwin.cols as usize
}

/// Move the cursor within the footwin to (row, col).
pub fn footwin_wmove(row: i32, col: i32) {
    let (fx, fy) = with_state(|s| (s.footwin.x, s.footwin.y));
    let _ = queue!(out(),
        MoveTo(fx + col as u16, fy + row as u16),
    );
}

/// Apply an interface color pair to the footwin (wattron equivalent).
pub fn footwin_wattron(pair: i32) {
    apply_interface_color(pair);
}

/// Remove an interface color pair from the footwin (wattroff equivalent).
pub fn footwin_wattroff(_pair: i32) {
    reset_color();
}

/// Print a string at the current cursor position in footwin.
pub fn footwin_waddstr(s: &str) {
    let _ = queue!(out(), Print(s));
}

/// Print a single character at the current cursor position in footwin.
pub fn footwin_waddch(c: char) {
    let _ = queue!(out(), Print(c));
}

/// Move to (row, col) in footwin and print a string.
pub fn footwin_mvwaddstr(row: i32, col: i32, s: &str) {
    let (fx, fy) = with_state(|s| (s.footwin.x, s.footwin.y));
    let _ = queue!(out(),
        MoveTo(fx + col as u16, fy + row as u16),
        Print(s),
    );
}

/// Move to (row, col) in footwin and print at most n bytes of a string.
pub fn footwin_mvwaddnstr(row: i32, col: i32, s: &str, n: usize) {
    let (fx, fy) = with_state(|s| (s.footwin.x, s.footwin.y));
    let truncated = &s[..actual_x(s, n).min(s.len())];
    let _ = queue!(out(),
        MoveTo(fx + col as u16, fy + row as u16),
        Print(truncated),
    );
}

/// Move to (row, col) in footwin and print a single character.
pub fn footwin_mvwaddch(row: i32, col: i32, c: char) {
    let (fx, fy) = with_state(|s| (s.footwin.x, s.footwin.y));
    let _ = queue!(out(),
        MoveTo(fx + col as u16, fy + row as u16),
        Print(c),
    );
}

/// Print `cols` spaces starting at (row, col) in footwin (fills a line).
pub fn footwin_mvwprintw_spaces(row: i32, col: i32, cols: usize) {
    let (fx, fy) = with_state(|s| (s.footwin.x, s.footwin.y));
    let spaces = " ".repeat(cols);
    let _ = queue!(out(),
        MoveTo(fx + col as u16, fy + row as u16),
        Print(&spaces),
    );
}

/// Flush footwin output (wnoutrefresh equivalent — writes to stdout buffer).
pub fn footwin_wnoutrefresh() {
    let _ = out().flush();
}

/// Return the number of edit window rows.
pub fn get_editwinrows() -> i32 {
    state().editwinrows
}

/// Return the margin (line-number column width).
pub fn get_margin() -> i32 {
    state().margin
}

/// Return reference to topwin for external use.
pub fn get_topwin() -> NanoWindow {
    state().topwin.clone()
}

/// Return reference to midwin for external use.
pub fn get_midwin() -> NanoWindow {
    state().midwin.clone()
}

/// Return reference to footwin for external use.
pub fn get_footwin() -> NanoWindow {
    state().footwin.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoverable_worker_panic_does_not_own_terminal_cleanup() {
        assert!(panic_should_restore_terminal(true));

        #[cfg(panic = "unwind")]
        assert!(!panic_should_restore_terminal(false));

        #[cfg(panic = "abort")]
        assert!(panic_should_restore_terminal(false));

        let previous = IS_TERMINAL_UI_THREAD.with(|is_ui_thread| is_ui_thread.replace(true));
        let worker_owns_terminal = std::thread::spawn(|| {
            IS_TERMINAL_UI_THREAD.with(Cell::get)
        }).join().unwrap();
        IS_TERMINAL_UI_THREAD.with(|is_ui_thread| is_ui_thread.set(previous));

        assert!(!worker_owns_terminal);
    }

    #[test]
    fn cleanup_plan_unwinds_every_partial_terminal_transition() {
        assert_eq!(
            cleanup_plan(RAW_MODE_ACTIVE),
            TerminalCleanupPlan {
                show_cursor: false,
                leave_alternate_screen: false,
                disable_raw_mode: true,
            },
        );
        assert_eq!(
            cleanup_plan(RAW_MODE_ACTIVE | ALTERNATE_SCREEN_ACTIVE),
            TerminalCleanupPlan {
                show_cursor: false,
                leave_alternate_screen: true,
                disable_raw_mode: true,
            },
        );
        assert_eq!(
            cleanup_plan(TERMINAL_READY),
            TerminalCleanupPlan {
                show_cursor: true,
                leave_alternate_screen: true,
                disable_raw_mode: true,
            },
        );
    }

    #[test]
    fn terminal_output_handles_can_be_used_interleaved() {
        let mut first = out();
        let mut second = out();

        // Keeping one handle alive while acquiring and using another must not
        // retain a borrow of the underlying writer.  Under the old `&'static
        // mut BufWriter` API, using `first` again after creating `second`
        // exercised the aliased mutable references under Miri.
        first.write_all(&[]).unwrap();
        second.write_all(&[]).unwrap();
        footwin_waddstr("");
        flush_out();
        first.write_all(&[]).unwrap();
    }

    fn reset_input_buffer() {
        KEY_BUFFER.with(|buffer| buffer.borrow_mut().clear());
        tl_set!(NEXTCODES_IDX, 0);
        tl_set!(WAITING_CODES, 0);
        tl_set!(ESCAPES, 0);
        tl_set!(FIRST_ESCAPE_WAS_ALONE, false);
        tl_set!(LAST_ESCAPE_WAS_ALONE, false);
        with_state_mut(|state| {
            state.shift_held = false;
            state.meta_key = false;
        });

        #[cfg(not(feature = "tiny"))]
        {
            tl_set!(RECORDING, false);
            tl_set!(MILESTONE, 0);
            MACRO_BUFFER.with(|buffer| buffer.borrow_mut().clear());
            PREVIOUS_MACRO.with(|buffer| *buffer.borrow_mut() = None);
        }
    }

    #[test]
    fn window_layout_matches_flag_combinations() {
        assert_eq!(
            calculate_window_layout(80, 24, false, false, false, false),
            WindowLayout { top_rows: 1, mid_rows: 20, mid_y: 1, foot_rows: 3, foot_y: 21 },
        );
        assert_eq!(
            calculate_window_layout(80, 24, false, false, false, true),
            WindowLayout { top_rows: 2, mid_rows: 19, mid_y: 2, foot_rows: 3, foot_y: 21 },
        );
        assert_eq!(
            calculate_window_layout(80, 24, false, false, true, false),
            WindowLayout { top_rows: 0, mid_rows: 21, mid_y: 0, foot_rows: 3, foot_y: 21 },
        );
        assert_eq!(
            calculate_window_layout(80, 24, false, true, false, false),
            WindowLayout { top_rows: 0, mid_rows: 22, mid_y: 0, foot_rows: 3, foot_y: 21 },
        );
        assert_eq!(
            calculate_window_layout(80, 24, true, false, false, false),
            WindowLayout { top_rows: 1, mid_rows: 22, mid_y: 1, foot_rows: 1, foot_y: 23 },
        );
    }

    #[test]
    fn window_layout_handles_flat_and_one_column_terminals() {
        assert_eq!(
            calculate_window_layout(1, 1, false, false, false, false),
            WindowLayout { top_rows: 0, mid_rows: 1, mid_y: 0, foot_rows: 1, foot_y: 0 },
        );
        assert_eq!(
            calculate_window_layout(1, 2, false, true, false, false),
            WindowLayout { top_rows: 0, mid_rows: 2, mid_y: 0, foot_rows: 1, foot_y: 1 },
        );
    }

    #[test]
    fn resize_safe_points_distinguish_events_from_real_input() {
        assert!(resize_was_requested(
            Some(THE_WINDOW_RESIZED as i32),
            false,
        ));
        assert!(resize_was_requested(None, true));
        assert!(!resize_was_requested(None, false));

        // A real key must reach its handler.  The pending atomic request is
        // consumed by the loop's next no-input safe-point check.
        assert!(!resize_was_requested(Some(b'x' as i32), true));
    }

    #[test]
    fn titlebar_layout_sacrifices_elements_in_gnu_order() {
        let wide = calculate_titlebar_layout("GNU nano", "", "", "file", true, 80);
        assert!(wide.show_upperleft);
        assert_eq!(wide.pluglen, breadth("Modified") + 1);
        assert_eq!(wide.statelen, 2);

        let without_version = calculate_titlebar_layout("GNU nano", "", "", "file", true, 25);
        assert!(!without_version.show_upperleft);
        assert_eq!(without_version.pluglen, breadth("Modified") + 1);
        assert_eq!(without_version.statelen, 2);

        let without_plug = calculate_titlebar_layout("GNU nano", "", "", "file", true, 15);
        assert!(!without_plug.show_upperleft);
        assert_eq!(without_plug.pluglen, 0);
        assert_eq!(without_plug.statelen, 2);

        let without_side_spaces = calculate_titlebar_layout("GNU nano", "", "", "file", true, 5);
        assert!(!without_side_spaces.show_upperleft);
        assert_eq!(without_side_spaces.pluglen, 0);
        assert_eq!(without_side_spaces.statelen, 0);
    }

    #[test]
    fn titlebar_layout_accounts_for_state_word_spacing() {
        let layout = calculate_titlebar_layout("GNU nano", "", "Modified", "file", false, 80);
        assert_eq!(layout.pathlen, breadth("file") + 1);
        assert_eq!(layout.statelen, breadth("Modified") + 2);
    }

    #[test]
    fn shifted_modifier_is_decoded_with_its_queued_event() {
        reset_input_buffer();
        translate_key_event(KeyEvent::new(
            KeyCode::Home,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        translate_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(parse_kbinput(), CONTROL_HOME as i32);
        assert!(state().shift_held);
        assert_eq!(parse_kbinput(), KEY_RIGHT);
        assert!(!state().shift_held);
    }

    #[test]
    fn plain_shift_navigation_uses_dedicated_codes() {
        reset_input_buffer();
        translate_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        translate_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT));

        assert_eq!(parse_kbinput(), KEY_LEFT);
        assert!(state().shift_held);
        assert_eq!(parse_kbinput(), KEY_END);
        assert!(state().shift_held);
    }

    #[test]
    fn ctrl_digits_and_punctuation_use_canonical_mapping() {
        for (character, expected) in [
            ('3', ESC),
            ('7', 31),
            ('8', DEL),
            ('?', DEL),
            ('2', 0),
            ('/', 31),
            ('[', 27),
            ('_', 31),
            ('A', 1),
        ] {
            reset_input_buffer();
            translate_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL));
            assert_eq!(get_input(None), expected, "Ctrl+{character}");
        }
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn completion_grid_ignores_zero_and_single_candidate_sets() {
        assert!(completion_grid(&[], 80, 20, false).cells.is_empty());
        assert!(completion_grid(&["only".to_string()], 80, 20, false).cells.is_empty());
        assert!(completion_grid(&["a".into(), "b".into()], 0, 20, false).cells.is_empty());
        assert!(completion_grid(&["a".into(), "b".into()], 80, 0, false).cells.is_empty());
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn completion_grid_places_multiple_candidates_in_columns() {
        let names = ["alpha", "beta", "gamma", "delta"].map(str::to_string);
        let grid = completion_grid(&names, 20, 6, false);

        assert_eq!(grid.name_width, 5);
        assert_eq!(
            grid.cells,
            vec![
                CompletionCell { row: 3, column: 0, match_index: Some(0) },
                CompletionCell { row: 3, column: 7, match_index: Some(1) },
                CompletionCell { row: 3, column: 14, match_index: Some(2) },
                CompletionCell { row: 4, column: 0, match_index: Some(3) },
            ],
        );
    }

    #[cfg(feature = "tabcomp")]
    #[test]
    fn completion_grid_handles_narrow_and_overfull_views() {
        let names: Vec<String> = (0..10).map(|index| format!("candidate-{index}")).collect();
        let grid = completion_grid(&names, 1, 3, false);

        assert_eq!(grid.name_width, 1);
        assert_eq!(grid.cells.len(), 3);
        assert_eq!(grid.cells[0].match_index, Some(0));
        assert_eq!(grid.cells[1].match_index, Some(1));
        assert_eq!(grid.cells[2], CompletionCell { row: 2, column: 0, match_index: None });

        // ZERO mode shares its bottom row with the status bar.  The grid must
        // stay out of that row instead of relying on unsigned subtraction.
        let reserved = completion_grid(&["a".into(), "b".into()], 10, 1, true);
        assert!(reserved.cells.is_empty());
    }

    #[cfg(all(feature = "tabcomp", feature = "utf8"))]
    #[test]
    fn completion_grid_measures_unicode_and_hidden_names_by_columns() {
        crate::chars::remember_utf8(true);
        let names = ["猫", ".hidden", "犬"].map(str::to_string);
        let grid = completion_grid(&names, 30, 5, false);

        assert_eq!(grid.name_width, breadth(".hidden"));
        assert_eq!(grid.cells.len(), names.len());
        assert_eq!(grid.cells[1].column, breadth(".hidden") + 2);
        crate::chars::remember_utf8(false);
    }

    #[cfg(not(feature = "tiny"))]
    #[test]
    fn macro_recording_restores_cancelled_macro_and_snips_stop_burst() {
        reset_input_buffer();
        MACRO_BUFFER.with(|buffer| *buffer.borrow_mut() = vec![10, 20]);

        assert_eq!(toggle_macro_recording(), MacroRecordingOutcome::Started);
        assert!(MACRO_BUFFER.with(|buffer| buffer.borrow().is_empty()));
        push_keycode(99);
        assert_eq!(toggle_macro_recording(), MacroRecordingOutcome::Cancelled);
        assert_eq!(MACRO_BUFFER.with(|buffer| buffer.borrow().clone()), vec![10, 20]);

        assert_eq!(toggle_macro_recording(), MacroRecordingOutcome::Started);
        push_keycode(65);
        tl_set!(MILESTONE, 1);
        push_keycode(99);
        assert_eq!(toggle_macro_recording(), MacroRecordingOutcome::Stopped);
        assert_eq!(MACRO_BUFFER.with(|buffer| buffer.borrow().clone()), vec![65]);
    }
}
