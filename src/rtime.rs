//! Ruby `Time`: the value model and the formatting core, ported from MRI's
//! `time.c` and `strftime.c`.
//!
//! A `Time` is an exact rational number of seconds since the Unix epoch plus
//! the zone mode it is viewed in, as MRI's `time_object` holds `timew` and a
//! TZMODE. Exactness matters: `Time.at(1.1)` keeps the Float's exact binary
//! value (its `nsec` is `100000000`, its `inspect` shows the full fraction) and
//! `Time.at(Rational(1, 3))` keeps a third of a second.
//!
//! The local zone is the C library's (`localtime_r` / `mktime`, after `tzset`
//! so a runtime `ENV["TZ"] =` takes effect), as in MRI.

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::host::{civil_from_days, days_from_civil};

/// How a `Time` is viewed (MRI's TZMODE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    /// `Time.utc`, `#utc`, `in: "UTC"` / `"Z"` / `"-00:00"`.
    Utc,
    /// The process's local zone (`Time.now`, `Time.at`, `Time.local`).
    Local,
    /// A fixed offset in seconds east of UTC (`in: "+09:00"`, `localtime(3600)`).
    Fixed(i64),
}

/// An instant: exact epoch seconds and the zone it is viewed in.
#[derive(Debug, Clone)]
pub struct RTime {
    pub secs: BigRational,
    pub zone: Zone,
}

/// The broken-down fields of an `RTime` in its own zone (MRI's `struct vtm`).
#[derive(Debug, Clone)]
pub struct Vtm {
    pub year: i64,
    pub mon: i64,
    pub mday: i64,
    pub hour: i64,
    pub min: i64,
    pub sec: i64,
    /// 0 = Sunday.
    pub wday: i64,
    /// 1..=366.
    pub yday: i64,
    pub isdst: bool,
    pub utc_offset: i64,
    /// The zone abbreviation (`"EST"`); `None` for a fixed offset, which MRI
    /// leaves unnamed.
    pub zone: Option<String>,
    pub utc: bool,
    /// The fractional second, in `[0, 1)`.
    pub subsec: BigRational,
    /// Whole epoch seconds (floored), for `%s`.
    pub epoch: BigInt,
}

extern "C" {
    fn tzset();
}

/// The local zone's `(utc_offset, isdst, abbreviation)` at epoch second `t`.
pub fn local_info(t: i64) -> (i64, bool, String) {
    unsafe {
        tzset();
        let tt: libc::time_t = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() {
            return (0, false, "UTC".to_string());
        }
        let abbr = if tm.tm_zone.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(tm.tm_zone as *const libc::c_char)
                .to_string_lossy()
                .into_owned()
        };
        (tm.tm_gmtoff as i64, tm.tm_isdst > 0, abbr)
    }
}

/// The epoch second of a local wall-clock time (`mktime`, DST left to the C
/// library to resolve, as MRI's `timelocalw` does through it).
pub fn local_epoch(year: i64, mon: i64, mday: i64, hour: i64, min: i64, sec: i64) -> i64 {
    // Resolve through UTC first so an out-of-range day (Feb 30) or hour 24
    // normalizes before the zone lookup, then correct by the offset in force.
    let guess = utc_epoch(year, mon, mday, hour, min, sec);
    unsafe {
        tzset();
        let (y, mo, d, h, mi, s) = fields_of(guess);
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = (y - 1900) as libc::c_int;
        tm.tm_mon = (mo - 1) as libc::c_int;
        tm.tm_mday = d as libc::c_int;
        tm.tm_hour = h as libc::c_int;
        tm.tm_min = mi as libc::c_int;
        tm.tm_sec = s as libc::c_int;
        tm.tm_isdst = -1;
        let t = libc::mktime(&mut tm);
        // A wall-clock time the fall-back transition repeats names two
        // instants; MRI's `find_time_t` answers the LATER one (standard time),
        // where `mktime` may pick either. Every offset in force within a day
        // of the guess is tried, and the latest instant that really reads back
        // as this wall-clock time wins. A time skipped by the spring-forward
        // gap matches none, and keeps `mktime`'s answer, as MRI's does.
        let mut best: Option<i64> = None;
        for probe in [guess - 86_400, guess, guess + 86_400] {
            let off = local_info(probe).0;
            let candidate = guess - off;
            if local_info(candidate).0 == off {
                best = Some(best.map_or(candidate, |b| b.max(candidate)));
            }
        }
        match best {
            Some(b) => b,
            None if t == -1 => guess - local_info(guess).0,
            None => t as i64,
        }
    }
}

/// The epoch second of a UTC wall-clock time; out-of-range fields carry.
pub fn utc_epoch(year: i64, mon: i64, mday: i64, hour: i64, min: i64, sec: i64) -> i64 {
    days_from_civil(year, mon, 1) * 86_400 + (mday - 1) * 86_400 + hour * 3600 + min * 60 + sec
}

fn fields_of(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

impl RTime {
    pub fn new(secs: BigRational, zone: Zone) -> Self {
        RTime { secs, zone }
    }

    /// Whole epoch seconds, floored (`to_i`).
    pub fn floor_secs(&self) -> BigInt {
        self.secs.floor().to_integer()
    }

    /// The fractional second in `[0, 1)` (`subsec`).
    pub fn subsec(&self) -> BigRational {
        &self.secs - self.secs.floor()
    }

    pub fn is_utc(&self) -> bool {
        self.zone == Zone::Utc
    }

    /// `utc_offset` in seconds east of UTC.
    pub fn utc_offset(&self) -> i64 {
        match self.zone {
            Zone::Utc => 0,
            Zone::Fixed(o) => o,
            Zone::Local => local_info(self.floor_secs().to_i64().unwrap_or(0)).0,
        }
    }

    /// The broken-down fields in this time's own zone.
    pub fn vtm(&self) -> Vtm {
        let epoch = self.floor_secs();
        let e = epoch.to_i64().unwrap_or(0);
        let (utc_offset, isdst, zone) = match self.zone {
            Zone::Utc => (0, false, Some("UTC".to_string())),
            Zone::Fixed(o) => (o, false, None),
            Zone::Local => {
                let (o, dst, abbr) = local_info(e);
                (o, dst, Some(abbr))
            }
        };
        let local = e + utc_offset;
        let days = local.div_euclid(86_400);
        let rem = local.rem_euclid(86_400);
        let (year, mon, mday) = civil_from_days(days);
        Vtm {
            year,
            mon,
            mday,
            hour: rem / 3600,
            min: rem % 3600 / 60,
            sec: rem % 60,
            // 1970-01-01 was a Thursday.
            wday: (days.rem_euclid(7) + 4) % 7,
            yday: days - days_from_civil(year, 1, 1) + 1,
            isdst,
            utc_offset,
            zone,
            utc: self.is_utc(),
            subsec: self.subsec(),
            epoch,
        }
    }

    /// `Time#to_s`.
    pub fn to_s(&self) -> String {
        if self.is_utc() {
            strftime("%Y-%m-%d %H:%M:%S UTC", &self.vtm()).unwrap_or_default()
        } else {
            strftime("%Y-%m-%d %H:%M:%S %z", &self.vtm()).unwrap_or_default()
        }
    }

    /// `Time#inspect` (MRI `time_inspect`): a nanosecond-exact fraction is
    /// printed as decimal digits, anything finer as a Rational after a space.
    pub fn inspect(&self) -> String {
        let v = self.vtm();
        let mut out = strftime("%Y-%m-%d %H:%M:%S", &v).unwrap_or_default();
        let nanos = &v.subsec * BigRational::from_integer(BigInt::from(1_000_000_000));
        if !v.subsec.is_zero() {
            if nanos.is_integer() {
                let digits = format!(".{:09}", nanos.to_integer());
                out.push_str(digits.trim_end_matches('0'));
            } else {
                out.push_str(&format!(" {}/{}", v.subsec.numer(), v.subsec.denom()));
            }
        }
        if self.is_utc() {
            out.push_str(" UTC");
        } else {
            let off = v.utc_offset;
            let sign = if off < 0 { '-' } else { '+' };
            let off = off.abs();
            out.push_str(&format!(" {sign}{:02}{:02}", off / 3600, off % 3600 / 60));
            if off % 60 != 0 {
                out.push_str(&format!("{:02}", off % 60));
            }
        }
        out
    }

    /// `Time#xmlschema(digits)` / `#iso8601`.
    pub fn xmlschema(&self, digits: i64) -> String {
        let v = self.vtm();
        let mut out = if (-9999..=9999).contains(&v.year) {
            if v.year < 0 {
                format!("-{:04}", -v.year)
            } else {
                format!("{:04}", v.year)
            }
        } else {
            v.year.to_string()
        };
        out.push_str(&format!(
            "-{:02}-{:02}T{:02}:{:02}:{:02}",
            v.mon, v.mday, v.hour, v.min, v.sec
        ));
        if digits > 0 {
            let scaled = (&v.subsec
                * BigRational::from_integer(BigInt::from(10).pow(digits as u32)))
            .floor()
            .to_integer();
            out.push_str(&format!(
                ".{:0>width$}",
                scaled.to_string(),
                width = digits as usize
            ));
        }
        if self.is_utc() {
            out.push('Z');
        } else {
            let off = v.utc_offset;
            let sign = if off < 0 { '-' } else { '+' };
            let m = off.abs() / 60;
            out.push_str(&format!("{sign}{:02}:{:02}", m / 60, m % 60));
        }
        out
    }

    /// `round`/`floor`/`ceil(ndigits)` (MRI `time_round` & co): the instant
    /// moved to a multiple of `10**-ndigits` seconds. Half rounds up.
    pub fn rounded(&self, ndigits: i64, mode: RoundMode) -> RTime {
        let den = BigRational::new(BigInt::one(), BigInt::from(10).pow(ndigits.max(0) as u32));
        let v = &self.secs - (&self.secs / &den).floor() * &den;
        let up = match mode {
            RoundMode::Floor => false,
            RoundMode::Ceil => !v.is_zero(),
            RoundMode::Round => v >= &den / BigRational::from_integer(BigInt::from(2)),
        };
        let secs = if up {
            &self.secs + (&den - &v)
        } else {
            &self.secs - &v
        };
        RTime::new(secs, self.zone.clone())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RoundMode {
    Round,
    Floor,
    Ceil,
}

/// Parse a zone argument the way MRI's `utc_offset_arg` does for a String:
/// `Ok(Zone::Utc)` for `"UTC"`/`"Z"`/`"-00:00"`, a fixed offset for
/// `"+HH"`, `"+HHMM"`, `"+HH:MM"`, `"+HHMMSS"`, `"+HH:MM:SS"` or a military
/// letter, else the ArgumentError message.
pub fn parse_utc_offset(s: &str) -> Result<Zone, String> {
    let invalid = || {
        format!(
            "\"+HH:MM\", \"-HH:MM\", \"UTC\" or \"A\"..\"I\",\"K\"..\"Z\" expected for utc_offset: {s}"
        )
    };
    let b = s.as_bytes();
    let two = |i: usize| -> Option<i64> {
        let (x, y) = (*b.get(i)?, *b.get(i + 1)?);
        (x.is_ascii_digit() && y.is_ascii_digit()).then(|| ((x - b'0') * 10 + (y - b'0')) as i64)
    };
    let (min_at, sec_at) = match b.len() {
        1 => {
            let c = b[0];
            let n = match c {
                b'Z' => return Ok(Zone::Utc),
                b'A'..=b'I' => (c - b'A' + 1) as i64,
                b'K'..=b'M' => (c - b'A') as i64,
                b'N'..=b'Y' => b'M' as i64 - c as i64,
                _ => return Err(invalid()),
            };
            return Ok(Zone::Fixed(n * 3600));
        }
        3 if s.eq_ignore_ascii_case("UTC") => return Ok(Zone::Utc),
        3 => (None, None),
        5 => (Some(3), None),
        7 => (Some(3), Some(5)),
        6 if b[3] == b':' => (Some(4), None),
        9 if b[3] == b':' && b[6] == b':' => (Some(4), Some(7)),
        _ => return Err(invalid()),
    };
    let mut n = 0;
    for (at, unit) in [(sec_at, 1), (min_at, 60)] {
        if let Some(i) = at {
            if b[i] > b'5' {
                return Err(invalid());
            }
            n += two(i).ok_or_else(invalid)? * unit;
        }
    }
    if b[0] != b'+' && b[0] != b'-' {
        return Err(invalid());
    }
    n += two(1).ok_or_else(invalid)? * 3600;
    if b[0] == b'-' {
        if n == 0 {
            return Ok(Zone::Utc);
        }
        n = -n;
    }
    Ok(Zone::Fixed(n))
}

/// MRI's `validate_utc_offset`.
pub fn validate_offset(off: i64) -> Result<(), String> {
    if off <= -86_400 || off >= 86_400 {
        return Err("utc_offset out of range".to_string());
    }
    Ok(())
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

// strftime flag bits (`enum {LEFT, CHCASE, LOWER, UPPER}`).
const LEFT: u8 = 1;
const CHCASE: u8 = 2;
const LOWER: u8 = 4;
const UPPER: u8 = 8;

fn case_conv(s: &str, flags: u8) -> String {
    match flags & (UPPER | LOWER) {
        UPPER => s.to_ascii_uppercase(),
        LOWER => s.to_ascii_lowercase(),
        _ => s.to_string(),
    }
}

/// MRI `weeknumber`: weeks into the year, the week starting on Sunday
/// (`first = 0`) or Monday (`first = 1`).
fn weeknumber(v: &Vtm, first: i64) -> i64 {
    let mut wday = v.wday;
    if first == 1 {
        wday = if wday == 0 { 6 } else { wday - 1 };
    }
    ((v.yday - 1 + 7 - wday) / 7).max(0)
}

/// MRI `iso8601wknum`.
fn iso8601wknum(year: i64, mon: i64, mday: i64, wday: i64, yday0: i64) -> i64 {
    let mut wd = wday;
    wd = if wd == 0 { 6 } else { wd - 1 };
    let mut weeknum = ((yday0 + 7 - wd) / 7).max(0);
    let mut jan1day = wday - (yday0 % 7);
    if jan1day < 0 {
        jan1day += 7;
    }
    match jan1day {
        1 => {}
        2..=4 => weeknum += 1,
        _ => {
            if weeknum == 0 {
                let leap = crate::host::is_leap_year(year - 1);
                let dec31_wday = if jan1day == 0 { 6 } else { jan1day - 1 };
                weeknum = iso8601wknum(year - 1, 12, 31, dec31_wday, 364 + leap as i64);
            }
        }
    }
    if mon == 12
        && ((wday == 1 && (29..=31).contains(&mday))
            || (wday == 2 && (mday == 30 || mday == 31))
            || (wday == 3 && mday == 31))
    {
        weeknum = 1;
    }
    weeknum
}

/// `printf("%*d")` / `printf("%0*d")` of `val` to `width`.
fn fmt_num(val: &BigInt, width: usize, zero: bool) -> String {
    if zero {
        let digits = val.abs().to_string();
        let sign = if val.is_negative() { "-" } else { "" };
        format!(
            "{sign}{:0>w$}",
            digits,
            w = width.saturating_sub(sign.len())
        )
    } else {
        format!("{:>width$}", val.to_string())
    }
}

/// `Time#strftime`, ported from MRI `rb_strftime_with_timespec`. `Err` is
/// MRI's `invalid format` (a directive that runs off the end of the format).
pub fn strftime(fmt: &str, v: &Vtm) -> Result<String, String> {
    let b = fmt.as_bytes();
    let end = b.len();
    let mut out = String::new();
    let mut f = 0;
    while f < end {
        if b[f] != b'%' {
            let next = b[f..]
                .iter()
                .position(|&c| c == b'%')
                .map_or(end, |p| f + p);
            out.push_str(&fmt[f..next]);
            f = next;
            continue;
        }
        let sp = f;
        let mut precision: i64 = -1;
        let mut flags: u8 = 0;
        let mut padding: Option<u8> = None;
        let mut colons = 0;
        // The text of a string-valued directive, emitted after the match.
        let text: String;
        loop {
            f += 1;
            if f >= end {
                return Err(format!("invalid format: {fmt}"));
            }
            // A numeric directive: FMT(def_pad, def_prec, val).
            let num = |out: &mut String, def_pad: u8, def_prec: i64, val: BigInt| {
                let prec = if flags & LEFT != 0 {
                    1
                } else if precision <= 0 {
                    def_prec
                } else {
                    precision
                };
                let zero = padding == Some(b'0') || (padding.is_none() && def_pad == b'0');
                out.push_str(&fmt_num(&val, prec as usize, zero));
            };
            // A composite directive: STRFTIME(sub).
            let sub = |out: &mut String, sub: &str| {
                let s = case_conv(&strftime(sub, v).unwrap_or_default(), flags);
                let len = s.len() as i64;
                if precision > len {
                    let pad = padding.unwrap_or(b' ') as char;
                    out.push_str(&pad.to_string().repeat((precision - len) as usize));
                }
                out.push_str(&s);
            };
            let bi = |n: i64| BigInt::from(n);
            match b[f] {
                b'%' => {
                    text = "%".to_string();
                    break;
                }
                c @ (b'a' | b'A') => {
                    if flags & CHCASE != 0 {
                        flags = (flags & !(LOWER | CHCASE)) | UPPER;
                    }
                    let d = DAYS[v.wday as usize];
                    text = if c == b'a' {
                        d[..3].to_string()
                    } else {
                        d.to_string()
                    };
                    break;
                }
                c @ (b'b' | b'h' | b'B') => {
                    if flags & CHCASE != 0 {
                        flags = (flags & !(LOWER | CHCASE)) | UPPER;
                    }
                    let m = MONTHS[(v.mon - 1) as usize];
                    text = if c == b'B' {
                        m.to_string()
                    } else {
                        m[..3].to_string()
                    };
                    break;
                }
                b'c' => sub(&mut out, "%a %b %e %H:%M:%S %Y"),
                b'd' => num(&mut out, b'0', 2, bi(v.mday)),
                b'H' => num(&mut out, b'0', 2, bi(v.hour)),
                b'I' => num(&mut out, b'0', 2, bi(hour12(v.hour))),
                b'j' => num(&mut out, b'0', 3, bi(v.yday)),
                b'm' => num(&mut out, b'0', 2, bi(v.mon)),
                b'M' => num(&mut out, b'0', 2, bi(v.min)),
                c @ (b'p' | b'P') => {
                    if (c == b'p' && flags & CHCASE != 0)
                        || (c == b'P' && flags & (CHCASE | UPPER) == 0)
                    {
                        flags = (flags & !(UPPER | CHCASE)) | LOWER;
                    }
                    text = if v.hour < 12 { "AM" } else { "PM" }.to_string();
                    break;
                }
                b's' => num(&mut out, b'0', 1, v.epoch.clone()),
                b'S' => num(&mut out, b'0', 2, bi(v.sec)),
                b'U' => num(&mut out, b'0', 2, bi(weeknumber(v, 0))),
                b'w' => num(&mut out, b'0', 1, bi(v.wday)),
                b'W' => num(&mut out, b'0', 2, bi(weeknumber(v, 1))),
                b'x' | b'D' => sub(&mut out, "%m/%d/%y"),
                b'X' | b'T' => sub(&mut out, "%H:%M:%S"),
                b'y' => num(&mut out, b'0', 2, bi(v.year.rem_euclid(100))),
                b'Y' => num(&mut out, b'0', if v.year >= 0 { 4 } else { 5 }, bi(v.year)),
                b'z' => out.push_str(&fmt_offset(v, flags, padding, precision, colons)),
                b'Z' => {
                    if flags & CHCASE != 0 {
                        flags = (flags & !(UPPER | CHCASE)) | LOWER;
                    }
                    text = if v.utc {
                        "UTC".to_string()
                    } else {
                        v.zone.clone().unwrap_or_default()
                    };
                    break;
                }
                b'n' => {
                    text = "\n".to_string();
                    break;
                }
                b't' => {
                    text = "\t".to_string();
                    break;
                }
                b'e' => num(&mut out, b' ', 2, bi(v.mday)),
                b'r' => sub(&mut out, "%I:%M:%S %p"),
                b'R' => sub(&mut out, "%H:%M"),
                b'k' => num(&mut out, b' ', 2, bi(v.hour)),
                b'l' => num(&mut out, b' ', 2, bi(hour12(v.hour))),
                b'v' => sub(&mut out, "%e-%^b-%4Y"),
                b'C' => num(&mut out, b'0', 2, bi(v.year.div_euclid(100))),
                c @ (b'E' | b'O') => {
                    let allowed: &[u8] = if c == b'E' {
                        b"cCxXyY"
                    } else {
                        b"deHkIlmMSuUVwWy"
                    };
                    if f + 1 < end && allowed.contains(&b[f + 1]) {
                        continue;
                    }
                    text = fmt[sp..=f].to_string();
                    flags = 0;
                    precision = -1;
                    break;
                }
                b'V' => num(&mut out, b'0', 2, bi(iso_week(v))),
                b'u' => num(&mut out, b'0', 1, bi(if v.wday == 0 { 7 } else { v.wday })),
                c @ (b'G' | b'g') => {
                    let w = iso_week(v);
                    let mut y = v.year;
                    if v.mon == 12 && w == 1 {
                        y += 1;
                    } else if v.mon == 1 && w >= 52 {
                        y -= 1;
                    }
                    if c == b'G' {
                        num(&mut out, b'0', if y >= 0 { 4 } else { 5 }, bi(y));
                    } else {
                        num(&mut out, b'0', 2, bi(y.rem_euclid(100)));
                    }
                }
                c @ (b'L' | b'N') => {
                    let w = if c == b'L' { 3 } else { 9 };
                    let p = if precision <= 0 { w } else { precision };
                    let scaled = (&v.subsec
                        * BigRational::from_integer(BigInt::from(10).pow(p as u32)))
                    .floor()
                    .to_integer();
                    out.push_str(&format!("{:0>w$}", scaled.to_string(), w = p as usize));
                }
                b'F' => sub(&mut out, "%Y-%m-%d"),
                b'-' | b'^' | b'#' | b'_' => {
                    if precision > 0 {
                        text = unknown(fmt, sp, f, &mut flags, &mut precision);
                        break;
                    }
                    match b[f] {
                        b'-' => {
                            flags |= LEFT;
                            padding = None;
                            precision = 0;
                        }
                        b'^' => flags |= UPPER,
                        b'#' => flags |= CHCASE,
                        _ => padding = Some(b' '),
                    }
                    continue;
                }
                b':' => {
                    let mut n = 1;
                    let mut ok = false;
                    while n <= 3 {
                        if f + n >= end {
                            break;
                        }
                        if b[f + n] == b'z' {
                            ok = true;
                            break;
                        }
                        if b[f + n] != b':' {
                            break;
                        }
                        n += 1;
                    }
                    if !ok {
                        text = unknown(fmt, sp, f, &mut flags, &mut precision);
                        break;
                    }
                    colons = n;
                    f += n - 1;
                    continue;
                }
                c @ b'0'..=b'9' => {
                    if c == b'0' {
                        padding = Some(b'0');
                    }
                    let start = f;
                    while f < end && b[f].is_ascii_digit() {
                        f += 1;
                    }
                    match fmt[start..f].parse::<i64>() {
                        Ok(n) if n <= i32::MAX as i64 => precision = n,
                        _ => {
                            f -= 1;
                            text = unknown(fmt, sp, f, &mut flags, &mut precision);
                            break;
                        }
                    }
                    f -= 1;
                    continue;
                }
                _ => {
                    text = unknown(fmt, sp, f, &mut flags, &mut precision);
                    break;
                }
            }
            text = String::new();
            precision = -1;
            break;
        }
        if !text.is_empty() {
            let len = text.len() as i64;
            if flags & LEFT == 0 && precision > len {
                let pad = padding.unwrap_or(b' ') as char;
                out.push_str(&pad.to_string().repeat((precision - len) as usize));
            }
            out.push_str(&case_conv(&text, flags));
        }
        f += 1;
    }
    Ok(out)
}

/// The `%z` family (MRI's MAILHEADER_EXT case): `%z` `+hhmm`, `%:z`
/// `+hh:mm`, `%::z` `+hh:mm:ss`, `%:::z` the shortest of those that is exact.
fn fmt_offset(v: &Vtm, flags: u8, padding: Option<u8>, precision: i64, colons: usize) -> String {
    let gmt = v.utc;
    let mut off = if gmt { 0 } else { v.utc_offset };
    let sign = if off < 0 || (gmt && flags & LEFT != 0) {
        off = -off;
        -1
    } else {
        1
    };
    let at_least = |limit: i64, sub: i64| {
        if precision <= limit {
            2
        } else {
            precision - sub
        }
    };
    let p = match colons {
        0 => at_least(5, 3),
        1 => at_least(6, 4),
        2 => at_least(9, 7),
        _ if off % 3600 == 0 => at_least(3, 1),
        _ if off % 60 == 0 => at_least(6, 4),
        _ => at_least(9, 7),
    } as usize;
    let hours = sign * (off / 3600);
    let signed = format!("{}{:0>p$}", if hours < 0 { '-' } else { '+' }, hours.abs());
    let mut out = if padding == Some(b' ') {
        // `%+*ld`: width p+1, space padded, no zero fill.
        let body = format!("{}{}", if hours < 0 { '-' } else { '+' }, hours.abs());
        format!("{body:>w$}", w = p + 1)
    } else {
        // `%+.*ld`: the sign, then at least p digits.
        signed
    };
    if sign < 0 && off < 3600 {
        // A zero hour prints `+0`; the minus is put back by hand.
        let at = if padding == Some(b' ') {
            out.len() - 2
        } else {
            0
        };
        out.replace_range(at..at + 1, "-");
    }
    let mut rest = off % 3600;
    if colons == 3 && rest == 0 {
        return out;
    }
    if colons >= 1 {
        out.push(':');
    }
    out.push_str(&format!("{:02}", rest / 60));
    rest %= 60;
    if colons == 3 && rest == 0 {
        return out;
    }
    if colons >= 2 {
        out.push_str(&format!(":{rest:02}"));
    }
    out
}

/// The `unknown:` path: the directive's own text, verbatim, flags dropped.
fn unknown(fmt: &str, sp: usize, f: usize, flags: &mut u8, precision: &mut i64) -> String {
    *flags = 0;
    *precision = -1;
    fmt[sp..=f].to_string()
}

fn hour12(h: i64) -> i64 {
    match h {
        0 => 12,
        h if h > 12 => h - 12,
        h => h,
    }
}

fn iso_week(v: &Vtm) -> i64 {
    iso8601wknum(v.year, v.mon, v.mday, v.wday, v.yday - 1)
}

/// Exact integer `10**n` as a Rational (for `subsec` scaling at callers).
pub fn pow10(n: u32) -> BigRational {
    BigRational::from_integer(BigInt::from(10).pow(n))
}

/// Split exact seconds into `(whole, nanoseconds)` for `usec`/`nsec`.
pub fn nanos_of(subsec: &BigRational) -> BigInt {
    (subsec * pow10(9)).floor().to_integer()
}
