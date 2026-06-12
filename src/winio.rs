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
use std::io::{self, Write, stdout};
use std::time::Duration;
use std::cell::RefCell;
use std::rc::Rc;
use crate::definitions::*;
use crate::global::{
    with_state, with_state_mut, NanoWindow,
    KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_HOME, KEY_END,
    KEY_PPAGE, KEY_NPAGE, KEY_DC, KEY_IC, KEY_BACKSPACE, KEY_ENTER,
    KEY_F0, key_f, A_REVERSE, shown_entries_for,
    flag_index, flag_mask,
};
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
// Re-entrancy: nano's draw functions call one another while "holding" the
// writer, exactly like they do with AppState. We therefore use the same
// UnsafeCell-in-a-Sync-static idiom as `NanoCell`; this is sound only because
// nano is strictly single-threaded.
// ---------------------------------------------------------------------------
struct OutCell(std::cell::UnsafeCell<Option<std::io::BufWriter<std::io::Stdout>>>);
// SAFETY: nano is strictly single-threaded; no concurrent access ever occurs.
unsafe impl Sync for OutCell {}
static OUT: OutCell = OutCell(std::cell::UnsafeCell::new(None));

/// Return the shared buffered writer, initialising it on first use.
#[inline]
pub fn out() -> &'static mut std::io::BufWriter<std::io::Stdout> {
    // SAFETY: single-threaded; matches the NanoCell re-entrant-access pattern.
    unsafe {
        let slot = &mut *OUT.0.get();
        slot.get_or_insert_with(|| std::io::BufWriter::with_capacity(64 * 1024, stdout()))
    }
}

/// Flush the shared buffered writer to the real terminal.
#[inline]
pub fn flush_out() {
    let _ = out().flush();
}

// ---------------------------------------------------------------------------
// Module-level statics (equivalent to file-scope C statics in winio.c)
// ---------------------------------------------------------------------------

thread_local! {
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
    #[cfg(not(feature = "tiny"))]
    static SWB_TEXT_OFFSET: RefCell<usize> = RefCell::new(0);
    #[cfg(not(feature = "tiny"))]
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

// A sentinel used in assemble_byte_code / assemble_unicode
const PROCEED: i64 = -44;
const INVALID_DIGIT: i64 = -77;

// ---------------------------------------------------------------------------
// Terminal initialisation / teardown
// ---------------------------------------------------------------------------

/* C: (new in Rust port) terminal_init: replaces initscr() + refresh() */
pub fn terminal_init() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(out(), EnterAlternateScreen, Hide)?;
    Ok(())
}

/* C: (new in Rust port) terminal_exit: replaces endwin() */
pub fn terminal_exit() -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(out(), Show, LeaveAlternateScreen)?;
    Ok(())
}

/* C: (new in Rust port) terminal_size() -> (cols, rows) */
pub fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/* C: regenerate_screen() / recalculate_screensize() */
pub fn recalculate_screensize() {
    let (cols, rows) = terminal_size();
    with_state_mut(|s| {
        // Update the three windows
        s.topwin = NanoWindow { rows: 1, cols, y: 0, x: 0 };
        // footwin height depends on NO_HELP and LINES
        let foot_rows = if s.flag_isset(NO_HELP) || (rows as i32) < 5 { 1 } else { 3 };
        let mid_rows = (rows as i32 - 1 - foot_rows).max(0) as u16;
        s.midwin = NanoWindow { rows: mid_rows, cols, y: 1, x: 0 };
        s.footwin = NanoWindow { rows: foot_rows as u16, cols, y: 1 + mid_rows, x: 0 };
        s.editwinrows = mid_rows as i32;
        s.editwincols = (cols as i32 - s.margin - s.sidebar).max(1);
    });
}

/* C: window_init() in nano.c — set up topwin/midwin/footwin from terminal size */
pub fn window_init() {
    recalculate_screensize();
}

// ---------------------------------------------------------------------------
// Macro recording (not NANO_TINY)
// ---------------------------------------------------------------------------

/* C: void record_macro(void) */
#[cfg(not(feature = "tiny"))]
pub fn record_macro() {
    RECORDING.with(|r| {
        let was_recording = *r.borrow();
        let now_recording = !was_recording;
        *r.borrow_mut() = now_recording;

        if now_recording {
            // Save old macro, start fresh
            MACRO_BUFFER.with(|mb| mb.borrow_mut().clear());
            MILESTONE.with(|m| *m.borrow_mut() = 0);
            statusline(MessageType::Remark, "Recording a macro...");
        } else {
            let milestone = tl_get!(MILESTONE);
            if milestone == 0 {
                // No keystrokes recorded; restore would have done nothing
                statusline(MessageType::Remark, "Cancelled");
            } else {
                // Snip the invoke keystroke
                MACRO_BUFFER.with(|mb| mb.borrow_mut().truncate(milestone));
                statusline(MessageType::Remark, "Stopped recording");
            }
        }
    });

    if with_state(|s| s.flag_isset(STATEFLAGS)) {
        titlebar(None);
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
        let ml = MACRO_BUFFER.with(|mb| mb.borrow().len());
        MILESTONE.with(|m| *m.borrow_mut() = ml);
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
    with_state_mut(|s| s.mute_modifiers = true);
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
    with_state_mut(|s| s.mute_modifiers = true);
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
                // Find closing brace
                let rest = &s[*pos + 1..];
                if let Some(close_idx) = rest.find('}') {
                    let inner = &rest[..close_idx];
                    // Handle {{} and {}}: literal { or }
                    if inner == "{" || inner == "}" {
                        let ch = inner.as_bytes()[0] as i32;
                        *pos += 3; // skip {X}
                        if *pos < bytes.len() {
                            put_back(MORE_PLANTS as i32);
                        }
                        return ch;
                    }
                    // It's a command name
                    let cmd = inner.to_string();
                    *pos += 2 + close_idx; // skip {inner}
                    if *pos < bytes.len() {
                        put_back(MORE_PLANTS as i32);
                    }
                    // Store commandname and resolve shortcut
                    with_state_mut(|st| {
                        st.commandname = Some(cmd.clone());
                        // strtosc equivalent: find shortcut index
                        let found = st.sclist.iter().position(|sc| {
                            sc.keystr == cmd.as_str() || sc.keystr.eq_ignore_ascii_case(&cmd)
                        });
                        st.planted_shortcut = found;
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
        with_state_mut(|s| s.spotlighted = false);
    } else if frame.is_some() {
        read_keys_from();
    }

    let waiting = tl_get!(WAITING_CODES);
    if waiting > 0 {
        KEY_BUFFER.with(|kb| {
            NEXTCODES_IDX.with(|ni| {
                WAITING_CODES.with(|wc| {
                    let buf = kb.borrow();
                    let mut idx = ni.borrow_mut();
                    let mut w = wc.borrow_mut();
                    *w -= 1;

                    #[cfg(feature = "nanorc")]
                    if buf[*idx] == MORE_PLANTS as i32 {
                        *idx += 1;
                        return get_code_from_plantation();
                    }

                    let code = buf[*idx];
                    *idx += 1;
                    code
                })
            })
        })
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
    let stdout = out();

    // Flush any pending output before blocking
    let _ = stdout.flush();

    // Show cursor if appropriate
    let reveal = tl_get!(REVEAL_CURSOR);
    let spotlight = with_state(|s| s.spotlighted);
    let show_cursor_flag = with_state(|s| s.flag_isset(SHOW_CURSOR));
    let currmenu = with_state(|s| s.currmenu);
    let lastmessage = with_state(|s| s.lastmessage);
    let lines = with_state(|s| s.midwin.rows + s.topwin.rows + s.footwin.rows);

    if reveal && (!spotlight || show_cursor_flag || currmenu == MSPELL)
        && (lines > 1 || lastmessage <= MessageType::Hush)
    {
        let _ = execute!(stdout, Show);
    }

    // Wait for and read the first event (blocking)
    let first_event = loop {
        match event::read() {
            Ok(ev) => break ev,
            Err(_) => {
                // Treat unrecoverable read failure as resize
                break Event::Resize(80, 24);
            }
        }
    };

    let _ = execute!(stdout, Hide);

    // Initialise buffer
    KEY_BUFFER.with(|kb| kb.borrow_mut().clear());
    NEXTCODES_IDX.with(|ni| *ni.borrow_mut() = 0);
    WAITING_CODES.with(|wc| *wc.borrow_mut() = 0);

    // Translate the first event and push codes
    translate_event(first_event);

    #[cfg(not(feature = "tiny"))]
    {
        let waiting = tl_get!(WAITING_CODES);
        if waiting > 0 {
            KEY_BUFFER.with(|kb| {
                let _buf = kb.borrow();
                MILESTONE.with(|m| *m.borrow_mut() = MACRO_BUFFER.with(|mb| mb.borrow().len()));
            });
        }
    }

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
        Event::Resize(_w, _h) => {
            // Signal a resize
            #[cfg(not(feature = "tiny"))]
            with_state_mut(|s| {
                s.the_window_resized = true;
            });
            #[cfg(not(feature = "tiny"))]
            push_keycode(THE_WINDOW_RESIZED as i32);
            #[cfg(feature = "tiny")]
            push_keycode(KEY_FRESH as i32);
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

    // For meta-prefixed keys, nano traditionally expects ESC then the key.
    // However, when crossterm delivers ALT already decoded, we set meta_key
    // and push only the underlying code — matching how parse_kbinput handles
    // single-escape sequences.
    if alt {
        with_state_mut(|s| s.meta_key = true);
    }

    match ke.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+letter → control code
                let code = (c as i32) & 0x1F;
                push_keycode(code);
            } else if alt {
                // Push ESC + character (standard nano escape-sequence convention)
                // For lowercase letters with shift, or uppercase without shift-metas,
                // we push the lowercase form.
                let ch = if shift && c.is_ascii_alphabetic() && !with_state(|s| s.shifted_metas) {
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
                with_state_mut(|s| s.shift_held = true);
                push_keycode(CONTROL_UP as i32);
            } else if ctrl {
                push_keycode(CONTROL_UP as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_PPAGE);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_UP);
            } else if alt {
                push_keycode(ALT_UP as i32);
            } else {
                push_keycode(KEY_UP);
            }
        }
        KeyCode::Down => {
            if ctrl && shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(CONTROL_DOWN as i32);
            } else if ctrl {
                push_keycode(CONTROL_DOWN as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_NPAGE);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_DOWN);
            } else if alt {
                push_keycode(ALT_DOWN as i32);
            } else {
                push_keycode(KEY_DOWN);
            }
        }
        KeyCode::Left => {
            if ctrl && shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(CONTROL_LEFT as i32);
            } else if ctrl {
                push_keycode(CONTROL_LEFT as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_HOME);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_LEFT);
            } else if alt {
                push_keycode(ALT_LEFT as i32);
            } else {
                push_keycode(KEY_LEFT);
            }
        }
        KeyCode::Right => {
            if ctrl && shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(CONTROL_RIGHT as i32);
            } else if ctrl {
                push_keycode(CONTROL_RIGHT as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_END);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_RIGHT);
            } else if alt {
                push_keycode(ALT_RIGHT as i32);
            } else {
                push_keycode(KEY_RIGHT);
            }
        }
        KeyCode::Home => {
            if ctrl {
                push_keycode(CONTROL_HOME as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_HOME);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_HOME);
            } else if alt {
                push_keycode(ALT_HOME as i32);
            } else {
                push_keycode(KEY_HOME);
            }
        }
        KeyCode::End => {
            if ctrl {
                push_keycode(CONTROL_END as i32);
            } else if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_END);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_END);
            } else if alt {
                push_keycode(ALT_END as i32);
            } else {
                push_keycode(KEY_END);
            }
        }
        KeyCode::PageUp => {
            if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_PPAGE);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_PPAGE);
            } else if alt {
                push_keycode(ALT_PAGEUP as i32);
            } else {
                push_keycode(KEY_PPAGE);
            }
        }
        KeyCode::PageDown => {
            if shift && alt {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_NPAGE);
            } else if shift {
                with_state_mut(|s| s.shift_held = true);
                push_keycode(KEY_NPAGE);
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
                            with_state_mut(|s| s.shift_held = true);
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
                            with_state_mut(|s| s.shift_held = true);
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
                    let sc_csd = with_state(|s| s.controlshiftdelete);
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
                    return with_state(|s| s.controlshiftdelete);
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
                    '2' => return with_state(|s| s.shiftaltup),
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
                    '2' => return with_state(|s| s.shiftaltdown),
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
                    '@' => return with_state(|s| s.shiftcontrolhome),
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
                    '@' => return with_state(|s| s.shiftcontrolend),
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
            with_state_mut(|s| s.shift_held = true);
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

    let currmenu = with_state(|s| s.currmenu);
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

    #[cfg(not(feature = "tiny"))]
    if keycode == THE_WINDOW_RESIZED as i32 {
        *count = 999;
        return Vec::new();
    }

    let mut yield_buf: Vec<i32> = vec![0; 6];

    #[cfg(feature = "utf8")]
    {
        let using_utf8 = with_state(|s| s.using_utf8);
        if using_utf8 && (keycode as u8).is_ascii_hexdigit() {
            let mut unicode = assemble_unicode(keycode);
            tl_set!(REVEAL_CURSOR, false);

            while unicode == PROCEED {
                let k = get_input(Some(()));
                unicode = assemble_unicode(k);
            }

            #[cfg(not(feature = "tiny"))]
            {
                let k = get_input(None); // peek at last read
                if k == THE_WINDOW_RESIZED as i32 {
                    *count = 999;
                    return Vec::new();
                }
            }

            if unicode == INVALID_DIGIT {
                // Skip continuation bytes if any
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
    let _preserve = with_state(|s| s.flag_isset(PRESERVE));
    let _raw_sequences = with_state(|s| s.flag_isset(RAW_SEQUENCES));

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
            let as_at = with_state(|s| s.as_an_at);
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
                let using_utf8 = with_state(|s| s.using_utf8);
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
                with_state_mut(|s| s.meta_key = true);
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
                let shifted = with_state(|s| s.shifted_metas);
                let kc = if keycode >= 'A' as i32 && keycode <= 'Z' as i32 && !shifted {
                    keycode | 0x20
                } else {
                    keycode
                };
                with_state_mut(|s| s.meta_key = true);
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
                'a' => { with_state_mut(|s| s.shift_held = true); KEY_PPAGE }
                #[cfg(not(feature = "tiny"))]
                'b' => { with_state_mut(|s| s.shift_held = true); KEY_NPAGE }
                #[cfg(not(feature = "tiny"))]
                'c' => { with_state_mut(|s| s.shift_held = true); KEY_HOME }
                #[cfg(not(feature = "tiny"))]
                'd' => { with_state_mut(|s| s.shift_held = true); KEY_END }
                _ => ERR_CODE,
            };
        } else if waiting > 0 && next != ESC && (keycode == '[' as i32 || keycode == 'O' as i32) {
            let result = parse_escape_sequence(keycode);
            with_state_mut(|s| s.meta_key = true);
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
                let using_utf8 = with_state(|s| s.using_utf8);
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
                let shifted = with_state(|s| s.shifted_metas);
                let kc = if keycode >= 'A' as i32 && keycode <= 'Z' as i32 && !shifted {
                    keycode | 0x20
                } else {
                    keycode
                };
                with_state_mut(|s| s.meta_key = true);
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
        if keycode == sup { with_state_mut(|s| s.shift_held = true); return KEY_UP; }
        if keycode == sdown { with_state_mut(|s| s.shift_held = true); return KEY_DOWN; }
        if keycode == scl { with_state_mut(|s| s.shift_held = true); return CONTROL_LEFT as i32; }
        if keycode == scr { with_state_mut(|s| s.shift_held = true); return CONTROL_RIGHT as i32; }
        if keycode == scu { with_state_mut(|s| s.shift_held = true); return CONTROL_UP as i32; }
        if keycode == scd { with_state_mut(|s| s.shift_held = true); return CONTROL_DOWN as i32; }
        if keycode == sch { with_state_mut(|s| s.shift_held = true); return CONTROL_HOME as i32; }
        if keycode == sce { with_state_mut(|s| s.shift_held = true); return CONTROL_END as i32; }
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
        if keycode == sal { with_state_mut(|s| s.shift_held = true); return KEY_HOME; }
        if keycode == sar { with_state_mut(|s| s.shift_held = true); return KEY_END; }
        if keycode == sau { with_state_mut(|s| s.shift_held = true); return KEY_PPAGE; }
        if keycode == sad { with_state_mut(|s| s.shift_held = true); return KEY_NPAGE; }

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
            return if with_state(|s| s.flag_isset(REBIND_DELETE)) { KEY_BACKSPACE } else { KEY_DC };
        }
        k if k == KEY_BACKSPACE => {
            return if with_state(|s| s.flag_isset(REBIND_DELETE)) { KEY_DC } else { KEY_BACKSPACE };
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

    let currmenu = with_state(|s| s.currmenu);
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

    let margin = with_state(|s| s.margin);
    *mouse_x = event.x as i32 - if in_middle { margin } else { 0 };
    *mouse_y = event.y as i32;

    let bstate = event.bstate;

    if bstate & (BUTTON1_RELEASED | BUTTON1_CLICKED) != 0 {
        let sidebar = with_state(|s| s.sidebar);
        let currmenu = with_state(|s| s.currmenu);

        if in_middle && sidebar != 0 && event.x == cols - 1 && currmenu == MMAIN {
            // Scroll bar click
            let editwinrows = with_state(|s| s.editwinrows) as i32;
            let total_lines = with_state(|s| {
                s.openfile.as_ref()
                    .and_then(|f| f.filebot.as_ref())
                    .map(|b| b.borrow().lineno)
                    .unwrap_or(1)
            }) as i32;
            let click_row = (*mouse_y - mid_y as i32).max(0);
            let _target_line = total_lines * click_row / editwinrows + 1;
            // goto_line_and_column is in move_.rs; stub here
            with_state_mut(|s| s.refresh_needed = true);
            return 0;
        }

        if in_footer && !with_state(|s| s.flag_isset(NO_HELP)) && currmenu != MYESNO {
            let lines_val = with_state(|s| s.footwin.rows + s.midwin.rows + s.topwin.rows);
            if *mouse_y == lines_val as i32 - 3 {
                return 0;
            }

            let foot_rel_y = (*mouse_y - foot_y as i32).max(0) as usize;
            let foot_rel_x = (*mouse_x).max(0) as usize;

            let currmenu = with_state(|s| s.currmenu);
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
    let stdout = out();
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
    let stdout = out();
    for row in 0..editwinrows {
        let _ = queue!(stdout,
            MoveTo(midwin_x, midwin_y + row as u16),
            Clear(ClearType::UntilNewLine),
        );
    }
}

/* C: void blank_statusbar(void) */
pub fn blank_statusbar() {
    let (footwin_x, footwin_y) = with_state(|s| (s.footwin.x, s.footwin.y));
    let stdout = out();
    let _ = queue!(stdout, MoveTo(footwin_x, footwin_y), Clear(ClearType::UntilNewLine));
}

/* C: void wipe_statusbar(void) */
pub fn wipe_statusbar() {
    with_state_mut(|s| s.lastmessage = MessageType::Vacuum);

    let (zero, minibar, lines, currmenu) = with_state(|s| (
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
        s.midwin.rows + s.topwin.rows + s.footwin.rows,
        s.currmenu,
    ));

    if (zero || minibar || lines == 1) && currmenu == MMAIN {
        return;
    }

    blank_statusbar();
    let _ = out().flush();
}

/* C: void blank_bottombars(void) */
pub fn blank_bottombars() {
    let (no_help, lines, footwin_y, footwin_x) = with_state(|s| (
        s.flag_isset(NO_HELP),
        s.midwin.rows + s.topwin.rows + s.footwin.rows,
        s.footwin.y,
        s.footwin.x,
    ));

    if !no_help && lines > 5 {
        let stdout = out();
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

    let (currmenu, zero, lines) = with_state(|s| (s.currmenu, s.flag_isset(ZERO), s.midwin.rows + s.topwin.rows + s.footwin.rows));
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

    // Handle case where first character starts before left edge
    // (partial character display with placeholders)
    if start_col < column && pos < text.len() && bytes[pos] != b'\t' {
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

    while pos < text.len() && cur_col < beyond {
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

/* C: int buffer_number(openfilestruct *buffer) — #ifdef ENABLE_MULTIBUFFER */
#[cfg(feature = "multibuffer")]
pub fn buffer_number() -> i32 {
    // In the Rust port the circular list is not yet fully realised;
    // return 1 as a stub that compiles.
    1
}

// ---------------------------------------------------------------------------
// show_states_at — show editor state flags in a window
// ---------------------------------------------------------------------------

/* C: void show_states_at(WINDOW *window) — #ifndef NANO_TINY */
#[cfg(not(feature = "tiny"))]
pub fn show_states_at_win(win: &NanoWindow, cur_y: u16, cur_x: u16) {
    let stdout = out();
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
    let stdout = out();
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

    with_state_mut(|s| s.as_an_at = false);

    let (upperleft, prefix, state, caption) = compute_titlebar_strings(path, currmenu, inhelp);

    let verlen = breadth(&upperleft) + 3;
    let prefixlen = if !prefix.is_empty() { breadth(&prefix) + 1 } else { 0 };
    let pathlen = breadth(&caption);
    let statelen = if !state.is_empty() { breadth(&state) + 2 } else { 0 };
    let cols = cols as usize;

    let pluglen: usize = 0; // placeholder for "Modified" space reservation

    let mut ver_use = verlen;
    let mut stat_use = statelen;
    let mut plg_use = pluglen;

    if ver_use + prefixlen + pathlen + plg_use + stat_use > cols {
        ver_use = 2;
    }
    if ver_use + prefixlen + pathlen + plg_use + stat_use > cols {
        plg_use = 0;
    }
    if ver_use + prefixlen + pathlen + plg_use + stat_use > cols {
        ver_use = 0;
        if stat_use > 2 { stat_use -= 2; }
    }

    let offset = if ver_use > 0 {
        ver_use + (cols.saturating_sub(ver_use + plg_use + stat_use + prefixlen + pathlen)) / 2
    } else {
        0
    };

    // Print version / buffer ranking
    if ver_use > 0 && ver_use + prefixlen + pathlen + plg_use + stat_use <= cols {
        let _ = queue!(stdout, MoveTo(topwin_x + 2, topwin_y));
        let _ = queue!(stdout, Print(&upperleft));
    }

    // Print prefix
    if ver_use + prefixlen + pathlen + plg_use + stat_use <= cols && !prefix.is_empty() {
        let _ = queue!(stdout, MoveTo(topwin_x + offset as u16, topwin_y));
        let _ = queue!(stdout, Print(&prefix));
        let _ = queue!(stdout, Print(" "));
    } else {
        let _ = queue!(stdout, MoveTo(topwin_x + offset as u16, topwin_y));
    }

    // Print path / title
    let _available = cols.saturating_sub(stat_use + plg_use);
    if pathlen + plg_use + stat_use <= cols {
        let disp = display_string(&caption, 0, pathlen, false, false);
        let _ = queue!(stdout, Print(&disp));
    } else if 5 + stat_use <= cols {
        let _ = queue!(stdout, Print("..."));
        let disp = display_string(&caption,
            3 + pathlen.saturating_sub(cols.saturating_sub(stat_use)),
            cols.saturating_sub(stat_use),
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
            if stat_use < cols {
                let state_col = (cols + 2).saturating_sub(stat_use);
                let _ = queue!(stdout, MoveTo(topwin_x + state_col as u16, topwin_y));
                show_states_at_win(&with_state(|s| s.topwin.clone()), 0, state_col as u16);
            }
        } else {
            print_state_word(&state, stat_use, cols, topwin_x, topwin_y);
        }
    }
    #[cfg(feature = "tiny")]
    {
        print_state_word(&state, stat_use, cols, topwin_x, topwin_y);
    }

    queue_reset_color(&mut stdout);
    let _ = stdout.flush();
}

fn print_state_word(state: &str, statelen: usize, cols: usize, x: u16, y: u16) {
    if statelen > 0 {
        let stdout = out();
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
            upperleft = format!("[{}/{}]", buffer_number(), buffer_number());
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
            let more_than_one = with_state(|s| s.more_than_one);
            if more_than_one {
                upperleft = format!("[{}/{}]", buffer_number(), buffer_number());
            } else {
                upperleft = "GNU nano".to_string();
            }
        }
        #[cfg(not(feature = "multibuffer"))]
        {
            upperleft = "GNU nano".to_string();
        }

        let (filename, modified, view_mode, restricted) = with_state(|s| {
            let f = s.openfile.as_ref();
            let fname = f.map(|f| f.filename.clone()).unwrap_or_default();
            let modif = f.map(|f| f.modified).unwrap_or(false);
            let view = s.flag_isset(VIEW_MODE);
            let rest = s.flag_isset(RESTRICTED);
            (fname, modif, view, rest)
        });

        if filename.is_empty() {
            caption = "New Buffer".to_string();
        } else {
            caption = filename;
        }

        if view_mode {
            state = "View".to_string();
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

    let stdout = out();

    // Draw colored bar
    apply_interface_color(mini_pair);

    let spaces = " ".repeat(cols);
    let _ = queue!(stdout, MoveTo(footwin_x, footwin_y), Print(&spaces));

    with_state_mut(|s| s.as_an_at = false);

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
        let next_is_self = with_state(|s| {
            // When there's only one buffer, openfile->next == openfile
            s.openfile.as_ref().map(|_| false).unwrap_or(true)
        });
        if !next_is_self && cols > 35 {
            let ranking = format!(" [{}/{}]", buffer_number(), buffer_number());
            if namewidth + placewidth + breadth(&ranking) + 32 < cols {
                let _ = queue!(stdout, Print(&ranking));
            }
        }
    }

    // Display cursor position
    if constant_show && namewidth + 32 < cols {
        let loc_col = (cols - 27 - placewidth) as u16;
        let _ = queue!(stdout, MoveTo(footwin_x + loc_col, footwin_y), Print(&location));
    }

    // Display hex code of character under cursor
    if constant_show && namewidth + 28 < cols {
        let hex_str = compute_cursor_hex();
        let hex_col = (cols - 23) as u16;
        let _ = queue!(stdout, MoveTo(footwin_x + hex_col, footwin_y), Print(&hex_str));
    }

    // Display state flags
    if stateflags && namewidth + 14 + 2 * padding < cols {
        let state_col = (cols - 11 - padding) as u16;
        show_states_at_win(&with_state(|s| s.footwin.clone()), 0, state_col);
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

// ---------------------------------------------------------------------------
// statusline
// ---------------------------------------------------------------------------

/* C: void statusline(message_type importance, const char *msg, ...) */
pub fn statusline(importance: MessageType, msg: &str) {
    // NOTE: do NOT reset WAITING_CODES here.
    // The C nano's statusline() never touched the key buffer; clearing it
    // destroys pending multi-byte character bytes (e.g. UTF-8 emoji continuations).

    let lastmessage = with_state(|s| s.lastmessage);

    // Ignore lower-importance messages
    if importance < lastmessage && lastmessage > MessageType::Notice {
        return;
    }

    let (cols, footwin_x, footwin_y, _zero, _minibar_on, _lines, _currmenu) = with_state(|s| (
        s.footwin.cols as usize,
        s.footwin.x,
        s.footwin.y,
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
        s.midwin.rows + s.topwin.rows + s.footwin.rows,
        s.currmenu,
    ));

    let stdout = out();

    // If multiple ALERT messages, add trailing dots
    if lastmessage == MessageType::Alert {
        let start_col = STATUSLINE_START_COL.with(|sc| *sc.borrow());
        if start_col > 4 {
            let alert_pair = with_state(|s| s.interface_color_pair[ERROR_MESSAGE]);
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
        with_state(|s| s.interface_color_pair[ERROR_MESSAGE])
    } else if importance == MessageType::Notice {
        with_state(|s| s.interface_color_pair[SELECTED_TEXT])
    } else {
        with_state(|s| s.interface_color_pair[STATUS_BAR])
    };

    if importance == MessageType::Alert {
        // beep equivalent — terminal bell
        let _ = print!("\x07");
    }

    with_state_mut(|s| s.lastmessage = importance);

    blank_statusbar();

    // Temporarily disable WHITESPACE_DISPLAY for the message
    let showed_whitespace = with_state(|s| s.flag_isset(WHITESPACE_DISPLAY));
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
    let quick_blank = with_state(|s| s.flag_isset(QUICK_BLANK));
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
    with_state_mut(|s| s.lastmessage = MessageType::Vacuum);
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
    let key_pair = with_state(|s| s.interface_color_pair[KEY_COMBO]);
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
    let func_pair = with_state(|s| s.interface_color_pair[FUNCTION_TAG]);
    queue_interface_color(&mut stdout, func_pair);
    let tag_len = actual_x(tag, remaining - 1);
    let tag_display = &tag[..tag_len];
    let _ = queue!(stdout, Print(tag_display));
    queue_reset_color(&mut stdout);
}

/// Internal: post_one_key with explicit row/col positioning (used by bottombars).
fn post_one_key_at(keystroke: &str, tag: &str, width: usize, row: u16, col: u16) {
    let (footwin_x, footwin_y) = with_state(|s| (s.footwin.x, s.footwin.y));
    let stdout = out();
    let _ = queue!(stdout, MoveTo(footwin_x + col, footwin_y + row));
    post_one_key(keystroke, tag, width as i32);
}

/* C: void bottombars(int menu) */
pub fn bottombars(menu: u32) {
    with_state_mut(|s| s.currmenu = menu);

    let (no_help, lines, zero, minibar_on) = with_state(|s| (
        s.flag_isset(NO_HELP),
        s.midwin.rows + s.topwin.rows + s.footwin.rows,
        s.flag_isset(ZERO),
        s.flag_isset(MINIBAR),
    ));

    let min_lines = if zero { 3 } else if minibar_on { 4 } else { 5 };
    if no_help || (lines as i32) < min_lines { return; }

    let number = shown_entries_for(menu);
    if number == 0 { return; }

    let cols = with_state(|s| s.footwin.cols) as usize;
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
            if Rc::ptr_eq(&l, &current) { break; }
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
#[cfg(not(feature = "tiny"))]
pub fn get_softwrap_breakpoint(
    linedata: &str,
    leftedge: usize,
    kickoff: &mut bool,
    end_of_line: &mut bool,
) -> usize {
    let editwincols = with_state(|s| s.editwincols) as usize;
    let at_blanks = with_state(|s| s.flag_isset(AT_BLANKS));
    let tabsize = with_state(|s| s.tabsize) as usize;
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
#[cfg(not(feature = "tiny"))]
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
#[cfg(not(feature = "tiny"))]
pub fn extra_chunks_in(linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, usize::MAX).0
}

/* C: size_t chunk_for(size_t column, linestruct *line) */
#[cfg(not(feature = "tiny"))]
pub fn chunk_for(column: usize, linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, column).0
}

/* C: size_t leftedge_for(size_t column, linestruct *line) */
#[cfg(not(feature = "tiny"))]
pub fn leftedge_for(column: usize, linedata: &str) -> usize {
    get_chunk_and_edge_for(linedata, column).1
}

/* C: void ensure_firstcolumn_is_aligned(void) */
#[cfg(not(feature = "tiny"))]
pub fn ensure_firstcolumn_is_aligned() {
    let softwrap = with_state(|s| s.flag_isset(SOFTWRAP));
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
    with_state_mut(|s| s.focusing = false);
}

/* C: size_t actual_last_column(size_t leftedge, size_t column) */
pub fn actual_last_column(leftedge: usize, column: usize) -> usize {
    #[cfg(not(feature = "tiny"))]
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
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
    let node = line.borrow();
    let line_lineno = node.lineno;
    let line_data: &str = &node.data;
    #[cfg(feature = "color")]
    let multidata: &[i16] = &node.multidata;
    let has_anchor = node.has_anchor;

    let (midwin_x, midwin_y, margin, cols, _sidebar, editwincols) = with_state(|s| {
        (s.midwin.x, s.midwin.y, s.margin, s.midwin.cols as usize, s.sidebar, s.editwincols as usize)
    });
    let stdout = out();

    let abs_y = midwin_y + row as u16;

    // Line numbers
    #[cfg(feature = "linenumbers")]
    if margin > 0 {
        let ln_pair = with_state(|s| s.interface_color_pair[LINE_NUMBER]);
        apply_interface_color(ln_pair);

        #[cfg(not(feature = "tiny"))]
        let softwrap = with_state(|s| s.flag_isset(SOFTWRAP));
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
            let using_utf8 = with_state(|s| s.using_utf8);
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
        { with_state(|s| s.flag_isset(SOFTWRAP)) }
        #[cfg(feature = "tiny")]
        { false }
    };

    if is_shorter || softwrap_on {
        let _ = queue!(stdout, Clear(ClearType::UntilNewLine));
    }

    // Scrollbar character
    #[cfg(not(feature = "tiny"))]
    {
        let sidebar_val = with_state(|s| s.sidebar);
        if sidebar_val != 0 {
            let bardata = with_state(|s| s.bardata.get(row as usize).copied().unwrap_or(b' ' as i32));
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
    apply_syntax_highlighting(row, converted, line_data, multidata, from_col, abs_y, midwin_x, margin);

    // Guide stripe
    #[cfg(not(feature = "tiny"))]
    {
        let stripe_col = with_state(|s| s.stripe_column);
        let sequel = tl_get!(SEQUEL_COLUMN);
        let inhelp = with_state(|s| s.inhelp);
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

            let guide_pair = with_state(|s| s.interface_color_pair[GUIDE_STRIPE]);
            apply_interface_color(guide_pair);
            let _ = queue!(stdout, MoveTo(midwin_x + margin as u16 + target_column as u16, abs_y),
                Print(&striped_char));
            reset_color();
        }
    }

    // Mark highlighting
    #[cfg(not(feature = "tiny"))]
    apply_mark_highlighting(row, converted, line_lineno, line_data, from_col, abs_y, midwin_x, margin);
}

/// Apply syntax color rules to a drawn row. (ENABLE_COLOR)
#[cfg(feature = "color")]
fn apply_syntax_highlighting(
    _row: i32,
    converted: &str,
    line_data: &str,
    multidata: &[i16],
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

    let no_syntax = with_state(|s| s.flag_isset(NO_SYNTAX));
    if no_syntax { return; }

    let from_x = tl_get!(FROM_X);
    let till_x = tl_get!(TILL_X);

    if let Some(syntax) = syntax {
        // Iterate through color rules (pointer-chained in C)
        // In Rust port we access via the SyntaxType's color list
        // varnish is Option<&ColorType>
        let mut varnish: Option<&ColorType> = syntax.color.as_deref();
        while let Some(v) = varnish {
            let attrs = v.attributes;
            let stdout = out();

            if v.end.is_none() {
                // Single-line rule
                let regex = match &v.start {
                    Some(r) => r,
                    None => { varnish = v.next.as_deref(); continue; }
                };

                let mut search_from = from_x;
                const PAINT_LIMIT: usize = 2000;

                while search_from < PAINT_LIMIT && search_from < till_x {
                    let search_in = &line_data[search_from..];
                    let _flags = if search_from == 0 { 0 } else { 1 }; // REG_NOTBOL approx
                    match regex.find(search_in) {
                        None => break,
                        Some(m) => {
                            let match_so = search_from + m.start();
                            let match_eo = search_from + m.end();
                            if match_so >= till_x { break; }
                            if match_so == match_eo {
                                if search_from >= line_data.len() { break; }
                                search_from = step_right(line_data, search_from);
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

                            if attrs & A_REVERSE != 0 {
                                let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
                            }
                            let _ = queue!(stdout,
                                MoveTo(midwin_x + margin as u16 + start_col as u16, abs_y),
                                Print(&converted[thetext_x..thetext_x + paintlen]),
                            );
                            reset_color();

                            search_from = match_eo;
                        }
                    }
                }
            } else {
                // Multiline rule — simplified, just check WHOLELINE/STARTSHERE
                let id = v.id as usize;
                if id < multidata.len() {
                    match multidata[id] {
                        WHOLELINE => {
                            if attrs & A_REVERSE != 0 {
                                let _ = queue!(stdout, SetAttribute(Attribute::Reverse));
                            }
                            let _ = queue!(stdout, MoveTo(midwin_x + margin as u16, abs_y), Print(converted));
                            reset_color();
                        }
                        ENDSHERE | STARTSHERE | JUSTONTHIS => {
                            // Partial line coloring — simplified
                        }
                        _ => {}
                    }
                }
            }

            varnish = v.next.as_deref();
        }
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

        let selected_pair = with_state(|s| s.interface_color_pair[SELECTED_TEXT]);
        apply_interface_color(selected_pair);

        let stdout = out();
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
    if with_state(|s| s.flag_isset(SOFTWRAP)) {
        return update_softwrapped_line(line);
    }

    #[cfg(not(feature = "tiny"))]
    { tl_set!(SEQUEL_COLUMN, 0); }

    let line_lineno = line.borrow().lineno;

    let from_col = {
        #[cfg(not(feature = "tiny"))]
        if with_state(|s| s.united_sidescroll) {
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
    let stdout = out();

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
    if spotlighted && current.as_ref().is_some_and(|c| Rc::ptr_eq(line, c)) {
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
    let mut from_col = if Rc::ptr_eq(line, &edittop) {
        edittop_firstcol
    } else {
        let b = edittop.borrow();
        row -= chunk_for(edittop_firstcol, &b.data) as i32;
        0
    };

    // Find out on which screen row the target line should be shown.
    let mut someline = Some(edittop);
    while let Some(sl) = someline {
        if Rc::ptr_eq(&sl, line) { break; }
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
    if spotlighted && current.as_ref().is_some_and(|c| Rc::ptr_eq(line, c)) {
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
        let united = with_state(|s| s.united_sidescroll);
        if united {
            with_state_mut(|s| s.refresh_needed = true);
        }
    }

    !with_state(|s| s.refresh_needed)
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
    let covered_lines = editwinrows as isize;

    let lowest = (from_line * editwinrows as isize) / total_lines;
    let highest = lowest + (editwinrows as isize * covered_lines) / total_lines;
    let highest = if editwinrows as isize > total_lines && !softwrap { editwinrows as isize } else { highest };

    let (midwin_x, midwin_y, cols) = with_state(|s| (s.midwin.x, s.midwin.y, s.midwin.cols));
    let bar_pair = with_state(|s| s.interface_color_pair[SCROLL_BAR]);

    let stdout = out();
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

    with_state_mut(|s| s.bardata = bardata);
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
        let stdout = out();
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
        let sidebar = with_state(|s| s.sidebar);
        if sidebar != 0 { draw_scrollbar(); }

        let softwrap = with_state(|s| s.flag_isset(SOFTWRAP));
        if softwrap {
            // Compensate for the earlier chunks of a softwrapped line.
            nrows += { let b = draw_line.borrow(); chunk_for(draw_leftedge, &b.data) as i32 };

            // Don't compensate for the chunks that are offscreen.
            if Rc::ptr_eq(&draw_line, &edittop) {
                nrows -= { let b = draw_line.borrow(); chunk_for(leftedge, &b.data) as i32 };
            }
        }
    }

    // Draw new content on the blank row (and on the bordering row too
    // when it was deemed necessary).
    let mut walker = Some(draw_line);
    while nrows > 0 {
        let Some(l) = walker else { break };
        let ix = if current.as_ref().is_some_and(|c| Rc::ptr_eq(&l, c)) { current_x } else { 0 };
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
        let jump = with_state(|s| s.flag_isset(JUMPY_SCROLLING));
        adjust_viewport(if jump { UpdateType::Centering } else { manner });
        with_state_mut(|s| s.refresh_needed = true);
        return;
    }

    #[cfg(not(feature = "tiny"))]
    {
        let united = with_state(|s| s.united_sidescroll);
        let brink = with_state(|s| s.openfile.as_ref().map(|f| f.brink).unwrap_or(0));
        let page = get_page_start(new_pww);
        if united && brink != page {
            with_state_mut(|s| s.refresh_needed = true);
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
            while !Rc::ptr_eq(&line, &current) {
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
    } else if !Rc::ptr_eq(old_current, &current) && get_page_start(was_pww) > 0 {
        // Otherwise, update old_current only if it differs from current
        // and was horizontally scrolled.
        update_line(old_current, 0);
    }

    // Update current if the mark is on or it has changed "page", or if it
    // differs from old_current and needs to be horizontally scrolled.
    let current_x = with_state(|s| s.openfile.as_ref().map(|f| f.current_x).unwrap_or(0));
    if line_needs_update(was_pww, new_pww)
        || (!Rc::ptr_eq(old_current, &current) && get_page_start(new_pww) > 0)
    {
        update_line(&current, current_x);
    }
}

/* C: void edit_refresh(void) */
pub fn edit_refresh() {
    if current_is_offscreen() {
        let focusing = with_state(|s| s.focusing);
        let jumpy = with_state(|s| s.flag_isset(JUMPY_SCROLLING));
        let manner = if focusing || jumpy { UpdateType::Centering } else { UpdateType::Flowing };
        adjust_viewport(manner);
    }

    #[cfg(not(feature = "tiny"))]
    {
        let united = with_state(|s| s.united_sidescroll);
        if united {
            let col = xplustabs();
            let page = get_page_start(col);
            with_state_mut(|s| { s.openfile.as_mut().map(|f| f.brink = page); });
        }
    }

    #[cfg(feature = "color")]
    {
        // Prepare palette if needed
        let _need_palette = with_state(|s| {
            s.openfile.as_ref().and_then(|f| f.syntax).is_some()
                && !s.have_palette
                && !s.flag_isset(NO_SYNTAX)
        });
        // prepare_palette() is in color.rs — stub call
    }

    #[cfg(not(feature = "tiny"))]
    {
        let sidebar = with_state(|s| s.sidebar);
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
        let index = if current.as_ref().is_some_and(|c| Rc::ptr_eq(&l, c)) { current_x } else { 0 };
        row += update_line(&l, index);
        line = l.borrow().next.clone();
    }

    // Blank remaining rows
    let (midwin_x, midwin_y, midwin_cols) = with_state(|s| (s.midwin.x, s.midwin.y, s.midwin.cols));
    let stdout = out();
    while row < editwinrows {
        let _ = queue!(stdout,
            MoveTo(midwin_x, midwin_y + row as u16),
            Clear(ClearType::UntilNewLine),
        );

        #[cfg(not(feature = "tiny"))]
        {
            let sidebar = with_state(|s| s.sidebar);
            if sidebar != 0 {
                let bardata = with_state(|s| s.bardata.get(row as usize).copied().unwrap_or(b' ' as i32));
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
    with_state_mut(|s| s.refresh_needed = false);
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
                let shim = if with_state(|s| s.flag_isset(ZERO))
                    && (with_state(|s| s.currmenu) == MREPLACEWITH
                        || with_state(|s| s.currmenu) == MYESNO)
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
    let softwrap = with_state(|s| s.flag_isset(SOFTWRAP));
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

    // Number of characters up to the cursor
    // (simplified: use current_x as byte offset)
    let sum = current_x; // full implementation would use number_of_characters_in()

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

    let spot_pair = with_state(|s| s.interface_color_pair[SPOTLIGHTED]);
    apply_interface_color(spot_pair);

    let stdout = out();
    let _ = queue!(stdout, Print(&word[..actual_x(&word, to_col_eff)]));

    if overshoots {
        let cols = with_state(|s| s.midwin.cols);
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

    let spot_pair = with_state(|s| s.interface_color_pair[SPOTLIGHTED]);

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
        let stdout = out();
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
    let with_interface = !with_state(|s| s.flag_isset(ZERO));
    let with_help = !with_state(|s| s.flag_isset(NO_HELP));

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

    let editwinrows = with_state(|s| s.editwinrows);
    let cols = with_state(|s| s.midwin.cols) as usize;

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
            let stdout = out();
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
        with_state_mut(|s| s.flags[flag_index(ZERO)] &= !flag_mask(ZERO));
    }
    if with_help {
        with_state_mut(|s| s.flags[flag_index(NO_HELP)] &= !flag_mask(NO_HELP));
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
    with_state(|s| s.footwin.cols) as usize
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
    with_state(|s| s.editwinrows)
}

/// Return the margin (line-number column width).
pub fn get_margin() -> i32 {
    with_state(|s| s.margin)
}

/// Return reference to topwin for external use.
pub fn get_topwin() -> NanoWindow {
    with_state(|s| s.topwin.clone())
}

/// Return reference to midwin for external use.
pub fn get_midwin() -> NanoWindow {
    with_state(|s| s.midwin.clone())
}

/// Return reference to footwin for external use.
pub fn get_footwin() -> NanoWindow {
    with_state(|s| s.footwin.clone())
}
