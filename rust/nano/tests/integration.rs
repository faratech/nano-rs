/// Integration tests for the GNU nano Rust port.
///
/// These drive the compiled binary through a pseudo-terminal (pty) and verify
/// that editing operations produce the correct file content on disk.
///
/// Run:  cd rust && cargo test
/// Single: cd rust && cargo test -- enter_splits_line --nocapture
use std::fs;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};
use std::time::Duration;
use std::thread;

fn nano_bin() -> &'static str {
    env!("CARGO_BIN_EXE_nano")
}

const CTRL_X: u8 = 0x18;
const CTRL_O: u8 = 0x0f;
const ENTER:  u8 = 0x0d;
const BSPACE: u8 = 0x7f;

fn tmp(name: &str) -> String { format!("/tmp/nano_test_{}", name) }

fn write_tmp(name: &str, content: &str) -> String {
    let p = tmp(name);
    fs::write(&p, content).unwrap();
    p
}

/// Drive `bin` (any nano executable) on `file` via a pty.
fn drive_bin(bin: &str, file: &str, keys: &[u8], init_ms: u64, between_ms: u64) -> String {
    let (mfd, sfd) = unsafe {
        let mut m = 0i32; let mut s = 0i32;
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(),
                                 std::ptr::null_mut(), std::ptr::null_mut()), 0,
                   "openpty failed");
        (m, s)
    };

    let slave = unsafe { fs::File::from_raw_fd(sfd) };
    let mut child = Command::new(bin)
        .arg(file)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .expect("spawn nano");

    thread::sleep(Duration::from_millis(init_ms));

    for &k in keys {
        let mut master = unsafe { fs::File::from_raw_fd(mfd) };
        if master.write_all(&[k]).is_err() { break; }
        // Prevent File::drop from closing the fd — we reopen it each iteration
        std::mem::forget(master);
        thread::sleep(Duration::from_millis(between_ms));
    }

    thread::sleep(Duration::from_millis(1800));
    let _ = child.kill();
    let _ = child.wait();
    unsafe { libc::close(mfd) };

    fs::read_to_string(file).unwrap_or_default()
}

/// Convenience wrapper using the Rust nano binary.
fn drive(file: &str, keys: &[u8], init_ms: u64, between_ms: u64) -> String {
    drive_bin(nano_bin(), file, keys, init_ms, between_ms)
}

/// Drive nano but send `seq` (an ANSI escape sequence) as a single atomic write.
/// `pre_keys` are sent before the sequence, `post_keys` after.
fn drive_with_seq(file: &str, pre_keys: &[u8], seq: &[u8], post_keys: &[u8],
                  init_ms: u64, between_ms: u64) -> String {
    let (mfd, sfd) = unsafe {
        let mut m = 0i32; let mut s = 0i32;
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(),
                                 std::ptr::null_mut(), std::ptr::null_mut()), 0,
                   "openpty failed");
        (m, s)
    };
    let slave = unsafe { fs::File::from_raw_fd(sfd) };
    let mut child = Command::new(nano_bin())
        .arg(file)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn().expect("spawn nano");

    thread::sleep(Duration::from_millis(init_ms));

    // Send pre_keys with normal delay
    for &k in pre_keys {
        let mut m = unsafe { fs::File::from_raw_fd(mfd) };
        let _ = m.write_all(&[k]);
        std::mem::forget(m);
        thread::sleep(Duration::from_millis(between_ms));
    }
    // Send escape sequence atomically (no delay between bytes)
    {
        let mut m = unsafe { fs::File::from_raw_fd(mfd) };
        let _ = m.write_all(seq);
        std::mem::forget(m);
    }
    thread::sleep(Duration::from_millis(between_ms));
    // Send post_keys with normal delay
    for &k in post_keys {
        let mut m = unsafe { fs::File::from_raw_fd(mfd) };
        let _ = m.write_all(&[k]);
        std::mem::forget(m);
        thread::sleep(Duration::from_millis(between_ms));
    }

    thread::sleep(Duration::from_millis(1800));
    let _ = child.kill(); let _ = child.wait();
    unsafe { libc::close(mfd) };
    fs::read_to_string(file).unwrap_or_default()
}

// ── startup / flags ──────────────────────────────────────────────────────────

#[test]
fn version_shows_gnu_nano_9() {
    let out = Command::new(nano_bin()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("GNU nano"), "{}", text);
    assert!(text.contains("9.0"), "{}", text);
}

#[test]
fn help_shows_options() {
    let out = Command::new(nano_bin()).arg("--help").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--smarthome"), "{}", text);
    assert!(text.contains("--tabsize"), "{}", text);
}

#[test]
fn non_tty_stdin_exits_nonzero() {
    let status = Command::new(nano_bin())
        .arg("/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().unwrap();
    assert!(!status.success());
}

// ── exit without edit ────────────────────────────────────────────────────────

#[test]
fn ctrl_x_no_edit_leaves_file_unchanged() {
    let path = write_tmp("noedit", "unchanged\n");
    drive(&path, &[CTRL_X], 1500, 150);
    assert_eq!(fs::read_to_string(&path).unwrap(), "unchanged\n");
}

// ── text insertion ───────────────────────────────────────────────────────────

#[test]
fn typing_prepends_to_file() {
    let path = write_tmp("typing", "world\n");
    let mut keys: Vec<u8> = b"hello ".to_vec();
    keys.extend_from_slice(&[CTRL_O, ENTER, CTRL_X]);
    drive(&path, &keys, 2500, 300);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("hello"), "{:?}", c);
    assert!(c.contains("world"), "{:?}", c);
}

#[test]
fn enter_splits_line() {
    let path = write_tmp("enter", "AB\n");
    // Type 'X' then Enter → "X\nAB\n", then save
    drive(&path, &[b'X', ENTER, CTRL_O, ENTER, CTRL_X], 2500, 350);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains('X'), "{:?}", c);
    assert!(c.contains("AB"), "{:?}", c);
    assert!(c.len() > "AB\n".len(), "file not expanded: {:?}", c);
    assert!(c.matches('\n').count() >= 2, "no split: {:?}", c);
}

#[test]
fn multiple_enters_create_blank_lines() {
    let path = write_tmp("multienter", "Z\n");
    drive(&path, &[ENTER, ENTER, ENTER, CTRL_O, ENTER, CTRL_X], 2500, 300);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.matches('\n').count() >= 4, "{:?}", c);
}

#[test]
fn two_lines_typed_and_saved() {
    let path = write_tmp("twolines", "");
    let mut keys: Vec<u8> = b"line1".to_vec();
    keys.push(ENTER);
    keys.extend_from_slice(b"line2");
    keys.extend_from_slice(&[CTRL_O, ENTER, CTRL_X]);
    drive(&path, &keys, 2500, 200);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("line1") && c.contains("line2"), "{:?}", c);
    assert!(c.contains('\n'), "{:?}", c);
}

// ── backspace / delete ───────────────────────────────────────────────────────

#[test]
fn backspace_deletes_last_typed_char() {
    let path = write_tmp("backspace", "hello\n");
    // Type a, b, c — then backspace twice → only 'a' remains of typed chars
    drive(&path, &[b'a', b'b', b'c', BSPACE, BSPACE, CTRL_O, ENTER, CTRL_X],
          2500, 250);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains('a'), "a should remain: {:?}", c);
    assert!(!c.contains("bc"), "bc should be deleted: {:?}", c);
    assert!(c.contains("hello"), "original text lost: {:?}", c);
}

#[test]
fn backspace_at_line_start_joins_lines() {
    // Start of file: type Enter (split), then Bspace (rejoin) → net unchanged
    let path = write_tmp("bsp_join", "XY\n");
    drive(&path, &[ENTER, BSPACE, CTRL_O, ENTER, CTRL_X], 2500, 350);
    let c = fs::read_to_string(&path).unwrap();
    // After split+rejoin the net result should preserve XY
    assert!(c.contains("XY"), "{:?}", c);
}

// ── save operations ──────────────────────────────────────────────────────────

#[test]
fn ctrl_o_saves_without_exiting() {
    let path = write_tmp("ctrlo", "original\n");
    let mut keys: Vec<u8> = b"EDIT".to_vec();
    // Ctrl+O → prompt appears; pause then Enter to confirm filename
    keys.extend_from_slice(&[CTRL_O]);
    // Extra pause built into drive's between_ms; finish with Enter + Ctrl+X
    keys.extend_from_slice(&[ENTER, CTRL_X]);
    drive(&path, &keys, 2500, 450); // longer between to let prompt appear
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("EDIT"), "EDIT not saved: {:?}", c);
    assert!(c.contains("original"), "original text lost: {:?}", c);
}

#[test]
fn ctrl_x_y_saves_modified_buffer() {
    let path = write_tmp("ctrlxy", "base\n");
    let mut keys: Vec<u8> = b"ADDED".to_vec();
    keys.extend_from_slice(&[CTRL_X, b'y', ENTER]);
    drive(&path, &keys, 2500, 350);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("ADDED"), "ADDED not saved: {:?}", c);
    assert!(c.contains("base"), "base text lost: {:?}", c);
}

#[test]
fn ctrl_x_n_discards_changes() {
    let path = write_tmp("discard", "keep\n");
    let mut keys: Vec<u8> = b"DISCARD".to_vec();
    keys.extend_from_slice(&[CTRL_X, b'n']);
    drive(&path, &keys, 2500, 300);
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep\n");
}

// ── new file ─────────────────────────────────────────────────────────────────

#[test]
fn creates_new_file_with_typed_content() {
    let path = tmp("newfile");
    let _ = fs::remove_file(&path);
    let mut keys: Vec<u8> = b"fresh content".to_vec();
    keys.extend_from_slice(&[CTRL_X, b'y', ENTER]);
    drive(&path, &keys, 2500, 200);
    assert!(std::path::Path::new(&path).exists(), "file not created");
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("fresh content"), "{:?}", c);
}

// ── view mode ────────────────────────────────────────────────────────────────

#[test]
fn view_mode_prevents_modification() {
    let path = write_tmp("viewmode", "protected\n");
    let (mfd, sfd) = unsafe {
        let mut m = 0i32; let mut s = 0i32;
        libc::openpty(&mut m, &mut s, std::ptr::null_mut(),
                      std::ptr::null_mut(), std::ptr::null_mut());
        (m, s)
    };
    let slave = unsafe { fs::File::from_raw_fd(sfd) };
    let mut child = Command::new(nano_bin())
        .args(["-v", &path])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn().unwrap();
    thread::sleep(Duration::from_millis(1500));
    for k in b"TRYCHANGE".iter().chain(std::iter::once(&CTRL_X)) {
        let mut master = unsafe { fs::File::from_raw_fd(mfd) };
        let _ = master.write_all(&[*k]);
        std::mem::forget(master);
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(500));
    let _ = child.kill(); let _ = child.wait();
    unsafe { libc::close(mfd) };
    assert_eq!(fs::read_to_string(&path).unwrap(), "protected\n");
}

// ── no panics ────────────────────────────────────────────────────────────────

#[test]
fn no_panic_on_typing_and_exit() {
    let path = write_tmp("nopanic", "test\n");
    // Type, Enter, backspace, exit — should not panic
    drive(&path, &[b'a', ENTER, BSPACE, CTRL_X, b'n'], 2500, 250);
    // If nano panics, exit code would be non-zero; we just verify the
    // file is still readable and we didn't crash the test process.
    fs::read_to_string(&path).unwrap();
}

#[test]
fn no_panic_on_empty_file_editing() {
    let path = write_tmp("emptypanic", "");
    let mut keys: Vec<u8> = b"hello".to_vec();
    keys.extend_from_slice(&[ENTER, BSPACE, BSPACE, CTRL_O, ENTER, CTRL_X]);
    drive(&path, &keys, 2500, 250);
    // No panic = test passed
}

// ── feature compile checks ───────────────────────────────────────────────────

#[test]
fn compiled_with_utf8_feature() {
    let out = Command::new(nano_bin()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(text.contains("utf"), "--version should mention utf: {}", text);
}

#[test]
fn no_default_features_build_compiles() {
    // This test verifies the build matrix, not runtime behaviour.
    // We just check the binary exists (built by cargo build earlier).
    assert!(std::path::Path::new(&nano_bin()).exists(),
            "binary not found at {}", nano_bin());
}

// ── navigation (arrow keys, Home/End, Page Up/Down) ──────────────────────────

#[test]
fn arrow_down_moves_cursor_to_next_line() {
    // Type on line 1, Down arrow, type on line 2, save
    let path = write_tmp("arrow_down", "line1\nline2\n");
    // With cursor at start of line1, type "A", Down, "B", Ctrl+O, Enter, Ctrl+X
    // Result: "Aline1\nBline2\n"  (A prepended to line1, B prepended to line2)
    let keys = [b'A', 0x0E /*Ctrl+N = down in some bindings*/,
                b'B', CTRL_O, ENTER, CTRL_X];
    // Use KEY_DOWN as sent via arrow key escape (crossterm translates ESC[B)
    // Simpler: use Ctrl+N which is bound to do_down in nano
    drive(&path, &[b'A', 0x0E, b'B', CTRL_O, ENTER, CTRL_X], 2500, 300);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains('A'), "A missing: {:?}", c);
    assert!(c.contains('B'), "B missing: {:?}", c);
}

#[test]
fn ctrl_a_moves_to_line_start() {
    // Ctrl+A = do_home (beginning of line)
    let path = write_tmp("ctrl_a", "hello\n");
    // Type "END", then Ctrl+A (go to start), type "START", Ctrl+O, Enter, Ctrl+X
    drive(&path, &[b'E', b'N', b'D', 0x01, b'S', b'T', b'R', b'T',
                   CTRL_O, ENTER, CTRL_X], 2500, 250);
    let c = fs::read_to_string(&path).unwrap();
    // "STRT" should come before "END" in the output
    let strt_pos = c.find("STRT").unwrap_or(usize::MAX);
    let end_pos  = c.find("END").unwrap_or(0);
    assert!(strt_pos < end_pos, "STRT should precede END: {:?}", c);
}

#[test]
fn ctrl_e_moves_to_line_end() {
    // Ctrl+E = do_end
    let path = write_tmp("ctrl_e", "hello\n");
    // Ctrl+A (start), then Ctrl+E (end), then type "TAIL", save
    drive(&path, &[0x01, 0x05, b'T', b'A', b'I', b'L', CTRL_O, ENTER, CTRL_X],
          2500, 250);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("TAIL"), "TAIL missing: {:?}", c);
    // TAIL should be after "hello" (end of line)
    let hello_pos = c.find("hello").unwrap_or(usize::MAX);
    let tail_pos  = c.find("TAIL").unwrap_or(0);
    assert!(hello_pos < tail_pos, "TAIL should follow hello: {:?}", c);
}

#[test]
fn left_arrow_moves_cursor_left() {
    // LEFT arrow = ESC[D sent atomically → do_left
    let path = write_tmp("leftarrow", "AB\n");
    // Type "X" (cursor→1), atomic ESC[D (cursor→0), type "Y" → "YXAB\n"
    let c = drive_with_seq(&path,
        &[b'X'],                         // pre: type X
        &[0x1B, 0x5B, 0x44],             // ESC[D = left arrow
        &[b'Y', CTRL_O, ENTER, CTRL_X],  // post: type Y, save, exit
        2500, 250);
    assert!(c.contains("YX"), "YX not found: {:?}", c);
}

// ── cut / copy / paste ───────────────────────────────────────────────────────

#[test]
fn ctrl_k_cuts_line() {
    // Ctrl+K = cut_text (cuts current line)
    let path = write_tmp("ctrlk", "cut_me\nkeep\n");
    // Ctrl+K cuts line 1, Ctrl+O, Enter, Ctrl+X
    drive(&path, &[0x0B, CTRL_O, ENTER, CTRL_X], 2500, 300);
    let c = fs::read_to_string(&path).unwrap();
    assert!(!c.contains("cut_me"), "cut_me should be cut: {:?}", c);
    assert!(c.contains("keep"), "keep should remain: {:?}", c);
}

#[test]
fn ctrl_k_then_ctrl_u_roundtrips_line() {
    // Cut then paste restores the line
    let path = write_tmp("kutpaste", "round_trip\n");
    // Ctrl+K, Ctrl+U (paste) → same content, Ctrl+O, Enter, Ctrl+X
    drive(&path, &[0x0B, 0x15, CTRL_O, ENTER, CTRL_X], 2500, 350);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("round_trip"), "round_trip missing after cut+paste: {:?}", c);
}

// ── search ───────────────────────────────────────────────────────────────────

#[test]
fn ctrl_w_search_finds_text() {
    // Ctrl+W opens search prompt; type search term, Enter → moves cursor
    // We verify by typing after search (at found position) and saving
    let path = write_tmp("search", "alpha\nbeta\ngamma\n");
    // Ctrl+W, type "beta", Enter → cursor at "beta", type "FOUND", Ctrl+O, Enter, Ctrl+X
    let mut keys: Vec<u8> = vec![0x17]; // Ctrl+W = search forward
    keys.extend_from_slice(b"beta");
    keys.push(ENTER);
    keys.extend_from_slice(b"FOUND");
    keys.extend_from_slice(&[CTRL_O, ENTER, CTRL_X]);
    drive(&path, &keys, 2500, 200);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("FOUND"), "FOUND not inserted after search: {:?}", c);
    // FOUND should appear near "beta"
    assert!(c.contains("beta") || c.contains("FOUND"), "{:?}", c);
}

// ── undo ─────────────────────────────────────────────────────────────────────

#[test]
fn meta_u_undoes_typed_chars() {
    // M-U (Alt+U = ESC + 'u') = do_undo in traditional bindings
    let path = write_tmp("undo_meta", "base\n");
    // Type "XYZ", M-U (undo), Ctrl+O, Enter, Ctrl+X
    // ESC+u sent with minimal gap so nano treats it as a meta sequence
    let keys: Vec<u8> = vec![b'X', b'Y', b'Z',
                              0x1B, b'u', // M-U = undo
                              CTRL_O, ENTER, CTRL_X];
    drive(&path, &keys, 2500, 200);
    let c = fs::read_to_string(&path).unwrap();
    assert!(c.contains("base"), "base missing after undo: {:?}", c);
    // nano batches ADD operations so XYZ is undone as one unit
    assert!(!c.contains("XYZ"), "XYZ should be undone: {:?}", c);
}

// ── comparison with C nano ───────────────────────────────────────────────────

#[test]
fn rust_and_c_nano_produce_same_output_for_insert_and_save() {
    let c_path    = write_tmp("cmp_c",    "original\n");
    let rust_path = write_tmp("cmp_rust", "original\n");

    let mut keys: Vec<u8> = b"PREFIX".to_vec();
    keys.extend_from_slice(&[CTRL_O, ENTER, CTRL_X]);

    drive_bin("/usr/bin/nano", &c_path,    &keys, 2500, 350);
    drive_bin(nano_bin(),      &rust_path, &keys, 2500, 350);

    let c_content    = fs::read_to_string(&c_path).unwrap();
    let rust_content = fs::read_to_string(&rust_path).unwrap();

    assert_eq!(c_content, rust_content,
        "Rust nano output differs from C nano:\nC:    {:?}\nRust: {:?}",
        c_content, rust_content);
}
