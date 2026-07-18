//! `parity` — differential harness: rubylang vs. the reference `ruby`.
//!
//! Development tool, not part of the runtime. It runs every snippet in
//! `tests/data/parity_corpus.rb` (separated by a `#==#` line) through both the
//! system `ruby` (the oracle) and rubylang in-process, and compares stdout.
//!
//! * default: print a parity report (parity / gap / panic counts, and each
//!   gap's expected-vs-got). Needs `ruby` on PATH.
//! * `--freeze`: capture the oracle output for every snippet into
//!   `tests/data/parity_expected.txt`, which the CI-safe `tests/parity.rs`
//!   replays with no `ruby` installed.
//!
//! Errors compare loosely: if the oracle exits non-zero and rubylang also errors,
//! that snippet is parity (both reject it); the corpus is otherwise all valid
//! programs whose stdout must match byte-for-byte.

use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::process::Command;

const SEP: &str = "#==#";
const CORPUS: &str = "tests/data/parity_corpus.rb";
const EXPECTED: &str = "tests/data/parity_expected.txt";

const EXAMPLES_DIR: &str = "examples";
const EXAMPLES_OUT: &str = "tests/data/examples";

fn main() {
    if std::env::args().any(|a| a == "--freeze-examples") {
        freeze_examples();
        return;
    }
    let freeze = std::env::args().any(|a| a == "--freeze");
    let corpus = std::fs::read_to_string(CORPUS).unwrap_or_else(|e| {
        eprintln!("parity: cannot read {CORPUS}: {e}");
        std::process::exit(1);
    });
    let snippets = split_snippets(&corpus);

    if freeze {
        let mut out = String::new();
        for (i, s) in snippets.iter().enumerate() {
            if i > 0 {
                out.push_str(SEP);
                out.push('\n');
            }
            out.push_str(&oracle(s));
        }
        std::fs::write(EXPECTED, out).expect("write expected");
        println!("froze {} oracle outputs -> {EXPECTED}", snippets.len());
        return;
    }

    let (mut parity, mut gaps, mut panics) = (0, Vec::new(), Vec::new());
    for (i, s) in snippets.iter().enumerate() {
        let want = oracle(s);
        match rubyrs_run(s) {
            Ok(got) if got == want => parity += 1,
            Ok(got) => gaps.push((i, s.clone(), want, got)),
            Err(e) => panics.push((i, s.clone(), e)),
        }
    }

    println!("parity: {}/{} snippets", parity, snippets.len());
    for (i, s, want, got) in &gaps {
        println!(
            "\n── GAP #{i} ──\n{}\n  expected: {:?}\n  got:      {:?}",
            first_line(s),
            want,
            got
        );
    }
    for (i, s, e) in &panics {
        println!("\n── PANIC #{i} ──\n{}\n  error: {e}", first_line(s));
    }
    if !gaps.is_empty() || !panics.is_empty() {
        println!("\n{} gap(s), {} panic(s)", gaps.len(), panics.len());
        std::process::exit(1);
    }
    println!("all snippets at parity");
}

/// Freeze the reference `ruby` stdout for every `examples/*.rb` into
/// `tests/data/examples/<name>.out`, which the CI-safe `tests/examples.rs`
/// replays. A script that the oracle rejects (non-zero exit) is a bug in the
/// example itself, so we refuse to freeze it.
fn freeze_examples() {
    std::fs::create_dir_all(EXAMPLES_OUT).expect("create examples out dir");
    let mut names: Vec<String> = std::fs::read_dir(EXAMPLES_DIR)
        .expect("read examples dir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|n| n.ends_with(".rb"))
        .collect();
    names.sort();
    for name in &names {
        let path = format!("{EXAMPLES_DIR}/{name}");
        let out = Command::new("ruby").arg(&path).output().expect("run ruby");
        if !out.status.success() {
            eprintln!(
                "parity: reference ruby rejected {path}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            std::process::exit(1);
        }
        let stem = name.strip_suffix(".rb").unwrap();
        std::fs::write(format!("{EXAMPLES_OUT}/{stem}.out"), &out.stdout).expect("write out");
    }
    println!("froze {} example outputs -> {EXAMPLES_OUT}/", names.len());
}

fn split_snippets(corpus: &str) -> Vec<String> {
    corpus
        .split(&format!("{SEP}\n"))
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Run a snippet through the reference `ruby`; on a non-zero exit, return a
/// normalized error marker so both sides can agree "this is rejected".
fn oracle(snippet: &str) -> String {
    match Command::new("ruby").arg("-e").arg(snippet).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(_) => "<error>".to_string(),
        Err(e) => format!("<oracle-unavailable: {e}>"),
    }
}

/// Evaluate a snippet in rubylang, capturing stdout and turning a runtime error or
/// panic into `Err`.
fn rubyrs_run(snippet: &str) -> Result<String, String> {
    let snippet = snippet.to_string();
    let res = std::panic::catch_unwind(move || capture_stdout(|| rubylang::eval_str(&snippet)));
    match res {
        Ok((Ok(_), out)) => Ok(out),
        Ok((Err(_), _)) => Ok("<error>".to_string()),
        Err(_) => Err("panicked".to_string()),
    }
}

/// Redirect fd 1 into a pipe for the duration of `f`, returning what was written.
fn capture_stdout<R>(f: impl FnOnce() -> R) -> (R, String) {
    // SAFETY: standard pipe + dup2 on the process's own stdout fd.
    unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return (f(), String::new());
        }
        let saved = libc::dup(1);
        libc::dup2(fds[1], 1);
        libc::close(fds[1]);

        let r = f();
        let _ = std::io::stdout().flush();

        libc::dup2(saved, 1);
        libc::close(saved);

        let mut out = String::new();
        let mut file = std::fs::File::from_raw_fd(fds[0]);
        let _ = file.read_to_string(&mut out);
        (r, out)
    }
}
