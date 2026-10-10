//! CI-safe replay of the parity corpus. Each snippet in
//! `tests/data/parity_corpus.rb` is run through the built `ruby` binary and its
//! stdout is asserted against `tests/data/parity_expected.txt` — the outputs the
//! reference `ruby` produced, frozen by `cargo run --bin parity -- --freeze`.
//!
//! This needs no `ruby` installed (unlike the `parity` dev tool), so CI runs it.
//! A regression that diverges rubyrs from the reference fails here with the
//! snippet and the expected-vs-got outputs.

use std::process::Command;

const SEP: &str = "#==#\n";

/// Maximum distance, in units in the last place, tolerated for float tokens in
/// the snippets listed in [`LIBM_SENSITIVE`].
const MAX_ULPS: u64 = 4;

/// Snippets whose frozen output depends on the platform libm: the reference was
/// frozen on macOS and glibc rounds transcendental functions (`exp`/`ln`/`pow`)
/// differently in the last ulp. Each entry is (index into the parsed corpus,
/// first line of that snippet, why). The first line pins the index so a corpus
/// edit that shifts snippets fails loudly instead of loosening the wrong one.
/// Every other snippet is compared by exact string equality. For listed
/// snippets only float tokens may differ, by at most [`MAX_ULPS`]; any other
/// difference still fails.
const LIBM_SENSITIVE: &[(usize, &str, &str)] = &[(
    713,
    "r = Rational(2, 3); c = Complex(1, 1)",
    "`Rational(1, 3) ** Complex(1, 1)` goes through ln/exp; the real part is \
     0.3333333333333333 on macOS libm and 0.33333333333333337 on glibc",
)];

/// A lexed piece of an output line: either a float literal or other text.
#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Text(&'a str),
    Float(&'a str),
}

/// Split `line` into float literals (`-?DIGITS.DIGITS(e[+-]?DIGITS)?`, not
/// glued to a preceding identifier character; a leading `-` is part of it) and the text between them.
/// Integers stay in the text so they are compared exactly.
fn lex_floats(line: &str) -> Vec<Tok<'_>> {
    let b = line.as_bytes();
    let glued_before = |i: usize| {
        i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.')
    };
    let digit_at = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let digits_end = |mut i: usize| {
        while digit_at(i) {
            i += 1;
        }
        i
    };
    let mut toks = Vec::new();
    let (mut text_start, mut i) = (0, 0);
    while i < b.len() {
        let neg = b[i] == b'-' && digit_at(i + 1);
        let start = if neg { i + 1 } else { i };
        if !digit_at(start) || (!neg && glued_before(i)) {
            i += 1;
            continue;
        }
        let mut j = digits_end(start);
        if b.get(j) != Some(&b'.') || !digit_at(j + 1) {
            i = j; // integer: skip the whole run so its tail is not re-lexed
            continue;
        }
        j = digits_end(j + 1);
        if matches!(b.get(j), Some(b'e' | b'E')) {
            let k = j + 1 + usize::from(matches!(b.get(j + 1), Some(b'+' | b'-')));
            if digit_at(k) {
                j = digits_end(k);
            }
        }
        if text_start < i {
            toks.push(Tok::Text(&line[text_start..i]));
        }
        toks.push(Tok::Float(&line[i..j]));
        text_start = j;
        i = j;
    }
    if text_start < b.len() {
        toks.push(Tok::Text(&line[text_start..]));
    }
    toks
}

/// Distance between two floats in units in the last place, or `None` when it is
/// undefined: either side NaN, or an infinity not matched by the same infinity.
/// `+0.0` and `-0.0` are distance 0; values of opposite sign are far apart.
fn ulp_distance(a: f64, b: f64) -> Option<u64> {
    if a.is_nan() || b.is_nan() {
        return None;
    }
    if a == b {
        return Some(0);
    }
    if a.is_infinite() || b.is_infinite() {
        return None;
    }
    // Map the IEEE bit pattern onto a monotonic unsigned line with +-0 merged.
    let key = |x: f64| {
        let magnitude = x.abs().to_bits();
        if x.is_sign_negative() {
            (1 << 63) - magnitude
        } else {
            (1 << 63) + magnitude
        }
    };
    Some(key(a).abs_diff(key(b)))
}

/// Compare `want` and `got` line by line: identical except that float tokens
/// may differ by at most `max_ulps`. `Err` describes the first difference.
fn eq_within_ulps(want: &str, got: &str, max_ulps: u64) -> Result<(), String> {
    let wl: Vec<_> = want.split('\n').collect();
    let gl: Vec<_> = got.split('\n').collect();
    if wl.len() != gl.len() {
        return Err(format!("line count {} != {}", wl.len(), gl.len()));
    }
    for (n, (w, g)) in wl.iter().zip(&gl).enumerate() {
        let (wt, gt) = (lex_floats(w), lex_floats(g));
        if wt.len() != gt.len() {
            return Err(format!("line {n}: token structure differs: {w:?} vs {g:?}"));
        }
        for (a, b) in wt.iter().zip(&gt) {
            match (a, b) {
                (Tok::Text(x), Tok::Text(y)) if x == y => {}
                (Tok::Float(x), Tok::Float(y)) if x == y => {}
                (Tok::Float(x), Tok::Float(y)) => {
                    let d = match (x.parse::<f64>(), y.parse::<f64>()) {
                        (Ok(fx), Ok(fy)) => ulp_distance(fx, fy),
                        _ => None,
                    };
                    match d {
                        Some(d) if d <= max_ulps => {}
                        Some(d) => {
                            return Err(format!(
                                "line {n}: {x} vs {y} differ by {d} ulps (> {max_ulps})"
                            ))
                        }
                        None => return Err(format!("line {n}: {x} vs {y} not comparable")),
                    }
                }
                _ => return Err(format!("line {n}: {w:?} vs {g:?}")),
            }
        }
    }
    Ok(())
}

fn snippets(text: &str) -> Vec<String> {
    text.split(SEP)
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// Split the frozen expected file, preserving the (possibly empty) output blocks
/// positionally so they line up with the corpus snippets.
fn expected_blocks(text: &str) -> Vec<String> {
    text.split(SEP).map(|s| s.to_string()).collect()
}

#[test]
fn corpus_matches_reference_ruby() {
    let corpus = include_str!("data/parity_corpus.rb");
    let expected = include_str!("data/parity_expected.txt");
    let ruby = env!("CARGO_BIN_EXE_ruby");

    let snips = snippets(corpus);
    let wants = expected_blocks(expected);
    // The comparison loop below is over `snips`; an empty corpus runs zero
    // iterations and the terminal `failures.is_empty()` passes having compared
    // nothing. This is the CI replay of the whole differential harness, so a
    // corpus that silently stopped parsing must fail, not report green.
    assert!(
        !snips.is_empty(),
        "tests/data/parity_corpus.rb parsed to zero snippets — the replay below \
         would pass having run nothing"
    );
    assert_eq!(
        snips.len(),
        wants.len(),
        "corpus ({}) and expected ({}) snippet counts differ — re-run `parity --freeze`",
        snips.len(),
        wants.len()
    );

    for (idx, first, _) in LIBM_SENSITIVE {
        assert_eq!(
            snips.get(*idx).and_then(|s| s.lines().next()),
            Some(*first),
            "LIBM_SENSITIVE index {idx} no longer points at its snippet"
        );
    }

    let mut failures = Vec::new();
    for (i, (snippet, want)) in snips.iter().zip(&wants).enumerate() {
        let out = Command::new(ruby)
            .arg("-e")
            .arg(snippet)
            .output()
            .expect("run ruby binary");
        let got = String::from_utf8_lossy(&out.stdout).to_string();
        let want = want.strip_suffix('\n').unwrap_or(want);
        // A frozen `<error>` means the reference `ruby` rejected the snippet.
        // Its stdout is empty on both sides, so there is nothing to compare —
        // but "exited non-zero" alone is satisfied by a great many wrong
        // outcomes, including the one that matters most: an interpreter PANIC
        // also exits non-zero and printed no Ruby diagnostic at all. Three
        // further things are therefore required, all of which the reference
        // does and none of which a panic does.
        if want == "<error>" {
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            let reported_an_exception = err
                .lines()
                .any(|l| l.trim_end().ends_with(')') && l.contains(" ("));
            let why = if out.status.success() {
                Some("the reference rejected it, but rubyrs accepted it".to_string())
            } else if !got.is_empty() {
                Some(format!(
                    "expected no stdout before the failure, got {got:?}"
                ))
            } else if err.contains("panicked at") {
                Some(format!("rubyrs PANICKED instead of raising: {err:?}"))
            } else if !reported_an_exception {
                Some(format!(
                    "expected a `… (SomeError)` diagnostic on stderr, got {err:?}"
                ))
            } else {
                None
            };
            if let Some(why) = why {
                failures.push(format!(
                    "── snippet #{i} ──\n{}\n  {why}",
                    snippet.lines().next().unwrap_or("")
                ));
            }
            continue;
        }
        let got_cmp = got.strip_suffix('\n').unwrap_or(&got);
        let mut note = String::new();
        if got_cmp != want {
            if let Some((_, _, why)) = LIBM_SENSITIVE.iter().find(|(idx, _, _)| *idx == i) {
                match eq_within_ulps(want, got_cmp, MAX_ULPS) {
                    Ok(()) => continue,
                    Err(e) => note = format!("\n  libm-sensitive ({why}); {e}"),
                }
            }
            failures.push(format!(
                "── snippet #{i} ──\n{}\n  expected: {:?}\n  got:      {:?}",
                snippet.lines().next().unwrap_or(""),
                want,
                got_cmp
            ));
            if let Some(f) = failures.last_mut() {
                f.push_str(&note);
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} parity regression(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[cfg(test)]
mod tolerance_tests {
    use super::*;

    fn next_up(x: f64) -> f64 {
        f64::from_bits(x.to_bits() + 1)
    }

    #[test]
    fn ulp_distance_counts_adjacent_floats() {
        assert_eq!(ulp_distance(1.0, 1.0), Some(0));
        assert_eq!(ulp_distance(1.0, next_up(1.0)), Some(1));
        let (a, b) = (0.3333333333333333, 0.33333333333333337);
        assert_eq!(ulp_distance(a, b), Some(1));
        assert_eq!(ulp_distance(b, a), Some(1));
        let mut x = 1.0;
        for _ in 0..4 {
            x = next_up(x);
        }
        assert_eq!(ulp_distance(1.0, x), Some(4));
    }

    #[test]
    fn ulp_distance_sign_and_zero() {
        assert_eq!(ulp_distance(0.0, -0.0), Some(0));
        // Smallest subnormals on either side of zero are 2 ulps apart.
        assert_eq!(ulp_distance(f64::from_bits(1), -f64::from_bits(1)), Some(2));
        assert!(ulp_distance(1.0, -1.0).unwrap() > 1 << 60);
        // Negative floats: growing the bit pattern grows the magnitude.
        assert_eq!(ulp_distance(-1.0, -next_up(1.0)), Some(1));
    }

    #[test]
    fn ulp_distance_nan_and_inf() {
        assert_eq!(ulp_distance(f64::NAN, 1.0), None);
        assert_eq!(ulp_distance(f64::NAN, f64::NAN), None);
        assert_eq!(ulp_distance(f64::INFINITY, f64::INFINITY), Some(0));
        assert_eq!(ulp_distance(f64::INFINITY, f64::NEG_INFINITY), None);
        assert_eq!(ulp_distance(f64::INFINITY, f64::MAX), None);
    }

    #[test]
    fn lexer_splits_floats_and_keeps_integers_as_text() {
        assert_eq!(
            lex_floats("(0.5-1.5e-3i)"),
            vec![
                Tok::Text("("),
                Tok::Float("0.5"),
                Tok::Float("-1.5e-3"),
                Tok::Text("i)"),
            ]
        );
        assert_eq!(
            lex_floats("12 x1.5 (3/4)"),
            vec![Tok::Text("12 x1.5 (3/4)")]
        );
        assert_eq!(
            lex_floats("[1, 2.0]"),
            vec![Tok::Text("[1, "), Tok::Float("2.0"), Tok::Text("]")]
        );
    }

    #[test]
    fn tolerates_last_ulp_float_difference_only() {
        let want = "(0.3333333333333333+0.5773502691896257i)\n(1/1)";
        let got = "(0.33333333333333337+0.5773502691896257i)\n(1/1)";
        assert_eq!(eq_within_ulps(want, got, 4), Ok(()));
        assert!(eq_within_ulps(want, got, 0).is_err());
    }

    #[test]
    fn rejects_beyond_tolerance_and_non_float_differences() {
        // 5 ulps apart.
        let mut x = 1.0f64;
        for _ in 0..5 {
            x = next_up(x);
        }
        let err = eq_within_ulps("1.0", &format!("{x:?}"), 4).unwrap_err();
        assert!(err.contains("5 ulps"), "{err}");
        // Sign flip of the imaginary part is a text difference.
        assert!(eq_within_ulps("(1.0+2.0i)", "(1.0-2.0i)", 4).is_err());
        // Integer / rational differences are exact.
        assert!(eq_within_ulps("(1/3)", "(1/4)", 4).is_err());
        assert!(eq_within_ulps("1.0\n2", "1.0\n3", 4).is_err());
        // Line-count and token-structure differences.
        assert!(eq_within_ulps("1.0", "1.0\n1.0", 4).is_err());
        assert!(eq_within_ulps("1.0", "NaN", 4).is_err());
        assert!(eq_within_ulps("Infinity", "-Infinity", 4).is_err());
    }
}
