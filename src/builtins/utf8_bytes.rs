//! String methods over UTF-8 BYTES: a receiver whose bytes are not valid
//! UTF-8 (a [`BROKEN_UTF8`] String), or a text receiver handed such a String
//! as an argument (`"ab" << "\xff"`).
//!
//! Each method is a port of its `string.c` function, written over the
//! [`utf8`] primitives that function's Onigmo calls stand for, so the answers
//! follow from the bytes the way MRI's do — an invalid byte is one character
//! when counted from the front, a search hit must start on a character head,
//! a broken needle is never found. Nothing is mapped into or out of a `&str`.
//!
//! [`dispatch`] answers `None` for a method it does not port; the caller
//! decides what that means (see `dispatch_broken_utf8`).

use super::*;
use crate::rstr::{utf8, BROKEN_UTF8};

/// The receiver a method runs on: the String value and its UTF-8 bytes.
struct Recv<'a> {
    v: &'a Value,
    b: Vec<u8>,
}

pub(super) fn dispatch(
    recv: &Value,
    bytes: Vec<u8>,
    name: &str,
    args: &[Value],
    block: Option<Value>,
) -> Result<Option<Value>, String> {
    let r = Recv { v: recv, b: bytes };
    frozen_guard(recv, name, STRING_MUTATORS)?;
    let a = |i: usize| args.get(i);
    Ok(Some(match (name, args.len()) {
        ("length" | "size", 0) => Value::Int(utf8::strlen(&r.b) as i64),
        ("chars" | "each_char", 0) => each_unit(&r, name, block)?,
        ("reverse", 0) => out(reverse(&r.b)),
        ("reverse!", 0) => {
            store(recv, reverse(&r.b));
            recv.clone()
        }
        ("ord", 0) => ord(&r.b)?,
        ("chr", 0) => out(r.b[..r.b.len().min(first_len(&r.b))].to_vec()),
        ("chop", 0) => out(r.b[..chopped_length(&r.b)].to_vec()),
        ("chop!", 0) => {
            if r.b.is_empty() {
                return Ok(Some(Value::Undef));
            }
            store(recv, r.b[..chopped_length(&r.b)].to_vec());
            recv.clone()
        }
        ("chomp", 0 | 1) => match chompped_length(&r.b, a(0))? {
            Some(n) => out(r.b[..n].to_vec()),
            None => out(r.b.clone()),
        },
        ("chomp!", 0 | 1) => {
            if r.b.is_empty() && args.is_empty() {
                return Ok(Some(Value::Undef));
            }
            match chompped_length(&r.b, a(0))? {
                Some(n) if n < r.b.len() => {
                    store(recv, r.b[..n].to_vec());
                    recv.clone()
                }
                _ => Value::Undef,
            }
        }
        ("[]" | "slice", 1 | 2) => aref(&r, args)?,
        ("slice!", 1 | 2) => slice_bang(&r, args)?,
        ("[]=", 2 | 3) => aset(&r, args)?,
        ("insert", 2) => {
            let pos = to_int(&args[0])?;
            if pos == -1 {
                append(&r, &args[1..])?;
            } else {
                let pos = if pos < 0 { pos + 1 } else { pos };
                update(&r, pos, 0, &args[1])?;
            }
            recv.clone()
        }
        ("include?", 1) => Value::Bool(strseq_index(&r.b, &args[0], 0, false)?.is_some()),
        ("index", 1 | 2) => index(&r, args)?,
        ("rindex", 1 | 2) => rindex(&r, args)?,
        ("bytesplice", _) => bytesplice(&r, args)?,
        ("byteindex", 1 | 2) => return byteindex(&r, args),
        ("byterindex", 1 | 2) => return byterindex(&r, args),
        ("start_with?", _) => start_with(&r.b, args)?,
        ("end_with?", _) => end_with(&r.b, args)?,
        ("partition", 1) => partition(&r, &args[0])?,
        ("rpartition", 1) => rpartition(&r, &args[0])?,
        ("delete_prefix" | "delete_prefix!", 1) => {
            let n = deleted_prefix_length(&r.b, &args[0])?;
            trimmed(&r, name, n, r.b.len())
        }
        ("delete_suffix" | "delete_suffix!", 1) => {
            let n = deleted_suffix_length(&r.b, &args[0])?;
            trimmed(&r, name, 0, r.b.len() - n)
        }
        ("lines" | "each_line", 0..=2) => lines(&r, name, args, block)?,
        ("sub" | "sub!" | "gsub" | "gsub!", 1 | 2) => sub(&r, name, args, block)?,
        ("center" | "ljust" | "rjust", 1 | 2) => justify(&r.b, name, args)?,
        ("<<", 1) | ("concat", _) => {
            append(&r, args)?;
            recv.clone()
        }
        ("prepend", _) => {
            let mut b = Vec::new();
            for x in args {
                b.extend(operand(x)?);
            }
            b.extend(&r.b);
            store(recv, b);
            recv.clone()
        }
        ("replace", 1) => {
            let content = match with_host(|h| h.rstr(&args[0]).cloned()) {
                Some(c) => c,
                None => crate::rstr::RStr::Text(implicit_str(&args[0])?),
            };
            with_host(|h| h.set_rstr(recv, content));
            recv.clone()
        }
        ("+", 1) => {
            let mut b = r.b.clone();
            b.extend(operand(&args[0])?);
            out(b)
        }
        ("*", 1) => out(r.b.repeat(checked_size(&args[0], 0, "negative argument")?)),
        ("%", 1) => string_format(&r.b, &args[0])?,
        ("succ" | "next", 0) => out(str_succ(r.b.clone())),
        ("succ!" | "next!", 0) => {
            store(recv, str_succ(r.b.clone()));
            recv.clone()
        }
        ("upto", 1 | 2) => upto(&r, args, block)?,
        ("casecmp", 1) => casecmp(&r.b, &args[0])?,
        ("<=>", 1) => match string_operand(&args[0]) {
            Some((b, enc)) => Value::Int(str_cmp(&r.b, &b, enc)),
            None => return Ok(None),
        },
        ("==" | "===" | "eql?", 1) => match string_operand(&args[0]) {
            Some((b, enc)) => Value::Bool(str_cmp(&r.b, &b, enc) == 0),
            None => return Ok(None),
        },
        ("grapheme_clusters" | "each_grapheme_cluster", 0) => graphemes(&r, name, block)?,
        _ => return Ok(None),
    }))
}

// ---- values in, values out --------------------------------------------------

/// A UTF-8 String of `b`: text when valid, broken UTF-8 otherwise.
fn out(b: Vec<u8>) -> Value {
    with_host(|h| h.new_utf8_from_bytes(b))
}

/// Re-store the receiver as the UTF-8 bytes `b`.
fn store(recv: &Value, b: Vec<u8>) {
    with_host(|h| h.set_utf8_bytes(recv, b));
}

fn is_regexp(v: &Value) -> bool {
    with_host(|h| h.class_of(v)) == "Regexp"
}

fn invalid_byte_sequence() -> String {
    raise_exc("ArgumentError", "invalid byte sequence in UTF-8")
}

/// A String argument's bytes and encoding, `None` for any other value.
fn string_operand(v: &Value) -> Option<(Vec<u8>, Option<&'static str>)> {
    with_host(|h| {
        let b = h.str_bytes(v)?.to_vec();
        Some((b, h.string_encoding(v)))
    })
}

/// `StringValue` + `rb_enc_check` of an argument combined with the UTF-8
/// receiver: its bytes. A non-UTF-8 String holding non-ASCII cannot join.
fn operand(v: &Value) -> Result<Vec<u8>, String> {
    let Some((b, enc)) = string_operand(v) else {
        return Ok(implicit_str(v)?.into_bytes());
    };
    match enc {
        Some(e) if e != BROKEN_UTF8 && !b.is_ascii() => Err(raise_exc(
            "Encoding::CompatibilityError",
            &format!(
                "incompatible character encodings: UTF-8 and {}",
                encoding_display_name(e)
            ),
        )),
        _ => Ok(b),
    }
}

/// How MRI's encoding messages spell an encoding (`BINARY (ASCII-8BIT)`).
fn encoding_display_name(enc: &str) -> String {
    match enc {
        "ASCII-8BIT" => "BINARY (ASCII-8BIT)".to_string(),
        other => other.to_string(),
    }
}

/// `is_broken_string`: bytes that are not valid UTF-8.
fn broken(b: &[u8]) -> bool {
    std::str::from_utf8(b).is_err()
}

/// `rb_obj_as_string` for text that joins the receiver's bytes.
fn as_string_bytes(v: &Value) -> Result<Vec<u8>, String> {
    if string_operand(v).is_some() {
        return operand(v);
    }
    Ok(with_host(|h| h.to_s(v)).into_bytes())
}

/// The character slices of `b`.
fn units(b: &[u8]) -> Vec<&[u8]> {
    utf8::char_starts(b)
        .windows(2)
        .map(|w| &b[w[0]..w[1]])
        .collect()
}

fn first_len(b: &[u8]) -> usize {
    if b.is_empty() {
        0
    } else {
        utf8::mbclen(b, 0)
    }
}

/// `single_byte_optimizable`: all ASCII, so a character is a byte.
fn single_byte(b: &[u8]) -> bool {
    b.is_ascii()
}

/// `str_strlen` as `rb_str_sublen` uses it: characters in `b[..pos]`.
fn sublen(b: &[u8], pos: usize) -> usize {
    utf8::strlen(&b[..pos])
}

// ---- characters -------------------------------------------------------------

/// `each_char` / `chars`: each character, an Enumerator without a block.
fn each_unit(r: &Recv, name: &str, block: Option<Value>) -> Result<Value, String> {
    let items: Vec<Value> = units(&r.b).into_iter().map(|u| out(u.to_vec())).collect();
    match block {
        Some(bl) => {
            for c in items {
                call_proc(&bl, &[c])?;
                if has_pending_signal() {
                    break;
                }
            }
            Ok(r.v.clone())
        }
        None if name == "chars" => Ok(new_arr(items)),
        None => Ok(with_host(|h| h.new_enumerator_of(items, name, r.v.clone()))),
    }
}

/// `rb_str_reverse`: the characters in reverse order.
fn reverse(b: &[u8]) -> Vec<u8> {
    units(b).into_iter().rev().flatten().copied().collect()
}

/// `rb_str_ord` (`rb_enc_codepoint`).
fn ord(b: &[u8]) -> Result<Value, String> {
    if b.is_empty() {
        return Err(raise_exc("ArgumentError", "empty string"));
    }
    match utf8::precise(b) {
        utf8::Mbc::Found(n) => {
            let c = std::str::from_utf8(&b[..n])
                .ok()
                .and_then(|s| s.chars().next())
                .map_or(0, u32::from);
            Ok(Value::Int(i64::from(c)))
        }
        _ => Err(invalid_byte_sequence()),
    }
}

/// `chopped_length`: drop the last character, or a trailing `\r\n` pair.
fn chopped_length(b: &[u8]) -> usize {
    let Some(mut p) = utf8::prev_char(b, 0, b.len()) else {
        return 0;
    };
    if p > 0 && b[p] == b'\n' {
        if let Some(p2) = utf8::prev_char(b, 0, p).filter(|&p2| b[p2] == b'\r') {
            p = p2;
        }
    }
    p
}

/// `smart_chomp`: a trailing `\n`, `\r\n` or `\r`.
fn smart_chomp(b: &[u8]) -> usize {
    let mut e = b.len();
    match b.last() {
        Some(b'\n') => {
            e -= 1;
            if e > 0 && b[e - 1] == b'\r' {
                e -= 1;
            }
        }
        Some(b'\r') => e -= 1,
        _ => {}
    }
    e
}

/// `chompped_length` for `chomp(rs)` — `None` for a `nil` separator, which
/// chomps nothing at all.
fn chompped_length(b: &[u8], rs: Option<&Value>) -> Result<Option<usize>, String> {
    let rs = match rs {
        None => return Ok(Some(smart_chomp(b))),
        Some(Value::Undef) => return Ok(None),
        Some(v) => operand(v)?,
    };
    let len = b.len();
    if len == 0 {
        return Ok(Some(0));
    }
    if rs.is_empty() {
        let mut e = len;
        while e > 0 && b[e - 1] == b'\n' {
            e -= 1;
            if e > 0 && b[e - 1] == b'\r' {
                e -= 1;
            }
        }
        return Ok(Some(e));
    }
    if rs.len() > len {
        return Ok(Some(len));
    }
    if rs == b"\n" {
        return Ok(Some(smart_chomp(b)));
    }
    if broken(&rs) {
        return Ok(Some(len));
    }
    let pp = len - rs.len();
    if b[len - 1] == rs[rs.len() - 1]
        && (rs.len() <= 1 || b[pp..] == rs[..])
        && utf8::at_char_boundary(b, 0, pp)
    {
        return Ok(Some(pp));
    }
    Ok(Some(len))
}

/// The receiver cut to `b[from..to]`: a new String, or for a `!` method the
/// receiver itself (`nil` when nothing was cut).
fn trimmed(r: &Recv, name: &str, from: usize, to: usize) -> Value {
    let cut = r.b[from..to].to_vec();
    if !name.ends_with('!') {
        return out(cut);
    }
    if cut.len() == r.b.len() {
        return Value::Undef;
    }
    store(r.v, cut);
    r.v.clone()
}

/// `rb_str_enumerate_grapheme_clusters`: the valid runs cluster as text and
/// each invalid byte is a cluster of its own — except a sequence the end cuts
/// short, where Onigmo's `\X` fails to match and the enumeration stops.
fn graphemes(r: &Recv, name: &str, block: Option<Value>) -> Result<Value, String> {
    let mut items = Vec::new();
    let mut p = 0;
    while p < r.b.len() {
        match utf8::precise(&r.b[p..]) {
            utf8::Mbc::Found(_) => {
                let run = match std::str::from_utf8(&r.b[p..]) {
                    Ok(s) => s,
                    Err(e) => std::str::from_utf8(&r.b[p..p + e.valid_up_to()]).unwrap_or(""),
                };
                for g in UnicodeSegmentation::graphemes(run, true) {
                    items.push(new_str(g.to_string()));
                }
                p += run.len();
            }
            utf8::Mbc::Invalid => {
                items.push(out(vec![r.b[p]]));
                p += 1;
            }
            utf8::Mbc::NeedMore => break,
        }
    }
    match block {
        Some(bl) => {
            for g in items {
                call_proc(&bl, &[g])?;
            }
            Ok(r.v.clone())
        }
        None if name == "grapheme_clusters" => Ok(new_arr(items)),
        None => Ok(with_host(|h| h.new_enumerator_of(items, name, r.v.clone()))),
    }
}

// ---- positions --------------------------------------------------------------

/// `str_nth_len` from `p`: the byte offset `nth` characters on, and how many
/// of them the end cut short.
fn nth_len(b: &[u8], mut p: usize, mut nth: usize) -> (usize, usize) {
    while p < b.len() && nth > 0 {
        p += utf8::mbclen(b, p);
        nth -= 1;
    }
    (p.min(b.len()), nth)
}

/// `str_nth`: the byte offset `nth` characters on from `p`, clamped.
fn nth(b: &[u8], p: usize, nth: usize) -> usize {
    nth_len(b, p, nth).0
}

/// `rb_str_subpos`: the byte span of `len` characters from character `beg`.
fn subpos(b: &[u8], mut beg: i64, mut len: i64) -> Option<(usize, usize)> {
    let blen = b.len() as i64;
    if len < 0 {
        return None;
    }
    if blen == 0 {
        len = 0;
    }
    if single_byte(b) {
        if beg > blen {
            return None;
        }
        if beg < 0 {
            beg += blen;
            if beg < 0 {
                return None;
            }
        }
        len = len.min(blen - beg);
        return (len >= 0).then_some((beg as usize, len as usize));
    }
    if beg < 0 {
        if len > -beg {
            len = -beg;
        }
        beg += utf8::strlen(b) as i64;
        if beg < 0 {
            return None;
        }
        // MRI points at BYTE `beg` here; the span is empty, so only the
        // length is ever read.
        if len == 0 {
            return Some((0, 0));
        }
    } else if beg > blen {
        return None;
    }
    if len == 0 {
        if beg as usize > utf8::strlen(b) {
            return None;
        }
        return Some((0, 0));
    }
    let (p, left) = nth_len(b, 0, beg as usize);
    if p == b.len() {
        return (left == 0).then_some((p, 0));
    }
    let e = nth(b, p, len as usize);
    Some((p, e - p))
}

/// `str_substr`: `len` characters from `beg`, or `nil`; `empty` is whether
/// an empty answer stands (`s[i, 0]`) or means `nil` (`s[i]`).
fn substr(b: &[u8], beg: i64, len: i64, empty: bool) -> Option<Vec<u8>> {
    let (p, n) = subpos(b, beg, len)?;
    if n == 0 && !empty {
        return None;
    }
    Some(b[p..p + n].to_vec())
}

/// `rb_range_beg_len`: a Range argument as `(beg, len)` over `len` units, or
/// `Err(None)` when it is not a Range. `err` is MRI's mode: 0 answers
/// `Ok(None)` out of range, 2 raises RangeError.
fn range_beg_len(v: &Value, len: i64, err: u8) -> Result<Option<Option<(i64, i64)>>, String> {
    use crate::host::{RANGE_BEGINLESS, RANGE_ENDLESS};
    let Some((lo, hi, excl)) = with_host(|h| h.as_range(v)) else {
        return Ok(None);
    };
    let mut beg = if lo == RANGE_BEGINLESS { 0 } else { lo };
    let (mut end, excl) = if hi == RANGE_ENDLESS {
        (-1, false)
    } else {
        (hi, excl)
    };
    let out_of_range = || -> Result<Option<Option<(i64, i64)>>, String> {
        if err != 0 {
            let shown = with_host(|h| h.inspect(v));
            return Err(raise_exc("RangeError", &format!("{shown} out of range")));
        }
        Ok(Some(None))
    };
    if beg < 0 {
        beg += len;
        if beg < 0 {
            return out_of_range();
        }
    }
    if end < 0 {
        end += len;
    }
    if !excl {
        end += 1;
    }
    if beg > len {
        return out_of_range();
    }
    end = end.min(len);
    Ok(Some(Some((beg, (end - beg).max(0)))))
}

/// `rb_strseq_index`: the byte offset of `sub` at or after `offset`
/// characters (bytes when `in_byte`), on a character head.
fn strseq_index(
    b: &[u8],
    sub: &Value,
    offset: i64,
    in_byte: bool,
) -> Result<Option<usize>, String> {
    let sub = operand(sub)?;
    if broken(&sub) || b.len() < sub.len() {
        return Ok(None);
    }
    let mut offset = offset;
    if offset != 0 {
        let str_len = if in_byte || single_byte(b) {
            b.len()
        } else {
            utf8::strlen(b)
        } as i64;
        let sub_len = if in_byte {
            sub.len()
        } else {
            utf8::strlen(&sub)
        } as i64;
        if offset < 0 {
            offset += str_len;
            if offset < 0 {
                return Ok(None);
            }
        }
        if str_len - offset < sub_len {
            return Ok(None);
        }
        if !in_byte {
            offset = utf8::offset(b, offset as usize) as i64;
        }
    }
    let offset = offset as usize;
    if sub.is_empty() {
        return Ok(Some(offset));
    }
    Ok(utf8::search(b, &sub, offset))
}

/// `rb_str_rindex`: the byte offset of the last `sub` at or before character
/// `pos`.
fn str_rindex(b: &[u8], sub: &Value, pos: i64) -> Result<Option<usize>, String> {
    let sub = operand(sub)?;
    if broken(&sub) {
        return Ok(None);
    }
    let len = if single_byte(b) {
        b.len()
    } else {
        utf8::strlen(b)
    } as i64;
    let slen = utf8::strlen(&sub) as i64;
    if len < slen {
        return Ok(None);
    }
    let pos = if len - pos < slen { len - slen } else { pos };
    if len == 0 {
        return Ok(Some(pos as usize));
    }
    if pos == 0 {
        return Ok(b.starts_with(&sub).then_some(0));
    }
    let s = nth(b, 0, pos as usize);
    Ok(utf8::rsearch(b, &sub, s))
}

fn index(r: &Recv, args: &[Value]) -> Result<Value, String> {
    let mut pos = 0;
    if let Some(p) = args.get(1) {
        let slen = utf8::strlen(&r.b) as i64;
        pos = to_int(p)?;
        if pos < 0 {
            pos += slen;
            if pos < 0 {
                return Ok(Value::Undef);
            }
        } else if pos > slen {
            return Ok(Value::Undef);
        }
    }
    if is_regexp(&args[0]) {
        return Err(invalid_byte_sequence());
    }
    Ok(match strseq_index(&r.b, &args[0], pos, false)? {
        Some(at) => Value::Int(sublen(&r.b, at) as i64),
        None => Value::Undef,
    })
}

fn rindex(r: &Recv, args: &[Value]) -> Result<Value, String> {
    let len = utf8::strlen(&r.b) as i64;
    let mut pos = len;
    if let Some(p) = args.get(1) {
        pos = to_int(p)?;
        if pos < 0 {
            pos += len;
            if pos < 0 {
                return Ok(Value::Undef);
            }
        }
        pos = pos.min(len);
    }
    if is_regexp(&args[0]) {
        return Err(invalid_byte_sequence());
    }
    Ok(match str_rindex(&r.b, &args[0], pos)? {
        Some(at) => Value::Int(sublen(&r.b, at) as i64),
        None => Value::Undef,
    })
}

/// `str_ensure_byte_pos`.
fn ensure_byte_pos(b: &[u8], pos: i64) -> Result<(), String> {
    if !single_byte(b) && !utf8::at_char_boundary(b, 0, pos as usize) {
        return Err(raise_exc(
            "IndexError",
            &format!("offset {pos} does not land on character boundary"),
        ));
    }
    Ok(())
}

/// A Regexp searched from a byte offset: a text receiver's is answered by the
/// text method (`None`) once the offset is checked; a broken one's refuses.
fn regexp_from_byte(r: &Recv, pat: &Value) -> Result<Option<()>, String> {
    if !is_regexp(pat) {
        return Ok(Some(()));
    }
    if broken(&r.b) {
        return Err(invalid_byte_sequence());
    }
    Ok(None)
}

fn byteindex(r: &Recv, args: &[Value]) -> Result<Option<Value>, String> {
    let mut pos = 0;
    if let Some(p) = args.get(1) {
        let slen = r.b.len() as i64;
        pos = to_int(p)?;
        if pos < 0 {
            pos += slen;
            if pos < 0 {
                return Ok(Some(Value::Undef));
            }
        } else if pos > slen {
            return Ok(Some(Value::Undef));
        }
    }
    ensure_byte_pos(&r.b, pos)?;
    if regexp_from_byte(r, &args[0])?.is_none() {
        return Ok(None);
    }
    Ok(Some(match strseq_index(&r.b, &args[0], pos, true)? {
        Some(at) => Value::Int(at as i64),
        None => Value::Undef,
    }))
}

fn byterindex(r: &Recv, args: &[Value]) -> Result<Option<Value>, String> {
    let len = r.b.len() as i64;
    let mut pos = len;
    if let Some(p) = args.get(1) {
        pos = to_int(p)?;
        if pos < 0 {
            pos += len;
            if pos < 0 {
                return Ok(Some(Value::Undef));
            }
        }
        pos = pos.min(len);
    }
    ensure_byte_pos(&r.b, pos)?;
    if regexp_from_byte(r, &args[0])?.is_none() {
        return Ok(None);
    }
    let sub = operand(&args[0])?;
    if broken(&sub) || r.b.len() < sub.len() {
        return Ok(Some(Value::Undef));
    }
    let pos = pos.min(len - sub.len() as i64) as usize;
    if r.b.is_empty() {
        return Ok(Some(Value::Int(pos as i64)));
    }
    let hit = if pos == 0 {
        r.b.starts_with(&sub).then_some(0)
    } else {
        utf8::rsearch(&r.b, &sub, pos)
    };
    Ok(Some(hit.map_or(Value::Undef, |h| Value::Int(h as i64))))
}

/// `str_check_beg_len`: a byte span of `b`, its ends on character heads.
fn check_beg_len(b: &[u8], beg: i64, len: i64) -> Result<(usize, usize), String> {
    let slen = b.len() as i64;
    if len < 0 {
        return Err(raise_exc("IndexError", &format!("negative length {len}")));
    }
    if slen < beg || (beg < 0 && beg + slen < 0) {
        return Err(raise_exc(
            "IndexError",
            &format!("index {beg} out of string"),
        ));
    }
    let beg = if beg < 0 { beg + slen } else { beg };
    let len = len.min(slen - beg);
    ensure_byte_pos(b, beg)?;
    ensure_byte_pos(b, beg + len)?;
    Ok((beg as usize, len as usize))
}

/// `rb_str_bytesplice`.
fn bytesplice(r: &Recv, args: &[Value]) -> Result<Value, String> {
    let argc = args.len();
    if !matches!(argc, 2 | 3 | 5) {
        return Err(raise_exc(
            "ArgumentError",
            &format!("wrong number of arguments (given {argc}, expected 2, 3, or 5)"),
        ));
    }
    let range = |v: &Value, len: usize| -> Result<(i64, i64), String> {
        match range_beg_len(v, len as i64, 2)? {
            Some(Some(span)) => Ok(span),
            _ => Err(raise_exc(
                "TypeError",
                &format!(
                    "wrong argument type {} (expected Range)",
                    with_host(|h| h.class_of(v).to_string())
                ),
            )),
        }
    };
    let by_range = argc == 2 || (argc == 3 && !matches!(args[0], Value::Int(_)));
    let (beg, len, val, vbeg, vlen);
    if by_range {
        (beg, len) = range(&args[0], r.b.len())?;
        val = operand(&args[1])?;
        (vbeg, vlen) = if argc == 2 {
            (0, val.len() as i64)
        } else {
            range(&args[2], val.len())?
        };
    } else {
        beg = to_int(&args[0])?;
        len = to_int(&args[1])?;
        val = operand(&args[2])?;
        (vbeg, vlen) = if argc == 3 {
            (0, val.len() as i64)
        } else {
            (to_int(&args[3])?, to_int(&args[4])?)
        };
    }
    let (beg, len) = check_beg_len(&r.b, beg, len)?;
    let (vbeg, vlen) = check_beg_len(&val, vbeg, vlen)?;
    let mut b = r.b.clone();
    b.splice(beg..beg + len, val[vbeg..vbeg + vlen].iter().copied());
    store(r.v, b);
    Ok(r.v.clone())
}

fn start_with(b: &[u8], args: &[Value]) -> Result<Value, String> {
    for a in args {
        if is_regexp(a) {
            return Err(invalid_byte_sequence());
        }
        let t = operand(a)?;
        if t.is_empty() {
            return Ok(Value::Bool(true));
        }
        if b.len() < t.len() || !utf8::at_char_right_boundary(b, 0, t.len()) {
            continue;
        }
        if b.starts_with(&t) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn end_with(b: &[u8], args: &[Value]) -> Result<Value, String> {
    for a in args {
        let t = operand(a)?;
        if t.is_empty() {
            return Ok(Value::Bool(true));
        }
        if b.len() < t.len() {
            continue;
        }
        let s = b.len() - t.len();
        if utf8::at_char_boundary(b, 0, s) && b.ends_with(&t) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// `deleted_prefix_length`.
fn deleted_prefix_length(b: &[u8], prefix: &Value) -> Result<usize, String> {
    let p = operand(prefix)?;
    if p.is_empty() || b.len() < p.len() || !b.starts_with(&p) {
        return Ok(0);
    }
    if broken(&p) && (!broken(b) || !utf8::at_char_right_boundary(b, 0, p.len())) {
        return Ok(0);
    }
    Ok(p.len())
}

/// `deleted_suffix_length`.
fn deleted_suffix_length(b: &[u8], suffix: &Value) -> Result<usize, String> {
    let s = operand(suffix)?;
    if broken(&s) || s.is_empty() || b.len() < s.len() {
        return Ok(0);
    }
    let before = b.len() - s.len();
    if !b.ends_with(&s) || !utf8::at_char_boundary(b, 0, before) {
        return Ok(0);
    }
    Ok(s.len())
}

fn partition(r: &Recv, sep: &Value) -> Result<Value, String> {
    if is_regexp(sep) {
        return Err(invalid_byte_sequence());
    }
    let sep_b = operand(sep)?;
    Ok(match strseq_index(&r.b, sep, 0, false)? {
        Some(pos) => new_arr(vec![
            out(r.b[..pos].to_vec()),
            sep.clone(),
            out(r.b[pos + sep_b.len()..].to_vec()),
        ]),
        None => new_arr(vec![
            out(r.b.clone()),
            new_str(String::new()),
            new_str(String::new()),
        ]),
    })
}

fn rpartition(r: &Recv, sep: &Value) -> Result<Value, String> {
    if is_regexp(sep) {
        return Err(invalid_byte_sequence());
    }
    let sep_b = operand(sep)?;
    let pos = utf8::strlen(&r.b) as i64;
    Ok(match str_rindex(&r.b, sep, pos)? {
        Some(pos) => new_arr(vec![
            out(r.b[..pos].to_vec()),
            sep.clone(),
            out(r.b[pos + sep_b.len()..].to_vec()),
        ]),
        None => new_arr(vec![
            new_str(String::new()),
            new_str(String::new()),
            out(r.b.clone()),
        ]),
    })
}

// ---- reading and writing spans ----------------------------------------------

/// `rb_str_aref_m`.
fn aref(r: &Recv, args: &[Value]) -> Result<Value, String> {
    let some = |o: Option<Vec<u8>>| o.map_or(Value::Undef, out);
    if is_regexp(&args[0]) {
        return Err(invalid_byte_sequence());
    }
    if args.len() == 2 {
        return Ok(some(substr(
            &r.b,
            to_int(&args[0])?,
            to_int(&args[1])?,
            true,
        )));
    }
    let indx = &args[0];
    if let Value::Int(i) = indx {
        return Ok(some(substr(&r.b, *i, 1, false)));
    }
    if string_operand(indx).is_some() {
        return Ok(match strseq_index(&r.b, indx, 0, false)? {
            Some(_) => with_host(|h| h.dup_value(indx)),
            None => Value::Undef,
        });
    }
    match range_beg_len(indx, utf8::strlen(&r.b) as i64, 0)? {
        Some(None) => Ok(Value::Undef),
        Some(Some((beg, len))) => Ok(some(substr(&r.b, beg, len, true))),
        None => Ok(some(substr(&r.b, to_int(indx)?, 1, false))),
    }
}

/// `rb_str_slice_bang`.
fn slice_bang(r: &Recv, args: &[Value]) -> Result<Value, String> {
    if is_regexp(&args[0]) {
        return Err(invalid_byte_sequence());
    }
    let indx = &args[0];
    let (beg, len, removed) = if args.len() == 2 {
        match subpos(&r.b, to_int(indx)?, to_int(&args[1])?) {
            Some((p, n)) => (p, n, None),
            None => return Ok(Value::Undef),
        }
    } else if let Value::Int(i) = indx {
        match subpos(&r.b, *i, 1) {
            Some((_, 0)) | None => return Ok(Value::Undef),
            Some((p, n)) => (p, n, None),
        }
    } else if string_operand(indx).is_some() {
        match strseq_index(&r.b, indx, 0, false)? {
            Some(p) => (
                p,
                operand(indx)?.len(),
                Some(with_host(|h| h.dup_value(indx))),
            ),
            None => return Ok(Value::Undef),
        }
    } else {
        match range_beg_len(indx, utf8::strlen(&r.b) as i64, 0)? {
            Some(None) => return Ok(Value::Undef),
            Some(Some((beg, len))) => match subpos(&r.b, beg, len) {
                Some((p, n)) => (p, n, None),
                None => return Ok(Value::Undef),
            },
            None => match subpos(&r.b, to_int(indx)?, 1) {
                Some((_, 0)) | None => return Ok(Value::Undef),
                Some((p, n)) => (p, n, None),
            },
        }
    };
    let mut rest = r.b.clone();
    let cut: Vec<u8> = rest.drain(beg..beg + len).collect();
    store(r.v, rest);
    Ok(removed.unwrap_or_else(|| out(cut)))
}

/// `rb_str_update`: replace `len` characters from character `beg` with `val`.
fn update(r: &Recv, beg: i64, len: i64, val: &Value) -> Result<(), String> {
    if len < 0 {
        return Err(raise_exc("IndexError", &format!("negative length {len}")));
    }
    let v = operand(val)?;
    // The receiver may have changed since `r` was read (`insert` re-reads).
    let b = with_host(|h| h.string_bytes(r.v));
    let slen = utf8::strlen(&b) as i64;
    if slen < beg || (beg < 0 && beg + slen < 0) {
        return Err(raise_exc(
            "IndexError",
            &format!("index {beg} out of string"),
        ));
    }
    let beg = if beg < 0 { beg + slen } else { beg };
    let len = len.min(slen - beg);
    let p = nth(&b, 0, beg as usize);
    let e = nth(&b, p, len as usize);
    let mut nb = b[..p].to_vec();
    nb.extend(&v);
    nb.extend(&b[e..]);
    store(r.v, nb);
    Ok(())
}

/// `rb_str_aset_m`.
fn aset(r: &Recv, args: &[Value]) -> Result<Value, String> {
    if is_regexp(&args[0]) {
        return Err(invalid_byte_sequence());
    }
    if args.len() == 3 {
        update(r, to_int(&args[0])?, to_int(&args[1])?, &args[2])?;
        return Ok(args[2].clone());
    }
    let (indx, val) = (&args[0], &args[1]);
    if let Value::Int(i) = indx {
        update(r, *i, 1, val)?;
    } else if string_operand(indx).is_some() {
        let Some(beg) = strseq_index(&r.b, indx, 0, false)? else {
            return Err(raise_exc("IndexError", "string not matched"));
        };
        let n = utf8::strlen(&operand(indx)?) as i64;
        update(r, sublen(&r.b, beg) as i64, n, val)?;
    } else {
        match range_beg_len(indx, utf8::strlen(&r.b) as i64, 2)? {
            Some(Some((beg, len))) => update(r, beg, len, val)?,
            _ => update(r, to_int(indx)?, 1, val)?,
        }
    }
    Ok(val.clone())
}

/// `<<` / `concat`: each argument's bytes (an Integer is a codepoint).
fn append(r: &Recv, args: &[Value]) -> Result<(), String> {
    let mut b = with_host(|h| h.string_bytes(r.v));
    for a in args {
        if let Value::Int(_) = a {
            b.extend(str_append_operand(a)?.into_bytes());
        } else {
            b.extend(operand(a)?);
        }
    }
    store(r.v, b);
    Ok(())
}

/// `rb_str_justify`.
fn justify(b: &[u8], name: &str, args: &[Value]) -> Result<Value, String> {
    let width = to_int(&args[0])?;
    let pad = match args.get(1) {
        Some(p) => operand(p)?,
        None => b" ".to_vec(),
    };
    let fclen = utf8::strlen(&pad) as i64;
    if pad.is_empty() || fclen == 0 {
        return Err(raise_exc("ArgumentError", "zero width padding"));
    }
    let len = utf8::strlen(b) as i64;
    if width < 0 || len >= width {
        return Ok(out(b.to_vec()));
    }
    let n = width - len;
    let llen = match name {
        "ljust" => 0,
        "rjust" => n,
        _ => n / 2,
    };
    let rlen = n - llen;
    let fill = |count: i64| -> Vec<u8> {
        let mut f = Vec::new();
        for _ in 0..count / fclen {
            f.extend(&pad);
        }
        f.extend(&pad[..utf8::offset(&pad, (count % fclen) as usize)]);
        f
    };
    let mut res = fill(llen);
    res.extend(b);
    res.extend(fill(rlen));
    Ok(out(res))
}

/// `rb_str_cmp`: bytewise, and two equal byte runs under labels that cannot
/// compare are ordered by encoding (UTF-8 above ASCII-8BIT, below US-ASCII).
fn str_cmp(a: &[u8], b: &[u8], b_enc: Option<&str>) -> i64 {
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Equal => {
            let comparable = a.is_ascii() || b_enc.map_or(true, |e| e == BROKEN_UTF8);
            match (comparable, b_enc) {
                (true, _) => 0,
                (false, Some("ASCII-8BIT")) => 1,
                (false, _) => -1,
            }
        }
    }
}

/// `str_casecmp`: ASCII letters fold, anything else compares by bytes.
fn casecmp(a: &[u8], other: &Value) -> Result<Value, String> {
    let Some((b, enc)) = string_operand(other) else {
        return Ok(Value::Undef);
    };
    if matches!(enc, Some(e) if e != BROKEN_UTF8) && !b.is_ascii() && !a.is_ascii() {
        return Ok(Value::Undef);
    }
    let (mut p1, mut p2) = (0, 0);
    while p1 < a.len() && p2 < b.len() {
        let (c1, c2) = (a[p1], b[p2]);
        let (l1, l2);
        if c1.is_ascii() && c2.is_ascii() {
            let (f1, f2) = (c1.to_ascii_lowercase(), c2.to_ascii_lowercase());
            if f1 != f2 {
                return Ok(Value::Int(if f1 < f2 { -1 } else { 1 }));
            }
            (l1, l2) = (1, 1);
        } else {
            l1 = utf8::mbclen(a, p1);
            l2 = utf8::mbclen(&b, p2);
            let n = l1.min(l2);
            match a[p1..p1 + n].cmp(&b[p2..p2 + n]) {
                std::cmp::Ordering::Less => return Ok(Value::Int(-1)),
                std::cmp::Ordering::Greater => return Ok(Value::Int(1)),
                std::cmp::Ordering::Equal if l1 != l2 => {
                    return Ok(Value::Int(if l1 < l2 { -1 } else { 1 }))
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        p1 += l1;
        p2 += l2;
    }
    Ok(Value::Int(match (p1 == a.len(), p2 == b.len()) {
        (true, true) => 0,
        (true, false) => -1,
        _ => 1,
    }))
}

// ---- lines ------------------------------------------------------------------

/// `rb_str_enumerate_lines` (`lines` / `each_line`).
fn lines(r: &Recv, name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let (opts, rest): (Option<&Value>, &[Value]) = match args.last() {
        Some(last) if with_host(|h| h.as_hash(last)).is_some() => {
            (Some(last), &args[..args.len() - 1])
        }
        _ => (None, args),
    };
    let chomp = optional_kwargs(opts, &["chomp"])?[0]
        .as_ref()
        .is_some_and(|v| with_host(|h| h.truthy(v)));
    let mut items = Vec::new();
    let b = &r.b;
    match rest.first() {
        Some(Value::Undef) => items.push(out(b.clone())),
        _ if b.is_empty() => {}
        rs => {
            let (rs, default) = match rs {
                Some(v) => (operand(v)?, false),
                None => (b"\n".to_vec(), true),
            };
            let piece = |from: usize, to: usize| out(b[from..to].to_vec());
            if rs.is_empty() {
                paragraphs(b, chomp, &mut |from, to| items.push(piece(from, to)));
            } else {
                let rsnewline = default || rs == b"\n";
                let mut sub = 0;
                while sub < b.len() {
                    let Some(hit) = find(&b[sub..], &rs).map(|p| sub + p) else {
                        break;
                    };
                    let adjusted = utf8::right_head(b, sub, hit);
                    if adjusted != hit {
                        sub = adjusted;
                        continue;
                    }
                    let next = hit + rs.len();
                    let mut end = next;
                    if chomp {
                        end = if rsnewline {
                            chomp_newline(b, sub, end)
                        } else {
                            end - rs.len()
                        };
                    }
                    items.push(piece(sub, end));
                    sub = next;
                }
                if sub != b.len() {
                    let mut end = b.len();
                    if chomp {
                        if rsnewline {
                            end = chomp_newline(b, sub, end);
                        } else if end - sub >= rs.len() && b[..end].ends_with(&rs) {
                            end -= rs.len();
                        }
                    }
                    items.push(piece(sub, end));
                }
            }
        }
    }
    match block {
        Some(bl) => {
            for l in items {
                call_proc(&bl, &[l])?;
            }
            Ok(r.v.clone())
        }
        None if name == "lines" => Ok(new_arr(items)),
        None => Ok(with_host(|h| h.new_enumerator_of(items, name, r.v.clone()))),
    }
}

/// `chomp_newline`: `e` with one trailing `\n` or `\r\n` taken off.
fn chomp_newline(b: &[u8], p: usize, mut e: usize) -> usize {
    if e > p && b[e - 1] == b'\n' {
        e -= 1;
        if e > p && b[e - 1] == b'\r' {
            e -= 1;
        }
    }
    e
}

/// The paragraph mode of `rb_str_enumerate_lines` (`lines("")`): each run of
/// lines ends at a blank line, the run of newlines kept with it (or, with
/// `chomp`, taken off).
fn paragraphs(b: &[u8], chomp: bool, emit: &mut dyn FnMut(usize, usize)) {
    let pend = b.len();
    let mut subend = 0;
    let mut subptr = Some(0);
    let mut eol: Option<usize> = None;
    while subend < pend {
        let mut chomp_rslen: i64 = 0;
        let mut rslen: usize;
        loop {
            let n = usize::from(b[subend] == b'\r');
            rslen = n + if subend + n < pend {
                utf8::mbclen(b, subend + n)
            } else {
                0
            };
            if b.get(subend + n) == Some(&b'\n') {
                if eol == Some(subend) {
                    break;
                }
                subend += rslen;
                if subptr.is_some() {
                    eol = Some(subend);
                    chomp_rslen = -(rslen as i64);
                }
            } else {
                if subptr.is_none() {
                    subptr = Some(subend);
                }
                subend += rslen;
            }
            rslen = 0;
            if subend >= pend {
                break;
            }
        }
        let Some(from) = subptr else {
            break;
        };
        if rslen == 0 {
            chomp_rslen = 0;
        }
        let extra = if chomp { chomp_rslen } else { rslen as i64 };
        emit(from, (subend as i64 + extra) as usize);
        subptr = None;
        eol = None;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---- substitution -----------------------------------------------------------

/// One match a substitution replaces: the byte span of every group (group 0
/// the whole match), and the pattern's group names.
struct Hit {
    groups: Vec<Option<(usize, usize)>>,
    names: Vec<(String, usize)>,
    /// Whether `\1`..`\9` refer to groups (a Regexp without named groups).
    numbered: bool,
}

/// `rb_str_sub_bang` / `str_gsub` over the receiver's bytes, for a String
/// pattern, or a Regexp on a text receiver.
fn sub(r: &Recv, name: &str, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let all = name.starts_with('g');
    let bang = name.ends_with('!');
    let b = &r.b;
    let pat = &args[0];
    let hash = args
        .get(1)
        .filter(|v| with_host(|h| h.as_hash(v)).is_some())
        .cloned();
    let repl = match (args.get(1), &hash) {
        (Some(v), None) => Some(operand(v)?),
        _ => None,
    };
    let hits = if is_regexp(pat) {
        regexp_hits(b, pat, all)?
    } else {
        let p = operand(pat)?;
        if broken(&p) {
            let shown = with_host(|h| h.inspect(pat));
            return Err(raise_exc(
                "RegexpError",
                &format!(
                    "invalid multibyte character: /{}/",
                    &shown[1..shown.len() - 1]
                ),
            ));
        }
        string_hits(b, &p, all)
    };
    // A String pattern's match is `rb_backref_set_string`'s MatchData over
    // the receiver's bytes; a Regexp's `$~` was set by `regexp_hits`.
    let string_md = |hit: &Hit| {
        let span = hit.groups[0].expect("group 0 always matches");
        with_host(|h| h.new_string_matchdata_bytes(b, span, pat.clone()))
    };
    // A block-less `gsub(pattern)` is the Enumerator of what each match
    // would hand a block, each element carrying its `$~`.
    if all && args.len() == 1 && block.is_none() {
        let items = hits.iter().map(|hit| match0(b, pat, hit)).collect();
        let e = with_host(|h| h.new_enumerator_of(items, name, r.v.clone()));
        if !is_regexp(pat) {
            let mds = hits.iter().map(string_md).collect();
            with_host(|h| h.set_enum_matches(&e, mds));
        }
        return Ok(e);
    }
    if hits.is_empty() {
        if !is_regexp(pat) {
            with_host(|h| h.set_global("~", Value::Undef));
        }
        return Ok(if bang { Value::Undef } else { out(b.clone()) });
    }
    let mut dest = Vec::with_capacity(b.len());
    let mut offset = 0;
    for (i, hit) in hits.iter().enumerate() {
        let (beg0, end0) = hit.groups[0].expect("group 0 always matches");
        // MRI sets `$~` before each replacement is computed; only a block or
        // a Hash (its default proc) can see any but the last.
        if !is_regexp(pat) && (block.is_some() || hash.is_some() || i + 1 == hits.len()) {
            let md = string_md(hit);
            with_host(|h| h.set_global("~", md));
        }
        let val = if let Some(bl) = &block {
            as_string_bytes(&call_proc(bl, &[match0(b, pat, hit)])?)?
        } else if let Some(h) = &hash {
            let key = out(b[beg0..end0].to_vec());
            as_string_bytes(&super::dispatch(h, "[]", &[key], None)?)?
        } else {
            regsub(repl.as_deref().unwrap_or_default(), b, hit)?
        };
        dest.extend(&b[offset..beg0]);
        dest.extend(val);
        offset = end0;
    }
    dest.extend(&b[offset..]);
    if bang {
        store(r.v, dest);
        return Ok(r.v.clone());
    }
    Ok(out(dest))
}

/// What a substitution's block is handed for `hit`: the matched text, or for
/// a String pattern the pattern itself.
fn match0(b: &[u8], pat: &Value, hit: &Hit) -> Value {
    if !is_regexp(pat) {
        return pat.clone();
    }
    let (beg, end) = hit.groups[0].expect("group 0 always matches");
    out(b[beg..end].to_vec())
}

/// Every place `str_gsub` replaces a String pattern (`rb_str_byteindex`),
/// an empty pattern stepping one character (`rb_enc_fast_mbclen`) past each.
fn string_hits(b: &[u8], p: &[u8], all: bool) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut offset = 0;
    while offset <= b.len() {
        let found = if p.is_empty() {
            Some(offset)
        } else {
            utf8::search(b, p, offset)
        };
        let Some(beg) = found else {
            break;
        };
        let end = beg + p.len();
        hits.push(Hit {
            groups: vec![Some((beg, end))],
            names: Vec::new(),
            numbered: false,
        });
        if !all {
            break;
        }
        offset = if beg == end {
            if end >= b.len() {
                break;
            }
            end + utf8::fast_mbclen(b, end)
        } else {
            end
        };
    }
    hits
}

/// The matches of a Regexp in a text receiver (a broken receiver refuses a
/// Regexp before reaching here); `$~` is each in turn, the last at the end.
fn regexp_hits(b: &[u8], pat: &Value, all: bool) -> Result<Vec<Hit>, String> {
    let Ok(s) = std::str::from_utf8(b) else {
        return Err(invalid_byte_sequence());
    };
    let re = str_regex(pat).expect("a Regexp pattern");
    let names = capture_name_map(&re);
    let matches = ruby_matches(&re, s);
    let take = if all {
        matches.len()
    } else {
        matches.len().min(1)
    };
    let mut hits = Vec::new();
    for caps in matches.iter().take(take) {
        set_match_globals(Some((caps, s)), &re, pat);
        hits.push(Hit {
            groups: (0..caps.len())
                .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
                .collect(),
            names: names.clone(),
            numbered: names.is_empty(),
        });
    }
    if hits.is_empty() {
        set_match_globals(None, &re, pat);
    }
    Ok(hits)
}

/// `rb_reg_regsub`: expand a replacement's back-references against `src`.
fn regsub(repl: &[u8], src: &[u8], hit: &Hit) -> Result<Vec<u8>, String> {
    let group = |no: usize| hit.groups.get(no).copied().flatten();
    let mut val = Vec::with_capacity(repl.len());
    let mut s = 0;
    while s < repl.len() {
        let c = repl[s];
        if !c.is_ascii() {
            let n = utf8::mbclen(repl, s);
            val.extend(&repl[s..s + n]);
            s += n;
            continue;
        }
        s += 1;
        if c != b'\\' || s == repl.len() {
            val.push(c);
            continue;
        }
        let c = repl[s];
        if !c.is_ascii() {
            let n = utf8::mbclen(repl, s);
            val.push(b'\\');
            val.extend(&repl[s..s + n]);
            s += n;
            continue;
        }
        s += 1;
        let no = match c {
            b'1'..=b'9' if hit.numbered => Some(usize::from(c - b'0')),
            b'1'..=b'9' => None,
            b'k' if repl.get(s) == Some(&b'<') => {
                let Some(close) = repl[s + 1..].iter().position(|&x| x == b'>') else {
                    return Err(raise_exc(
                        "RuntimeError",
                        "invalid group name reference format",
                    ));
                };
                let name = String::from_utf8_lossy(&repl[s + 1..s + 1 + close]).into_owned();
                s += close + 2;
                match hit.names.iter().find(|(n, _)| *n == name) {
                    Some((_, i)) => Some(*i),
                    None => {
                        return Err(raise_exc(
                            "IndexError",
                            &format!("undefined group name reference: {name}"),
                        ))
                    }
                }
            }
            b'0' | b'&' => Some(0),
            b'`' => {
                val.extend(&src[..group(0).map_or(0, |g| g.0)]);
                None
            }
            b'\'' => {
                val.extend(&src[group(0).map_or(src.len(), |g| g.1)..]);
                None
            }
            b'+' => (1..hit.groups.len()).rev().find(|&i| group(i).is_some()),
            b'\\' => {
                val.push(b'\\');
                None
            }
            _ => {
                val.push(b'\\');
                val.push(c);
                None
            }
        };
        if let Some((from, to)) = no.and_then(group) {
            val.extend(&src[from..to]);
        }
    }
    Ok(val)
}

// ---- succession ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Neighbor {
    Found,
    Wrapped,
    NotChar,
}

fn found_len(p: &[u8]) -> Option<usize> {
    match utf8::precise(p) {
        utf8::Mbc::Found(n) => Some(n),
        _ => None,
    }
}

fn codepoint(p: &[u8]) -> u32 {
    std::str::from_utf8(p)
        .ok()
        .and_then(|s| s.chars().next())
        .map_or(0, u32::from)
}

/// `enc_succ_char` / `enc_pred_char` for UTF-8: step the bytes of `p` to the
/// next (`up`) or previous character of the same length.
fn step_char(p: &mut [u8], up: bool) -> Neighbor {
    let len = p.len();
    let (wrap_from, wrap_to) = if up { (0xFF, 0x00) } else { (0x00, 0xFF) };
    loop {
        let mut i = len;
        while i > 0 && p[i - 1] == wrap_from {
            p[i - 1] = wrap_to;
            i -= 1;
        }
        if i == 0 {
            return Neighbor::Wrapped;
        }
        let i = i - 1;
        p[i] = if up { p[i] + 1 } else { p[i] - 1 };
        let l = utf8::precise(p);
        if let utf8::Mbc::Found(l) = l {
            if l == len {
                return Neighbor::Found;
            }
            p[l..].fill(wrap_from);
        }
        if l == utf8::Mbc::Invalid && i < len - 1 {
            let mut len2 = len - 1;
            while len2 > 0 && utf8::precise(&p[..len2]) == utf8::Mbc::Invalid {
                len2 -= 1;
            }
            p[len2 + 1..].fill(wrap_from);
        }
    }
}

/// `enc_succ_alnum_char`: the next digit or letter of the same kind, or
/// `Wrapped` with the first of the run in `carry` (`"z"` carries `"a"`,
/// `"9"` carries `"1"`).
fn succ_alnum_char(p: &mut [u8], carry: &mut Vec<u8>) -> Neighbor {
    let c = codepoint(p);
    let digit = crate::unicode_tables::is_digit(c);
    if !digit && !crate::unicode_tables::is_alpha(c) {
        return Neighbor::NotChar;
    }
    let same_kind = |p: &[u8]| {
        let c = codepoint(p);
        if digit {
            crate::unicode_tables::is_digit(c)
        } else {
            crate::unicode_tables::is_alpha(c)
        }
    };
    let save = p.to_vec();
    for _ in 0..=1 {
        if step_char(p, true) == Neighbor::Found && same_kind(p) {
            return Neighbor::Found;
        }
    }
    p.copy_from_slice(&save);
    let mut range = 1;
    loop {
        let save = p.to_vec();
        if step_char(p, false) != Neighbor::Found || !same_kind(p) {
            p.copy_from_slice(&save);
            break;
        }
        range += 1;
    }
    if range == 1 {
        return Neighbor::NotChar;
    }
    *carry = p.to_vec();
    if digit {
        step_char(carry, true);
    }
    Neighbor::Wrapped
}

/// `str_succ` over UTF-8 bytes.
fn str_succ(mut s: Vec<u8>) -> Vec<u8> {
    if s.is_empty() {
        return s;
    }
    let e = s.len();
    let mut carry = vec![1u8];
    let mut carry_pos = 0;
    let mut last_alnum: Option<usize> = None;
    let mut found_alnum = false;
    let mut neighbor = Neighbor::Found;
    let mut at = e;
    while let Some(p) = utf8::prev_char(&s, 0, at) {
        at = p;
        if let (Neighbor::NotChar, Some(la)) = (neighbor, last_alnum) {
            let (l, c) = (s[la], s[p]);
            let breaks = if l.is_ascii_alphabetic() {
                c.is_ascii_digit()
            } else {
                l.is_ascii_digit() && c.is_ascii_alphabetic()
            };
            if breaks {
                break;
            }
        }
        let Some(l) = found_len(&s[p..e]) else {
            continue;
        };
        neighbor = succ_alnum_char(&mut s[p..p + l], &mut carry);
        match neighbor {
            Neighbor::NotChar => continue,
            Neighbor::Found => return s,
            Neighbor::Wrapped => last_alnum = Some(p),
        }
        found_alnum = true;
        carry_pos = p;
    }
    if !found_alnum {
        let mut at = e;
        while let Some(p) = utf8::prev_char(&s, 0, at) {
            at = p;
            let Some(l) = found_len(&s[p..e]) else {
                continue;
            };
            let mut tmp = s[p..p + l].to_vec();
            match step_char(&mut tmp, true) {
                Neighbor::Found => {
                    s[p..p + l].copy_from_slice(&tmp);
                    return s;
                }
                Neighbor::Wrapped => s[p..p + l].copy_from_slice(&tmp),
                Neighbor::NotChar => {}
            }
            if found_len(&s[p..p + l]) != Some(l) {
                step_char(&mut s[p..p + l], true);
            }
            carry_pos = p;
        }
    }
    s.splice(carry_pos..carry_pos, carry);
    s
}

/// `rb_str_upto_each` for a receiver that is not ASCII (so neither of MRI's
/// single-character and all-digit shortcuts applies).
fn upto(r: &Recv, args: &[Value], block: Option<Value>) -> Result<Value, String> {
    let end = operand(&args[0])?;
    let excl = args.get(1).is_some_and(|v| with_host(|h| h.truthy(v)));
    let mut items = Vec::new();
    let n = str_cmp(&r.b, &end, None);
    if !(n > 0 || (excl && n == 0)) {
        let after_end = str_succ(end.clone());
        let mut current = r.b.clone();
        while current != after_end {
            let next = (excl || current != end).then(|| str_succ(current.clone()));
            items.push(out(current.clone()));
            let Some(next) = next else {
                break;
            };
            current = next;
            if (excl && current == end) || current.len() > end.len() || current.is_empty() {
                break;
            }
        }
    }
    match block {
        Some(bl) => {
            for x in items {
                call_proc(&bl, &[x])?;
            }
            Ok(r.v.clone())
        }
        None => Ok(with_host(|h| {
            h.new_enumerator_of(items, "upto", r.v.clone())
        })),
    }
}
