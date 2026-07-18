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
        // A frozen `<error>` means the reference `ruby` rejected the snippet;
        // assert rubyrs also rejects it (non-zero exit) rather than matching
        // stdout, which is empty on both sides.
        if want == "<error>" {
            if out.status.success() {
                failures.push(format!(
                    "── snippet #{i} ──\n{}\n  expected: reference rejected it, but rubyrs accepted it",
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
