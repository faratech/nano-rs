#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
use crate::definitions::*;
use crate::global::STATE;
use unicode_width::UnicodeWidthChar;

// chars.c -- character-classification and multibyte-string functions for GNU nano (Rust port)
// Copyright (C) 2001-2011, 2013-2026 Free Software Foundation, Inc.
// Copyright (C) 2016-2021 Benno Schulenberg

// Cached copy of AppState.using_utf8.  The flag is decided once during
// startup (nano.rs) and never changes afterwards; caching it here avoids
// a thread-local STATE lookup on every character scanned in the
// width/stepping hot paths below.
static USING_UTF8: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record the startup-determined UTF-8 mode (also kept in AppState.using_utf8).
#[inline]
pub fn remember_utf8(on: bool) {
    USING_UTF8.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the locale UTF-8?  Cached equivalent of `STATE.using_utf8`.
#[inline]
pub fn using_utf8() -> bool {
    USING_UTF8.load(std::sync::atomic::Ordering::Relaxed)
}

// ── Character classification ─────────────────────────────────────────────────

/* C: bool is_alpha_char(const char *c)
 * Return true when the character at the start of `c` is some kind of letter. */
#[cfg(feature = "speller")]
pub fn is_alpha_char(c: &str) -> bool {
    match c.chars().next() {
        Some(ch) => ch.is_alphabetic(),
        None => false,
    }
}

/* C: bool is_alnum_char(const char *c)
 * Return true when the character at the start of `c` is a letter or a digit. */
pub fn is_alnum_char(c: &str) -> bool {
    match c.chars().next() {
        Some(ch) => ch.is_alphanumeric(),
        None => false,
    }
}

/* C: bool is_blank_char(const char *c)
 * Return true when the character at the start of `c` is a space, tab, or other
 * Unicode whitespace (but NOT a newline — mirrors iswblank behaviour). */
pub fn is_blank_char(c: &str) -> bool {
    let bytes = c.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // Fast path for pure-ASCII bytes (signed-char check mirrors the C code)
    let first = bytes[0];
    if first < 0x80 {
        return first == b' ' || first == b'\t';
    }
    // Multi-byte path: decode the first char and test Unicode blank category.
    match c.chars().next() {
        // iswblank matches horizontal whitespace (space / tab and Unicode equivalents)
        Some(ch) => {
            matches!(
                ch,
                '\u{0009}' // CHARACTER TABULATION
                | '\u{0020}' // SPACE
                | '\u{00A0}' // NO-BREAK SPACE
                | '\u{1680}' // OGHAM SPACE MARK
                | '\u{2000}'..='\u{200A}' // various typographic spaces
                | '\u{202F}' // NARROW NO-BREAK SPACE
                | '\u{205F}' // MEDIUM MATHEMATICAL SPACE
                | '\u{3000}' // IDEOGRAPHIC SPACE
            )
        }
        None => false,
    }
}

/* C: bool is_cntrl_char(const char *c)
 * Return true when the character at the start of `c` is a control character.
 * Mirrors the C implementation's byte-level checks (including UTF-8 upper
 * control codes U+0080..U+009F). */
pub fn is_cntrl_char(c: &str) -> bool {
    let bytes = c.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    let using_utf8 = using_utf8();

    if using_utf8 {
        // C: (c[0] & 0xE0) == 0  → bytes 0x00-0x1F
        // C: c[0] == DEL_CODE    → 0x7F
        // C: (signed char)c[0] == -62 && (signed char)c[1] < -96
        //     → 0xC2 followed by 0x80..0x9F  (U+0080..U+009F, upper C1 controls)
        let b0 = bytes[0];
        if (b0 & 0xE0) == 0 || b0 == DEL_CODE as u8 {
            return true;
        }
        if b0 == 0xC2 {
            if let Some(&b1) = bytes.get(1) {
                // (signed char)b1 < -96  →  b1 as i8 < -96  →  b1 > 0x9F and b1 < 0x80 false,
                // actually (signed char)b1 < -96 means (b1 as i8) < -96
                // -96 as u8 is 0xA0, so b1 as i8 < -96 means b1 in 0x80..0x9F
                if (b1 as i8) < -96_i8 {
                    return true;
                }
            }
        }
        false
    } else {
        // Non-UTF-8: (c & 0x60) == 0 || c == DEL_CODE
        let b = bytes[0];
        (b & 0x60) == 0 || b == DEL_CODE as u8
    }
}

/* C: bool is_punct_char(const char *c)
 * Return true when the character at the start of `c` is a punctuation character. */
pub fn is_punct_char(c: &str) -> bool {
    match c.chars().next() {
        Some(ch) => ch.is_ascii_punctuation() || unicode_is_punct(ch),
        None => false,
    }
}

/// Helper: Unicode punctuation check (mirrors glibc's iswpunct).
///
/// glibc's iswpunct returns true for any GRAPHIC (printable) character that is
/// neither alphanumeric nor whitespace — which on glibc includes symbols.  The
/// previous implementation blanket-classified essentially every non-alphanumeric
/// code point (including non-graphic format/ignorable characters) as punctuation.
fn unicode_is_punct(ch: char) -> bool {
    if ch.is_alphanumeric() || ch.is_whitespace() || ch.is_control() {
        return false;
    }
    // Exclude format / Default_Ignorable code points, which are not graphic and
    // must not count as punctuation (they would corrupt word-boundary detection).
    !is_ignorable_format(ch)
}

/// Well-known Cf/format and Default_Ignorable code points that are not graphic.
fn is_ignorable_format(ch: char) -> bool {
    matches!(ch as u32,
        0x00AD                |   // soft hyphen
        0x200B..=0x200F       |   // zero-width space/joiners, LRM/RLM
        0x202A..=0x202E       |   // bidi embedding/override
        0x2060..=0x2064       |   // word joiner, invisible operators
        0x206A..=0x206F       |   // deprecated format controls
        0xFEFF                |   // BOM / zero-width no-break space
        0xFFF9..=0xFFFB       |   // interlinear annotation anchors
        0x1D173..=0x1D17A     |   // musical symbol format controls
        0xE0000..=0xE007F         // tag characters
    )
}

/* C: bool is_word_char(const char *c, bool allow_punct)
 * Return true when the character is word-forming: alphanumeric, in `word_chars`,
 * or (when `allow_punct` is true) punctuation. */
pub fn is_word_char(c: &str, allow_punct: bool) -> bool {
    if c.is_empty() {
        return false;
    }

    if is_alnum_char(c) {
        return true;
    }

    if allow_punct && is_punct_char(c) {
        return true;
    }

    // Check user-defined word_chars list
    let word_chars = STATE.with(|s| s.borrow().word_chars.clone());
    if let Some(ref wc) = word_chars {
        if !wc.is_empty() {
            // Collect the first multibyte char from `c` as a &str slice
            let ch_len = char_length(c);
            let symbol = &c[..ch_len];
            return wc.contains(symbol);
        }
    }

    false
}

// ── Control-character representation ─────────────────────────────────────────

/* C: char control_rep(const signed char c)
 * Return the visible representation of a (single-byte) control character. */
pub fn control_rep(c: i8) -> char {
    if c == DEL_CODE as i8 {
        '?'
    } else if c == -97_i8 {
        // 0x9F — represented as '='
        '='
    } else if c < 0 {
        // Upper C1 control codes: add 224 (0xE0) to get a printable Latin char
        char::from_u32((c as i32 + 224) as u32).unwrap_or('?')
    } else {
        // C0 control codes: add 64 ('@' = 0x40)
        char::from_u32((c as i32 + 64) as u32).unwrap_or('?')
    }
}

/* C: char control_mbrep(const char *c, bool isdata)
 * Return the visible representation of a multibyte control character. */
pub fn control_mbrep(c: &str, isdata: bool) -> char {
    let bytes = c.as_bytes();
    if bytes.is_empty() {
        return '?';
    }

    // An embedded newline is an encoded NUL when it is data
    if bytes[0] == b'\n' {
        let as_an_at = STATE.with(|s| s.borrow().as_an_at);
        if isdata || as_an_at {
            return '@';
        }
    }

    let using_utf8 = using_utf8();

    if using_utf8 {
        if (bytes[0] as u8) < 128 {
            control_rep(bytes[0] as i8)
        } else if bytes.len() > 1 {
            control_rep(bytes[1] as i8)
        } else {
            '?'
        }
    } else {
        control_rep(bytes[0] as i8)
    }
}

// ── UTF-8 / multibyte decoding ────────────────────────────────────────────────

/* C: int mbtowide(wchar_t *wc, const char *c)
 * Convert the multibyte sequence at the start of `c` to a Rust char.
 * Returns Ok((char, byte_length)) on success, or Err(()) for an invalid sequence.
 * This mirrors nano's custom UTF-8 decoder exactly. */
pub fn mbtowide(c: &str) -> Result<(char, usize), ()> {
    let bytes = c.as_bytes();
    if bytes.is_empty() {
        return Err(());
    }

    let using_utf8 = using_utf8();

    if (bytes[0] as i8) < 0 && using_utf8 {
        let v1 = bytes[0];
        if bytes.len() < 2 {
            return Err(());
        }
        let v2 = bytes[1] ^ 0x80;

        if v2 > 0x3F || v1 < 0xC2 {
            return Err(());
        }

        if v1 < 0xE0 {
            let codepoint = (((v1 & 0x1F) as u32) << 6) | (v2 as u32);
            return char::from_u32(codepoint).map(|ch| (ch, 2)).ok_or(());
        }

        if bytes.len() < 3 {
            return Err(());
        }
        let v3 = bytes[2] ^ 0x80;
        if v3 > 0x3F {
            return Err(());
        }

        if v1 < 0xF0 {
            if (v1 > 0xE0 || v2 >= 0x20) && (v1 != 0xED || v2 < 0x20) {
                let codepoint = (((v1 & 0x0F) as u32) << 12)
                    | ((v2 as u32) << 6)
                    | (v3 as u32);
                return char::from_u32(codepoint).map(|ch| (ch, 3)).ok_or(());
            } else {
                return Err(());
            }
        }

        if bytes.len() < 4 {
            return Err(());
        }
        let v4 = bytes[3] ^ 0x80;
        if v4 > 0x3F || v1 > 0xF4 {
            return Err(());
        }

        if (v1 > 0xF0 || v2 >= 0x10) && (v1 != 0xF4 || v2 < 0x10) {
            let codepoint = (((v1 & 0x07) as u32) << 18)
                | ((v2 as u32) << 12)
                | ((v3 as u32) << 6)
                | (v4 as u32);
            return char::from_u32(codepoint).map(|ch| (ch, 4)).ok_or(());
        } else {
            return Err(());
        }
    }

    // ASCII or non-UTF-8 byte
    let codepoint = bytes[0] as u32;
    char::from_u32(codepoint).map(|ch| (ch, 1)).ok_or(())
}

/* C: bool is_doublewidth(const char *ch)
 * Return true when the character at the start of `ch` occupies two terminal columns. */
#[cfg(feature = "utf8")]
pub fn is_doublewidth(ch: &str) -> bool {
    let bytes = ch.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // Only from U+1100 can code points have double width; U+1100 encodes as 0xE1..
    if bytes[0] < 0xE1 {
        return false;
    }
    let using_utf8 = using_utf8();
    if !using_utf8 {
        return false;
    }
    match mbtowide(ch) {
        Ok((wc, _)) => UnicodeWidthChar::width(wc) == Some(2),
        Err(_) => false,
    }
}

/* C: bool is_zerowidth(const char *ch)
 * Return true when the character at the start of `ch` occupies zero terminal columns. */
#[cfg(feature = "utf8")]
pub fn is_zerowidth(ch: &str) -> bool {
    let bytes = ch.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // Only from U+0300 can code points have zero width; U+0300 encodes as 0xCC..
    if bytes[0] < 0xCC {
        return false;
    }
    let using_utf8 = using_utf8();
    if !using_utf8 {
        return false;
    }
    match mbtowide(ch) {
        Ok((wc, _)) => {
            // OpenBSD workaround: private-use area U+F0000+ always returns false.
            #[cfg(target_os = "openbsd")]
            if wc as u32 >= 0xF0000 {
                return false;
            }
            UnicodeWidthChar::width(wc) == Some(0)
        }
        Err(_) => false,
    }
}

// ── Byte/character length ─────────────────────────────────────────────────────

/* C: int char_length(const char *pointer)
 * Return the number of bytes in the character that starts at `pointer`.
 * Mirrors nano's custom validation logic exactly. */
pub fn char_length(s: &str) -> usize {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 1; // safety fallback
    }

    let using_utf8 = using_utf8();

    if using_utf8 && bytes[0] > 0xC1 {
        let c1 = bytes[0];

        if bytes.len() < 2 {
            return 1;
        }
        let c2 = bytes[1];
        if (c2 ^ 0x80) > 0x3F {
            return 1;
        }
        if c1 < 0xE0 {
            return 2;
        }

        if bytes.len() < 3 {
            return 1;
        }
        if (bytes[2] ^ 0x80) > 0x3F {
            return 1;
        }
        if c1 < 0xF0 {
            if (c1 > 0xE0 || c2 >= 0xA0) && (c1 != 0xED || c2 < 0xA0) {
                return 3;
            } else {
                return 1;
            }
        }

        if bytes.len() < 4 {
            return 1;
        }
        if (bytes[3] ^ 0x80) > 0x3F {
            return 1;
        }
        if c1 > 0xF4 {
            return 1;
        }
        if (c1 > 0xF0 || c2 >= 0x90) && (c1 != 0xF4 || c2 < 0x90) {
            return 4;
        }
    }

    1
}

/* C: size_t mbstrlen(const char *pointer)
 * Return the number of (multibyte) characters in the given string. */
pub fn mbstrlen(s: &str) -> usize {
    let bytes = s.as_bytes();
    // nano treats an embedded NUL as the end of the string (C string semantics).
    let end = match bytes.iter().position(|&b| b == 0) {
        Some(p) => p,
        None => bytes.len(),
    };
    if !using_utf8() {
        // Single-byte locale: exactly one character per byte.
        return end;
    }
    // UTF-8: the character count is the number of bytes that are NOT continuation
    // bytes (0x80..=0xBF). Because a Rust &str is always well-formed UTF-8, this is
    // identical to walking char_length() per character, but in one branch-light,
    // auto-vectorizable pass instead of a function call per character — this is the
    // dominant cost in count_chars_in_chain / totsize on large files.
    bytes[..end].iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

/* C: int collect_char(const char *string, char *thechar)
 * Return the length (in bytes) of the character at the start of `string`,
 * and return a copy of that character as a String. */
pub fn collect_char(s: &str) -> (usize, String) {
    let charlen = char_length(s);
    let safe_end = charlen.min(s.len());
    (charlen, s[..safe_end].to_string())
}

/* C: int advance_over(const char *string, size_t *column)
 * Return the number of bytes in the character at the start of `string`,
 * and add that character's display width to `*column`. */
pub fn advance_over(s: &str, column: &mut usize) -> usize {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 1;
    }

    let using_utf8 = using_utf8();

    if (bytes[0] as i8) < 0 && using_utf8 {
        // UTF-8 upper control code: two bytes, two columns
        if bytes[0] == 0xC2 {
            if let Some(&b1) = bytes.get(1) {
                if (b1 as i8) < -96_i8 {
                    *column += 2;
                    return 2;
                }
            }
        }

        // General multi-byte character
        match mbtowide(s) {
            Err(_) => {
                *column += 1;
                return 1;
            }
            Ok((wc, charlen)) => {
                let width = UnicodeWidthChar::width(wc);
                #[cfg(target_os = "openbsd")]
                let w = if width.is_none() || wc as u32 >= 0xF0000 {
                    1usize
                } else {
                    width.unwrap_or(1)
                };
                #[cfg(not(target_os = "openbsd"))]
                let w = width.unwrap_or(1);

                *column += w;
                return charlen;
            }
        }
    }

    // Single-byte character
    let b = bytes[0];
    if b < 0x20 {
        if b == b'\t' {
            // Tabs are rare: read tabsize only when one is actually met.
            let ts = STATE.with(|s| s.borrow().tabsize) as usize;
            *column += ts - *column % ts;
        } else {
            *column += 2; // C0 control characters shown as ^X (2 columns)
        }
    } else if b > 0x7E && b < 0xA0 {
        // Non-printable high bytes (0x7F and 0x80..0x9F range in non-UTF-8)
        *column += 2;
    } else {
        *column += 1;
    }

    1
}

// ── Stepping through a multibyte string ──────────────────────────────────────

/* C: size_t step_left(const char *buf, size_t pos)
 * Return the byte index of the start of the multibyte character immediately
 * before position `pos` in `buf`. */
pub fn step_left(buf: &str, pos: usize) -> usize {
    let using_utf8 = using_utf8();

    if using_utf8 {
        if pos == 0 {
            return 0;
        }

        let bytes = buf.as_bytes();
        let _start_search = if pos < 4 { 0 } else { pos - 4 };

        // Probe backwards for a valid UTF-8 starter byte
        let before = if pos >= 1 && is_utf8_starter(bytes[pos - 1]) {
            pos - 1
        } else if pos >= 2 && is_utf8_starter(bytes[pos - 2]) {
            pos - 2
        } else if pos >= 3 && is_utf8_starter(bytes[pos - 3]) {
            pos - 3
        } else if pos >= 4 && is_utf8_starter(bytes[pos - 4]) {
            pos - 4
        } else {
            pos - 1
        };

        // Walk forward from `before` until we reach `pos`, tracking char lengths
        let mut cur = before;
        let mut prev = before;
        while cur < pos {
            prev = cur;
            cur += char_length(&buf[cur..]);
        }
        prev
    } else {
        if pos == 0 { 0 } else { pos - 1 }
    }
}

/// Return true when the byte could be the first byte of a UTF-8 sequence
/// (not a continuation byte: i.e., not in range 0x80..0xBF).
#[inline]
fn is_utf8_starter(b: u8) -> bool {
    // Continuation bytes have the form 10xxxxxx (0x80..0xBF)
    // A starter byte is anything outside that range.
    (b as i8) > -65_i8  // −65 as i8 is 0xBF; bytes > 0xBF or < 0x80 are starters
}

/* C: size_t step_right(const char *buf, size_t pos)
 * Return the byte index of the start of the multibyte character immediately
 * after position `pos` in `buf`. */
pub fn step_right(buf: &str, pos: usize) -> usize {
    pos + char_length(&buf[pos..])
}

// ── Case-insensitive multibyte comparisons ────────────────────────────────────

/* C: int mbstrcasecmp(const char *s1, const char *s2)
 * Case-insensitive string comparison for multibyte strings.
 * Returns 0 if equal, negative if s1 < s2, positive if s1 > s2. */
pub fn mbstrcasecmp(s1: &str, s2: &str) -> i32 {
    mbstrncasecmp(s1, s2, usize::MAX)
}

/* C: int mbstrncasecmp(const char *s1, const char *s2, size_t n)
 * Case-insensitive comparison of up to `n` characters of two multibyte strings. */
pub fn mbstrncasecmp(s1: &str, s2: &str, n: usize) -> i32 {
    let using_utf8 = using_utf8();

    if using_utf8 {
        let mut p1 = 0usize;
        let mut p2 = 0usize;
        let b1 = s1.as_bytes();
        let b2 = s2.as_bytes();
        let mut remaining = n;

        while p1 < b1.len() && b1[p1] != 0 && p2 < b2.len() && b2[p2] != 0 && remaining > 0 {
            let byte1 = b1[p1] as i8;
            let byte2 = b2[p2] as i8;

            // Fast ASCII path (both chars are ASCII)
            if byte1 >= 0 && byte2 >= 0 {
                let u1 = b1[p1];
                let u2 = b2[p2];
                let lower1 = if u1.is_ascii_uppercase() { u1 | 0x20 } else { u1 };
                let lower2 = if u2.is_ascii_uppercase() { u2 | 0x20 } else { u2 };
                if lower1 != lower2 {
                    return lower1 as i32 - lower2 as i32;
                }
                p1 += 1;
                p2 += 1;
                remaining -= 1;
                continue;
            }

            // Multi-byte path
            let res1 = mbtowide(&s1[p1..]);
            let res2 = mbtowide(&s2[p2..]);

            match (res1, res2) {
                (Ok((wc1, len1)), Ok((wc2, len2))) => {
                    let lower1 = wc1.to_lowercase().next().unwrap_or(wc1);
                    let lower2 = wc2.to_lowercase().next().unwrap_or(wc2);
                    if lower1 != lower2 {
                        return lower1 as i32 - lower2 as i32;
                    }
                    p1 += len1;
                    p2 += len2;
                }
                (Err(_), Err(_)) => {
                    if b1[p1] != b2[p2] {
                        return b1[p1] as i32 - b2[p2] as i32;
                    }
                    p1 += 1;
                    p2 += 1;
                }
                (Err(_), Ok(_)) => return 1,
                (Ok(_), Err(_)) => return -1,
            }
            remaining -= 1;
        }

        if remaining > 0 {
            let c1 = if p1 < b1.len() { b1[p1] } else { 0 };
            let c2 = if p2 < b2.len() { b2[p2] } else { 0 };
            c1 as i32 - c2 as i32
        } else {
            0
        }
    } else {
        // Non-UTF-8: byte-level case-insensitive comparison
        let bytes1 = s1.as_bytes();
        let bytes2 = s2.as_bytes();
        for i in 0..n {
            let b1 = bytes1.get(i).copied().unwrap_or(0);
            let b2 = bytes2.get(i).copied().unwrap_or(0);
            let l1 = b1.to_ascii_lowercase();
            let l2 = b2.to_ascii_lowercase();
            if l1 != l2 {
                return l1 as i32 - l2 as i32;
            }
            if b1 == 0 {
                break;
            }
        }
        0
    }
}

/* C: char *mbstrcasestr(const char *haystack, const char *needle)
 * Case-insensitive substring search for multibyte strings.
 * Returns the byte offset of the first match in `haystack`, or None. */
pub fn mbstrcasestr(haystack: &str, needle: &str) -> Option<usize> {
    let using_utf8 = using_utf8();

    if using_utf8 {
        let needle_chars = mbstrlen(needle);
        let mut pos = 0;
        let bytes = haystack.as_bytes();
        while pos < bytes.len() && bytes[pos] != 0 {
            if mbstrncasecmp(&haystack[pos..], needle, needle_chars) == 0 {
                return Some(pos);
            }
            pos += char_length(&haystack[pos..]);
        }
        None
    } else {
        // Non-UTF-8: ASCII case-insensitive search
        let hay_lower: String = haystack.to_ascii_lowercase();
        let needle_lower: String = needle.to_ascii_lowercase();
        hay_lower.find(&needle_lower)
    }
}

/* C: char *revstrstr(const char *haystack, const char *needle, const char *pointer)
 * Reverse strstr — find last occurrence of `needle` in `haystack` that starts
 * at or before `start_offset`. */
pub fn revstrstr(haystack: &str, needle: &str, start_offset: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start_offset);
    }
    let needle_len = needle.len();
    let tail_len = haystack.len().saturating_sub(start_offset);

    let mut ptr = if tail_len < needle_len {
        start_offset.saturating_sub(needle_len - tail_len)
    } else {
        start_offset
    };

    let hay_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    loop {
        if ptr + needle_len <= hay_bytes.len()
            && &hay_bytes[ptr..ptr + needle_len] == needle_bytes
        {
            return Some(ptr);
        }
        if ptr == 0 {
            break;
        }
        ptr -= 1;
    }
    None
}

/* C: char *revstrcasestr(const char *haystack, const char *needle, const char *pointer)
 * Reverse case-insensitive strstr (single-byte version). */
pub fn revstrcasestr(haystack: &str, needle: &str, start_offset: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start_offset);
    }
    let needle_len = needle.len();
    let tail_len = haystack.len().saturating_sub(start_offset);

    let mut ptr = if tail_len < needle_len {
        start_offset.saturating_sub(needle_len - tail_len)
    } else {
        start_offset
    };

    let hay_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    loop {
        if ptr + needle_len <= hay_bytes.len() {
            let hay_slice = &hay_bytes[ptr..ptr + needle_len];
            let matches = hay_slice
                .iter()
                .zip(needle_bytes.iter())
                .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase());
            if matches {
                return Some(ptr);
            }
        }
        if ptr == 0 {
            break;
        }
        ptr -= 1;
    }
    None
}

/* C: char *mbrevstrcasestr(const char *haystack, const char *needle, const char *pointer)
 * Reverse case-insensitive search for multibyte strings, starting at `start_offset`. */
pub fn mbrevstrcasestr(haystack: &str, needle: &str, start_offset: usize) -> Option<usize> {
    let using_utf8 = using_utf8();

    if using_utf8 {
        let needle_chars = mbstrlen(needle);
        let tail_chars = mbstrlen(&haystack[start_offset..]);

        let mut ptr = if tail_chars < needle_chars {
            let diff = needle_chars - tail_chars;
            let mut off = start_offset;
            for _ in 0..diff {
                if off == 0 {
                    break;
                }
                off = step_left(haystack, off);
            }
            off
        } else {
            start_offset
        };

        if ptr > haystack.len() {
            return None;
        }

        loop {
            if mbstrncasecmp(&haystack[ptr..], needle, needle_chars) == 0 {
                return Some(ptr);
            }
            if ptr == 0 {
                return None;
            }
            ptr = step_left(haystack, ptr);
        }
    } else {
        revstrcasestr(haystack, needle, start_offset)
    }
}

// ── Character search helpers ──────────────────────────────────────────────────

/* C: const char *mbstrchr(const char *string, const char *chr)
 * Find the first occurrence of the multibyte character `chr` in `string`.
 * Returns the byte offset of the match, or None. */
#[cfg(any(not(feature = "tiny"), feature = "justify"))]
pub fn mbstrchr(string: &str, chr: &str) -> Option<usize> {
    let using_utf8 = using_utf8();

    if using_utf8 {
        let (wc_needle, bad_c) = match mbtowide(chr) {
            Ok((wc, _)) => (wc as u32, false),
            Err(_) => (chr.as_bytes().first().copied().unwrap_or(0) as u32, true),
        };

        let mut pos = 0;
        let bytes = string.as_bytes();

        while pos < bytes.len() && bytes[pos] != 0 {
            let (ws, bad_s, symlen) = match mbtowide(&string[pos..]) {
                Ok((wc, len)) => (wc as u32, false, len),
                Err(_) => (bytes[pos] as u32, true, 1usize),
            };

            if ws == wc_needle && bad_s == bad_c {
                return Some(pos);
            }

            pos += symlen;
        }

        None
    } else {
        // Single-byte: find first occurrence of chr[0]
        let target = chr.as_bytes().first().copied()?;
        string.as_bytes().iter().position(|&b| b == target)
    }
}

/* C: char *mbstrpbrk(const char *string, const char *accept)
 * Find the first character in `string` that is also in `accept` (multibyte-aware). */
#[cfg(not(feature = "tiny"))]
pub fn mbstrpbrk(string: &str, accept: &str) -> Option<usize> {
    let mut pos = 0;
    let bytes = string.as_bytes();

    while pos < bytes.len() && bytes[pos] != 0 {
        #[cfg(any(not(feature = "tiny"), feature = "justify"))]
        if mbstrchr(accept, &string[pos..]).is_some() {
            return Some(pos);
        }
        pos += char_length(&string[pos..]);
    }

    None
}

/* C: char *mbrevstrpbrk(const char *head, const char *accept, const char *pointer)
 * Find the first character (searching backwards from `start_offset`) in `head`
 * that is also in `accept` (multibyte-aware). */
#[cfg(not(feature = "tiny"))]
pub fn mbrevstrpbrk(head: &str, accept: &str, start_offset: usize) -> Option<usize> {
    let head_bytes = head.as_bytes();

    // If pointer is at the end (NUL), step back one character
    let mut ptr = if start_offset >= head_bytes.len() || head_bytes[start_offset] == 0 {
        if start_offset == 0 {
            return None;
        }
        step_left(head, start_offset)
    } else {
        start_offset
    };

    loop {
        #[cfg(any(not(feature = "tiny"), feature = "justify"))]
        if mbstrchr(accept, &head[ptr..]).is_some() {
            return Some(ptr);
        }
        if ptr == 0 {
            return None;
        }
        ptr = step_left(head, ptr);
    }
}

// ── String predicate helpers ──────────────────────────────────────────────────

/* C: bool has_blank_char(const char *string)
 * Return true if the given string contains at least one blank character. */
pub fn has_blank_char(string: &str) -> bool {
    let mut pos = 0;
    let bytes = string.as_bytes();
    while pos < bytes.len() && bytes[pos] != 0 {
        if is_blank_char(&string[pos..]) {
            return true;
        }
        pos += char_length(&string[pos..]);
    }
    false
}

/* C: bool white_string(const char *string)
 * Return true when the given string is empty or consists entirely of blanks
 * (or carriage returns). */
pub fn white_string(string: &str) -> bool {
    let mut pos = 0;
    let bytes = string.as_bytes();
    while pos < bytes.len() && bytes[pos] != 0 {
        let ch = bytes[pos];
        if !is_blank_char(&string[pos..]) && ch != b'\r' {
            return false;
        }
        pos += char_length(&string[pos..]);
    }
    true
}

/* C: void strip_leading_blanks_from(char *string)
 * Remove leading spaces and tabs from the given string (in-place). */
#[cfg(any(feature = "speller", feature = "color"))]
pub fn strip_leading_blanks_from(s: &mut String) {
    let trimmed = s.trim_start_matches(|c| c == ' ' || c == '\t').to_string();
    *s = trimmed;
}
