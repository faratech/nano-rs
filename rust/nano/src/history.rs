#![allow(unused, non_snake_case, dead_code, non_camel_case_types)]
use crate::definitions::*;
use crate::global::STATE;

/// Which history list we are operating on.
/// Defined outside the feature gate so prompt.rs can reference it unconditionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Search,
    Replace,
    Execute,
}

// This entire module is guarded by the "histories" feature.
#[cfg(feature = "histories")]
mod inner {

use crate::definitions::*;
use crate::global::STATE;
use crate::{ISSET, SET, UNSET, TOGGLE};

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as IoWrite};
use std::time::SystemTime;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SEARCH_HISTORY: &str = "search_history";
const POSITION_HISTORY: &str = "filepos_history";

// File-level statics translated as thread_local! RefCells.

thread_local! {
    /// Whether any of the history lists has changed.
    static HISTORY_CHANGED: std::cell::RefCell<bool> = std::cell::RefCell::new(false);

    /// The name of the positions-register file.
    static REGISTERNAME: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);

    /// The last time the positions-register file was written (Unix seconds).
    static LATEST_TIMESTAMP: std::cell::RefCell<i64> = std::cell::RefCell::new(942_927_132);

    /// A list of recently opened files with their last cursor position.
    static POSITIONS_REGISTER: std::cell::RefCell<Vec<PositionRecord>> =
        std::cell::RefCell::new(Vec::new());
}

/// Rust equivalent of positionstruct.
/* C: typedef struct positionstruct { ... } positionstruct; */
#[derive(Debug, Clone)]
pub struct PositionRecord {
    /// The full path plus name of the file.
    pub filename: String,
    /// The line where the cursor was when the file was closed.
    pub linenumber: isize,
    /// The column where the cursor was.
    pub columnnumber: isize,
    /// The line numbers where anchors were placed, in string form.
    pub anchors: Option<String>,
}

// ---------------------------------------------------------------------------
// history_init
// ---------------------------------------------------------------------------

/* C: void history_init(void) */
/// Initialize the lists of historical search and replace strings
/// and the list of historical executed commands.
pub fn history_init() {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        // Each list is a Vec<String> with the current position index.
        // In the C code, each list is a doubly-linked list ending with an
        // empty sentinel node; the "current position" pointer starts at the
        // sentinel (bottom). We represent each history list as
        // Vec<String> (oldest first) plus a cursor index.
        st.search_history_items.clear();
        st.search_history_pos = 0;

        st.replace_history_items.clear();
        st.replace_history_pos = 0;

        st.execute_history_items.clear();
        st.execute_history_pos = 0;
    });
}

// ---------------------------------------------------------------------------
// reset_history_pointer_for
// ---------------------------------------------------------------------------

/* C: void reset_history_pointer_for(const linestruct *item) */
/// Reset the pointer into the history list that contains item to the bottom.
pub fn reset_history_pointer_for(which: HistoryKind) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        match which {
            HistoryKind::Search  => st.search_history_pos  = st.search_history_items.len(),
            HistoryKind::Replace => st.replace_history_pos = st.replace_history_items.len(),
            HistoryKind::Execute => st.execute_history_pos = st.execute_history_items.len(),
        }
    });
}

// HistoryKind is defined at the outer module level (above mod inner).
use super::HistoryKind;

// ---------------------------------------------------------------------------
// find_in_history  (internal helper)
// ---------------------------------------------------------------------------

/* C: linestruct *find_in_history(const linestruct *start, const linestruct *end,
 *                                 const char *text, size_t len) */
/// Search `items[start_idx..=end_idx]` backward (from start_idx toward 0)
/// for an entry whose first `len` bytes match `text`.
/// The C code traverses via ->prev (so from `start` toward `htop`).
/// In our Vec representation "older" entries have lower indices.
/// `start_idx` is inclusive, stop *before* going past `stop_idx`.
fn find_in_history<'a>(
    items: &'a [String],
    start_idx: usize,
    stop_idx: usize,
    text: &str,
    len: usize,
) -> Option<usize> {
    // C loop: for (item = start; item != end->prev && item != NULL; item = item->prev)
    // In the C linked list, "start" is the most-recent non-sentinel node and we walk
    // prev (toward older). Here items[0] is oldest. We iterate from start_idx down to
    // stop_idx (inclusive).
    let safe_len = len.min(text.len());
    let prefix = &text[..safe_len];

    let mut idx = start_idx;
    loop {
        let item = &items[idx];
        let item_prefix = if item.len() >= safe_len { &item[..safe_len] } else { item.as_str() };
        if item_prefix == prefix {
            return Some(idx);
        }
        if idx == stop_idx {
            break;
        }
        if idx == 0 {
            break;
        }
        idx -= 1;
    }
    None
}

// ---------------------------------------------------------------------------
// update_history
// ---------------------------------------------------------------------------

/* C: void update_history(linestruct **item, const char *text, bool avoid_duplicates) */
/// Update a history list with a fresh string text.
/// `which` identifies the list; after the call the list's current position
/// is reset to the bottom (most recent).
pub fn update_history(which: HistoryKind, text: &str, avoid_duplicates: bool) {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        let items: &mut Vec<String> = match which {
            HistoryKind::Search  => &mut st.search_history_items,
            HistoryKind::Replace => &mut st.replace_history_items,
            HistoryKind::Execute => &mut st.execute_history_items,
        };

        // When requested, check if the string is already in the history.
        if avoid_duplicates {
            if let Some(pos) = items.iter().position(|s| s == text) {
                items.remove(pos);
            }
        }

        // If the history is full, delete the oldest item.
        if items.len() >= MAX_SEARCH_HISTORY {
            items.remove(0);
        }

        // Append the fresh string.
        items.push(text.to_string());

        // Reset the current position to the bottom.
        let new_pos = items.len();
        match which {
            HistoryKind::Search  => st.search_history_pos  = new_pos,
            HistoryKind::Replace => st.replace_history_pos = new_pos,
            HistoryKind::Execute => st.execute_history_pos = new_pos,
        }
    });

    // Indicate that the history needs to be saved on exit.
    HISTORY_CHANGED.with(|hc| *hc.borrow_mut() = true);
}

// ---------------------------------------------------------------------------
// get_history_completion  (#[cfg(feature = "tabcomp")])
// ---------------------------------------------------------------------------

/* C: char *get_history_completion(linestruct **here, char *string, size_t len) */
/// Go backward through a history list starting at the current position,
/// searching for a string that is a tab completion of the given string
/// (comparing only its first `len` characters).  When found, update the
/// current position pointer and return the found item's text.  If no
/// match is found, return the original string unchanged.
#[cfg(feature = "tabcomp")]
pub fn get_history_completion(which: HistoryKind, string: &str, len: usize) -> String {
    // Read items and current position first (immutable borrow).
    let (items, here) = STATE.with(|s| {
        let st = s.borrow();
        let (items, pos) = match which {
            HistoryKind::Search  => (st.search_history_items.clone(),  st.search_history_pos),
            HistoryKind::Replace => (st.replace_history_items.clone(), st.replace_history_pos),
            HistoryKind::Execute => (st.execute_history_items.clone(), st.execute_history_pos),
        };
        (items, pos)
    });

    let safe_len = len.min(string.len());
    let prefix = &string[..safe_len];

    // Search from current position - 1 down to 0 (toward oldest / htop).
    if here > 0 {
        let mut idx = here - 1;
        loop {
            if items[idx].starts_with(prefix) && items[idx] != string {
                // Update position (separate mutable borrow).
                STATE.with(|s| {
                    let mut st = s.borrow_mut();
                    match which {
                        HistoryKind::Search  => st.search_history_pos  = idx,
                        HistoryKind::Replace => st.replace_history_pos = idx,
                        HistoryKind::Execute => st.execute_history_pos = idx,
                    }
                });
                return items[idx].clone();
            }
            if idx == 0 { break; }
            idx -= 1;
        }
    }

    // Now search from the bottom (newest) down to here (exclusive).
    let bottom = items.len();
    if bottom > 0 {
        let mut idx = bottom - 1;
        loop {
            if idx == here { break; }
            if items[idx].starts_with(prefix) && items[idx] != string {
                STATE.with(|s| {
                    let mut st = s.borrow_mut();
                    match which {
                        HistoryKind::Search  => st.search_history_pos  = idx,
                        HistoryKind::Replace => st.replace_history_pos = idx,
                        HistoryKind::Execute => st.execute_history_pos = idx,
                    }
                });
                return items[idx].clone();
            }
            if idx == 0 { break; }
            idx -= 1;
        }
    }

    // No useful match — return the original string.
    string.to_string()
}

// ---------------------------------------------------------------------------
// have_statedir
// ---------------------------------------------------------------------------

/* C: bool have_statedir(void) */
/// Check whether we have or could make a directory for history files.
pub fn have_statedir() -> bool {
    let homedir_opt: Option<String> = STATE.with(|s| s.borrow().homedir.clone());

    // First try ~/.nano/
    if let Some(ref homedir) = homedir_opt {
        let statedir = format!("{}/.nano/", homedir);
        if let Ok(meta) = fs::metadata(&statedir) {
            if meta.is_dir() {
                let regname = format!("{}{}", statedir, POSITION_HISTORY);
                STATE.with(|s| {
                    s.borrow_mut().statedir = Some(statedir.clone());
                });
                REGISTERNAME.with(|r| *r.borrow_mut() = Some(regname));
                return true;
            }
        }
    }

    // Fall back to XDG_DATA_HOME or ~/.local/share/nano/
    let xdg_data_dir: Option<String> = std::env::var("XDG_DATA_HOME").ok();

    if homedir_opt.is_none() && xdg_data_dir.is_none() {
        return false;
    }

    let statedir = if let Some(ref xdg) = xdg_data_dir {
        format!("{}/nano/", xdg)
    } else {
        format!("{}/.local/share/nano/", homedir_opt.as_deref().unwrap_or(""))
    };

    match fs::metadata(&statedir) {
        Err(_) => {
            // Try to create the directory (and parents if needed).
            if xdg_data_dir.is_none() {
                if let Some(ref homedir) = homedir_opt {
                    let _ = fs::create_dir(format!("{}/.local", homedir));
                    let _ = fs::create_dir(format!("{}/.local/share", homedir));
                }
            }
            if let Err(e) = fs::create_dir(&statedir) {
                eprintln!(
                    "Unable to create directory {}: {}\n\
                     It is required for saving/loading search history or cursor positions.",
                    statedir, e
                );
                return false;
            }
        }
        Ok(meta) if !meta.is_dir() => {
            eprintln!(
                "Path {} is not a directory and needs to be.\n\
                 Nano will be unable to load or save search history or cursor positions.",
                statedir
            );
            return false;
        }
        _ => {}
    }

    let regname = format!("{}{}", statedir, POSITION_HISTORY);
    STATE.with(|s| {
        s.borrow_mut().statedir = Some(statedir);
    });
    REGISTERNAME.with(|r| *r.borrow_mut() = Some(regname));
    true
}

// ---------------------------------------------------------------------------
// recode helpers  (NUL ↔ LF)
// ---------------------------------------------------------------------------

/// Replace every 0x00 byte in `data` with 0x0A (LF).
/// This is the Rust equivalent of recode_NUL_to_LF().
fn recode_nul_to_lf(data: &mut Vec<u8>) {
    for b in data.iter_mut() {
        if *b == 0x00 {
            *b = b'\n';
        }
    }
}

/// Replace every 0x0A (LF) byte with 0x00, returning the original length.
/// This is the Rust equivalent of recode_LF_to_NUL() which mutates in place.
fn recode_lf_to_nul(data: &mut [u8]) -> usize {
    let len = data.len();
    for b in data.iter_mut() {
        if *b == b'\n' {
            *b = 0x00;
        }
    }
    len
}

// ---------------------------------------------------------------------------
// load_history
// ---------------------------------------------------------------------------

/* C: void load_history(void) */
/// Load the histories for Search, Replace With, and Execute Command.
pub fn load_history() {
    let statedir_opt: Option<String> = STATE.with(|s| s.borrow().statedir.clone());
    let statedir = match statedir_opt {
        Some(d) => d,
        None => return,
    };
    let historyname = format!("{}{}", statedir, SEARCH_HISTORY);

    let file = match File::open(&historyname) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!("Error reading {}: {}", historyname, e);
            UNSET!(HISTORYLOG);
            return;
        }
    };

    let mut reader = BufReader::new(file);
    // 0 = search, 1 = replace, 2 = execute
    let mut which = HistoryKind::Search;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        // Strip trailing newline.
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        let read = buf.len();

        if read > 0 {
            recode_nul_to_lf(&mut buf);
            // Convert bytes to String (best-effort for non-UTF-8).
            let s = String::from_utf8_lossy(&buf).into_owned();
            update_history(which, &s, false /* IGNORE_DUPLICATES */);
        } else {
            // Empty line separates the three lists.
            which = match which {
                HistoryKind::Search  => HistoryKind::Replace,
                HistoryKind::Replace => HistoryKind::Execute,
                HistoryKind::Execute => HistoryKind::Execute,
            };
        }
    }

    // Reading in the lists has marked them as changed; undo this side effect.
    HISTORY_CHANGED.with(|hc| *hc.borrow_mut() = false);
}

// ---------------------------------------------------------------------------
// write_list  (internal)
// ---------------------------------------------------------------------------

/* C: bool write_list(const linestruct *head, FILE *histories) */
/// Write the entries of a history list, oldest to newest, to `writer`.
/// Returns Ok(()) on success, Err on I/O failure.
fn write_list<W: IoWrite>(items: &[String], writer: &mut W) -> io::Result<()> {
    for item in items {
        // Encode embedded newlines as NUL bytes before writing.
        let mut bytes = item.as_bytes().to_vec();
        let length = recode_lf_to_nul(&mut bytes);
        writer.write_all(&bytes[..length])?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// save_history
// ---------------------------------------------------------------------------

/* C: void save_history(void) */
/// Save the histories for Search, Replace With, and Execute Command.
pub fn save_history() {
    let changed = HISTORY_CHANGED.with(|hc| *hc.borrow());
    if !changed {
        return;
    }

    let statedir_opt: Option<String> = STATE.with(|s| s.borrow().statedir.clone());
    let statedir = match statedir_opt {
        Some(d) => d,
        None => return,
    };
    let historyname = format!("{}{}", statedir, SEARCH_HISTORY);

    let mut file = match File::create(&historyname) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error writing {}: {}", historyname, e);
            return;
        }
    };

    // Don't allow others to read or write the history file.
    if let Err(e) = fs::set_permissions(&historyname,
            fs::Permissions::from_mode(0o600)) {
        eprintln!("Cannot limit permissions on {}: {}", historyname, e);
    }

    let (search_items, replace_items, execute_items) = STATE.with(|s| {
        let st = s.borrow();
        (
            st.search_history_items.clone(),
            st.replace_history_items.clone(),
            st.execute_history_items.clone(),
        )
    });

    let mut ok = write_list(&search_items, &mut file).is_ok();
    // Separator between sections (empty line already provided by write_list
    // flushing a newline after each entry; in the C code the three lists are
    // separated by a blank line written by the top-level save).
    // Write a blank line between lists.
    if ok { ok = file.write_all(b"\n").is_ok(); }
    if ok { ok = write_list(&replace_items, &mut file).is_ok(); }
    if ok { ok = file.write_all(b"\n").is_ok(); }
    if ok { ok = write_list(&execute_items, &mut file).is_ok(); }

    if !ok {
        eprintln!("Error writing {}", historyname);
    }
}

// ---------------------------------------------------------------------------
// stringify_anchors
// ---------------------------------------------------------------------------

/* C: char *stringify_anchors(void) */
/// Return as a string the line numbers of the lines with an anchor.
pub fn stringify_anchors() -> String {
    #[cfg(not(feature = "tiny"))]
    {
        STATE.with(|s| {
            let st = s.borrow();
            let mut result = String::new();
            if let Some(ref openfile) = st.openfile {
                let mut line_opt = openfile.filetop.clone();
                while let Some(line_rc) = line_opt {
                    let line = line_rc.borrow();
                    if line.has_anchor {
                        result.push_str(&format!("{} ", line.lineno));
                    }
                    line_opt = line.next.clone();
                }
            }
            result
        })
    }
    #[cfg(feature = "tiny")]
    {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// restore_anchors
// ---------------------------------------------------------------------------

/* C: void restore_anchors(char *string) */
/// Set an anchor for each line number in the given string.
pub fn restore_anchors(string: &str) {
    #[cfg(not(feature = "tiny"))]
    {
        STATE.with(|s| {
            let st = s.borrow();
            if let Some(ref openfile) = st.openfile {
                let mut remaining = string;
                let mut line_opt = openfile.filetop.clone();

                while !remaining.is_empty() {
                    // Find the space separator.
                    let space_pos = match remaining.find(' ') {
                        Some(p) => p,
                        None => return,
                    };
                    let number: isize = match remaining[..space_pos].parse() {
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    remaining = &remaining[space_pos + 1..];

                    // Advance to the target line number.
                    loop {
                        match line_opt.clone() {
                            None => return,
                            Some(ref rc) => {
                                let lineno = rc.borrow().lineno;
                                if lineno >= number {
                                    if lineno == number {
                                        rc.borrow_mut().has_anchor = true;
                                    }
                                    break;
                                }
                                line_opt = rc.borrow().next.clone();
                            }
                        }
                    }
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// load_positions_register
// ---------------------------------------------------------------------------

/* C: void load_positions_register(void) */
/// Load the recorded cursor positions for files that were opened.
pub fn load_positions_register() {
    let regname_opt = REGISTERNAME.with(|r| r.borrow().clone());
    let regname = match regname_opt {
        Some(n) => n,
        None => return,
    };

    let file = match File::open(&regname) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!("Error reading {}: {}", regname, e);
            UNSET!(POSITIONLOG);
            return;
        }
    };

    let mut reader = BufReader::new(file);
    let mut records: Vec<PositionRecord> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut count = 0;

    loop {
        if count >= 200 {
            break;
        }
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).unwrap_or(0);
        if n <= 1 {
            // 0 = EOF, 1 = only a newline (empty line)
            break;
        }
        // Strip trailing newline.
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }

        // Find the start of the path (first '/').
        let slash_pos = match buf.iter().position(|&b| b == b'/') {
            Some(p) => p,
            None => continue,
        };

        let anchors_bytes = if slash_pos > 0 {
            Some(buf[..slash_pos].to_vec())
        } else {
            None
        };

        // The path-and-place portion starts at slash_pos.
        let mut path_and_place = buf[slash_pos..].to_vec();

        // Decode NULs as embedded newlines.
        recode_nul_to_lf(&mut path_and_place);

        let pap_str = String::from_utf8_lossy(&path_and_place).into_owned();

        // The format is: "<filename> <lineno> <colno>"
        // Find the LAST space (column number) and the second-to-last (line number).
        let col_space = match pap_str.rfind(' ') {
            Some(p) => p,
            None => continue,
        };
        let line_space = match pap_str[..col_space].rfind(' ') {
            Some(p) => p,
            None => continue,
        };

        let filename = pap_str[..line_space].to_string();
        let linenumber: isize = pap_str[line_space + 1..col_space]
            .trim().parse().unwrap_or(0);
        let columnnumber: isize = pap_str[col_space + 1..]
            .trim().parse().unwrap_or(0);

        let anchors = anchors_bytes.map(|ab| {
            String::from_utf8_lossy(&ab).into_owned()
        });

        records.push(PositionRecord { filename, linenumber, columnnumber, anchors });
        count += 1;
    }

    POSITIONS_REGISTER.with(|pr| *pr.borrow_mut() = records);

    // Record the file's mtime.
    if let Ok(meta) = fs::metadata(&regname) {
        if let Ok(modified) = meta.modified() {
            let secs = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            LATEST_TIMESTAMP.with(|lt| *lt.borrow_mut() = secs);
        }
    }
}

// ---------------------------------------------------------------------------
// save_positions_register
// ---------------------------------------------------------------------------

/* C: void save_positions_register(void) */
/// Save the recorded cursor positions for files that were opened.
pub fn save_positions_register() {
    let regname_opt = REGISTERNAME.with(|r| r.borrow().clone());
    let regname = match regname_opt {
        Some(n) => n,
        None => return,
    };

    let mut file = match File::create(&regname) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error writing {}: {}", regname, e);
            return;
        }
    };

    // Don't allow others to read or write the positions-register file.
    if let Err(e) = fs::set_permissions(&regname,
            fs::Permissions::from_mode(0o600)) {
        eprintln!("Cannot limit permissions on {}: {}", regname, e);
    }

    let records = POSITIONS_REGISTER.with(|pr| pr.borrow().clone());

    for (idx, item) in records.iter().enumerate() {
        if idx >= 200 {
            break;
        }

        // First write the string of line numbers with anchors, if any.
        if let Some(ref anchors) = item.anchors {
            if !anchors.is_empty() {
                let anchor_bytes = anchors.as_bytes();
                if file.write_all(anchor_bytes).is_err() {
                    eprintln!("Error writing {}", regname);
                }
            }
        }

        // Build the path-and-place string.
        let path_and_place = format!(
            "{} {} {}\n",
            item.filename, item.linenumber, item.columnnumber
        );
        let mut bytes = path_and_place.into_bytes();

        // Encode newlines in filenames as NULs.
        let length = recode_lf_to_nul(&mut bytes);
        // Restore the terminating newline.
        if length > 0 {
            bytes[length - 1] = b'\n';
        }

        if file.write_all(&bytes[..length]).is_err() {
            eprintln!("Error writing {}", regname);
        }
    }

    // Record the new mtime.
    if let Ok(meta) = fs::metadata(&regname) {
        if let Ok(modified) = meta.modified() {
            let secs = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            LATEST_TIMESTAMP.with(|lt| *lt.borrow_mut() = secs);
        }
    }
}

// ---------------------------------------------------------------------------
// reload_positions_if_needed
// ---------------------------------------------------------------------------

/* C: void reload_positions_if_needed(void) */
/// Reload the positions-register file if it has been modified since last load.
pub fn reload_positions_if_needed() {
    let regname_opt = REGISTERNAME.with(|r| r.borrow().clone());
    let regname = match regname_opt {
        Some(n) => n,
        None => return,
    };

    let mtime = match fs::metadata(&regname) {
        Ok(meta) => match meta.modified() {
            Ok(t) => t
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            Err(_) => return,
        },
        Err(_) => return,
    };

    let latest = LATEST_TIMESTAMP.with(|lt| *lt.borrow());
    if mtime == latest {
        return;
    }

    // Clear the in-memory list.
    POSITIONS_REGISTER.with(|pr| pr.borrow_mut().clear());

    // Reload.
    load_positions_register();
}

// ---------------------------------------------------------------------------
// update_positions_register
// ---------------------------------------------------------------------------

/* C: void update_positions_register(void) */
/// Update the recorded last file positions with the current position in the
/// current buffer.  If no existing entry is found, add a new one at the top.
pub fn update_positions_register() {
    let filename_opt: Option<String> = STATE.with(|s| {
        s.borrow().openfile.as_ref().map(|f| f.filename.clone())
    });
    let fullpath_opt: Option<String> = filename_opt.as_deref()
        .and_then(|name| crate::files::get_full_path(name));
    let fullpath = match fullpath_opt {
        Some(p) => p,
        None => return,
    };

    reload_positions_if_needed();

    let lineno = STATE.with(|s| {
        let st = s.borrow();
        st.openfile.as_ref()
            .and_then(|f| f.current.as_ref())
            .map(|l| l.borrow().lineno)
            .unwrap_or(1)
    });
    let col = crate::utils::xplustabs() as isize + 1;
    let anchors = stringify_anchors();
    let (linenumber, columnnumber) = (lineno, col);

    POSITIONS_REGISTER.with(|pr| {
        let mut records = pr.borrow_mut();

        // Remove any existing entry for this file.
        records.retain(|r| r.filename != fullpath);

        // Insert a new record at the front.
        let new_record = PositionRecord {
            filename: fullpath,
            linenumber,
            columnnumber,
            anchors: if anchors.is_empty() { None } else { Some(anchors) },
        };
        records.insert(0, new_record);
    });

    save_positions_register();
}

// ---------------------------------------------------------------------------
// restore_cursor_position_if_any
// ---------------------------------------------------------------------------

/* C: void restore_cursor_position_if_any(void) */
/// Check whether the current filename matches an entry in the list of
/// recorded positions.  If yes, restore the relevant cursor position.
pub fn restore_cursor_position_if_any() {
    let filename_opt: Option<String> = STATE.with(|s| {
        s.borrow().openfile.as_ref().map(|f| f.filename.clone())
    });
    let fullpath_opt: Option<String> = filename_opt.as_deref()
        .and_then(|name| crate::files::get_full_path(name));
    let fullpath = match fullpath_opt {
        Some(p) => p,
        None => return,
    };

    reload_positions_if_needed();

    let record_opt = POSITIONS_REGISTER.with(|pr| {
        pr.borrow()
            .iter()
            .find(|r| r.filename == fullpath)
            .cloned()
    });

    if let Some(record) = record_opt {
        if let Some(ref anchors) = record.anchors {
            restore_anchors(anchors);
        }
        crate::search::goto_line_and_column(record.linenumber, record.columnnumber, true);
    }
}

} // mod inner

// Re-export everything from inner when the feature is enabled.
#[cfg(feature = "histories")]
pub use inner::*;
