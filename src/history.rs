#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]

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

    use crate::UNSET;
    use crate::definitions::*;
    use crate::global::STATE;

    use sha2::{Digest, Sha256};
    use std::fs::{self, File};
    use std::io::{self, BufRead, BufReader, Read, Write as IoWrite};
    use std::time::SystemTime;

    const SEARCH_HISTORY: &str = "search_history";
    const POSITION_HISTORY: &str = "filepos_history";

    // File-level statics translated as thread_local! RefCells.

    thread_local! {
        /// Whether any of the history lists has changed.
        static HISTORY_CHANGED: std::cell::RefCell<bool> = std::cell::RefCell::new(false);

        /// The name of the positions-register file.
        static REGISTERNAME: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);

        /// Identity of the positions-register snapshot currently in memory.
        /// Timestamp and length make the common check cheap to understand, while
        /// the digest catches rewrites on coarse-timestamp filesystems and
        /// same-length updates by another nano process.
        static LATEST_STAMP: std::cell::RefCell<Option<PositionFileStamp>> = const {
            std::cell::RefCell::new(None)
        };

        /// A list of recently opened files with their last cursor position.
        static POSITIONS_REGISTER: std::cell::RefCell<Vec<PositionRecord>> =
            std::cell::RefCell::new(Vec::new());
    }

    const MAX_POSITION_FILE_BYTES: u64 = 16 * 1024 * 1024;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct PositionFileStamp {
        modified: Option<SystemTime>,
        metadata_len: u64,
        digest: [u8; 32],
    }

    struct PositionFileSnapshot {
        bytes: Vec<u8>,
        stamp: PositionFileStamp,
    }

    fn stamp_position_bytes(
        modified: Option<SystemTime>,
        metadata_len: u64,
        bytes: &[u8],
    ) -> PositionFileStamp {
        PositionFileStamp {
            modified,
            metadata_len,
            digest: Sha256::digest(bytes).into(),
        }
    }

    fn read_position_snapshot(path: &str) -> io::Result<PositionFileSnapshot> {
        let file = File::open(path)?;
        let initial_len = file.metadata()?.len();
        if initial_len > MAX_POSITION_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "positions register is unreasonably large",
            ));
        }

        // Read one extra byte so growth after the metadata check cannot silently
        // bypass the bound.
        let mut bytes = Vec::with_capacity(initial_len as usize);
        (&file)
            .take(MAX_POSITION_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_POSITION_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "positions register is unreasonably large",
            ));
        }

        // Read metadata from the same open file description after consuming the
        // snapshot, avoiding a pathname replacement race between data and stamp.
        // The digest remains decisive if an in-place writer leaves timestamp and
        // length unchanged.
        let metadata = file.metadata()?;
        let stamp = stamp_position_bytes(metadata.modified().ok(), metadata.len(), &bytes);
        Ok(PositionFileSnapshot { bytes, stamp })
    }

    fn records_from_position_bytes(bytes: &[u8]) -> Vec<PositionRecord> {
        let mut reader = BufReader::new(io::Cursor::new(bytes));
        let mut records = Vec::new();
        let mut buf = Vec::new();
        let mut count = 0;

        loop {
            if count >= 200 {
                break;
            }
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf).unwrap_or(0);
            if n <= 1 {
                break;
            }
            count += 1;
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }

            if let Some(record) = parse_position_record(&buf) {
                records.push(record);
            }
        }

        records
    }

    fn install_position_snapshot(snapshot: PositionFileSnapshot) {
        let records = records_from_position_bytes(&snapshot.bytes);
        POSITIONS_REGISTER.with(|register| *register.borrow_mut() = records);
        LATEST_STAMP.with(|latest| *latest.borrow_mut() = Some(snapshot.stamp));
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
                HistoryKind::Search => st.search_history_pos = st.search_history_items.len(),
                HistoryKind::Replace => st.replace_history_pos = st.replace_history_items.len(),
                HistoryKind::Execute => st.execute_history_pos = st.execute_history_items.len(),
            }
        });
    }

    /// Return whether the selected history cursor is at its conceptual empty
    /// bottom entry.
    pub fn history_is_at_bottom(which: HistoryKind) -> bool {
        STATE.with(|s| {
            let st = s.borrow();
            match which {
                HistoryKind::Search => st.search_history_pos >= st.search_history_items.len(),
                HistoryKind::Replace => st.replace_history_pos >= st.replace_history_items.len(),
                HistoryKind::Execute => st.execute_history_pos >= st.execute_history_items.len(),
            }
        })
    }

    /// Move one entry toward the oldest item and return its text.  A stale cursor
    /// is first clamped to the bottom so history replacement cannot cause an
    /// out-of-bounds access.
    pub fn older_history_item(which: HistoryKind) -> Option<String> {
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            let st = &mut *st;
            let (items, pos) = match which {
                HistoryKind::Search => (&st.search_history_items, &mut st.search_history_pos),
                HistoryKind::Replace => (&st.replace_history_items, &mut st.replace_history_pos),
                HistoryKind::Execute => (&st.execute_history_items, &mut st.execute_history_pos),
            };

            *pos = (*pos).min(items.len());
            if *pos == 0 {
                return None;
            }

            *pos -= 1;
            Some(items[*pos].clone())
        })
    }

    /// Move one entry toward the conceptual empty bottom and return its text.
    /// Reaching the bottom returns `None`; the prompt layer can then restore the
    /// draft that preceded history navigation.
    pub fn newer_history_item(which: HistoryKind) -> Option<String> {
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            let st = &mut *st;
            let (items, pos) = match which {
                HistoryKind::Search => (&st.search_history_items, &mut st.search_history_pos),
                HistoryKind::Replace => (&st.replace_history_items, &mut st.replace_history_pos),
                HistoryKind::Execute => (&st.execute_history_items, &mut st.execute_history_pos),
            };

            *pos = (*pos).min(items.len());
            if *pos < items.len() {
                *pos += 1;
            }

            if *pos < items.len() {
                Some(items[*pos].clone())
            } else {
                None
            }
        })
    }

    // HistoryKind is defined at the outer module level (above mod inner).
    use super::HistoryKind;

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
                HistoryKind::Search => &mut st.search_history_items,
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
                HistoryKind::Search => st.search_history_pos = new_pos,
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
                HistoryKind::Search => (st.search_history_items.clone(), st.search_history_pos),
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
                            HistoryKind::Search => st.search_history_pos = idx,
                            HistoryKind::Replace => st.replace_history_pos = idx,
                            HistoryKind::Execute => st.execute_history_pos = idx,
                        }
                    });
                    return items[idx].clone();
                }
                if idx == 0 {
                    break;
                }
                idx -= 1;
            }
        }

        // Now search from the bottom (newest) down to here (exclusive).
        let bottom = items.len();
        if bottom > 0 {
            let mut idx = bottom - 1;
            loop {
                if idx == here {
                    break;
                }
                if items[idx].starts_with(prefix) && items[idx] != string {
                    STATE.with(|s| {
                        let mut st = s.borrow_mut();
                        match which {
                            HistoryKind::Search => st.search_history_pos = idx,
                            HistoryKind::Replace => st.replace_history_pos = idx,
                            HistoryKind::Execute => st.execute_history_pos = idx,
                        }
                    });
                    return items[idx].clone();
                }
                if idx == 0 {
                    break;
                }
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
            format!(
                "{}/.local/share/nano/",
                homedir_opt.as_deref().unwrap_or("")
            )
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
                    HistoryKind::Search => HistoryKind::Replace,
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(&historyname, fs::Permissions::from_mode(0o600)) {
                eprintln!("Cannot limit permissions on {}: {}", historyname, e);
            }
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
        if ok {
            ok = file.write_all(b"\n").is_ok();
        }
        if ok {
            ok = write_list(&replace_items, &mut file).is_ok();
        }
        if ok {
            ok = file.write_all(b"\n").is_ok();
        }
        if ok {
            ok = write_list(&execute_items, &mut file).is_ok();
        }

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

    fn position_path_start(buf: &[u8]) -> Option<usize> {
        for i in 0..buf.len() {
            if buf[i] == b'/' {
                return Some(i);
            }

            if i + 1 < buf.len() && buf[i] == b'\\' && buf[i + 1] == b'\\' {
                return Some(i);
            }

            if i + 2 < buf.len()
                && buf[i].is_ascii_alphabetic()
                && buf[i + 1] == b':'
                && (buf[i + 2] == b'\\' || buf[i + 2] == b'/')
            {
                return Some(i);
            }
        }

        None
    }

    fn parse_position_record(buf: &[u8]) -> Option<PositionRecord> {
        let path_start = position_path_start(buf)?;

        let anchors_bytes = if path_start > 0 {
            Some(buf[..path_start].to_vec())
        } else {
            None
        };

        let mut path_and_place = buf[path_start..].to_vec();
        recode_nul_to_lf(&mut path_and_place);

        let pap_str = String::from_utf8_lossy(&path_and_place).into_owned();

        // The format is: "<filename> <lineno> <colno>".
        let col_space = pap_str.rfind(' ')?;
        let line_space = pap_str[..col_space].rfind(' ')?;

        let filename = pap_str[..line_space].to_string();
        if filename.is_empty() {
            return None;
        }

        let linenumber: isize = pap_str[line_space + 1..col_space].trim().parse().ok()?;
        let columnnumber: isize = pap_str[col_space + 1..].trim().parse().ok()?;

        let anchors = anchors_bytes.map(|ab| String::from_utf8_lossy(&ab).into_owned());

        Some(PositionRecord {
            filename,
            linenumber,
            columnnumber,
            anchors,
        })
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

        let snapshot = match read_position_snapshot(&regname) {
            Ok(snapshot) => snapshot,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                POSITIONS_REGISTER.with(|register| register.borrow_mut().clear());
                LATEST_STAMP.with(|latest| *latest.borrow_mut() = None);
                return;
            }
            Err(e) => {
                eprintln!("Error reading {}: {}", regname, e);
                UNSET!(POSITIONLOG);
                return;
            }
        };
        install_position_snapshot(snapshot);
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

        let records = POSITIONS_REGISTER.with(|pr| pr.borrow().clone());
        let mut bytes = Vec::new();

        for (idx, item) in records.iter().enumerate() {
            if idx >= 200 {
                break;
            }

            if let Some(ref anchors) = item.anchors {
                if !anchors.is_empty() {
                    bytes.extend_from_slice(anchors.as_bytes());
                }
            }

            let path_and_place = format!(
                "{} {} {}\n",
                item.filename, item.linenumber, item.columnnumber
            );
            let start = bytes.len();
            bytes.extend_from_slice(path_and_place.as_bytes());
            let length = recode_lf_to_nul(&mut bytes[start..]);
            if length > 0 {
                bytes[start + length - 1] = b'\n';
            }
        }

        let mut file = match File::create(&regname) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error writing {}: {}", regname, e);
                return;
            }
        };

        // Don't allow others to read or write the positions-register file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
                eprintln!("Cannot limit permissions on {}: {}", regname, e);
            }
        }

        if let Err(error) = file.write_all(&bytes).and_then(|()| file.flush()) {
            eprintln!("Error writing {}: {}", regname, error);
            return;
        }

        if let Ok(metadata) = file.metadata() {
            let stamp = stamp_position_bytes(metadata.modified().ok(), metadata.len(), &bytes);
            LATEST_STAMP.with(|latest| *latest.borrow_mut() = Some(stamp));
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

        let snapshot = match read_position_snapshot(&regname) {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let had_snapshot = LATEST_STAMP.with(|latest| latest.borrow().is_some());
                if had_snapshot {
                    POSITIONS_REGISTER.with(|register| register.borrow_mut().clear());
                    LATEST_STAMP.with(|latest| *latest.borrow_mut() = None);
                }
                return;
            }
            Err(_) => return,
        };

        let unchanged =
            LATEST_STAMP.with(|latest| latest.borrow().as_ref() == Some(&snapshot.stamp));
        if unchanged {
            return;
        }

        install_position_snapshot(snapshot);
    }

    // ---------------------------------------------------------------------------
    // update_positions_register
    // ---------------------------------------------------------------------------

    /* C: void update_positions_register(void) */
    /// Update the recorded last file positions with the current position in the
    /// current buffer.  If no existing entry is found, add a new one at the top.
    pub fn update_positions_register() {
        let filename_opt: Option<String> =
            STATE.with(|s| s.borrow().openfile.as_ref().map(|f| f.filename.clone()));
        let fullpath_opt: Option<String> = filename_opt
            .as_deref()
            .and_then(|name| crate::files::get_full_path(name));
        let fullpath = match fullpath_opt {
            Some(p) => p,
            None => return,
        };

        reload_positions_if_needed();

        let lineno = STATE.with(|s| {
            let st = s.borrow();
            st.openfile
                .as_ref()
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
                anchors: if anchors.is_empty() {
                    None
                } else {
                    Some(anchors)
                },
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
        let filename_opt: Option<String> =
            STATE.with(|s| s.borrow().openfile.as_ref().map(|f| f.filename.clone()));
        let fullpath_opt: Option<String> = filename_opt
            .as_deref()
            .and_then(|name| crate::files::get_full_path(name));
        let fullpath = match fullpath_opt {
            Some(p) => p,
            None => return,
        };

        reload_positions_if_needed();

        let record_opt = POSITIONS_REGISTER
            .with(|pr| pr.borrow().iter().find(|r| r.filename == fullpath).cloned());

        if let Some(record) = record_opt {
            if let Some(ref anchors) = record.anchors {
                restore_anchors(anchors);
            }
            crate::search::goto_line_and_column(record.linenumber, record.columnnumber, true);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn all_prompt_histories_navigate_to_and_from_the_bottom() {
            history_init();

            for kind in [
                HistoryKind::Search,
                HistoryKind::Replace,
                HistoryKind::Execute,
            ] {
                update_history(kind, "older", false);
                update_history(kind, "newer — 草稿", false);

                assert!(history_is_at_bottom(kind));
                assert_eq!(older_history_item(kind).as_deref(), Some("newer — 草稿"));
                assert_eq!(older_history_item(kind).as_deref(), Some("older"));
                assert_eq!(older_history_item(kind), None);
                assert_eq!(newer_history_item(kind).as_deref(), Some("newer — 草稿"));
                assert_eq!(newer_history_item(kind), None);
                assert!(history_is_at_bottom(kind));
            }
        }

        #[test]
        fn stale_history_cursor_is_clamped_before_navigation() {
            history_init();
            update_history(HistoryKind::Search, "latest", false);
            STATE.with(|state| state.borrow_mut().search_history_pos = usize::MAX);

            assert_eq!(
                older_history_item(HistoryKind::Search).as_deref(),
                Some("latest")
            );
        }

        #[test]
        fn position_stamp_detects_equal_metadata_rewrites() {
            let modified = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(42));
            let old_bytes = b"/tmp/a 10 20\n";
            let new_bytes = b"/tmp/b 10 20\n";
            assert_eq!(old_bytes.len(), new_bytes.len());

            let old_stamp = stamp_position_bytes(modified, old_bytes.len() as u64, old_bytes);
            let new_stamp = stamp_position_bytes(modified, new_bytes.len() as u64, new_bytes);

            assert_ne!(old_stamp, new_stamp);
        }

        #[test]
        fn parses_unix_position_record_with_anchors() {
            let record =
                parse_position_record(b"12 34 /tmp/file name.txt 9 17").expect("position record");

            assert_eq!(record.anchors.as_deref(), Some("12 34 "));
            assert_eq!(record.filename, "/tmp/file name.txt");
            assert_eq!(record.linenumber, 9);
            assert_eq!(record.columnnumber, 17);
        }

        #[test]
        fn parses_windows_drive_position_record() {
            let record = parse_position_record(br"7 C:\Users\me\file name.txt 23 5")
                .expect("position record");

            assert_eq!(record.anchors.as_deref(), Some("7 "));
            assert_eq!(record.filename, r"C:\Users\me\file name.txt");
            assert_eq!(record.linenumber, 23);
            assert_eq!(record.columnnumber, 5);
        }

        #[test]
        fn parses_windows_unc_position_record() {
            let record =
                parse_position_record(br"\\server\share\file.txt 3 4").expect("position record");

            assert_eq!(record.anchors, None);
            assert_eq!(record.filename, r"\\server\share\file.txt");
            assert_eq!(record.linenumber, 3);
            assert_eq!(record.columnnumber, 4);
        }

        #[test]
        fn rejects_malformed_position_record_numbers() {
            assert!(parse_position_record(b"/tmp/file nope 4").is_none());
        }
    }
} // mod inner

// Re-export everything from inner when the feature is enabled.
#[cfg(feature = "histories")]
pub use inner::*;
