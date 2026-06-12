#![allow(unused, non_snake_case, dead_code, non_camel_case_types, unpredictable_function_pointer_comparisons)]
use crate::definitions::*;
use crate::global::STATE;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// utils.c -- utility functions for GNU nano (Rust port)
// Copyright (C) 1999-2011, 2013-2026 Free Software Foundation, Inc.
// Copyright (C) 2016, 2017, 2019, 2020, 2026 Benno Schulenberg

/* C: void get_homedir(void)
 * Set global `homedir` to the user's home directory.
 * First try $HOME; if that is unset or we are root, consult the passwd database.
 * On Windows there is no $HOME or passwd database, so fall back to the
 * standard %USERPROFILE% (then %HOMEDRIVE%%HOMEPATH%) variables. */
pub fn get_homedir() {
    let already_set = STATE.with(|s| s.borrow().homedir.is_some());
    if already_set {
        return;
    }

    // Try $HOME first
    let mut homenv: Option<String> = std::env::var("HOME").ok().filter(|s| !s.is_empty());

    // When $HOME is unset, or we are root (euid == 0), try the passwd database
    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() };
        if homenv.is_none() || euid == 0 {
            // Safety: getpwuid returns a pointer valid until the next call;
            // we copy pw_dir immediately.
            let pw = unsafe { libc::getpwuid(euid) };
            if !pw.is_null() {
                let dir = unsafe { std::ffi::CStr::from_ptr((*pw).pw_dir) };
                if let Ok(s) = dir.to_str() {
                    if !s.is_empty() {
                        homenv = Some(s.to_string());
                    }
                }
            }
        }
    }

    // On Windows, $HOME is normally unset and there is no passwd database.
    // Resolve the home directory the way the platform expects: %USERPROFILE%
    // first, then the %HOMEDRIVE% + %HOMEPATH% pair.
    #[cfg(windows)]
    {
        if homenv.is_none() {
            homenv = std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty());
        }
        if homenv.is_none() {
            let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
            let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
            if let (Some(drive), Some(path)) = (drive, path) {
                homenv = Some(format!("{}{}", drive, path));
            }
        }
    }

    if let Some(home) = homenv {
        STATE.with(|s| {
            s.borrow_mut().homedir = Some(home);
        });
    }
}

/* C: const char *tail(const char *path)
 * Return the filename part of the given path (everything after the last '/'). */
pub fn tail(path: &str) -> &str {
    match path.rfind('/') {
        None => path,
        Some(pos) => &path[pos + 1..],
    }
}

/* C: char *concatenate(const char *path, const char *name)
 * Return a copy of the two given strings welded together. */
pub fn concatenate(path: &str, name: &str) -> String {
    let mut result = String::with_capacity(path.len() + name.len());
    result.push_str(path);
    result.push_str(name);
    result
}

/* C: int digits(ssize_t n)
 * Return the number of decimal digits that the given integer n takes up.
 * The C version always returns at least 2; we preserve that behaviour. */
pub fn digits(n: isize) -> i32 {
    if n < 100_000 {
        if n < 1_000 {
            if n < 100 {
                2
            } else {
                3
            }
        } else if n < 10_000 {
            4
        } else {
            5
        }
    } else if n < 10_000_000 {
        if n < 1_000_000 {
            6
        } else {
            7
        }
    } else if n < 100_000_000 {
        8
    } else {
        9
    }
}

/* C: bool parse_num(const char *string, ssize_t *result)
 * Read a decimal integer from the given string.
 * Returns Some(value) on success, None on failure. */
pub fn parse_num(s: &str) -> Option<isize> {
    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    // Reject strings with trailing non-numeric content (mimics strtol excess check)
    trimmed.parse::<isize>().ok()
}

/* C: bool parse_line_column(const char *string, ssize_t *line, ssize_t *column)
 * Read one number (or two separated by comma, period, or colon) from the string.
 * Returns (line, column); either or both may be None on failure.
 * The tuple second element is always None when there is only one number. */
pub fn parse_line_column(s: &str) -> (Option<isize>, Option<isize>) {
    // Skip leading spaces
    let s = s.trim_start_matches(' ');

    // Find separator: comma, period, or colon
    let sep_pos = s.find(|c| c == ',' || c == '.' || c == ':');

    match sep_pos {
        None => {
            // No separator — parse the whole string as line number
            (parse_num(s), None)
        }
        Some(pos) => {
            let after_sep = &s[pos + 1..];
            let col = parse_num(after_sep);

            if pos == 0 {
                // Separator at start: no line number given
                (None, col)
            } else {
                let line = parse_num(&s[..pos]);
                (line, col)
            }
        }
    }
}

/* C: void recode_NUL_to_LF(char *string, size_t length)
 * In the given byte slice, recode each embedded NUL (0x00) as a newline (0x0A). */
pub fn recode_NUL_to_LF(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        if *byte == 0 {
            *byte = b'\n';
        }
    }
}

/* C: size_t recode_LF_to_NUL(char *string)
 * In the given byte vector, recode each embedded newline (0x0A) as a NUL (0x00),
 * and return the number of bytes in the string up to (but not including) the
 * first NUL terminator, or the whole length if there is none. */
pub fn recode_LF_to_NUL(buf: &mut Vec<u8>) -> usize {
    let mut count = 0;
    for byte in buf.iter_mut() {
        if *byte == 0 {
            break;
        }
        if *byte == b'\n' {
            *byte = 0;
        }
        count += 1;
    }
    count
}

/* C: void free_chararray(char **array, size_t len)
 * Free the memory of the given array.  In Rust, just drop it. */
#[cfg(any(not(feature = "tiny"), feature = "tabcomp", feature = "browser"))]
pub fn free_chararray(array: Vec<String>) {
    drop(array);
}

/* C: bool is_separate_word(size_t position, size_t length, const char *text)
 * Return TRUE when the word starting at position (of the given length) in text
 * is a separate word — not part of a longer word. */
#[cfg(feature = "speller")]
pub fn is_separate_word(position: usize, length: usize, text: &str) -> bool {
    use crate::chars::is_alpha_char;
    let before_pos = crate::chars::step_left(text, position);
    let before = &text[before_pos..];
    let after = &text[position + length..];

    let left_ok = position == 0 || !is_alpha_char(before);
    let right_ok = after.is_empty() || !is_alpha_char(after);

    left_ok && right_ok
}

/* C: const char *strstrwrapper(const char *haystack, const char *needle, const char *start)
 * Search for needle in haystack, respecting the USE_REGEXP, BACKWARDS_SEARCH, and
 * CASE_SENSITIVE flags stored in global state.  Returns the byte offset of the
 * match within `haystack`, or None. */
pub fn strstrwrapper<'a>(
    haystack: &'a str,
    needle: &str,
    start_offset: usize,
) -> Option<usize> {
    let use_regexp = STATE.with(|s| s.borrow().flag_isset(crate::definitions::USE_REGEXP));
    let backwards = STATE.with(|s| s.borrow().flag_isset(crate::definitions::BACKWARDS_SEARCH));
    let case_sensitive = STATE.with(|s| s.borrow().flag_isset(crate::definitions::CASE_SENSITIVE));

    if use_regexp {
        // Regex searching is handled via the compiled search_regexp stored in STATE.
        // We delegate to the state's compiled regex.
        let found = STATE.with(|s| {
            let st = s.borrow();
            if let Some(ref re) = st.search_regexp {
                if backwards {
                    // Find last match that starts at or before start_offset
                    let mut last: Option<usize> = None;
                    let mut search_from = 0;
                    while let Some(m) = re.find(&haystack[search_from..]) {
                        let abs = search_from + m.start();
                        if abs > start_offset {
                            break;
                        }
                        last = Some(abs);
                        let next = search_from + m.start() + crate::chars::char_length(&haystack[search_from + m.start()..]);
                        if next >= haystack.len() {
                            break;
                        }
                        search_from = next;
                    }
                    last
                } else {
                    re.find(&haystack[start_offset..]).map(|m| start_offset + m.start())
                }
            } else {
                None
            }
        });
        return found;
    }

    if case_sensitive {
        if backwards {
            revstrstr(haystack, needle, start_offset)
        } else {
            haystack[start_offset..].find(needle).map(|p| start_offset + p)
        }
    } else if backwards {
        mbrevstrcasestr(haystack, needle, start_offset)
    } else {
        crate::chars::mbstrcasestr(&haystack[start_offset..], needle)
            .map(|p| start_offset + p)
    }
}

/* Helper: reverse strstr — find last occurrence of needle in haystack that
 * starts at or before start_offset. */
pub fn revstrstr(haystack: &str, needle: &str, start_offset: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start_offset);
    }
    let needle_len = needle.len();
    let hay_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    // Start scanning from `start_offset`, going backwards
    let max_start = start_offset.min(haystack.len().saturating_sub(needle_len));
    // We scan from max_start down to 0
    let mut pos = max_start as isize;
    while pos >= 0 {
        let p = pos as usize;
        if p + needle_len <= hay_bytes.len() && &hay_bytes[p..p + needle_len] == needle_bytes {
            return Some(p);
        }
        pos -= 1;
    }
    None
}

/* Helper: reverse case-insensitive strstr for multibyte strings. */
pub fn mbrevstrcasestr(haystack: &str, needle: &str, start_offset: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start_offset);
    }
    let needle_chars: usize = crate::chars::mbstrlen(needle);
    let tail_chars = crate::chars::mbstrlen(&haystack[start_offset..]);

    // Compute starting position for backwards scan
    let mut ptr_offset = if tail_chars < needle_chars {
        // Go back enough characters
        let diff = needle_chars - tail_chars;
        let mut off = start_offset;
        for _ in 0..diff {
            if off == 0 {
                break;
            }
            off = crate::chars::step_left(haystack, off);
        }
        off
    } else {
        start_offset
    };

    loop {
        if crate::chars::mbstrncasecmp(&haystack[ptr_offset..], needle, needle_chars) == 0 {
            return Some(ptr_offset);
        }
        if ptr_offset == 0 {
            return None;
        }
        ptr_offset = crate::chars::step_left(haystack, ptr_offset);
    }
}

/* C: void *nmalloc(size_t howmuch)
 * Allocate `howmuch` bytes.  In Rust, allocation is managed automatically;
 * this function is provided for API compatibility and returns a Vec<u8>. */
pub fn nmalloc(howmuch: usize) -> Vec<u8> {
    vec![0u8; howmuch]
}

/* C: void *nrealloc(void *section, size_t howmuch)
 * Resize a Vec<u8> to `howmuch` bytes. */
pub fn nrealloc(mut v: Vec<u8>, howmuch: usize) -> Vec<u8> {
    v.resize(howmuch, 0u8);
    v
}

/* C: char *mallocstrcpy(char *dest, const char *src)
 * Overwrite `dest` with a copy of `src`. */
pub fn mallocstrcpy(dest: &mut String, src: &str) {
    dest.clear();
    dest.push_str(src);
}

/* C: char *measured_copy(const char *string, size_t count)
 * Return an allocated copy of the first `count` bytes of `string`. */
pub fn measured_copy(s: &str, count: usize) -> String {
    // count is in bytes, not characters
    let safe_count = count.min(s.len());
    // Make sure we end on a char boundary
    let boundary = (0..=safe_count)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    s[..boundary].to_string()
}

/* C: char *copy_of(const char *string)
 * Return an allocated copy of the given string. */
pub fn copy_of(s: &str) -> String {
    s.to_string()
}

/* C: char *free_and_assign(char *dest, char *src)
 * Free `dest` and return `src`.  In Rust: just return `src` (drop is automatic). */
pub fn free_and_assign(_dest: String, src: String) -> String {
    src
}

/* C: size_t get_page_start(size_t column)
 * Return the column number of the first character displayed in the edit window
 * when the cursor is at the given column.  Used for horizontal scrolling. */
pub fn get_page_start(column: usize) -> usize {
    #[cfg(not(feature = "tiny"))]
    {
        let (united, brink, editwincols, jumpy, softwrap, cushion) = STATE.with(|s| {
            let st = s.borrow();
            (
                st.united_sidescroll,
                st.openfile_brink(),
                st.editwincols.max(0) as usize,
                st.flag_isset(crate::definitions::JUMPY_SCROLLING),
                st.flag_isset(crate::definitions::SOFTWRAP),
                crate::definitions::CUSHION,
            )
        });

        if united {
            if column < cushion {
                return 0;
            } else if column < brink + cushion {
                if jumpy {
                    return if column > editwincols / 2 { column - editwincols / 2 } else { 0 };
                } else {
                    return column - cushion;
                }
            } else if column > brink + editwincols.saturating_sub(cushion + 1) {
                let pad = if jumpy { editwincols / 2 } else { cushion };
                return column - editwincols + pad + 1;
            } else {
                return brink;
            }
        }

        let softwrap_flag = softwrap;
        let editwincols_val = editwincols;

        let ecols = editwincols_val.max(2) as usize; // guard against <=0 editwincols
        if column == 0 || column + 2 < ecols || softwrap_flag {
            return 0;
        } else if ecols > 8 {
            return column.saturating_sub(6) - column.saturating_sub(6) % (ecols - 8);
        } else {
            return column.saturating_sub(ecols.saturating_sub(2));
        }
    }

    #[cfg(feature = "tiny")]
    {
        let (editwincols, softwrap) = STATE.with(|s| {
            let st = s.borrow();
            (st.editwincols.max(0) as usize, st.flag_isset(crate::definitions::SOFTWRAP))
        });
        if column == 0 || column + 2 < editwincols || softwrap {
            0
        } else if editwincols > 8 {
            column - 6 - (column - 6) % (editwincols - 8)
        } else {
            column - (editwincols - 2)
        }
    }
}

/* C: size_t xplustabs(void)
 * Return the zero-based column position of the cursor in the current line. */
pub fn xplustabs() -> usize {
    STATE.with(|s| {
        let st = s.borrow();
        let current_x = st.current_x();
        let data = st.current_line_data();
        wideness(&data, current_x)
    })
}

/* C: size_t actual_x(const char *text, size_t column)
 * Return the byte index in `text` of the character that, when displayed,
 * will not overshoot the given column. */
pub fn actual_x(text: &str, column: usize) -> usize {
    let mut width: usize = 0;
    let mut pos: usize = 0;
    let bytes = text.as_bytes();

    while pos < text.len() {
        let charlen = crate::chars::advance_over(&text[pos..], &mut width);
        if width > column {
            break;
        }
        pos += charlen;
    }

    pos
}

/* C: size_t wideness(const char *text, size_t maxlen)
 * How many columns wide are the first `maxlen` bytes of `text`? */
pub fn wideness(text: &str, maxlen: usize) -> usize {
    if maxlen == 0 {
        return 0;
    }

    let mut width: usize = 0;
    let mut remaining = maxlen;
    let mut pos: usize = 0;

    while pos < text.len() {
        let charlen = crate::chars::advance_over(&text[pos..], &mut width);
        if remaining <= charlen {
            break;
        }
        remaining -= charlen;
        pos += charlen;
    }

    width
}

/* C: size_t breadth(const char *text)
 * Return the number of columns that the given text occupies. */
pub fn breadth(text: &str) -> usize {
    let mut span: usize = 0;
    let mut pos: usize = 0;

    while pos < text.len() {
        let charlen = crate::chars::advance_over(&text[pos..], &mut span);
        pos += charlen;
    }

    span
}

/* C: void new_magicline(void)
 * Append a new empty magic line to the end of the buffer. */
pub fn new_magicline() {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        st.append_magicline();
    });
}

/* C: void remove_magicline(void)
 * Remove the magic line from the end of the buffer if there is one and
 * it is not the only line. */
#[cfg(any(not(feature = "tiny"), feature = "help"))]
pub fn remove_magicline() {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        st.remove_magicline_if_empty();
    });
}

/* C: bool mark_is_before_cursor(void)
 * Return TRUE when the mark is before or at the cursor. */
#[cfg(not(feature = "tiny"))]
pub fn mark_is_before_cursor() -> bool {
    STATE.with(|s| {
        let st = s.borrow();
        st.mark_is_before_cursor()
    })
}

/* C: void get_region(linestruct **top, ..., linestruct **bot, ...)
 * Return the start and end coordinates of the marked region as byte offsets
 * and line references (encoded as line numbers here for portability). */
#[cfg(not(feature = "tiny"))]
pub fn get_region() -> (usize, usize, usize, usize) {
    // Returns (top_lineno, top_x, bot_lineno, bot_x)
    STATE.with(|s| {
        let st = s.borrow();
        st.get_region_coords()
    })
}

/* C: void get_range(linestruct **top, linestruct **bot)
 * Get the set of lines to work on — either just the current line or the
 * first-to-last lines of the marked region (excluding the last line if the
 * cursor is at its start). */
#[cfg(not(feature = "tiny"))]
pub fn get_range() -> (usize, usize) {
    // Returns (top_lineno, bot_lineno)
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        st.get_range_linenos()
    })
}

/* C: linestruct *line_from_number(ssize_t number)
 * Return the line that has the given line number, walking from the
 * current line in whichever direction is closer.  Returns None when
 * the number is out of range (where C would walk off the list). */
pub fn line_from_number(number: isize) -> Option<LinePtr> {
    let mut line = STATE.with(|s| s.borrow().openfile.as_ref().and_then(|f| f.current.clone()))?;

    if line.borrow().lineno > number {
        while line.borrow().lineno != number {
            let prev = line.borrow().prev.as_ref().and_then(|w| w.upgrade());
            line = prev?;
        }
    } else {
        while line.borrow().lineno != number {
            let next = line.borrow().next.clone();
            line = next?;
        }
    }

    Some(line)
}

/* C: size_t number_of_characters_in(const linestruct *begin, const linestruct *end)
 * Count the number of characters from `begin` to `end` (inclusive),
 * adding one for each newline between lines but not counting the final newline. */
pub fn number_of_characters_in(lines: &[String], begin: usize, end: usize) -> usize {
    // `lines` is a slice of line data strings; begin/end are indices into that slice.
    let mut count: usize = 0;
    for i in begin..=end {
        count += crate::chars::mbstrlen(&lines[i]) + 1;
    }
    // Do not count the final newline
    count.saturating_sub(1)
}
