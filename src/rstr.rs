//! The content of a Ruby String: its bytes and the encoding that reads them —
//! MRI's `RString`, with the one shortcut that valid UTF-8 is kept as a Rust
//! `String` so the overwhelmingly common case borrows as `&str` for free.
//!
//! # The two shapes
//!
//! * [`RStr::Text`] — UTF-8 whose bytes ARE valid UTF-8. Every String method
//!   runs on it directly.
//! * [`RStr::Bytes`] — every other String: the raw bytes plus the canonical
//!   name of their encoding (`"ASCII-8BIT"`, `"US-ASCII"`, `"ISO-8859-1"`, or
//!   `"UTF-8"` for UTF-8 whose bytes are NOT valid — [`BROKEN_UTF8`]).
//!
//! Nothing is smuggled: an invalid byte is a byte. MRI's encoding semantics
//! read straight off the shape — `valid_encoding?` is "not broken UTF-8",
//! `force_encoding` relabels without touching a byte, `bytes`/`byteslice`/
//! `getbyte` index the vector.
//!
//! # Character views
//!
//! The String methods written over `&str` see a byte-oriented String through
//! its LATIN-1 VIEW ([`RStr::view`]): byte `NN` as the character `U+00NN`.
//! For a single-byte encoding that view is exact — one character per byte,
//! both directions, no byte value left out — so `length`, `[]`, `reverse`,
//! `index`, `==` and the rest answer MRI's byte-oriented result unchanged.
//!
//! Broken UTF-8 has no exact `&str` view at all (its characters are a mix of
//! code points and lone bytes), so its character methods run over the bytes
//! themselves, segmented by [`utf8_units`]; see `dispatch_broken_utf8` in
//! `builtins.rs`.

use std::borrow::Cow;

/// The label of a [`RStr::Bytes`] holding UTF-8 whose bytes are not valid.
pub const BROKEN_UTF8: &str = "UTF-8";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RStr {
    /// Valid UTF-8 — the fast path.
    Text(String),
    /// Bytes under a non-UTF-8 encoding, or invalid UTF-8 under `"UTF-8"`.
    Bytes(Vec<u8>, &'static str),
}

impl RStr {
    /// The encoding name of a byte String, `None` for UTF-8 text.
    pub fn encoding(&self) -> Option<&'static str> {
        match self {
            RStr::Text(_) => None,
            RStr::Bytes(_, enc) => Some(enc),
        }
    }
    /// The bytes the String holds.
    pub fn bytes(&self) -> &[u8] {
        match self {
            RStr::Text(s) => s.as_bytes(),
            RStr::Bytes(b, _) => b,
        }
    }
    /// The text a String method written over `&str` sees: the text itself, or
    /// the Latin-1 view of a byte String (see the module notes).
    pub fn view(&self) -> Cow<'_, str> {
        match self {
            RStr::Text(s) => Cow::Borrowed(s),
            RStr::Bytes(b, _) => Cow::Owned(bytes_to_latin1(b)),
        }
    }
    /// Replace the content with the view text `s`, keeping the encoding: a
    /// byte String reads `s` back through the Latin-1 view.
    pub fn set_view(&mut self, s: String) {
        match self {
            RStr::Text(t) => *t = s,
            RStr::Bytes(b, _) => *b = latin1_to_bytes(&s),
        }
    }
    /// Relabel to the non-UTF-8 encoding `enc`, reading text as a Latin-1 view
    /// (the contract every caller that tags a freshly built String relies on).
    /// Tagging `"UTF-8"` re-stores the bytes through [`RStr::from_utf8`].
    pub fn set_encoding(&mut self, enc: &'static str) {
        let bytes = match std::mem::replace(self, RStr::Text(String::new())) {
            RStr::Text(s) => latin1_to_bytes(&s),
            RStr::Bytes(b, _) => b,
        };
        *self = if enc == BROKEN_UTF8 {
            RStr::from_utf8(bytes)
        } else {
            RStr::Bytes(bytes, enc)
        };
    }
    /// Drop the encoding, the view text becoming the String's UTF-8 text.
    pub fn clear_encoding(&mut self) {
        if let RStr::Bytes(b, _) = self {
            *self = RStr::Text(bytes_to_latin1(b));
        }
    }
    /// A UTF-8 String holding `bytes`: text when they are valid, broken UTF-8
    /// otherwise.
    pub fn from_utf8(bytes: Vec<u8>) -> RStr {
        match String::from_utf8(bytes) {
            Ok(s) => RStr::Text(s),
            Err(e) => RStr::Bytes(e.into_bytes(), BROKEN_UTF8),
        }
    }
    /// Whether this is UTF-8 whose bytes are not valid.
    pub fn is_broken_utf8(&self) -> bool {
        matches!(self, RStr::Bytes(_, BROKEN_UTF8))
    }
}

/// The Latin-1 view of `bytes`: byte `NN` as `U+00NN`.
pub fn bytes_to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// The bytes a Latin-1 view spells. A character past `U+00FF` cannot have come
/// from a view, only from UTF-8 text given a byte label, and stands for its
/// own UTF-8 bytes (MRI's `"日".b` is `"\xE6\x97\xA5"`).
pub fn latin1_to_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match u8::try_from(u32::from(c)) {
            Ok(b) => out.push(b),
            Err(_) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    out
}

/// MRI's characters of UTF-8 bytes: each valid sequence is one character and
/// each byte of an invalid sequence is a character on its own
/// (`"\xE3\x81a".chars` is `["\xE3", "\x81", "a"]`).
pub fn utf8_units(bytes: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    bytes.utf8_chunks().flat_map(|chunk| {
        let valid = chunk.valid();
        let chars = valid
            .char_indices()
            .map(move |(i, c)| &valid.as_bytes()[i..i + c.len_utf8()]);
        let invalid = chunk.invalid().chunks(1);
        chars.chain(invalid)
    })
}

/// Onigmo's UTF-8 character primitives, which MRI's String methods are built
/// from (`enc/utf_8.c`, `regenc.c`, `encoding.c`). Every position is a byte
/// offset into `b`. Two notions of "where a character starts" coexist in MRI
/// and both are needed: walking FORWARD from the start splits an invalid
/// sequence into one-byte characters ([`mbclen`]), while stepping BACK from a
/// position skips continuation bytes to the nearest lead byte
/// ([`left_head`]) — so `"\xE3\x81".chop` drops both bytes while its `length`
/// is 2.
pub mod utf8 {
    /// `ONIGENC_PRECISE_MBC_ENC_LEN`: what the bytes at the start of `b` are.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mbc {
        /// A valid character of this many bytes.
        Found(usize),
        /// A valid prefix that the end of `b` cuts short.
        NeedMore,
        Invalid,
    }

    pub fn precise(b: &[u8]) -> Mbc {
        let Some(&first) = b.first() else {
            return Mbc::NeedMore;
        };
        // The range the SECOND byte must fall in, and the sequence length.
        let (second, len) = match first {
            0x00..=0x7F => return Mbc::Found(1),
            0xC2..=0xDF => (0x80..=0xBF, 2),
            0xE0 => (0xA0..=0xBF, 3),
            0xE1..=0xEC | 0xEE..=0xEF => (0x80..=0xBF, 3),
            0xED => (0x80..=0x9F, 3),
            0xF0 => (0x90..=0xBF, 4),
            0xF1..=0xF3 => (0x80..=0xBF, 4),
            0xF4 => (0x80..=0x8F, 4),
            _ => return Mbc::Invalid,
        };
        for i in 1..len {
            let Some(&c) = b.get(i) else {
                return Mbc::NeedMore;
            };
            let ok = if i == 1 {
                second.contains(&c)
            } else {
                (0x80..=0xBF).contains(&c)
            };
            if !ok {
                return Mbc::Invalid;
            }
        }
        Mbc::Found(len)
    }

    /// `rb_enc_mbclen`: the length of the character at `p`, an invalid or
    /// truncated sequence counting its first byte alone.
    pub fn mbclen(b: &[u8], p: usize) -> usize {
        match precise(&b[p..]) {
            Mbc::Found(n) => n,
            _ => 1,
        }
    }

    /// `onigenc_mbclen` (`rb_enc_fast_mbclen`): like [`mbclen`], except that a
    /// sequence the end cuts short runs to the end.
    pub fn fast_mbclen(b: &[u8], p: usize) -> usize {
        match precise(&b[p..]) {
            Mbc::Found(n) => n,
            Mbc::NeedMore => b.len() - p,
            Mbc::Invalid => 1,
        }
    }

    /// `left_adjust_char_head`: back from `s` over continuation bytes, no
    /// further than `start`.
    pub fn left_head(b: &[u8], start: usize, s: usize) -> usize {
        let mut p = s;
        while p > start && p < b.len() && b[p] & 0xC0 == 0x80 {
            p -= 1;
        }
        p
    }

    /// `onigenc_get_right_adjust_char_head`: `s` when it starts a character,
    /// else the start of the next one.
    pub fn right_head(b: &[u8], start: usize, s: usize) -> usize {
        let p = left_head(b, start, s);
        if p < s {
            p + fast_mbclen(b, p)
        } else {
            p
        }
    }

    /// `rb_enc_prev_char`: the head of the character before `s`.
    pub fn prev_char(b: &[u8], start: usize, s: usize) -> Option<usize> {
        (s > start).then(|| left_head(b, start, s - 1))
    }

    /// `at_char_boundary`: whether `s` starts a character.
    pub fn at_char_boundary(b: &[u8], start: usize, s: usize) -> bool {
        left_head(b, start, s) == s
    }

    /// `at_char_right_boundary`: whether `s` ends a character.
    pub fn at_char_right_boundary(b: &[u8], start: usize, s: usize) -> bool {
        right_head(b, start, s) == s
    }

    /// The byte offset of every character start in `b` (by [`mbclen`]),
    /// followed by `b.len()`: character `i` is `b[at[i]..at[i + 1]]`.
    pub fn char_starts(b: &[u8]) -> Vec<usize> {
        let mut at = Vec::with_capacity(b.len() + 1);
        let mut p = 0;
        while p < b.len() {
            at.push(p);
            p += mbclen(b, p);
        }
        at.push(b.len());
        at
    }

    /// `str_strlen`: the number of characters.
    pub fn strlen(b: &[u8]) -> usize {
        let mut n = 0;
        let mut p = 0;
        while p < b.len() {
            p += mbclen(b, p);
            n += 1;
        }
        n
    }

    /// `str_offset`: the byte offset of character `nth`, clamped to the end.
    pub fn offset(b: &[u8], nth: usize) -> usize {
        let mut p = 0;
        for _ in 0..nth {
            if p >= b.len() {
                break;
            }
            p += mbclen(b, p);
        }
        p.min(b.len())
    }

    /// `rb_memsearch` + `strseq_core`: the first byte offset at or after
    /// `from` where `sub` occurs starting on a character head (by
    /// [`right_head`] relative to `from`).
    pub fn search(b: &[u8], sub: &[u8], from: usize) -> Option<usize> {
        let mut start = from;
        loop {
            let hay = b.get(start..)?;
            let pos = start + find(hay, sub)?;
            let t = right_head(b, start, pos);
            if t == pos {
                return Some(pos);
            }
            if t >= b.len() {
                return None;
            }
            start = t;
        }
    }

    /// `str_rindex`: the last byte offset at or before `s` where `sub` occurs
    /// starting on a character head.
    pub fn rsearch(b: &[u8], sub: &[u8], s: usize) -> Option<usize> {
        if sub.is_empty() {
            return Some(s);
        }
        if b[s..].starts_with(sub) {
            return Some(s);
        }
        let mut searchlen = s + 1;
        while searchlen > 0 {
            let hit = b[..searchlen.min(b.len())]
                .iter()
                .rposition(|&c| c == sub[0])?;
            let adjusted = left_head(b, 0, hit);
            if hit == adjusted && b[hit..].starts_with(sub) {
                return Some(hit);
            }
            searchlen = adjusted;
        }
        None
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        hay.windows(needle.len()).position(|w| w == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_forward_and_stepping_back_disagree_on_a_cut_sequence() {
        let b = b"a\xE3\x81";
        assert_eq!(utf8::strlen(b), 3);
        assert_eq!(utf8::prev_char(b, 0, b.len()), Some(1));
        assert_eq!(utf8::char_starts(b), vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_search_hit_inside_a_sequence_is_skipped() {
        // The needle occurs at 1, inside the character "\xE3\x81\x82".
        assert_eq!(utf8::search(b"\xE3\x81\x82x", b"\x81\x82x", 0), None);
        assert_eq!(utf8::search(b"a\xFFb", b"b", 0), Some(2));
    }

    #[test]
    fn units_split_invalid_sequences_byte_by_byte() {
        let units: Vec<&[u8]> = utf8_units(b"\xE3\x81a\xE3\x81\x82\xFF").collect();
        assert_eq!(
            units,
            vec![&b"\xE3"[..], b"\x81", b"a", "\u{3042}".as_bytes(), b"\xFF"]
        );
    }

    #[test]
    fn latin1_view_round_trips_every_byte() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(latin1_to_bytes(&bytes_to_latin1(&all)), all);
    }

    #[test]
    fn tagging_utf8_normalizes_valid_bytes_to_text() {
        let mut s = RStr::Text("\u{e9}".into()); // the view of byte 0xE9
        s.set_encoding(BROKEN_UTF8);
        assert_eq!(s, RStr::Bytes(vec![0xE9], BROKEN_UTF8));
        let mut t = RStr::Text("\u{c3}\u{a9}".into()); // the view of "é"'s bytes
        t.set_encoding(BROKEN_UTF8);
        assert_eq!(t, RStr::Text("\u{e9}".into()));
    }
}
