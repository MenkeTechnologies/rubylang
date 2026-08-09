//! CI-safe replay of the example programs. Every `examples/*.rb` is run through
//! the built `ruby` binary; the test asserts it exits successfully (the scripts
//! embed `check`/`raise` assertions, so a non-zero exit means a self-test failed)
//! and that its stdout matches `tests/data/examples/<name>.out`, frozen by
//! `cargo run --bin parity -- --freeze-examples`.
//!
//! This needs no reference interpreter installed, so CI runs it. A regression
//! that diverges rubylang from the frozen output — or breaks one of the
//! in-script assertions — fails here, naming the example and the
//! expected-vs-got output.
//!
//! Not every frozen file is reference output, and the difference is recorded
//! rather than assumed. A few examples exercise libraries rubylang embeds
//! (`sqlite3`, the bundled Rack / ActiveRecord-lite) that a stock MRI cannot
//! load, so MRI exits with a `LoadError` and there is no reference stdout to
//! freeze; those keep a rubylang self-baseline, which pins behaviour against
//! regression but proves nothing about parity.
//! `tests/data/examples/PROVENANCE.tsv` labels each one `reference` or
//! `NOT-REFERENCE`, and `provenance_covers_every_example` below keeps the labels
//! from drifting as examples are added.

use std::path::Path;
use std::process::Command;

#[test]
fn examples_match_reference_ruby() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let ruby = env!("CARGO_BIN_EXE_ruby");
    let examples_dir = Path::new(manifest).join("examples");
    let out_dir = Path::new(manifest).join("tests/data/examples");

    let mut scripts: Vec<_> = std::fs::read_dir(&examples_dir)
        .expect("read examples dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "rb").unwrap_or(false))
        .collect();
    scripts.sort();
    assert!(
        !scripts.is_empty(),
        "no example scripts found in {examples_dir:?}"
    );

    let mut failures = Vec::new();
    for path in &scripts {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let expected_path = out_dir.join(format!("{stem}.out"));
        let expected = match std::fs::read_to_string(&expected_path) {
            Ok(s) => s,
            Err(_) => {
                failures.push(format!(
                    "{stem}: missing frozen output {expected_path:?} — run `cargo run --bin parity -- --freeze-examples`"
                ));
                continue;
            }
        };

        let out = Command::new(ruby)
            .arg(path)
            .output()
            .expect("run ruby binary");
        let got = String::from_utf8_lossy(&out.stdout).to_string();

        if !out.status.success() {
            failures.push(format!(
                "{stem}: exited non-zero (an in-script assertion failed):\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }
        if got != expected {
            failures.push(format!(
                "{stem}: stdout differs from reference\n  expected: {expected:?}\n  got:      {got:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} example regression(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Every example must carry a provenance label, so a newly added one cannot
/// quietly acquire a rubylang self-baseline that reads like reference output.
///
/// This asserts coverage and label validity only — it never asserts that a
/// given example IS reference-backed, because whether MRI can run one depends
/// on what is installed on the machine that froze it.
#[test]
fn provenance_covers_every_example() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let provenance_path = Path::new(manifest).join("tests/data/examples/PROVENANCE.tsv");
    let text = std::fs::read_to_string(&provenance_path).unwrap_or_else(|e| {
        panic!(
            "cannot read {provenance_path:?}: {e} — run \
             `cargo run --bin parity -- --freeze-examples`"
        )
    });

    let mut labelled = std::collections::BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let stem = cols.next().unwrap_or("").to_string();
        let label = cols.next().unwrap_or("").to_string();
        assert!(
            label == "reference" || label == "NOT-REFERENCE",
            "{provenance_path:?}: {stem} has unknown label {label:?}"
        );
        labelled.insert(stem, label);
    }

    let examples_dir = Path::new(manifest).join("examples");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&examples_dir).expect("read examples dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().map(|x| x != "rb").unwrap_or(true) {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if !labelled.contains_key(&stem) {
            missing.push(stem);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "these examples have no row in {provenance_path:?}, so it is not knowable \
         whether their frozen output came from MRI or from rubylang itself: \
         {missing:?}. Re-run `cargo run --bin parity -- --freeze-examples`."
    );
}
