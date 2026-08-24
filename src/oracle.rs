//! Locating the ORACLE — the reference MRI `ruby` the differential harnesses
//! compare rubylang against.
//!
//! # Why this is not `Command::new("ruby")`
//!
//! rubylang installs its own binary under the name `ruby`. On a machine where
//! it is installed, `/opt/homebrew/bin/ruby` is a symlink into
//! `../Cellar/rubylang/<ver>/bin/ruby` and every `ruby` found on `PATH` is
//! rubylang. A harness that spawns bare `ruby` therefore compares rubylang to
//! *itself*: every snippet agrees, the fuzzer reports zero divergences, and
//! `--freeze` writes rubylang's own stdout into `tests/data/` as though it were
//! the reference output. Nothing fails, so nothing reveals it.
//!
//! That failure mode is silent and self-reinforcing, so this module refuses to
//! guess. A candidate is only accepted after it PROVES it is MRI, and there is
//! no fallback to bare `ruby` — an unresolvable oracle is a hard error.
//!
//! # The four independent proofs
//!
//! A candidate must pass all of them; any one failing rejects it.
//!
//! 1. **Not us, by path.** The candidate is canonicalized (following the
//!    Homebrew symlink) and rejected if it lands inside a `rubylang`
//!    installation or in a `target/` build directory.
//! 2. **Not us, by CONTENT.** The candidate's executable is scanned for
//!    [`SELF_MARKER`], a string every rubylang build contains and no MRI build
//!    can. See the erosion note below: this is the proof that does not decay.
//! 3. **MRI-shaped `--version`.** MRI prints
//!    `ruby 4.0.6 (2026-07-14 revision 03b6d3f889) +PRISM [arm64-darwin25]`.
//!    rubylang prints `ruby 3.4.0 (rubylang 0.1.5) [aarch64-macos]`.
//! 4. **`RUBY_ENGINE`.** MRI prints `ruby`; anything that cannot answer the
//!    question the same way is not the reference.
//!
//! # Why proofs 3 and 4 are not enough, and were quietly getting weaker
//!
//! Proofs 3 and 4 both ask the candidate to DESCRIBE ITSELF, and rubylang's job
//! is to become indistinguishable from MRI. Every such proof therefore decays as
//! the frontend matures, and the decay is invisible: nothing fails when a clause
//! stops discriminating, it just stops contributing.
//!
//! That already happened. Proof 3 once rested on the whole `ruby X.Y.Z (…
//! revision …) [platform]` shape. rubylang has since grown the bracketed
//! platform and the leading `ruby `, so of the three clauses only `revision`
//! still separates the two — measured again this round, and still true:
//!
//! ```text
//! $ /opt/homebrew/opt/ruby/bin/ruby --version
//! ruby 4.0.6 (2026-07-14 revision 03b6d3f889) +PRISM [arm64-darwin25]
//! $ target/debug/ruby --version
//! ruby 3.4.0 (rubylang 0.1.5) [aarch64-macos]
//! ```
//!
//! Proof 4 is one line from being defeated the same way: `RUBY_ENGINE` answers
//! `"rubylang"` only because rubylang chooses to, and a compatibility shim that
//! made it answer `"ruby"` would silently turn the whole guard into proof 1.
//!
//! Proof 2 cannot decay, because it does not ask a question the frontend gets to
//! answer. A rubylang binary contains [`SELF_MARKER`]; an MRI binary does not,
//! and no amount of MRI mimicry puts it there. It survives the candidate being
//! renamed, installed outside a `rubylang` path, given MRI's exact `--version`
//! and made to answer `RUBY_ENGINE` as `"ruby"`. The only way to defeat it is to
//! delete the marker from rubylang, which `tests/entry_points.rs` pins.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A string every rubylang build contains and no MRI build can.
///
/// This is the one part of the guard that does not depend on rubylang
/// describing itself honestly, so it is the one part that cannot be eroded by
/// rubylang getting better at being MRI. It is deliberately unmistakable rather
/// than short: it must never collide with an ordinary string inside a real
/// interpreter.
pub const SELF_MARKER: &str = "rubylang-differential-oracle-self-marker/v1";

/// Keeps [`SELF_MARKER`] in the emitted binary whether or not any reachable code
/// path formats it. Without this the linker is free to drop the literal from a
/// build that never prints it, and proof 2 would pass a rubylang.
#[used]
static SELF_MARKER_ANCHOR: &[u8] = SELF_MARKER.as_bytes();

/// The environment variable that names the oracle explicitly. When it is set
/// but unusable, resolution is a HARD ERROR rather than a fallback: silently
/// answering with a different interpreter answers a different question.
pub const ORACLE_ENV: &str = "RUBYLANG_ORACLE_RUBY";

/// Kept working for the fuzzer's original spelling.
pub const ORACLE_ENV_LEGACY: &str = "RUBYLANG_FUZZ_RUBY";

/// Absolute candidate paths, best first. The keg-only Homebrew prefixes come
/// before the shared `bin` dirs because that is exactly where a real MRI lives
/// on a machine whose `bin/ruby` has been taken over by rubylang.
///
/// Bare `ruby` is deliberately absent: a `PATH` lookup is what produces the
/// self-comparison this module exists to prevent.
pub const CANDIDATES: &[&str] = &[
    "/opt/homebrew/opt/ruby/bin/ruby",
    "/usr/local/opt/ruby/bin/ruby",
    "/opt/homebrew/bin/ruby",
    "/usr/local/bin/ruby",
    "/usr/bin/ruby",
];

/// A verified reference interpreter.
#[derive(Debug, Clone)]
pub struct Oracle {
    /// The path to spawn. Absolute, and canonicalized past any symlink.
    pub path: PathBuf,
    /// The `--version` line, so a recorded divergence is attributable to the
    /// exact interpreter that produced it.
    pub version: String,
}

impl Oracle {
    /// `ruby <args…>` on the reference interpreter.
    pub fn command(&self) -> Command {
        Command::new(&self.path)
    }

    /// `<path> (<version>)`, for run headers and report files.
    pub fn id(&self) -> String {
        format!("{} ({})", self.path.display(), self.version)
    }
}

/// Whether `path`'s bytes contain `needle`, read in chunks so the size of the
/// candidate does not matter. Chunks overlap by `needle.len() - 1` bytes so a
/// match straddling a boundary is still found. An unreadable file answers
/// false: proof 1 and the `--version` probe below deal with those.
fn file_contains(path: &Path, needle: &[u8]) -> bool {
    use std::io::Read as _;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let overlap = needle.len().saturating_sub(1);
    let mut buf = vec![0u8; 1 << 20];
    let mut filled = 0usize;
    loop {
        match f.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => {
                filled += n;
                if buf[..filled].windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
                if filled + overlap >= buf.len() {
                    buf.copy_within(filled - overlap..filled, 0);
                    filled = overlap;
                }
            }
            Err(_) => return false,
        }
    }
}

/// Why a candidate is not the reference interpreter.
fn reject_reason(path: &Path) -> Option<String> {
    let real = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => return Some(format!("cannot canonicalize: {e}")),
    };
    let shown = real.display().to_string();

    // Proof 1: not us, by path.
    if shown.contains("rubylang") {
        return Some(format!(
            "resolves to {shown} — that is rubylang itself, not the reference"
        ));
    }
    if shown.contains("/target/debug/") || shown.contains("/target/release/") {
        return Some(format!("resolves to a build directory ({shown})"));
    }

    // Proof 2: not us, by content. Unlike every other proof here, this one does
    // not ask the candidate anything, so mimicry cannot answer its way past it.
    if file_contains(&real, SELF_MARKER.as_bytes()) {
        return Some(format!(
            "{shown} carries rubylang's own build marker — that is rubylang \
             itself, however it identifies"
        ));
    }

    // Proof 3: MRI-shaped `--version`.
    let ver = match Command::new(&real).arg("--version").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => return Some(format!("`--version` exited {}", o.status)),
        Err(e) => return Some(format!("cannot run `--version`: {e}")),
    };
    if !ver.starts_with("ruby ") || !ver.contains("revision ") || !ver.ends_with(']') {
        return Some(format!(
            "`--version` printed {ver:?}, which is not MRI's \
             `ruby X.Y.Z (… revision …) [platform]`"
        ));
    }

    // Proof 4: RUBY_ENGINE.
    match Command::new(&real)
        .arg("-e")
        .arg("print RUBY_ENGINE")
        .output()
    {
        Ok(o) if o.status.success() => {
            let engine = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if engine != "ruby" {
                return Some(format!("RUBY_ENGINE is {engine:?}, not \"ruby\""));
            }
        }
        Ok(o) => return Some(format!("`-e 'print RUBY_ENGINE'` exited {}", o.status)),
        Err(e) => return Some(format!("cannot run `-e`: {e}")),
    }

    None
}

/// Verify one explicit path, returning the reason it is unusable.
pub fn verify(path: &Path) -> Result<Oracle, String> {
    if let Some(why) = reject_reason(path) {
        return Err(format!("{}: {why}", path.display()));
    }
    let real = path.canonicalize().map_err(|e| e.to_string())?;
    let version = Command::new(&real)
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    Ok(Oracle {
        path: real,
        version: String::from_utf8_lossy(&version.stdout).trim().to_string(),
    })
}

/// Resolve the oracle, or explain every candidate that was rejected.
///
/// An explicitly requested oracle that fails verification is never silently
/// replaced by a search result.
pub fn find() -> Result<Oracle, String> {
    for var in [ORACLE_ENV, ORACLE_ENV_LEGACY] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return verify(Path::new(&p))
                    .map_err(|why| format!("{var}={p} is unusable — {why}"));
            }
        }
    }

    let mut rejected = Vec::new();
    for cand in CANDIDATES {
        let path = Path::new(cand);
        if !path.exists() {
            continue;
        }
        match verify(path) {
            Ok(o) => return Ok(o),
            Err(why) => rejected.push(format!("  {why}")),
        }
    }

    Err(format!(
        "no reference MRI `ruby` found. Set {ORACLE_ENV} to one.\n\
         Bare `ruby` is never tried: rubylang installs under that name, so a \
         PATH lookup would compare rubylang to itself.\n\
         Rejected candidates:\n{}",
        if rejected.is_empty() {
            "  (none of the candidate paths exist)".to_string()
        } else {
            rejected.join("\n")
        }
    ))
}

/// Resolve the oracle or exit(2) with the reason. For the dev harnesses, whose
/// whole output is meaningless without a real reference interpreter.
pub fn require_oracle(tool: &str) -> Oracle {
    match find() {
        Ok(o) => o,
        Err(why) => {
            eprintln!("{tool}: {why}");
            std::process::exit(2);
        }
    }
}
