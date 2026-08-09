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
//! # The three independent proofs
//!
//! A candidate must pass all of them; any one failing rejects it.
//!
//! 1. **Not us, by path.** The candidate is canonicalized (following the
//!    Homebrew symlink) and rejected if it lands inside a `rubylang`
//!    installation or in a `target/` build directory. This is the check that
//!    survives rubylang growing better MRI mimicry.
//! 2. **MRI-shaped `--version`.** MRI prints
//!    `ruby 4.0.6 (2026-07-14 revision 03b6d3f889) +PRISM [arm64-darwin25]`.
//!    rubylang prints `ruby 0.1.3` — no `revision`, no bracketed platform.
//! 3. **`RUBY_ENGINE`.** MRI prints `ruby`; anything that cannot answer the
//!    question the same way is not the reference.

use std::path::{Path, PathBuf};
use std::process::Command;

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

    // Proof 2: MRI-shaped `--version`.
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

    // Proof 3: RUBY_ENGINE.
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
