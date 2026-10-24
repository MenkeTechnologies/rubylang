//! MRI's character-selector tables — `tr_setup_table`, `trnext` and
//! `tr_find` in `string.c` — and the String methods that read them:
//! `count`, `delete`/`delete!`, `squeeze`/`squeeze!`, and
//! `strip`/`lstrip`/`rstrip` (and their `!` forms) given selectors.
//!
//! Every method runs over the receiver's BYTES and decodes a character the
//! way `rb_enc_codepoint_len` does, so the same code serves UTF-8 text, a
//! byte String (each byte its own code point) and a String whose bytes are
//! not valid in its encoding, which raises exactly where MRI's loop first
//! has to decode an invalid sequence.

use super::*;
use crate::rstr::utf8;
use std::collections::HashSet;

/// `TR_TABLE_MAX`: code points below this are answered by the flat table.
const TR_TABLE_MAX: usize = 256;

/// How a String's bytes are read as characters.
#[derive(Clone, Copy)]
struct Enc {
    /// The encoding's name, for `invalid byte sequence in NAME`.
    name: &'static str,
    kind: EncKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EncKind {
    Utf8,
    /// US-ASCII: one byte per character, a byte above 0x7F invalid.
    UsAscii,
    /// Every other encoding rubylang stores: one byte per character, the
    /// byte being its code point.
    SingleByte,
}

impl Enc {
    fn of(v: &Value) -> Enc {
        match with_host(|h| h.string_encoding(v)) {
            None | Some(crate::host::BROKEN_UTF8) => Enc {
                name: "UTF-8",
                kind: EncKind::Utf8,
            },
            Some("US-ASCII") => Enc {
                name: "US-ASCII",
                kind: EncKind::UsAscii,
            },
            Some(name) => Enc {
                name,
                kind: EncKind::SingleByte,
            },
        }
    }

    /// `rb_enc_codepoint_len`: the character at the start of `b` and its
    /// length; an invalid sequence is an error.
    fn codepoint_len(self, b: &[u8]) -> Result<(u32, usize), String> {
        let invalid = || {
            raise_exc(
                "ArgumentError",
                &format!("invalid byte sequence in {}", self.name),
            )
        };
        match self.kind {
            EncKind::Utf8 => match utf8::precise(b) {
                utf8::Mbc::Found(n) => Ok((decode_utf8(&b[..n]), n)),
                _ => Err(invalid()),
            },
            EncKind::UsAscii if b[0] > 0x7F => Err(invalid()),
            _ => Ok((u32::from(b[0]), 1)),
        }
    }

    /// `rb_enc_prev_char` over a String already known to be valid.
    fn prev_char(self, b: &[u8], start: usize, s: usize) -> Option<usize> {
        match self.kind {
            EncKind::Utf8 => utf8::prev_char(b, start, s),
            _ => (s > start).then(|| s - 1),
        }
    }

    /// `rb_enc_mbmaxlen(enc) == 1`.
    fn single_byte(self) -> bool {
        self.kind != EncKind::Utf8
    }
}

/// The code point of one valid UTF-8 sequence.
fn decode_utf8(b: &[u8]) -> u32 {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.chars().next())
        .map_or(0, u32::from)
}

/// Whether `b` holds a sequence `enc` cannot read (`ENC_CODERANGE_BROKEN`).
fn is_broken(b: &[u8], enc: Enc) -> bool {
    match enc.kind {
        EncKind::Utf8 => std::str::from_utf8(b).is_err(),
        EncKind::UsAscii => !b.is_ascii(),
        EncKind::SingleByte => false,
    }
}

/// `single_byte_optimizable`: 7-bit content, or a one-byte encoding.
fn single_byte_optimizable(b: &[u8], enc: Enc) -> bool {
    enc.single_byte() || b.is_ascii()
}

/// `struct tr` and `trnext`: the code points a selector names, ranges
/// expanded one at a time.
struct Tr<'a> {
    s: &'a [u8],
    p: usize,
    enc: Enc,
    gen: bool,
    now: u32,
    max: u32,
}

impl Tr<'_> {
    fn next(&mut self) -> Result<Option<u32>, String> {
        if self.gen {
            // MRI steps past code points the encoding cannot spell
            // (`ONIGENC_CODE_TO_MBCLEN <= 0`); between two decoded code
            // points there are none in UTF-8 or a one-byte encoding.
            self.now += 1;
            if self.now < self.max {
                return Ok(Some(self.now));
            }
            self.gen = false;
            return Ok(Some(self.max));
        }
        let end = self.s.len();
        if self.p == end {
            return Ok(None);
        }
        if self.s[self.p] == b'\\' && self.p + 1 < end {
            self.p += 1;
        }
        let (now, n) = self.enc.codepoint_len(&self.s[self.p..])?;
        self.now = now;
        self.p += n;
        if self.p < end && self.s[self.p] == b'-' && self.p + 1 < end {
            self.p += 1;
            let (c, n) = self.enc.codepoint_len(&self.s[self.p..])?;
            self.p += n;
            if now > c {
                let msg = match (char::from_u32(now), char::from_u32(c)) {
                    (Some(a), Some(b)) if now < 0x80 && c < 0x80 => {
                        format!("invalid range \"{a}-{b}\" in string transliteration")
                    }
                    _ => "invalid range in string transliteration".to_string(),
                };
                return Err(raise_exc("ArgumentError", &msg));
            }
            if now < c {
                self.gen = true;
                self.max = c;
            }
        }
        Ok(Some(now))
    }
}

/// The intersection of one or more selectors: `stable` for the code points
/// below [`TR_TABLE_MAX`] (its last slot answers for every other code point),
/// and MRI's `del` / `nodel` hashes for the code points above it.
struct TrTable {
    stable: [bool; TR_TABLE_MAX + 1],
    del: Option<HashSet<u32>>,
    nodel: Option<HashSet<u32>>,
}

impl TrTable {
    /// `tr_setup_table_multi`, with the `StringValue` and `rb_enc_check` each
    /// selector gets first.
    fn new(recv: &Value, selectors: &[Value]) -> Result<TrTable, String> {
        let mut t = TrTable {
            stable: [false; TR_TABLE_MAX + 1],
            del: None,
            nodel: None,
        };
        for (i, sel) in selectors.iter().enumerate() {
            let bytes = selector_bytes(recv, sel)?;
            t.add(&bytes, Enc::of(sel), i == 0)?;
        }
        Ok(t)
    }

    /// `tr_setup_table` for one selector.
    fn add(&mut self, sel: &[u8], enc: Enc, first: bool) -> Result<(), String> {
        let mut tr = Tr {
            s: sel,
            p: 0,
            enc,
            gen: false,
            now: 0,
            max: 0,
        };
        let cflag = sel.len() > 1 && sel[0] == b'^';
        if cflag {
            tr.p = 1;
        }
        if first {
            self.stable[..TR_TABLE_MAX].fill(true);
            self.stable[TR_TABLE_MAX] = cflag;
        } else if self.stable[TR_TABLE_MAX] && !cflag {
            self.stable[TR_TABLE_MAX] = false;
        }
        let mut buf = [cflag; TR_TABLE_MAX];
        // Which hash this selector writes (`table`), and the one it reads
        // (`ptable`): a negated selector adds to `nodel` itself; a plain one
        // builds a fresh `del` holding what the previous `del` also held.
        let mut table: Option<bool> = None;
        let mut ptable: Option<HashSet<u32>> = None;
        while let Some(c) = tr.next()? {
            if (c as usize) < TR_TABLE_MAX {
                buf[c as usize] = !cflag;
                continue;
            }
            if table.is_none() && (first || self.del.is_some() || self.stable[TR_TABLE_MAX]) {
                if cflag {
                    ptable = self.nodel.clone();
                    self.nodel.get_or_insert_with(HashSet::new);
                } else {
                    ptable = self.del.take();
                    self.del = Some(HashSet::new());
                }
                table = Some(cflag);
            }
            let Some(negated) = table else {
                continue;
            };
            if ptable.as_ref().map_or(true, |p| cflag ^ p.contains(&c)) {
                let target = if negated {
                    &mut self.nodel
                } else {
                    &mut self.del
                };
                target.get_or_insert_with(HashSet::new).insert(c);
            }
        }
        for (slot, b) in self.stable.iter_mut().zip(buf) {
            *slot = *slot && b;
        }
        if table.is_none() && !cflag {
            self.del = None;
        }
        Ok(())
    }

    /// `tr_find`.
    fn find(&self, c: u32) -> bool {
        if (c as usize) < TR_TABLE_MAX {
            return self.stable[c as usize];
        }
        if let Some(del) = &self.del {
            if del.contains(&c) && self.nodel.as_ref().map_or(true, |n| !n.contains(&c)) {
                return true;
            }
        } else if self.nodel.as_ref().is_some_and(|n| n.contains(&c)) {
            return false;
        }
        self.stable[TR_TABLE_MAX]
    }
}

/// `StringValue(selector)` then `rb_enc_check(str, selector)`: the
/// selector's bytes.
fn selector_bytes(recv: &Value, sel: &Value) -> Result<Vec<u8>, String> {
    let sel = match with_host(|h| h.str_bytes(sel).is_some()) {
        true => sel.clone(),
        false => new_str(implicit_str(sel)?),
    };
    with_host(|h| h.check_encoding_compatible(recv, &sel))?;
    Ok(with_host(|h| h.string_bytes(&sel)))
}

/// A String of `bytes` in `recv`'s encoding.
fn new_like(recv: &Value, bytes: Vec<u8>) -> Value {
    with_host(|h| match h.string_encoding(recv) {
        Some(e) if e != crate::host::BROKEN_UTF8 => h.new_byte_string(bytes, e),
        _ => h.new_utf8_from_bytes(bytes),
    })
}

/// Re-store `recv` as `bytes`, keeping its encoding.
fn store_like(recv: &Value, bytes: Vec<u8>) {
    with_host(|h| match h.string_encoding(recv) {
        Some(e) if e != crate::host::BROKEN_UTF8 => {
            h.set_rstr(recv, crate::rstr::RStr::Bytes(bytes, e));
        }
        _ => h.set_utf8_bytes(recv, bytes),
    });
}

fn modify_check(recv: &Value, name: &str) -> Result<(), String> {
    frozen_guard(recv, name, &[])
}

/// The methods this module answers, or `None` to leave `name` to the
/// generic String dispatch. A selector-less `strip` family call is left
/// there unless the receiver is broken, which the text fast path cannot see.
pub(super) fn dispatch(recv: &Value, name: &str, args: &[Value]) -> Option<Result<Value, String>> {
    Some(match name {
        "count" if !args.is_empty() => count(recv, args),
        "delete" | "delete!" => delete(recv, name, args),
        "squeeze" | "squeeze!" => squeeze(recv, name, args),
        "strip" | "strip!" | "lstrip" | "lstrip!" | "rstrip" | "rstrip!" => {
            if args.is_empty() {
                let b = with_host(|h| h.str_bytes(recv).map(<[u8]>::to_vec))?;
                if !is_broken(&b, Enc::of(recv)) {
                    return None;
                }
            }
            strip(recv, name, args)
        }
        _ => return None,
    })
}

/// `rb_str_count`.
fn count(recv: &Value, args: &[Value]) -> Result<Value, String> {
    let table = TrTable::new(recv, args)?;
    let b = with_host(|h| h.string_bytes(recv));
    let enc = Enc::of(recv);
    let (mut n, mut s) = (0i64, 0);
    while s < b.len() {
        if b[s] < 0x80 {
            n += i64::from(table.stable[b[s] as usize]);
            s += 1;
        } else {
            let (c, len) = enc.codepoint_len(&b[s..])?;
            n += i64::from(table.find(c));
            s += len;
        }
    }
    Ok(Value::Int(n))
}

/// `rb_str_delete_bang`; `delete` runs it on a copy.
fn delete(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let bang = name.ends_with('!');
    let b = with_host(|h| h.string_bytes(recv));
    if b.is_empty() {
        return Ok(if bang {
            Value::Undef
        } else {
            new_like(recv, b)
        });
    }
    if args.is_empty() {
        return Err(raise_exc(
            "ArgumentError",
            "wrong number of arguments (given 0, expected 1+)",
        ));
    }
    let table = TrTable::new(recv, args)?;
    if bang {
        modify_check(recv, name)?;
    }
    let enc = Enc::of(recv);
    let (mut out, mut modify, mut s) = (Vec::with_capacity(b.len()), false, 0);
    while s < b.len() {
        let (c, len) = if b[s] < 0x80 {
            (u32::from(b[s]), 1)
        } else {
            enc.codepoint_len(&b[s..])?
        };
        if table.find(c) {
            modify = true;
        } else {
            out.extend_from_slice(&b[s..s + len]);
        }
        s += len;
    }
    if !bang {
        return Ok(new_like(recv, out));
    }
    if !modify {
        return Ok(Value::Undef);
    }
    store_like(recv, out);
    Ok(recv.clone())
}

/// `rb_str_squeeze_bang`; `squeeze` runs it on a copy.
fn squeeze(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let bang = name.ends_with('!');
    let b = with_host(|h| h.string_bytes(recv));
    let enc = Enc::of(recv);
    let mut singlebyte = single_byte_optimizable(&b, enc);
    let table = if args.is_empty() {
        None
    } else {
        let t = TrTable::new(recv, args)?;
        for sel in args {
            let sb = with_host(|h| h.string_bytes(sel));
            singlebyte &= single_byte_optimizable(&sb, Enc::of(sel));
        }
        Some(t)
    };
    if bang {
        modify_check(recv, name)?;
    }
    if b.is_empty() {
        return Ok(if bang {
            Value::Undef
        } else {
            new_like(recv, b)
        });
    }
    // A run of one character keeps its first; with selectors, only a
    // character they name is squeezed.
    let keep = |c: u32, save: Option<u32>, in_set: &dyn Fn() -> bool| {
        Some(c) != save || table.is_some() && !in_set()
    };
    let (mut out, mut save, mut s) = (Vec::with_capacity(b.len()), None, 0);
    while s < b.len() {
        let (c, len) = if singlebyte || b[s] < 0x80 {
            (u32::from(b[s]), 1)
        } else {
            enc.codepoint_len(&b[s..])?
        };
        let in_set = || {
            table.as_ref().is_some_and(|t| {
                if singlebyte || c < 0x80 {
                    t.stable[c as usize]
                } else {
                    t.find(c)
                }
            })
        };
        if keep(c, save, &in_set) {
            out.extend_from_slice(&b[s..s + len]);
            save = Some(c);
        }
        s += len;
    }
    if !bang {
        return Ok(new_like(recv, out));
    }
    if out.len() == b.len() {
        return Ok(Value::Undef);
    }
    store_like(recv, out);
    Ok(recv.clone())
}

/// `rb_str_strip`, `rb_str_lstrip`, `rb_str_rstrip` and their `!` forms.
fn strip(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    let bang = name.ends_with('!');
    if bang {
        modify_check(recv, name)?;
    }
    let base = name.trim_end_matches('!');
    let table = if args.is_empty() {
        None
    } else {
        Some(TrTable::new(recv, args)?)
    };
    let b = with_host(|h| h.string_bytes(recv));
    let enc = Enc::of(recv);
    let loffset = if base == "rstrip" {
        0
    } else {
        lstrip_offset(&b, enc, table.as_ref())?
    };
    let roffset = if base == "lstrip" {
        0
    } else {
        rstrip_offset(&b, loffset, enc, table.as_ref())?
    };
    let cut = b[loffset..b.len() - roffset].to_vec();
    if !bang {
        return Ok(new_like(recv, cut));
    }
    if loffset == 0 && roffset == 0 {
        return Ok(Value::Undef);
    }
    store_like(recv, cut);
    Ok(recv.clone())
}

/// `rb_isspace` and NUL: what a selector-less strip removes.
fn space_or_nul(c: u32) -> bool {
    c == 0 || c == u32::from(b' ') || (0x09..=0x0D).contains(&c)
}

/// `lstrip_offset` / `lstrip_offset_table`.
fn lstrip_offset(b: &[u8], enc: Enc, table: Option<&TrTable>) -> Result<usize, String> {
    let mut s = 0;
    if table.is_none() && single_byte_optimizable(b, enc) {
        while s < b.len() && space_or_nul(u32::from(b[s])) {
            s += 1;
        }
        return Ok(s);
    }
    while s < b.len() {
        let (c, n) = enc.codepoint_len(&b[s..])?;
        let strip = match table {
            Some(t) => t.find(c),
            None => space_or_nul(c),
        };
        if !strip {
            break;
        }
        s += n;
    }
    Ok(s)
}

/// `rstrip_offset` / `rstrip_offset_table`: from the end back to `start`;
/// a receiver with an invalid sequence is refused before anything is read.
fn rstrip_offset(
    b: &[u8],
    start: usize,
    enc: Enc,
    table: Option<&TrTable>,
) -> Result<usize, String> {
    if is_broken(b, enc) {
        return Err(raise_exc(
            "Encoding::CompatibilityError",
            &format!("invalid byte sequence in {}", enc.name),
        ));
    }
    let mut t = b.len();
    while let Some(p) = enc.prev_char(b, start, t) {
        let (c, _) = enc.codepoint_len(&b[p..t])?;
        let strip = match table {
            Some(tab) => tab.find(c),
            None => space_or_nul(c),
        };
        if !strip {
            break;
        }
        t = p;
    }
    Ok(b.len() - t)
}
