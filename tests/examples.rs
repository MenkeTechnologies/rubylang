//! CI-safe replay of the example programs. Every `examples/*.rb` is run through
//! the built `ruby` binary; the test asserts it exits successfully (the scripts
//! embed `check`/`raise` assertions, so a non-zero exit means a self-test failed)
//! and that its stdout matches `tests/data/examples/<name>.out` — the output the
//! reference `ruby` produced, frozen by `cargo run --bin parity -- --freeze-examples`.
//!
//! This needs no `ruby` installed, so CI runs it. A regression that diverges
//! rubylang from the reference — or breaks one of the in-script assertions —
//! fails here, naming the example and the expected-vs-got output.

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
