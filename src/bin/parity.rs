//! `parity` — differential harness: rubyrs vs. the reference `ruby`.
//!
//! Development tool, not part of the runtime. It runs every snippet in
//! `tests/data/parity_corpus.rb` (separated by a `#==#` line) through both the
//! system `ruby` (the oracle) and rubyrs in-process, and compares stdout.
//!
//! * default: print a parity report (parity / gap / panic counts, and each
//!   gap's expected-vs-got). Needs `ruby` on PATH.
//! * `--freeze`: capture the oracle output for every snippet into
//!   `tests/data/parity_expected.txt`, which the CI-safe `tests/parity.rs`
//!   replays with no `ruby` installed.
//!
//! Errors compare loosely: if the oracle exits non-zero and rubyrs also errors,
//! that snippet is parity (both reject it); the corpus is otherwise all valid
//! programs whose stdout must match byte-for-byte.

use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::process::Command;

const SEP: &str = "#==#";
const CORPUS: &str = "tests/data/parity_corpus.rb";
const EXPECTED: &str = "tests/data/parity_expected.txt";

fn main() {
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

/// Evaluate a snippet in rubyrs, capturing stdout and turning a runtime error or
/// panic into `Err`.
fn rubyrs_run(snippet: &str) -> Result<String, String> {
    let snippet = snippet.to_string();
    let res = std::panic::catch_unwind(move || capture_stdout(|| rubyrs::eval_str(&snippet)));
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
