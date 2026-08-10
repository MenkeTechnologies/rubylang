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
    assert_eq!(
        snips.len(),
        wants.len(),
        "corpus ({}) and expected ({}) snippet counts differ — re-run `parity --freeze`",
        snips.len(),
        wants.len()
    );

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
        if got_cmp != want {
            failures.push(format!(
                "── snippet #{i} ──\n{}\n  expected: {:?}\n  got:      {:?}",
                snippet.lines().next().unwrap_or(""),
                want,
                got_cmp
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} parity regression(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
