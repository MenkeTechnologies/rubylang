//! Differential parity fuzzer: reference `ruby -e <s>` vs rubylang `ruby -e <s>`.
//!
//! Generates thousands of grammar-driven, deterministic-output Ruby snippets,
//! runs each through both interpreters, and reports every case where stdout OR
//! exit code diverge. Each case is produced from a per-index seed so any
//! divergence replays exactly: `parity-fuzz --seed <N> --once`.
//!
//! Ported from the zshrs harness (`zshrs/bins/parity-fuzz.rs`): same RunOut /
//! render / differs / run_with_timeout infra, same seed→deterministic Mode
//! dispatch, same parallel workers, delta-debug `minimize`, `--verify`
//! K-consecutive re-check, `--baseline` allowlist + gap `signature`, `--once`
//! replay, and report file under `target/parity-fuzz/divergences.txt`. Only the
//! generators and the invocation (Ruby, not zsh) differ.
//!
//! The generators are biased toward the historically weak areas of a Ruby
//! frontend (float shortest-repr, integer division/modulo sign, format specs,
//! slicing, block-based enumerables, string methods). Pure random bytes only
//! produce mutual syntax errors that agree on both sides and teach nothing.
//!
//! Determinism invariant: the generator NEVER emits a construct whose output is
//! nondeterministic for reasons unrelated to parity — no `Time`, no `rand`, no
//! `object_id`/`hash`, no bare-object `#<...>` prints, no Set iteration-order
//! output (always sort first). Ruby Hash is insertion-ordered, so Hash literals
//! are safe. Every program prints something deterministic so an empty-vs-empty
//! run can never hide a gap.
//!
//! Build:  cargo build --bin parity-fuzz
//! Run:    ./target/debug/parity-fuzz --count 5000

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Also compare stderr (normalized) when set via `--stderr`.
static CMP_STDERR: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// PRNG — inline splitmix64, no `rand` dependency.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn seed(s: u64) -> Rng {
        // Avoid a zero state degenerating; splitmix64 tolerates any seed but a
        // nonzero start keeps the first draw well-mixed.
        Rng(s ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n >= 1).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Inclusive range `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ---------------------------------------------------------------------------
// Interpreter locations / invocation.
// ---------------------------------------------------------------------------

/// The rubylang binary under test — a sibling of this harness exe. Always an
/// absolute path so it can never be confused with the reference `ruby` on PATH
/// (they share the name `ruby`).
fn ours_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_ruby") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("ruby");
            if cand.exists() {
                return cand;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("ruby")
}

/// The ORACLE: the reference MRI `ruby`. Every divergence is "rubylang disagrees
/// with THIS ruby", so which ruby it is, is part of the result.
///
/// `RUBYLANG_FUZZ_RUBY` names it explicitly; if set but unusable this is a HARD
/// ERROR (falling back to a different ruby would silently answer a different
/// question). Otherwise the first existing system path wins. Candidates are
/// absolute system paths, never `target/`, so the oracle can never resolve to
/// our own binary.
fn oracle_path() -> &'static str {
    static ORACLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ORACLE.get_or_init(|| {
        if let Ok(p) = std::env::var("RUBYLANG_FUZZ_RUBY") {
            if !Path::new(&p).exists() {
                eprintln!("parity-fuzz: RUBYLANG_FUZZ_RUBY={p}: no such file");
                std::process::exit(2);
            }
            return p;
        }
        for p in [
            "/opt/homebrew/bin/ruby",
            "/usr/local/bin/ruby",
            "/usr/bin/ruby",
        ] {
            if Path::new(p).exists() {
                return p.to_string();
            }
        }
        "ruby".to_string()
    })
}

/// `<path> (<ruby --version>)`, for the run header and the report file so a
/// divergence record is attributable to the exact oracle that produced it.
fn oracle_id() -> String {
    let path = oracle_path();
    let ver = Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    format!("{path} ({ver})")
}

/// Raw bytes, never `String`: Ruby can emit output that is not valid UTF-8
/// (`"\xff"`, an 8-bit encoding). Comparing bytes keeps the surface honest;
/// lossy rendering is for the human-facing report only.
struct RunOut {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: i32,
    timed_out: bool,
}

/// Render captured bytes for a report. Invalid UTF-8 is shown lossily AND
/// followed by a hex line, so two different invalid byte strings do not both
/// render to U+FFFD and hide a divergence.
fn render(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches('\n');
    if std::str::from_utf8(bytes).is_err() {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        return format!("{text}\n  (hex) {}", hex.join(" "));
    }
    text.to_string()
}

/// Strip the leading `-e:LINE:` / `<path>:LINE:` location and the `ruby:` tag so
/// diagnostics can be compared for wording, not for the exact interpreter name
/// or line prefix.
fn norm_stderr(s: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(s);
    let mut out = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Drop a leading `-e:12:` / `foo.rb:12:in ...` location prefix.
        let l = match line.find(':') {
            Some(idx) if line[..idx].contains("-e") || line[..idx].ends_with(".rb") => {
                // strip `-e:NN:` (two colons in)
                let rest = &line[idx + 1..];
                match rest.find(": ") {
                    Some(j) => &rest[j + 2..],
                    None => line,
                }
            }
            _ => line,
        };
        let l = l.strip_prefix("ruby: ").unwrap_or(l);
        out.push_str(l);
    }
    out.into_bytes()
}

/// The divergence predicate. stdout + exit always; stderr only under `--stderr`.
fn differs(a: &RunOut, b: &RunOut) -> bool {
    if a.stdout != b.stdout || a.exit != b.exit {
        return true;
    }
    if CMP_STDERR.load(Ordering::Relaxed) {
        return norm_stderr(&a.stderr) != norm_stderr(&b.stderr);
    }
    false
}

/// Spawn `cmd` and wait up to `timeout`, killing it if it overruns.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> RunOut {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return RunOut {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit: -999,
                timed_out: false,
            }
        }
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                use std::io::Read;
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut buf);
                }
                let mut ebuf = Vec::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut ebuf);
                }
                return RunOut {
                    stdout: buf,
                    stderr: ebuf,
                    exit: status.code().unwrap_or(-1),
                    timed_out: false,
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOut {
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        exit: -1,
                        timed_out: true,
                    };
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                return RunOut {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit: -998,
                    timed_out: false,
                }
            }
        }
    }
}

fn run_oracle(script: &str, timeout: Duration) -> RunOut {
    let mut cmd = Command::new(oracle_path());
    cmd.args(["-e", script]);
    run_with_timeout(cmd, timeout)
}

fn run_ours(script: &str, bin: &Path, timeout: Duration) -> RunOut {
    let mut cmd = Command::new(bin);
    cmd.args(["-e", script]);
    // A stale rkyv cache would let a chunk that once worked keep passing.
    cmd.env_remove("RUBYLANG_CACHE");
    run_with_timeout(cmd, timeout)
}

// ---------------------------------------------------------------------------
// Generators — one per Mode. Each returns a statement list; joined by newlines
// into a program. Most emit a single deterministic `p`/`puts` probe.
// ---------------------------------------------------------------------------

const INTS: &[&str] = &[
    "0", "1", "2", "7", "10", "-3", "-7", "42", "100", "-1", "5", "9",
];
const FLOATS: &[&str] = &[
    "0.1", "0.2", "1.5", "3.14", "2.0", "-1.5", "10.0", "0.0", "100.25", "-0.5", "1e10", "1.0",
];
const WORDS: &[&str] = &[
    "foo", "bar", "baz", "hello", "world", "abc", "xyz", "Ruby", "Lang",
];

fn ii<'a>(r: &mut Rng) -> &'a str {
    r.pick(INTS)
}
fn ff<'a>(r: &mut Rng) -> &'a str {
    r.pick(FLOATS)
}
fn ww<'a>(r: &mut Rng) -> &'a str {
    r.pick(WORDS)
}

fn one(s: String) -> Vec<String> {
    vec![s]
}

fn gen_arith(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let ops = ["+", "-", "*", "/", "%", "**"];
    let a = ii(r);
    let b = ii(r);
    let c = ii(r);
    let op1 = r.pick(&ops);
    let op2 = r.pick(&ops);
    // Guard divide/modulo by zero producing a mutual error is fine (both agree),
    // but keep the second operand nonzero often so real arithmetic is exercised.
    one(match r.below(4) {
        0 => format!("p {a} {op1} {b} {op2} {c}"),
        1 => format!("p ({a} {op1} {b}) {op2} {c}"),
        2 => format!("p -{a} {op1} {b}"),
        _ => format!("p {a}.fdiv({b})"),
    })
}

fn gen_bignum(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let e = r.range(20, 200);
    let base = r.pick(&["2", "3", "7", "10"]);
    one(match r.below(4) {
        0 => format!("p {base} ** {e}"),
        1 => format!("p (1..{}).reduce(1, :*)", r.range(15, 40)),
        2 => format!("p ({base} ** {e}).to_s.length"),
        _ => format!("p ({base} ** {e}) + 1"),
    })
}

fn gen_floatfmt(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = ff(r);
    let b = ff(r);
    let ops = ["+", "-", "*", "/"];
    let op = r.pick(&ops);
    one(match r.below(6) {
        0 => format!("p {a} {op} {b}"),
        1 => format!("p {}.0 / {}.0", r.range(1, 9), r.range(1, 9)),
        2 => format!("p ({a} {op} {b}).round({})", r.range(0, 6)),
        3 => format!("p {a}.to_s"),
        4 => format!("p 1e{}", r.range(-20, 300)),
        _ => format!("p {a} {op} {b} {op} {}", ff(r)),
    })
}

fn gen_strings(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let w = ww(r);
    let i = r.range(-4, 5);
    let j = r.range(-4, 5);
    one(match r.below(6) {
        0 => format!("p \"{w}\"[{i}]"),
        1 => format!("p \"{w}\"[{i}, {}]", r.range(0, 4)),
        2 => format!("p \"{w}\"[{i}..{j}]"),
        3 => format!("p \"{w}\"[{i}...{j}]"),
        4 => format!("p \"{w}\" * {}", r.range(0, 4)),
        _ => format!("p \"{w}\".include?(\"{}\")", &w[..1.min(w.len())]),
    })
}

fn gen_interp(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let w = ww(r);
    let n = ii(r);
    one(match r.below(5) {
        0 => format!("p \"val=#{{{n} + {}}}\"", ii(r)),
        1 => format!("p \"#{{'{w}'.upcase}}!\""),
        2 => format!(
            "p \"[#{{[1,2,3].map {{ |x| x * {} }}.join(',')}}]\"",
            r.range(1, 4)
        ),
        3 => format!("p \"#{{{n}}}-#{{'{w}'.length}}\""),
        _ => format!("puts \"a#{{{n} * 2}}b#{{'{w}'.reverse}}c\""),
    })
}

fn gen_ranges(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = r.range(-3, 5);
    let b = r.range(a, a + 8);
    one(match r.below(6) {
        0 => format!("p ({a}..{b}).to_a"),
        1 => format!("p ({a}...{b}).to_a"),
        2 => format!("p ({a}..{b}).sum"),
        3 => format!("p ({a}..{b}).step({}).to_a", r.range(2, 3)),
        4 => format!("p ({a}..{b}).map {{ |x| x * x }}"),
        _ => format!("p ({a}..{b}).select {{ |x| x % 2 == 0 }}"),
    })
}

fn arr_lit(r: &mut Rng) -> String {
    let n = r.range(3, 6) as usize;
    let items: Vec<String> = (0..n).map(|_| ii(r).to_string()).collect();
    format!("[{}]", items.join(", "))
}

fn gen_arraymeth(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = arr_lit(r);
    one(match r.below(12) {
        0 => format!("p {a}.map {{ |x| x + 1 }}"),
        1 => format!("p {a}.select {{ |x| x > 0 }}"),
        2 => format!("p {a}.reject {{ |x| x > 0 }}"),
        3 => format!("p {a}.reduce(:+)"),
        4 => format!("p {a}.uniq.sort"),
        5 => format!("p {a}.min"),
        6 => format!("p {a}.max"),
        7 => format!("p {a}.first({})", r.range(1, 3)),
        8 => format!("p {a}.last({})", r.range(1, 3)),
        9 => format!(
            "p {a}.take({}) + {a}.drop({})",
            r.range(1, 3),
            r.range(1, 3)
        ),
        10 => format!("p {a}.zip({a})"),
        _ => format!("p {a}.each_with_index.map {{ |x, i| x * i }}"),
    })
}

fn hash_lit(r: &mut Rng) -> String {
    let n = r.range(2, 4) as usize;
    let items: Vec<String> = (0..n)
        .map(|k| format!("{:?} => {}", format!("k{k}"), ii(r)))
        .collect();
    format!("{{{}}}", items.join(", "))
}

fn gen_hashmeth(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let h = hash_lit(r);
    one(match r.below(6) {
        0 => format!("p {h}.keys"),
        1 => format!("p {h}.values"),
        2 => format!("p {h}.map {{ |k, v| [k, v + 1] }}"),
        3 => format!("p {h}.select {{ |k, v| v > 0 }}"),
        4 => format!("p {h}.to_a.sort"),
        _ => format!("p {h}.each_pair.map {{ |k, v| \"#{{k}}=#{{v}}\" }}.sort"),
    })
}

fn gen_sorting(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = arr_lit(r);
    let w = format!(
        "[{}]",
        (0..4)
            .map(|_| format!("{:?}", ww(r)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    one(match r.below(6) {
        0 => format!("p {a}.sort"),
        1 => format!("p {a}.sort {{ |x, y| y <=> x }}"),
        2 => format!("p {a}.sort_by {{ |x| -x }}"),
        3 => format!("p {w}.sort_by(&:length)"),
        4 => format!("p {a}.min_by {{ |x| x.abs }}"),
        _ => format!("p {a}.max_by {{ |x| x.abs }}"),
    })
}

fn gen_formatspec(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(-100, 100);
    let f = ff(r);
    one(match r.below(8) {
        0 => format!("p format(\"%.3f\", {f})"),
        1 => format!("p \"%05d\" % {n}"),
        2 => format!("p \"%x\" % {}", n.abs()),
        3 => format!("p \"%b\" % {}", n.abs()),
        4 => format!("p \"%e\" % {f}"),
        5 => format!("p \"%o\" % {}", n.abs()),
        6 => format!("p \"%-8s|\" % \"{}\"", ww(r)),
        _ => format!("p sprintf(\"%+d %8.2f\", {n}, {f})"),
    })
}

fn gen_blocks(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(3, 6);
    one(match r.below(6) {
        0 => format!("p (1..{n}).map {{ |x| x ** 2 }}"),
        1 => format!("p (1..{n}).select(&:even?)"),
        2 => format!("p (1..{n}).reduce(0) {{ |acc, x| acc + x }}"),
        3 => format!("r = []; {n}.times {{ |i| r << i }}; p r"),
        4 => format!("r = []; 1.upto({n}) {{ |i| r << i * i }}; p r"),
        _ => format!("p (1..{n}).each_with_object([]) {{ |x, a| a << x + 1 }}"),
    })
}

fn gen_symbols(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let w = ww(r);
    one(match r.below(5) {
        0 => format!("p :{w}"),
        1 => format!("p \"{w}\".to_sym"),
        2 => format!("p :{w}.to_s"),
        3 => format!("h = {{ {w}: {} }}; p h[:{w}]", ii(r)),
        _ => format!("p %i[{} {} {}]", ww(r), ww(r), ww(r)),
    })
}

fn gen_ternary(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = ii(r);
    let b = ii(r);
    one(match r.below(5) {
        0 => format!("p {a} > {b} ? \"hi\" : \"lo\""),
        1 => format!("x = nil; x ||= {a}; p x"),
        2 => format!("x = {a}; x ||= {b}; p x"),
        3 => format!("x = {a}; x += {b}; p x"),
        _ => format!("x = {a}; x &&= {b}; p x"),
    })
}

fn gen_comparison(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = ii(r);
    let b = ii(r);
    one(match r.below(6) {
        0 => format!("p {a} <=> {b}"),
        1 => format!("p {a}.0 == {a}"),
        2 => format!("p [{a}, {b}] <=> [{b}, {a}]"),
        3 => format!("p [{a}, {b}, {}].min", ii(r)),
        4 => format!("p ({a}..{b}).include?({})", ii(r)),
        _ => format!("p \"{}\" <=> \"{}\"", ww(r), ww(r)),
    })
}

fn gen_printf(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = ii(r);
    let a = arr_lit(r);
    one(match r.below(6) {
        0 => format!("printf(\"%d-%d\\n\", {n}, {})", ii(r)),
        1 => format!("puts {a}"),
        2 => format!("print {n}, \" \", {}, \"\\n\"", ii(r)),
        3 => format!("p {a}"),
        4 => format!("puts [{}, [{}, {}]].inspect", ii(r), ii(r), ii(r)),
        _ => format!("$stdout.write(\"{}\\n\")", ww(r)),
    })
}

fn gen_string_ops(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let w = ww(r);
    one(match r.below(11) {
        0 => format!("p \"{w}\".upcase"),
        1 => format!("p \"{w}\".reverse"),
        2 => format!("p \"{w} {w}\".split"),
        3 => format!("p [\"{w}\", \"{}\"].join(\"-\")", ww(r)),
        4 => format!("p \"{w}\".gsub(\"{}\", \"X\")", &w[..1.min(w.len())]),
        5 => format!("p \"{w}\".sub(/./, \"Q\")"),
        6 => format!("p \"  {w}  \".strip"),
        7 => format!("p \"{w}\".chars"),
        8 => format!("p \"{w}\".center({}, \"*\")", r.range(6, 12)),
        9 => format!("p \"{w}\".ljust({}, \".\")", r.range(6, 12)),
        _ => format!("p \"{w}\".tr(\"a-y\", \"b-z\")"),
    })
}

fn gen_caseexpr(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(0, 12);
    let w = ww(r);
    one(match r.below(4) {
        0 => format!(
            "x = {n}; p (case x; when 0..3 then \"lo\"; when 4..8 then \"mid\"; else \"hi\"; end)"
        ),
        1 => format!(
            "v = {n}; p (case v; when Integer then \"int\"; when String then \"str\"; else \"?\"; end)"
        ),
        2 => format!(
            "s = \"{w}\"; p (case s; when /^[a-c]/ then \"early\"; when /^[x-z]/ then \"late\"; else \"mid\"; end)"
        ),
        _ => format!(
            "x = {n}; r = case x when 0 then :zero when 1..5 then :small else :big end; p r"
        ),
    })
}

// ---------------------------------------------------------------------------
// Mode plumbing.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Mode {
    Arith,
    Bignum,
    Floatfmt,
    Strings,
    Interp,
    Ranges,
    Arraymeth,
    Hashmeth,
    Sorting,
    Formatspec,
    Blocks,
    Symbols,
    Ternary,
    Comparison,
    Printf,
    StringOps,
    Caseexpr,
    Intmeth,
    Regex,
    Enumerable,
    Exceptions,
    Struct,
    Rational,
    Patternmatch,
    Kernelconv,
    Loopflow,
    Hashenum,
    Enumext,
    Kwargs,
    Metaprog,
    Mixins,
    Lambda,
    Enumlazy,
    Setops,
    Frozen,
    Datacls,
    Strenc,
    Complexnum,
    Objintro,
    Blockflow,
    Methobj,
    Multiyield,
    Eqlident,
    Numwide,
    /// Round-robin over every mode in `ALL_MODES`. Not itself a member of
    /// `ALL_MODES` (that would recurse), so adding a mode never changes any
    /// other mode's own seed→case mapping — but it DOES reshuffle which mode
    /// a given `all` seed lands on, so an `all` run is only comparable to
    /// another `all` run at the same `ALL_MODES` length.
    All,
}

const ALL_MODES: &[Mode] = &[
    Mode::Arith,
    Mode::Bignum,
    Mode::Floatfmt,
    Mode::Strings,
    Mode::Interp,
    Mode::Ranges,
    Mode::Arraymeth,
    Mode::Hashmeth,
    Mode::Sorting,
    Mode::Formatspec,
    Mode::Blocks,
    Mode::Symbols,
    Mode::Ternary,
    Mode::Comparison,
    Mode::Printf,
    Mode::StringOps,
    Mode::Caseexpr,
    Mode::Intmeth,
    Mode::Regex,
    Mode::Enumerable,
    Mode::Exceptions,
    Mode::Struct,
    Mode::Rational,
    Mode::Patternmatch,
    Mode::Kernelconv,
    Mode::Loopflow,
    Mode::Hashenum,
    Mode::Enumext,
    Mode::Kwargs,
    Mode::Metaprog,
    Mode::Mixins,
    Mode::Lambda,
    Mode::Enumlazy,
    Mode::Setops,
    Mode::Frozen,
    Mode::Datacls,
    Mode::Strenc,
    Mode::Complexnum,
    Mode::Objintro,
    Mode::Blockflow,
    Mode::Methobj,
    Mode::Multiyield,
    Mode::Eqlident,
    Mode::Numwide,
];

fn gen_intmeth(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = ii(r);
    let b = ii(r);
    one(match r.below(14) {
        0 => format!("p {a}.gcd({b})"),
        1 => format!("p {a}.lcm({b})"),
        2 => format!("p {a}.abs"),
        3 => format!("p {a}.divmod({b})"),
        4 => format!("p {a}.abs.bit_length"),
        5 => format!("p {a}.abs.digits"),
        6 => format!("p {a}.to_s(2)"),
        7 => format!(
            "p {}.pow({}, {})",
            r.range(2, 9),
            r.range(1, 6),
            r.range(2, 9)
        ),
        8 => format!("p({a} & {b})"),
        9 => format!("p({a} | {b})"),
        10 => format!("p({a} ^ {b})"),
        11 => format!("p({a} << {})", r.range(0, 5)),
        12 => format!("p [{a}.even?, {a}.odd?, {a}.zero?]"),
        _ => format!(
            "p {a}.abs.to_s({}).to_i({})",
            r.range(2, 17),
            r.range(2, 17)
        ),
    })
}

/// Regexps as MATCHERS (cases 0-9) and as VALUES (cases 10-18).
///
/// The value half is the part a match-only generator cannot reach: Ruby defines
/// `Regexp#==` by source + options + encoding, so two separately-written
/// literals with the same source are the SAME value and collapse under `uniq`,
/// `-`, `include?` and as a Hash key. An implementation that leaves them on
/// object identity answers every one of those the other way, and no amount of
/// `=~`/`scan`/`gsub` fuzzing will show it.
///
/// The flag pool carries `im` and `mi` on purpose: options are a normalized
/// BITMASK in Ruby, so those two are equal, which a comparison written on the
/// raw flag TEXT gets wrong while still passing every single-flag case.
fn gen_regex(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let s = format!("{}{}", ww(r), ww(r));
    let pats = [
        "[a-c]+", "o+", "[aeiou]", "\\w+", "l+", "[a-z]{2}", "^.", ".$",
    ];
    let p = r.pick(&pats);
    // A second literal, drawn independently, so a pair is same-source about as
    // often as it is different-source.
    let q = r.pick(&pats);
    let flags = ["", "i", "m", "x", "im", "mi", "imx"];
    let (f, g) = (r.pick(&flags), r.pick(&flags));
    one(match r.below(19) {
        0 => format!("p(\"{s}\" =~ /{p}/)"),
        1 => format!("p \"{s}\".match?(/{p}/)"),
        2 => format!("p \"{s}\".scan(/{p}/)"),
        3 => format!("p \"{s}\".gsub(/{p}/, \"X\")"),
        4 => format!("p \"{s}\".sub(/{p}/, \"X\")"),
        5 => format!("p \"{s}\"[/{p}/]"),
        6 => format!("p \"{s}\".match(/{p}/) ? \"m\" : \"no\""),
        7 => format!("p \"{s}\".gsub(/([a-z])\\1/, \"D\")"),
        8 => format!("p \"{s}\".split(/{p}/)"),
        9 => format!("p \"{s}\".scan(/{p}/).length"),
        // --- Regexp as a value ---
        10 => format!("p(/{p}/{f} == /{q}/{g})"),
        11 => format!("p(/{p}/{f}.eql?(/{q}/{g}))"),
        // The hash VALUE is not comparable between two interpreters; whether two
        // hashes AGREE is.
        12 => format!("p(/{p}/{f}.hash == /{q}/{g}.hash)"),
        13 => format!("p([/{p}/{f}, /{q}/{g}, /{p}/{f}].uniq.size)"),
        14 => format!("p({{/{p}/{f} => 1}}[/{q}/{g}])"),
        15 => format!("p([/{p}/{f}].include?(/{q}/{g}))"),
        16 => format!("p(([/{p}/{f}, /{q}/{g}] - [/{p}/{f}]).size)"),
        17 => format!("p([/{p}/{f}.options, /{p}/{f}.inspect, /{p}/{f}.source])"),
        // Single-quoted: a double-quoted "\\w+" would collapse the escape in the
        // Ruby string literal before `Regexp.new` ever saw it.
        _ => format!("p(Regexp.new('{p}') == /{p}/)"),
    })
}

fn gen_enumerable(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let a = arr_lit(r);
    one(match r.below(14) {
        0 => format!("p {a}.each_slice(2).to_a"),
        1 => format!("p {a}.each_cons(2).to_a"),
        2 => format!("p {a}.partition {{ |x| x > 0 }}"),
        3 => format!("p {a}.group_by {{ |x| x % 2 }}"),
        4 => format!("p {a}.flat_map {{ |x| [x, -x] }}"),
        5 => format!("p {a}.chunk_while {{ |a, b| b > a }}.to_a"),
        6 => format!("p {a}.take_while {{ |x| x > 0 }}"),
        7 => format!("p {a}.drop_while {{ |x| x > 0 }}"),
        8 => format!("p {a}.each_with_object([]) {{ |x, m| m << x * 2 }}"),
        9 => format!("p {a}.tally"),
        10 => format!("p {a}.min_by {{ |x| x.abs }}"),
        11 => format!("p {a}.max_by {{ |x| x.abs }}"),
        12 => format!("p {a}.sum"),
        _ => format!("p {a}.sort_by {{ |x| -x }}"),
    })
}

fn gen_exceptions(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let w = ww(r);
    let n = ii(r);
    one(match r.below(9) {
        0 => format!("p (begin; raise \"{w}\"; rescue => e; e.message; end)"),
        1 => format!("p (begin; Integer(\"{w}\"); rescue ArgumentError; :caught; end)"),
        2 => format!("p (begin; {n} / 0; rescue ZeroDivisionError => e; e.message; end)"),
        3 => "p (begin; [].fetch(9); rescue IndexError; :idx; end)".to_string(),
        4 => format!(
            "p (begin; raise ArgumentError, \"{w}\"; rescue => e; [e.class.to_s, e.message]; end)"
        ),
        5 => "r = []; begin; r << 1; raise \"x\"; rescue; r << 2; ensure; r << 3; end; p r"
            .to_string(),
        6 => format!("p (begin; {{}}.fetch(:{w}); rescue KeyError; :key; end)"),
        7 => "class E1 < StandardError; end; p (begin; raise E1; rescue E1; :custom; end)"
            .to_string(),
        _ => "p (begin; nil.foo; rescue NoMethodError; :nome; end)".to_string(),
    })
}

fn gen_struct(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (x, y) = (ii(r), ii(r));
    one(match r.below(8) {
        0 => format!("S = Struct.new(:a, :b); p S.new({x}, {y}).to_a"),
        1 => format!("S = Struct.new(:a, :b); p S.new({x}, {y}).to_h"),
        2 => format!("S = Struct.new(:a, :b); p S.new({x}, {y}).members"),
        3 => format!("S = Struct.new(:a, :b); p(S.new({x}, {y}) == S.new({x}, {y}))"),
        4 => format!("S = Struct.new(:a, :b); s = S.new({x}, {y}); p [s.a, s.b]"),
        5 => format!("S = Struct.new(:a, :b); s = S.new({x}, {y}); s.a = {y}; p s.a"),
        6 => format!("S = Struct.new(:a, :b, keyword_init: true); p S.new(a: {x}, b: {y}).to_h"),
        _ => format!("S = Struct.new(:a, :b); p S.new({x}, {y})[0]"),
    })
}

/// Loop control flow: `break`/`next`/`redo`/`retry` in every construct that owns
/// them, plus `for`'s no-scope rule (the loop variable AND any local the body
/// assigns outlive the loop, and every closure the body makes shares the one
/// binding). Every `redo`/`retry` is guarded by a counter so the program always
/// terminates — a hang is a divergence the harness reports as a timeout, not a
/// gap, so a non-terminating generator would silently teach nothing.
fn gen_loopflow(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(2, 5);
    let k = r.range(1, 3);
    one(match r.below(18) {
        0 => format!("c = 0; {n}.times {{ |i| c += 1; redo if c == {k} }}; p c"),
        1 => format!(
            "c = 0; r = []; [1, 2, 3].each {{ |x| c += 1; redo if c == {k}; r << [x, c] }}; p [r, c]"
        ),
        2 => format!(
            "i = 0; c = 0\nwhile i < {n}\n  c += 1\n  if c == {k}\n    i += 1\n    redo\n  end\n  i += 1\nend\np [i, c]"
        ),
        3 => format!("c = 0\nfor x in 1..{n}\n  c += 1\n  redo if c == {k}\nend\np [c, x]"),
        4 => format!(
            "c = 0\nuntil c >= {n}\n  c += 1\n  redo if c == {k}\nend\np c"
        ),
        5 => format!(
            "c = 0\ni = 0\nwhile i < {n}\n  begin\n    c += 1\n    redo if c == {k}\n  end\n  i += 1\nend\np [i, c]"
        ),
        6 => format!("r = []; [1, 2, 3].each {{ |x| next if x == {k}; r << x }}; p r"),
        7 => format!("p (while true do break {n} end)"),
        8 => format!("i = 0; while i < {n}; i += 1; break if i == {k}; end; p i"),
        9 => format!("for i in 1..{n}\n  sq = i * i\nend\np [i, sq, defined?(sq)]"),
        10 => format!("for i in []\n  z = {n}\nend\np [i, z, defined?(z)]"),
        11 => format!(
            "ps = []\nfor i in 0...{n}\n  t = i * 2\n  ps << -> {{ [i, t] }}\nend\np ps.map(&:call)"
        ),
        12 => format!(
            "for k, v in {{a: 1, b: {n}}}\n  s = \"#{{k}}=#{{v}}\"\nend\np [k, v, s]"
        ),
        13 => format!(
            "a = 0\nbegin\n  a += 1\n  raise \"x\" if a < {k}\nrescue\n  retry if a < {k}\nend\np a"
        ),
        14 => format!(
            "for i in 1..{n}\n  if i == {k}\n    m = i\n  end\nend\np [m, defined?(m)]"
        ),
        15 => format!(
            "out = []\nfor i in 1..{n}\n  for j in 1..2\n    out << i * j\n  end\nend\np [out, i, j]"
        ),
        16 => format!("c = 0; [1, 2].map {{ |x| c += 1; redo if c == {k}; x * 10 }}.then {{ |a| p [a, c] }}"),
        _ => format!(
            "r = []\n{n}.times do |i|\n  r << i\n  break if i == {k}\nend\np r"
        ),
    })
}

/// Hash through Enumerable. MRI derives these from `Hash#each`, which yields the
/// whole `[k, v]` pair as ONE value — so a one-parameter block sees the pair,
/// not the key. That distinction is invisible to a `{ |k, v| }` block (it
/// auto-splats), which is why it survived the original corpus.
fn gen_hashenum(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let h = hash_lit(r);
    one(match r.below(20) {
        0 => format!("r = []; {h}.each {{ |x| r << x }}; p r"),
        1 => format!("r = []; {h}.each_pair {{ |x| r << x }}; p r"),
        2 => format!("p {h}.map {{ |x| x }}"),
        3 => format!("p {h}.collect {{ |x| x }}"),
        4 => format!("p {h}.find {{ |x| x.is_a?(Array) }}"),
        5 => format!("p {h}.detect {{ |x| x[1] > 0 }}"),
        6 => format!("p {h}.count {{ |x| x.is_a?(Array) }}"),
        7 => format!("p {h}.count"),
        8 => format!("p {h}.sum(0) {{ |x| x[1] }}"),
        9 => format!("p {h}.flat_map {{ |x| x }}"),
        10 => format!("p {h}.filter_map {{ |x| x[0] }}"),
        11 => format!("p [{h}.any? {{ |x| x.is_a?(Array) }}, {h}.all? {{ |x| x.size == 2 }}, {h}.none? {{ |x| x.nil? }}]"),
        12 => format!("p {h}.take_while {{ |x| x.is_a?(Array) }}"),
        13 => format!("p {h}.drop_while {{ |x| x.is_a?(Array) }}"),
        14 => format!("p {h}.find_index {{ |x| x[1] > 0 }}"),
        15 => format!("p {h}.min_by {{ |x| x[1] }}"),
        16 => format!("p {h}.sort_by {{ |x| x[0] }}"),
        17 => format!("p {h}.to_h {{ |k, v| [k.to_s, v] }}"),
        18 => format!("p {h}.each_with_object([]) {{ |x, a| a << x }}"),
        _ => format!("p [{h}.first, {h}.reverse_each.to_a, {h}.tally.size]"),
    })
}

/// `Enumerator.new { |y| ... }` external iteration. MRI runs the block on a
/// Fiber, so `next` advances exactly one `y <<` — a side effect or a raise after
/// the second yield must surface on the *third* `next`, and an endless generator
/// must answer `next` rather than hang. Endless generators are always consumed
/// by a bounded `next`/`first`/`take`, never by `to_a`.
fn gen_enumext(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(1, 4);
    let m = r.range(2, 5);
    one(match r.below(14) {
        0 => "e = Enumerator.new {{ |y| y << 1; y << 2; y << 3 }}; p [e.next, e.next, e.next]".to_string(),
        1 => "e = Enumerator.new {{ |y| y << 1; y << 2 }}; p [e.next, e.peek, e.next]".to_string(),
        2 => "log = []\ne = Enumerator.new {{ |y| log << :a; y << 1; log << :b; y << 2; log << :c }}\np log\np e.next\np log\np e.next\np log"
            .to_string(),
        3 => "e = Enumerator.new {{ |y| y << 1; raise \"boom\" }}\np e.next\nbegin\n  e.next\nrescue => ex\n  p [ex.class, ex.message]\nend"
            .to_string(),
        4 => "e = Enumerator.new {{ |y| y << 1; raise ArgumentError, \"bad\" }}\np e.next\nbegin\n  e.next\nrescue ArgumentError => ex\n  p [ex.class, ex.is_a?(StandardError)]\nend"
            .to_string(),
        5 => "e = Enumerator.new {{ |y| i = 0; loop {{ y << i; i += 1 }} }}\np [e.next, e.next, e.next]"
            .to_string(),
        6 => format!(
            "e = Enumerator.new {{ |y| i = 0; loop {{ y << i; i += 1 }} }}\np e.first({m})\np e.take({n})"
        ),
        7 => format!(
            "e = Enumerator.new {{ |y| i = 0; loop {{ y << i * 2; i += 1 }} }}\np e.lazy.map {{ |x| x + 1 }}.first({m})"
        ),
        8 => format!(
            "e = Enumerator.new {{ |y| {n}.times {{ |i| y << i }} }}\nr = []\nloop {{ r << e.next }}\np r"
        ),
        9 => "e = Enumerator.new {{ |y| y << 1; y << 2 }}\np e.next\ne.rewind\np e.next\np e.to_a"
            .to_string(),
        10 => "e = Enumerator.new {{ |y| y << 1 }}\np e.next\nbegin\n  e.next\nrescue StopIteration => ex\n  p ex.class\nend"
            .to_string(),
        11 => format!(
            "a = Enumerator.new {{ |y| {m}.times {{ |i| y << \"a#{{i}}\" }} }}\nb = Enumerator.new {{ |y| {m}.times {{ |i| y << \"b#{{i}}\" }} }}\np [a.next, b.next, a.next, b.next]"
        ),
        12 => format!("e = Enumerator.new {{ |y| y.yield({n}, {m}); y << 9 }}; p [e.next, e.next]"),
        _ => "e = [1, 2, 3].each; p [e.next, e.peek, e.next, e.next]".to_string(),
    })
}

/// Keyword arguments: required / defaulted / `**rest`, the positional+keyword
/// split, and the double-splat call form.
fn gen_kwargs(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (ii(r), ii(r));
    one(match r.below(12) {
        0 => format!("def f(x:, y: {b}) = [x, y]\np f(x: {a})"),
        1 => format!("def f(x:, y:) = [x, y]\np f(y: {b}, x: {a})"),
        2 => format!("def f(a, x: {b}) = [a, x]\np f({a})"),
        3 => format!("def f(**o) = o\np f(a: {a}, b: {b})"),
        4 => format!("def f(x:, **o) = [x, o]\np f(x: {a}, z: {b})"),
        5 => format!("def f(x:, y: {b}) = [x, y]\nh = {{x: {a}, y: {b}}}\np f(**h)"),
        6 => format!("def f(a, b = {b}, *r, k:, **o) = [a, b, r, k, o]\np f({a}, k: {b})"),
        7 => "def f(x:) = x\nbegin\n  f\nrescue ArgumentError => e\n  p e.class\nend".to_string(),
        8 => format!("def f(x:) = x\nbegin\n  f(x: {a}, y: {b})\nrescue ArgumentError => e\n  p e.class\nend"),
        9 => format!("f = ->(x:, y: {b}) {{ [x, y] }}\np f.call(x: {a})"),
        10 => format!("def f(*a, **o) = [a, o]\np f({a}, {b}, k: {a})"),
        _ => format!("def f(x: {a}) = x\np [f, f(x: {b})]"),
    })
}

/// Metaprogramming hooks: `method_missing`/`respond_to_missing?`,
/// `define_method`, `Module#prepend` + `super`, singleton methods, and
/// `send`/`public_send`.
fn gen_metaprog(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (ii(r), ii(r));
    let w = ww(r);
    one(match r.below(14) {
        0 => format!(
            "class C\n  def method_missing(n, *a) = [n, a]\n  def respond_to_missing?(n, p = false) = true\nend\np C.new.{w}({a})"
        ),
        1 => format!(
            "class C\n  def method_missing(n, *a) = n.to_s\nend\np [C.new.respond_to?(:{w}), C.new.{w}]"
        ),
        2 => format!(
            "class C\n  define_method(:m) {{ |x| x + {a} }}\nend\np C.new.m({b})"
        ),
        3 => format!(
            "class C\n  [:a, :b].each {{ |n| define_method(n) {{ n.to_s * {} }} }}\nend\np [C.new.a, C.new.b]",
            r.range(1, 3)
        ),
        4 => "module M\n  def m = \"M\" + super\nend\nclass C\n  prepend M\n  def m = \"C\"\nend\np C.new.m"
            .to_string(),
        5 => "module M\n  def m = \"M\"\nend\nclass C\n  include M\n  def m = \"C\" + super\nend\np C.new.m"
            .to_string(),
        6 => format!("class C; def m = {a}; end\np [C.new.send(:m), C.new.public_send(:m)]"),
        7 => format!(
            "o = Object.new\ndef o.m = {a}\np [o.m, o.singleton_methods.sort]"
        ),
        8 => format!(
            "class C\n  def m = {a}\nend\nC.class_eval {{ def n = {b} }}\np [C.new.m, C.new.n]"
        ),
        9 => format!(
            "class C; end\nC.define_method(:m) {{ {a} }}\np C.new.m"
        ),
        10 => format!(
            "class C\n  def initialize; @x = {a}; end\nend\np C.new.instance_variable_get(:@x)"
        ),
        11 => format!(
            "class C; def m = {a}; end\np [C.instance_methods(false).sort, C.new.respond_to?(:m)]"
        ),
        12 => format!(
            "module M\n  def self.included(b) = b.const_set(:TAG, {a})\nend\nclass C\n  include M\nend\np C::TAG"
        ),
        _ => format!(
            "class C\n  def method_missing(n, *a)\n    n.to_s.start_with?(\"get_\") ? {a} : super\n  end\nend\nbegin\n  C.new.nope\nrescue NoMethodError => e\n  p e.class\nend\np C.new.get_x"
        ),
    })
}

/// `Comparable` / `Enumerable` mixed into a user class: every derived method has
/// to come from the single `<=>` / `each` the class defines.
fn gen_mixins(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (r.range(0, 9), r.range(0, 9));
    let cmp = "class N\n  include Comparable\n  attr_reader :v\n  def initialize(v) = @v = v\n  def <=>(o) = v <=> o.v\n  def to_s = \"N(#{v})\"\nend";
    let enu = "class Bag\n  include Enumerable\n  def initialize(*xs) = @xs = xs\n  def each(&b) = @xs.each(&b)\nend";
    one(match r.below(14) {
        0 => format!("{cmp}\np(N.new({a}) < N.new({b}))"),
        1 => format!("{cmp}\np(N.new({a}) >= N.new({b}))"),
        2 => format!("{cmp}\np N.new({a}).between?(N.new(0), N.new(9))"),
        3 => format!("{cmp}\np N.new({a}).clamp(N.new(2), N.new(6)).to_s"),
        4 => format!("{cmp}\np [N.new({a}), N.new({b})].max.to_s"),
        5 => format!("{cmp}\np [N.new({a}), N.new({b})].sort.map(&:to_s)"),
        6 => format!("{enu}\np Bag.new({a}, {b}, 3).map {{ |x| x * 2 }}"),
        7 => format!("{enu}\np Bag.new({a}, {b}, 3).select(&:even?)"),
        8 => format!("{enu}\np Bag.new({a}, {b}, 3).sort"),
        9 => format!("{enu}\np Bag.new({a}, {b}, 3).include?({a})"),
        10 => format!("{enu}\np Bag.new({a}, {b}, 3).each_with_index.to_a"),
        11 => format!(
            "{enu}\np [Bag.new({a}, {b}).min, Bag.new({a}, {b}).max, Bag.new({a}, {b}).sum]"
        ),
        12 => format!("{enu}\np Bag.new({a}, {b}, 3).partition(&:odd?)"),
        _ => format!("{enu}\np Bag.new({a}, {b}, 3).each_slice(2).to_a"),
    })
}

/// Rational arithmetic (cases 0-10) and ORDERING FAILURE (cases 11-16).
///
/// Rational gets `< <= > >=` from `Comparable`, so they are defined by `<=>`:
/// when `<=>` answers nil the operator raises `ArgumentError`, it does not
/// answer false. Two things make this worth generating rather than asserting:
///
/// - It is ASYMMETRIC. `Rational(1,2) < Float::NAN` raises, but
///   `Float::NAN < Rational(1,2)` is plain IEEE and answers false, because
///   `Float#<` is its own method and never consults `<=>`. An implementation
///   that "raises on a NaN operand" passes the first and fails the second, so
///   both directions are generated (cases 11/12) against the same operator.
/// - The operand's name in the message is not its class. MRI's `rb_cmperr`
///   prints `inspect` for a Float or a special constant (`NaN`, `nil`, `:sym`)
///   and the CLASS for everything else (`String`, `Array`), so case 13 draws
///   from both kinds.
fn gen_rational(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (r.range(1, 12), r.range(1, 12));
    let (c, d) = (r.range(1, 12), r.range(1, 12));
    let n = ii(r);
    let relop = r.pick(&["<", ">", "<=", ">="]);
    // Left column renders via `inspect` in the message, right column via class.
    let operand = r.pick(&["Float::NAN", "nil", ":sym", "\"s\"", "[1]", "Object.new"]);
    one(match r.below(17) {
        0 => format!("p(Rational({a}, {b}) + Rational({c}, {d}))"),
        1 => format!("p(Rational({a}, {b}) - Rational({c}, {d}))"),
        2 => format!("p(Rational({a}, {b}) * Rational({c}, {d}))"),
        3 => format!("p(Rational({a}, {b}) / Rational({c}, {d}))"),
        4 => format!("p(Rational({a}, {b}) % Rational({c}, {d}))"),
        5 => format!("p(Rational({a}, {b}) ** {})", r.range(-3, 4)),
        6 => format!("p(Rational({a}, {b}) <=> Rational({c}, {d}))"),
        7 => format!("p(Rational({a}, {b}) + {n})"),
        8 => format!("p(Rational({a}, {b}).to_f.round(6))"),
        9 => format!("p [Rational({a}, {b}).numerator, Rational({a}, {b}).denominator]"),
        10 => format!("p({n} / Rational({c}, {d}))"),
        // Rational on the LEFT: `<=>` is nil, so Comparable raises.
        11 => format!(
            "begin\n  p(Rational({a}, {b}) {relop} Float::NAN)\nrescue ArgumentError => e\n  p e.message\nend"
        ),
        // Float on the left: `Float#<` is IEEE and must NOT raise.
        12 => format!(
            "begin\n  p(Float::NAN {relop} Rational({a}, {b}))\nrescue ArgumentError => e\n  p e.message\nend"
        ),
        13 => format!(
            "begin\n  p(Rational({a}, {b}) {relop} {operand})\nrescue => e\n  p [e.class, e.message]\nend"
        ),
        // `<=>` itself answers nil rather than raising — the value the operators
        // are derived from, pinned separately so a fix can't satisfy the
        // operators by making `<=>` raise too.
        14 => format!("p(Rational({a}, {b}) <=> Float::NAN)"),
        // Reached through Comparable's other two derived methods, and through
        // the sort that uses `<=>` directly.
        15 => format!(
            "begin\n  p(Rational({a}, {b}).clamp(Rational({c}, {d}), Float::NAN))\nrescue ArgumentError => e\n  p e.message\nend"
        ),
        _ => format!(
            "begin\n  p([Rational({a}, {b}), Float::NAN].sort)\nrescue ArgumentError => e\n  p e.message\nend"
        ),
    })
}

fn gen_patternmatch(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(0, 12);
    let (x, y) = (ii(r), ii(r));
    one(match r.below(8) {
        0 => format!("case [{x}, {y}]; in [a, b]; p a + b; end"),
        1 => format!("case {{a: {x}, b: {y}}}; in {{a:, b:}}; p [a, b]; end"),
        2 => format!("case [1, {x}, 3]; in [1, m, 3]; p m; in _; p :no; end"),
        3 => format!("case {n}; in 0..5; p :lo; in Integer; p :hi; end"),
        4 => format!("case [{x}, {y}]; in [Integer => a, Integer => b]; p a * b; end"),
        5 => format!("case [1, 2, {x}, {y}]; in [_, _, *rest]; p rest; end"),
        6 => format!(
            "case {{name: \"{}\", age: {n}}}; in {{name: String => s}}; p s; end",
            ww(r)
        ),
        _ => {
            format!("r = (case {n}; in 0 then :z; in n if n > 5 then :big; else :small; end); p r")
        }
    })
}

/// Non-local control flow **as the value of a block-taking call**.
///
/// `loopflow` already emits `break`/`next`/`redo`, but always as a statement
/// inside a loop whose result is thrown away (`arr.each { break }; p r`). The
/// value the block-taking call itself evaluates to was never probed, and that
/// is exactly where MRI's rule lives: a `break` ends the invocation the block
/// LITERAL was written on and becomes its value. Every case here therefore puts
/// the call in value position — `p(recv.meth { break v })` — and varies the
/// method, so a method that silently returns its receiver (or swallows the
/// statement's output entirely) cannot hide.
///
/// Also covers the shapes a per-method generator cannot reach: `break` crossing
/// a user-defined `yield`, blocks nested two deep, `break`/`next`/`return`
/// interacting with `begin/rescue/ensure`, and proc-vs-lambda `return`.
fn gen_blockflow(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let v = ii(r);
    let w = ww(r);
    // Block-taking methods whose value is observable. Each entry is
    // (receiver, method-call-prefix, block params).
    let arr_meths = [
        "each",
        "map",
        "select",
        "filter",
        "reject",
        "find",
        "detect",
        "find_index",
        "sort_by",
        "min_by",
        "max_by",
        "group_by",
        "partition",
        "take_while",
        "drop_while",
        "sum",
        "count",
        "flat_map",
        "each_with_index",
        "each_entry",
        "find_all",
        "filter_map",
        "reverse_each",
        "each_index",
        "any?",
        "all?",
        "none?",
        "one?",
        "delete_if",
        "keep_if",
    ];
    let hash_meths = [
        "each",
        "each_pair",
        "map",
        "select",
        "filter",
        "reject",
        "find",
        "detect",
        "each_with_index",
        "each_key",
        "each_value",
        "sort_by",
        "min_by",
        "max_by",
        "group_by",
        "partition",
        "sum",
        "count",
        "any?",
        "all?",
        "none?",
        "one?",
        "delete_if",
        "keep_if",
        "transform_values",
        "transform_keys",
        "filter_map",
        "flat_map",
        "each_entry",
        "find_all",
    ];
    one(match r.below(24) {
        // The core shape: the break value IS the call's value.
        0 => format!("p([1, 2, 3].{} {{ |x| break {v} }})", r.pick(&arr_meths)),
        1 => format!(
            "p({{a: 1, b: 2}}.{} {{ |k, vv| break {v} }})",
            r.pick(&hash_meths)
        ),
        // Conditional break — the loop runs partway, then yields the value.
        2 => format!(
            "p([1, 2, 3, 4].{} {{ |x| break x * 10 if x == {}; x }})",
            r.pick(&arr_meths),
            r.range(1, 4)
        ),
        // Arity-taking iterators that return the receiver without a break.
        3 => format!("p([1, 2, 3, 4].each_slice({}) {{ |s| break {v} }})", r.range(1, 3)),
        4 => format!("p([1, 2, 3, 4].each_cons({}) {{ |s| break {v} }})", r.range(1, 3)),
        5 => format!("p([1, 2, 3].each_with_object([]) {{ |x, acc| break {v} }})"),
        6 => format!("p(\"{w}\".each_char {{ |c| break {v} }})"),
        7 => format!("p(\"a\\nb\\nc\".each_line {{ |l| break {v} }})"),
        8 => format!("p((1..{}).each_with_index {{ |x, i| break {v} }})", r.range(2, 5)),
        9 => format!("p({}.times {{ |i| break {v} }})", r.range(1, 4)),
        10 => format!("p(1.upto({}) {{ |i| break {v} }})", r.range(2, 5)),
        // Break crossing a user-defined `yield`: the method ends and the value
        // belongs to the literal's call site.
        11 => format!(
            "def y\n  yield 1\n  :after\nend\np(y {{ |x| break {v} }})"
        ),
        12 => format!(
            "def y2\n  r = yield 1\n  [:got, r]\nend\np(y2 {{ |x| next {v} }})"
        ),
        // Nested two deep: only the inner call is the inner break's target.
        13 => format!(
            "p([1, 2].map {{ |a| [3, 4].each {{ |b| break {v} }} }})"
        ),
        14 => format!(
            "p([1, 2].each {{ |a| [3, 4].each {{ |b| break {v} }}; }})"
        ),
        // `return` from a block is non-local: it ends the defining method.
        15 => format!(
            "def m\n  [1, 2, 3].each {{ |x| return x + {} if x == 2 }}\n  :fell\nend\np m",
            r.range(0, 9)
        ),
        16 => format!(
            "def m2\n  [1, 2].each {{ |a| [3, 4].each {{ |b| return [a, b] if b == 4 }} }}\n  :none\nend\np m2"
        ),
        // proc vs lambda `return` differ; verify rather than assume.
        17 => format!(
            "def pr\n  q = Proc.new {{ return {v} }}\n  q.call\n  :after\nend\np pr"
        ),
        18 => format!(
            "def la\n  q = lambda {{ return {v} }}\n  q.call\n  :after\nend\np la"
        ),
        19 => format!("l = lambda {{ break {v} }}\np l.call"),
        // ensure must still run, and must not eat the value.
        20 => format!(
            "def e\n  o = []\n  [1, 2, 3].each {{ |i| begin; o << i; break if i == 2; ensure; o << :E; end }}\n  o\nend\np e"
        ),
        21 => format!(
            "def e2\n  begin\n    [1, 2].each {{ |i| return {v} if i == 2 }}\n  ensure\n    nil\n  end\n  :no\nend\np e2"
        ),
        22 => format!(
            "def e3\n  r = begin\n    {v}\n  rescue\n    -1\n  ensure\n    -2\n  end\n  r\nend\np e3"
        ),
        // `for` does NOT create a scope; a block does.
        _ => format!(
            "for i in 1..{}\n  sq = i\nend\np [i, sq, defined?(sq)]\n[9].each {{ |jj| kk = jj }}\np [defined?(jj), defined?(kk)]",
            r.range(2, 4)
        ),
    })
}

fn gen_kernelconv(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = ii(r);
    let w = ww(r);
    one(match r.below(11) {
        0 => format!("p Integer(\"{}\")", r.range(0, 999)),
        1 => "p Integer(\"ff\", 16)".to_string(),
        2 => format!(
            "p Integer(\"{}\", 2)",
            if r.below(2) == 0 { "1010" } else { "1101" }
        ),
        3 => format!("p Float(\"{}.5\")", r.range(0, 99)),
        4 => "p Array(nil)".to_string(),
        5 => format!("p Array([{n}])"),
        6 => format!("p Array({n})"),
        7 => format!("p (begin; Integer(\"{w}\"); rescue ArgumentError; :bad; end)"),
        8 => format!("p String({n})"),
        9 => format!("p format(\"%05.2f\", {})", r.range(0, 99)),
        _ => format!("p Integer({n}.to_f)"),
    })
}

/// Sources that yield TWO values per iteration (`each_with_index`,
/// `each_with_object`, `y.yield a, b`) driven through every kind of consumer,
/// plus `.lazy` over a non-Array source.
///
/// The reshaping is per consumer and not derivable: `take_while`'s block sees the
/// first value while the element it keeps is the packed pair, `select`'s block
/// sees the pair, and `map`'s sees the first. A generator that varies only the
/// consumer, or only the block shape, cannot tell those apart — this one varies
/// both, and the block shapes include the ones with different binding rules
/// (`{ |x| }`, `{ |x, i| }`, `{ |*a| }`).
fn gen_multiyield(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    // Sources whose every iteration yields two values.
    let pair_srcs = [
        "[10, 20, 30].each_with_index",
        "[10, 20].each_with_object([])",
        "%w[a b c].each_with_index",
        "(1..3).each_with_index",
        "Enumerator.new { |y| y.yield 1, 2; y.yield 3, 4 }",
        "Enumerator.new { |y| y.yield 5, 6 }",
    ];
    // Sources whose iteration yields ONE value that happens to be an array — the
    // control group: their block must NOT see it unpacked.
    let pack_srcs = [
        "{a: 1, b: 2}",
        "[[1, 2], [3, 4]]",
        "[1, 2, 3, 4].each_slice(2)",
        "[1, 2, 3].each_cons(2)",
        "Enumerator.new { |y| y << [1, 2]; y << [3, 4] }",
    ];
    // Block bodies that DISCRIMINATE: their answer differs depending on whether
    // the block was handed the first value or the packed pair.
    let probes = [
        "{ |x| x.is_a?(Array) }",
        "{ |x| x.to_s.size > 1 }",
        "{ |x, i| x.is_a?(Array) }",
        "{ |*a| a.size > 1 }",
        "{ |x, i| i.nil? }",
    ];
    let src = if r.below(3) == 0 {
        r.pick(&pack_srcs)
    } else {
        r.pick(&pair_srcs)
    };
    let b = r.pick(&probes);
    // Consumers split three ways in MRI: block sees the first value and the
    // answer is the block's, block sees the first value but the answer is the
    // source element, block sees the packed pair.
    // `each_entry` is absent on purpose: its value is the Enumerator itself, and
    // a generator-backed one inspects with its address, which is not comparable.
    let consumers = [
        "map",
        "collect",
        "flat_map",
        "filter_map",
        "take_while",
        "count",
        "find_index",
        "any?",
        "all?",
        "none?",
        "one?",
        "select",
        "filter",
        "reject",
        "sort_by",
        "group_by",
        "partition",
        "drop_while",
        "find",
        "detect",
        "min_by",
        "max_by",
        "uniq",
        "find_all",
    ];
    one(match r.below(13) {
        // A lazy pipeline's own `inspect` — one `#<Enumerator::Lazy: …>` per
        // stage around the object `.lazy` was called on. Not for a generator
        // source: MRI shows the Generator's ADDRESS there, which is not
        // comparable between two interpreters.
        10 if !src.starts_with("Enumerator.new") => format!(
            "p({src}.lazy.{}{})",
            r.pick(&["map { |x| x }", "select { |x| x }", "take_while { |x| x }", "reject { |x| x }"]),
            r.pick(&["", ".take(2)", ".drop(1)", ".zip([1, 2])", ".map { |x| x }"])
        ),
        // `break` inside a block a lazy pipeline STORED: the call it was written
        // on already returned, so MRI raises `LocalJumpError` when the block
        // finally runs.
        11 => format!(
            "p({src}.lazy.{} {{ break {} }}.to_a)",
            r.pick(&["map", "select", "reject", "take_while", "drop_while", "filter_map"]),
            r.range(1, 9)
        ),
        12 => format!(
            "begin\n  {src}.lazy.{} {{ break }}.first(1)\nrescue LocalJumpError => e\n  p e.message\nend",
            r.pick(&["map", "select", "take_while", "filter_map"])
        ),
        0 | 10 => format!("p({src}.{} {b})", r.pick(&consumers)),
        1 => format!("p({src}.to_a)"),
        2 => format!("{src}.each {b}\np :done"),
        // `Enumerator#each` answers the object being iterated — except on a
        // generator, where MRI answers the generator block's own value (a
        // Yielder, which has no `to_a`, so the reference run only errors). See
        // BUGS.md; the case is not spent on a snippet MRI itself rejects.
        3 if !src.starts_with("Enumerator.new") => {
            format!("p({src}.each {} .to_a)", "{ |x| x }")
        }
        3 => format!("p({src}.to_a.size)"),
        4 => format!("p({src}.map {b})"),
        5 => format!("p({src}.take_while {b})"),
        6 => format!(
            "p({src}.lazy.{} {b}.first(2))",
            r.pick(&[
                "map",
                "select",
                "take_while",
                "reject",
                "filter_map",
                "drop_while"
            ])
        ),
        7 => format!("p({src}.lazy.map {} .to_a)", "{ |x| x }"),
        8 => format!("p({src}.each_with_object([]) {} )", "{ |x, acc| acc << x }"),
        _ => format!(
            "p({src}.{} {b})",
            r.pick(&[
                "count",
                "find_index",
                "any?",
                "all?",
                "none?",
                "one?",
                "take_while"
            ])
        ),
    })
}

/// `Method`/`UnboundMethod` reflection: `#arity`, `#owner`, `#parameters`,
/// `#name`, `#curry` over BUILT-IN methods as well as written ones.
///
/// A built-in has no written parameter list, so every one of those answers comes
/// out of a table of what MRI declares — which is exactly the kind of surface a
/// generator that only ever probes written `def`s reports zero divergences on.
/// The receiver list therefore spans the core classes, and the method list per
/// receiver spans fixed-arity, optional-argument and variadic built-ins, plus the
/// ones a MODULE defines (`Comparable#between?`, `Enumerable#each_slice`,
/// `Kernel#frozen?`) where the owner is not the receiver's own class.
///
/// Not emitted: `parameters` of a `define_method` body that takes `**rest` — the
/// parser desugars the collector into a synthetic capture param, so its name is
/// not recorded anywhere and rubylang reports an unnamed `[:keyrest]`.
fn gen_methobj(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    // (receiver literal, methods to probe on it). Mixed on purpose: fixed arity,
    // `-1` variadic, `-2` "one required then variadic", and module-owned ones.
    let recvs: &[(&str, &[&str])] = &[
        (
            "3",
            &[
                "+",
                "*",
                "divmod",
                "gcd",
                "times",
                "to_s",
                "round",
                "between?",
                "clamp",
                "digits",
                "fdiv",
                "pow",
                "abs",
                "upto",
                "coerce",
                "puts",
                "frozen?",
                "instance_of?",
            ],
        ),
        (
            "1.5",
            &[
                "+", "round", "floor", "divmod", "between?", "step", "to_i", "nan?", "coerce",
            ],
        ),
        (
            "\"ab\"",
            &[
                "upcase",
                "sub",
                "gsub",
                "split",
                "each_char",
                "center",
                "index",
                "between?",
                "start_with?",
                "delete",
                "count",
                "tr",
                "slice",
                "unpack",
                "%",
            ],
        ),
        (
            ":sym",
            &["to_proc", "to_s", "succ", "between?", "start_with?", "[]"],
        ),
        (
            "[1, 2]",
            &[
                "push",
                "map",
                "first",
                "include?",
                "each_slice",
                "inject",
                "flatten",
                "join",
                "rotate",
                "sample",
                "sum",
                "zip",
                "each_cons",
                "min_by",
                "take_while",
                "dig",
            ],
        ),
        (
            "{a: 1}",
            &[
                "each",
                "fetch",
                "map",
                "merge",
                "store",
                "dig",
                "any?",
                "each_with_object",
                "min_by",
                "group_by",
                "count",
                "to_a",
            ],
        ),
        (
            "(1..3)",
            &["step", "each", "cover?", "sum", "each_slice", "min", "to_a"],
        ),
        (
            "nil",
            &[
                "to_a",
                "to_s",
                "inspect",
                "&",
                "frozen?",
                "instance_variable_get",
            ],
        ),
        (
            "Object.new",
            &[
                "frozen?",
                "instance_variable_get",
                "respond_to?",
                "tap",
                "then",
                "send",
            ],
        ),
    ];
    // Class receivers: the owner is a singleton class (`#<Class:Integer>`) or,
    // for the inherited ones, `Class` / `Module` / `Kernel`.
    let classes: &[(&str, &[&str])] = &[
        (
            "Integer",
            &[
                "sqrt",
                "try_convert",
                "name",
                "instance_method",
                "ancestors",
            ],
        ),
        (
            "String",
            &["new", "try_convert", "name", "instance_methods"],
        ),
        ("Array", &["new", "[]", "try_convert", "superclass"]),
        ("Hash", &["new", "[]", "name", "include?"]),
        ("Math", &["sqrt", "hypot", "log", "atan2", "cbrt"]),
        ("Struct", &["new", "name"]),
    ];
    // Written parameter shapes — the `def` side of the same three answers.
    let defs = [
        ("def m(a); end", "m"),
        ("def m(a, b); end", "m"),
        ("def m(a, b = 1); end", "m"),
        ("def m(a, *b); end", "m"),
        ("def m(*a); end", "m"),
        ("def m(a, *b, c); end", "m"),
        ("def m(a, b: 1); end", "m"),
        ("def m(a, b:); end", "m"),
        ("def m(a, **b); end", "m"),
        ("def m(a, &b); end", "m"),
        ("def m(a, b = 1, *c, d:, e: 2, **f, &g); end", "m"),
        ("def m; end", "m"),
    ];
    let (rc, ms) = r.pick(recvs);
    let m = r.pick(ms);
    let (cls, cms) = r.pick(classes);
    let cm = r.pick(cms);
    let (d, dn) = r.pick(&defs);
    one(match r.below(20) {
        0 => format!("p({rc}.method(:{m}).arity)"),
        1 => format!("p({rc}.method(:{m}).owner.to_s)"),
        2 => format!("p({rc}.method(:{m}).parameters)"),
        3 => format!("p([{rc}.method(:{m}).arity, {rc}.method(:{m}).owner.to_s])"),
        4 => format!("p({rc}.method(:{m}).name)"),
        5 => format!("p {cls}.method(:{cm}).arity"),
        6 => format!("p {cls}.method(:{cm}).owner.to_s"),
        7 => format!("p {cls}.method(:{cm}).parameters"),
        8 => format!("class C\n  {d}\nend\np C.new.method(:{dn}).arity"),
        9 => format!("class C\n  {d}\nend\np C.new.method(:{dn}).parameters"),
        10 => format!("class C\n  {d}\nend\np C.new.method(:{dn}).owner.to_s"),
        11 => format!("class C\n  {d}\nend\np C.instance_method(:{dn}).owner.to_s"),
        12 => format!("class C\n  {d}\nend\np C.instance_method(:{dn}).arity"),
        13 => format!("class C\n  {d}\nend\np C.new.method(:{dn}).unbind.owner.to_s"),
        // A module-owned method: the owner is the module, never the class.
        14 => format!(
            "module M\n  {d}\nend\nclass C\n  {}\nend\np [C.new.method(:{dn}).owner.to_s, C.new.method(:{dn}).arity]",
            if r.below(2) == 0 { "include M" } else { "prepend M" }
        ),
        15 => format!(
            "class C\n  include Comparable\n  def <=>(o) = 0\nend\np [C.new.method(:{}).owner.to_s, C.new.method(:{}).arity]",
            r.pick(&["between?", "clamp", "<", ">="]),
            r.pick(&["between?", "clamp", "<", ">="])
        ),
        16 => format!(
            "class C\n  include Enumerable\n  def each; yield 1; end\nend\np [C.new.method(:{}).owner.to_s, C.new.method(:{}).arity]",
            r.pick(&["map", "each_slice", "sort_by", "include?", "first"]),
            r.pick(&["map", "each_slice", "sort_by", "include?", "first"])
        ),
        // `define_method` bodies report the block's shape, strictly.
        17 => format!(
            "class C\n  define_method(:d) {{ |{}| }}\nend\np [C.new.method(:d).arity, C.new.method(:d).parameters]",
            r.pick(&["x", "x, y", "x, y = 1", "x, *y", "", "x, k: 1"])
        ),
        // `curry` reads the same arity a built-in reports.
        18 => format!("p 3.method(:{}).curry[{}]", r.pick(&["+", "*", "-", "gcd"]), r.range(1, 9)),
        // A user subclass of a built-in inherits the built-in's owner. The method
        // names are per-parent: a name the parent does not have is a `NameError`
        // in MRI, which spends the case without comparing anything.
        _ => {
            let (parent, pms): &(&str, &[&str]) = r.pick(&[
                ("Array", &["each", "size", "first", "frozen?", "instance_of?", "to_s"] as &[&str]),
                ("Hash", &["each", "size", "frozen?", "instance_of?", "to_s", "fetch"]),
                ("String", &["size", "upcase", "frozen?", "instance_of?", "to_s", "sub"]),
            ]);
            format!(
                "class C < {parent}\nend\np [C.new.method(:{}).owner.to_s, C.new.method(:{}).arity]",
                r.pick(pms),
                r.pick(pms)
            )
        }
    })
}

/// Ruby has TWO equalities and they disagree: `==` coerces across numeric
/// classes (`1 == 1.0`) while `eql?`/`hash` — the pair Hash keys, `uniq`, the
/// Array set operators and `Set` are all defined in terms of — does not
/// (`1.eql?(1.0)` is false, and the two are distinct Hash keys). Every case
/// here prints the SAME operation over an `eql?`-equal pair and over a merely
/// `==`-equal pair, so an implementation that picks either equality for both
/// is caught: answering with `==` throughout collapses `[1, 1.0]`, answering
/// with identity throughout splits `[1, 1]`. The rule is recursive — a
/// container is `eql?` only when its elements are — so the pairs are also
/// nested one and two levels deep, and used as Hash/Set keys where the
/// identity is what selects the bucket.
fn gen_eqlident(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(1, 6);
    let m = r.range(1, 6);
    // `same` and `wide` hold equal numeric VALUES; only their classes differ,
    // so `==` cannot tell them apart and `eql?` must.
    let same = format!("{n}");
    let wide = format!("{n}.0");
    let pre = "require \"set\"";
    one(match r.below(24) {
        0 => format!("p [{same}.eql?({same}), {same}.eql?({wide}), {wide}.eql?({wide})]"),
        1 => format!("p [[{same}].eql?([{same}]), [{same}].eql?([{wide}])]"),
        2 => format!("p [[[{same}]].eql?([[{same}]]), [[{same}]].eql?([[{wide}]])]"),
        3 => format!("p [{{ a: {same} }}.eql?({{ a: {same} }}), {{ a: {same} }}.eql?({{ a: {wide} }})]"),
        4 => format!("p [{same}, {wide}, {same}, {m}].uniq"),
        5 => format!("p [[{same}], [{wide}], [{same}]].uniq"),
        6 => format!("p([{same}, {m}] | [{wide}])"),
        7 => format!("p([{same}, {m}] & [{wide}])"),
        8 => format!("p([{same}, {wide}] - [{same}])"),
        9 => format!("p [[{same}], [{m}]].intersect?([[{wide}]])"),
        10 => format!("h = {{}}\nh[{same}] = :i\nh[{wide}] = :f\np [h.size, h[{same}], h[{wide}]]"),
        11 => format!(
            "h = {{}}\nh[[{same}]] = :i\nh[[{wide}]] = :f\np [h.size, h[[{same}]], h[[{wide}]]]"
        ),
        12 => format!(
            "h = {{}}\nh[{{ a: {same} }}] = :i\nh[{{ a: {wide} }}] = :f\np [h.size, h[{{ a: {same} }}]]"
        ),
        13 => format!("h = {{}}\nh[{{ a: {same} }}] = :i\np h.keys"),
        14 => format!("{pre}\ns = Set.new\ns << {{ a: {same} }}\ns << {{ a: {same} }}\np [s.size, s.to_a]"),
        15 => format!("{pre}\np [(Set[{same}] & Set[{wide}]).to_a, (Set[{same}] - Set[{wide}]).to_a]"),
        16 => format!(
            "{pre}\np [Set[{same}].subset?(Set[{wide}]), Set[{same}].subset?(Set[{same}, {m}])]"
        ),
        17 => format!(
            "{pre}\np [Set[{same}].disjoint?(Set[{wide}]), Set[{same}].intersect?(Set[{same}])]"
        ),
        18 => format!("{pre}\nh = {{}}\nh[Set[{same}]] = :x\np [h[Set[{same}]], h.keys]"),
        19 => format!("p [{{ a: {n}, b: {m} }}.hash == {{ b: {m}, a: {n} }}.hash, {{ a: {n} }}.hash == {{ a: {n}.0 }}.hash]"),
        20 => format!("p [[{same}].hash == [{same}].hash, [{same}].hash == [{wide}].hash]"),
        21 => format!(
            "S = Struct.new(:a)\np [S.new({same}).eql?(S.new({same})), S.new({same}).eql?(S.new({wide}))]"
        ),
        22 => format!(
            "D = Data.define(:a)\np [D.new(a: {same}).eql?(D.new(a: {same})), D.new(a: {same}).eql?(D.new(a: {wide}))]"
        ),
        _ => format!("p [{same}.eql?({wide}), {same} == {wide}, [{same}] == [{wide}], [{same}].eql?([{wide}])]"),
    })
}

/// Comparison ACROSS the numeric classes, which `bignum` never reached — its
/// four arms only ever build one big integer and print it, so no case ever put
/// a BigInt, a Rational or a Complex on one side of a comparison and a Float on
/// the other. Ruby's rule is not "convert both to Float": `Integer == Float` is
/// EXACT, so `(10**23) == 1e23` is false while `(10**23).to_f == 1e23` is true,
/// and `Rational == Float` really is `to_f`-lossy, so `Rational(1, 3) == 1.0/3`
/// is true. Every case prints an equality beside the ordering of the same pair,
/// so an implementation that answers one with the other is caught.
fn gen_numwide(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let e = r.range(20, 90);
    let base = r.pick(&["2", "3", "7", "10"]);
    let big = format!("{base}**{e}");
    let (n, d) = (r.range(1, 9), r.range(2, 9));
    one(match r.below(18) {
        0 => format!("p [{big} == ({big}).to_f, ({big}).to_f == {big}]"),
        1 => format!("p [{big} == ({big}).to_f + 1, {big} + 1 == ({big}).to_f]"),
        2 => format!("p [{big} != ({big}).to_f, {big} != ({big}).to_f * 2]"),
        3 => format!("p [{big} <=> ({big}).to_f, {big} <=> ({big}).to_f * 2]"),
        4 => format!("p [({big}).to_f <=> {big}, {big} <=> {big}]"),
        5 => format!("p [{big} < ({big}).to_f * 2, {big} > ({big}).to_f / 2]"),
        6 => format!("p [{big} <= ({big}).to_f, {big} >= ({big}).to_f]"),
        7 => format!("p [{big} == Float::INFINITY, {big} < Float::INFINITY]"),
        8 => format!("p [{big} == Float::NAN, {big} <=> Float::NAN]"),
        9 => format!("p [Rational({n}, {d}) == {n}.0 / {d}, Rational({n}, {d}) == 0.0]"),
        10 => format!("p [{n}.0 / {d} == Rational({n}, {d}), Rational({n}, {d}) <=> {n}.0 / {d}]"),
        11 => format!("p [Rational({n}, 1) == {n}, Rational({n}, 1) == {n}.0]"),
        12 => format!("p [Complex({n}, 0) == {n}.0, Complex({n}, 1) == {n}.0]"),
        13 => format!("p [{n}.0 == Complex({n}, 0), Complex({n}.0, 0) == {n}]"),
        14 => format!("p [{big} == Rational({big}, 1), Rational({big}, 1) == ({big}).to_f]"),
        15 => format!("p [({big}).eql?(({big}).to_f), ({big}).eql?({big})]"),
        16 => format!("p [[{big}, ({big}).to_f].uniq.size, [{big}, {big}].uniq.size]"),
        _ => format!("p [({big}).coerce(1.0), ({big}).coerce(1)]"),
    })
}

fn gen_case(seed: u64, mode: Mode) -> Vec<String> {
    match mode {
        Mode::Arith => gen_arith(seed),
        Mode::Bignum => gen_bignum(seed),
        Mode::Floatfmt => gen_floatfmt(seed),
        Mode::Strings => gen_strings(seed),
        Mode::Interp => gen_interp(seed),
        Mode::Ranges => gen_ranges(seed),
        Mode::Arraymeth => gen_arraymeth(seed),
        Mode::Hashmeth => gen_hashmeth(seed),
        Mode::Sorting => gen_sorting(seed),
        Mode::Formatspec => gen_formatspec(seed),
        Mode::Blocks => gen_blocks(seed),
        Mode::Symbols => gen_symbols(seed),
        Mode::Ternary => gen_ternary(seed),
        Mode::Comparison => gen_comparison(seed),
        Mode::Printf => gen_printf(seed),
        Mode::StringOps => gen_string_ops(seed),
        Mode::Caseexpr => gen_caseexpr(seed),
        Mode::Intmeth => gen_intmeth(seed),
        Mode::Regex => gen_regex(seed),
        Mode::Enumerable => gen_enumerable(seed),
        Mode::Exceptions => gen_exceptions(seed),
        Mode::Struct => gen_struct(seed),
        Mode::Rational => gen_rational(seed),
        Mode::Patternmatch => gen_patternmatch(seed),
        Mode::Kernelconv => gen_kernelconv(seed),
        Mode::Loopflow => gen_loopflow(seed),
        Mode::Hashenum => gen_hashenum(seed),
        Mode::Enumext => gen_enumext(seed),
        Mode::Kwargs => gen_kwargs(seed),
        Mode::Metaprog => gen_metaprog(seed),
        Mode::Mixins => gen_mixins(seed),
        Mode::Lambda => gen_lambda(seed),
        Mode::Enumlazy => gen_enumlazy(seed),
        Mode::Setops => gen_setops(seed),
        Mode::Frozen => gen_frozen(seed),
        Mode::Datacls => gen_datacls(seed),
        Mode::Strenc => gen_strenc(seed),
        Mode::Complexnum => gen_complexnum(seed),
        Mode::Objintro => gen_objintro(seed),
        Mode::Blockflow => gen_blockflow(seed),
        Mode::Methobj => gen_methobj(seed),
        Mode::Multiyield => gen_multiyield(seed),
        Mode::Eqlident => gen_eqlident(seed),
        Mode::Numwide => gen_numwide(seed),
        Mode::All => gen_case(seed, ALL_MODES[(seed as usize) % ALL_MODES.len()]),
    }
}

/// Lambda-vs-proc semantics: strict arity (a lambda raises where a block binds
/// nil), no auto-splat, keyword/splat/block parameters, `arity`, `curry`,
/// composition, `define_method` bodies, and `Method` objects. The arity check
/// runs before the body, so the probe distinguishes "raised" from "ran".
fn gen_lambda(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (r.range(0, 9), r.range(0, 9));
    let guard =
        |body: &str| format!("begin\n  p({body})\nrescue ArgumentError => e\n  p e.message\nend");
    one(match r.below(24) {
        0 => guard(&format!("->(x, y) {{ [x, y] }}.call({a})")),
        1 => guard(&format!("->(x, y) {{ [x, y] }}.call({a}, {b}, 1)")),
        2 => guard(&format!("->(x, y = {b}) {{ [x, y] }}.call({a})")),
        3 => guard(&format!("->(x, *r) {{ [x, r] }}.call({a}, {b})")),
        4 => guard("->(x, *r) { [x, r] }.call"),
        5 => guard(&format!("->(k:) {{ k }}.call(k: {a})")),
        6 => guard("->(k:) { k }.call"),
        7 => guard(&format!("->(k:) {{ k }}.call(k: {a}, j: {b})")),
        8 => guard(&format!("->(x, k: {b}) {{ [x, k] }}.call({a})")),
        9 => guard(&format!("->(x, **o) {{ [x, o] }}.call({a}, z: {b})")),
        10 => guard(&format!("proc {{ |x, y| [x, y] }}.call({a})")),
        11 => guard(&format!("proc {{ |x, y| [x, y] }}.call({a}, {b}, 1)")),
        12 => guard(&format!("lambda {{ |x, y| [x, y] }}.call({a})")),
        13 => guard(&format!("[[{a}, {b}]].map(&->(x, y) {{ x + y }})")),
        14 => guard(&format!("[[{a}, {b}]].map {{ |x, y| x + y }}")),
        15 => "p [->(){}.arity, ->(x){}.arity, ->(x, y = 1){}.arity, ->(*a){}.arity, ->(k:){}.arity, ->(k: 1){}.arity]".to_string(),
        16 => "p [proc{}.arity, proc{|x|}.arity, proc{|x, y = 1|}.arity, proc{|*a|}.arity, proc{|k: 1|}.arity]".to_string(),
        17 => format!("p ->(x, y) {{ x + y }}.curry[{a}][{b}]"),
        18 => guard(&format!("->(x, y) {{ x + y }}.curry.call({a}, {b}, 1)")),
        19 => format!("f = ->(x) {{ x + {a} }} >> ->(y) {{ y * 2 }}\np [f.call({b}), f.lambda?]"),
        20 => format!(
            "class C\n  define_method(:m) {{ |x, y| [x, y] }}\nend\nbegin\n  p C.new.m({a})\nrescue ArgumentError => e\n  p e.message\nend"
        ),
        21 => format!(
            "def m(x, y) = [x, y]\nbegin\n  p method(:m).call({a})\nrescue ArgumentError => e\n  p e.message\nend\np [method(:m).arity, method(:m).to_proc.lambda?]"
        ),
        22 => format!("p ->(&b) {{ b.call({a}) }}.call {{ |v| v * 2 }}"),
        _ => format!(
            "p [(->(x) {{ x > {a} }} === {b}), (case {b} when ->(x) {{ x > {a} }} then :hi else :lo end)]"
        ),
    })
}

/// Enumerator shape and laziness: which methods answer an Enumerator, the
/// grouping methods (`chunk`/`chunk_while`/`slice_when`/`each_slice`/
/// `each_cons`), — the part that used to hang — block-less enumerator
/// methods over an INFINITE source, bounded by `first`/`take`/`next`, and
/// `Enumerator::Lazy#uniq` (cases 22-25).
///
/// `uniq` is the one lazy stage that has to carry state ACROSS elements, so it
/// cannot be expressed by the per-element map/filter shape the other stages
/// share. Over an infinite source (`(1..)` mapped into a small residue set) it
/// is also the case that proves the stage stays lazy: a `uniq` that materializes
/// its source first cannot terminate at all, so it fails by hanging rather than
/// by answering wrong. The block form (`uniq { }`) keys on the block's value but
/// yields the ORIGINAL element, which a keyless implementation gets wrong.
fn gen_enumlazy(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let n = r.range(1, 3);
    let k = r.range(2, 4);
    let arr = "[1, 2, 4, 5, 7]";
    let gen = "e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }";
    // Modulus of the residue map feeding the infinite-source uniq cases.
    // `x % m` over `(1..)` yields exactly `m` distinct values, and the case that
    // also rejects one yields `m - 1`, so the bound below asks for FEWER than
    // that. Asking for more does not diverge — it does not terminate, in MRI as
    // much as here, and the run is spent on two timeouts instead of a result.
    let m = r.range(3, 6);
    let j = k.min(m - 1);
    one(match r.below(26) {
        0 => format!("p {arr}.each_slice({k}).class"),
        1 => format!("p {arr}.each_cons({k}).class"),
        2 => format!("p {arr}.chunk_while {{ |a, b| b == a + 1 }}.class"),
        3 => format!("p {arr}.slice_when {{ |a, b| b > a + 1 }}.class"),
        4 => format!("p {arr}.chunk {{ |x| x.even? }}.class"),
        5 => format!("p {arr}.each_slice({k}).to_a"),
        6 => format!("p {arr}.each_cons({k}).to_a"),
        7 => format!("p {arr}.chunk_while {{ |a, b| b == a + 1 }}.to_a"),
        8 => format!("p {arr}.slice_when {{ |a, b| b > a + 1 }}.to_a"),
        9 => format!("p {arr}.chunk {{ |x| x.even? }}.to_a"),
        10 => {
            format!("begin\n  p {arr}.chunk_while\nrescue ArgumentError => e\n  p e.message\nend")
        }
        11 => format!("begin\n  p {arr}.slice_when\nrescue ArgumentError => e\n  p e.message\nend"),
        12 => format!("{gen}\np e.first({k})"),
        13 => format!("{gen}\np e.take({k})"),
        14 => format!("{gen}\np e.each_slice({n}).first({k})"),
        15 => format!("{gen}\np e.each_cons({n}).first({k})"),
        16 => format!("{gen}\np e.each_with_index.first({k})"),
        17 => format!("{gen}\np e.map.first({k})"),
        18 => format!("{gen}\np e.each_with_object([]).first({n})"),
        19 => format!("p (1..).each_slice({n}).first({k})"),
        20 => format!("p (1..).each_with_index.first({k})"),
        21 => format!("p (1..Float::INFINITY).each_cons({n}).first({k})"),
        // Finite source: `to_a` forces the whole pipeline, so the stage's output
        // ORDER is pinned too (Ruby keeps first-occurrence order).
        22 => "p [1, 1, 2, 4, 2, 1, 5].lazy.uniq.to_a".to_string(),
        // Keyed by the block, yielding the original element.
        23 => format!("p {arr}.lazy.uniq {{ |x| x % {m} }}.to_a"),
        // Infinite source, bounded by `first`: only a genuinely lazy `uniq`
        // terminates.
        24 => format!("p (1..).lazy.map {{ |x| x % {m} }}.uniq.first({j})"),
        // Composed with the stages either side of it.
        _ => format!(
            "p (1..).lazy.map {{ |x| x % {m} }}.uniq.map {{ |x| x * 2 }}.reject {{ |x| x == 0 }}.first({j})"
        ),
    })
}

/// `Set`: construction, the algebra operators, mutation, and the predicates —
/// plus `<=>` (cases 16-20).
///
/// `Set#<=>` is a SUBSET-RELATION comparison, not an ordering: `0` when the sets
/// are equal, `-1`/`1` for a proper subset/superset, and `nil` whenever neither
/// contains the other or the operand is not a Set. The subset PREDICATES
/// (`subset?`/`<=`, case 6) can all be right while `<=>` is wrong, because a
/// Set that falls through to `Array#<=>` still answers `-1`/`0`/`1` — just
/// element-wise and ordered, and it never answers `nil`. So the incomparable
/// pair and the non-Set operand are the two cases that actually discriminate.
fn gen_setops(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (r.range(1, 5), r.range(1, 5));
    let pre = "require \"set\"";
    one(match r.below(21) {
        0 => format!("{pre}\np Set.new([{a}, {b}, {a}]).to_a.sort"),
        1 => format!("{pre}\np (Set[{a}, {b}] | Set[3, 4]).to_a.sort"),
        2 => format!("{pre}\np (Set[{a}, {b}, 3] & Set[3, 4]).to_a.sort"),
        3 => format!("{pre}\np (Set[{a}, {b}, 3] - Set[3]).to_a.sort"),
        4 => format!("{pre}\np (Set[{a}, {b}] ^ Set[{b}, 9]).to_a.sort"),
        5 => format!("{pre}\np Set[{a}, {b}].include?({a})"),
        6 => {
            format!("{pre}\np [Set[{a}].subset?(Set[{a}, {b}]), Set[{a}, {b}].superset?(Set[{a}])]")
        }
        7 => {
            format!("{pre}\ns = Set.new\ns << {a}\ns.add({b})\ns.add({a})\np [s.size, s.to_a.sort]")
        }
        8 => format!("{pre}\ns = Set[{a}, {b}, 3]\ns.delete({a})\np s.to_a.sort"),
        9 => format!("{pre}\np Set[{a}, {b}].map {{ |x| x * 2 }}.sort"),
        10 => format!("{pre}\np Set[{a}, {b}].select(&:even?).to_a.sort"),
        11 => format!("{pre}\np [Set[{a}].empty?, Set.new.empty?]"),
        12 => format!("{pre}\np (Set[{a}, {b}] == Set[{b}, {a}])"),
        13 => format!("{pre}\np Set[{a}, {b}].disjoint?(Set[7, 8])"),
        14 => format!("{pre}\np Set[{a}, {b}].to_a.sum"),
        15 => format!("{pre}\np Set.new([{a}, {b}]).each_with_object([]) {{ |x, m| m << x }}.sort"),
        // Proper subset / proper superset / equal-ignoring-order.
        16 => format!("{pre}\np (Set[{a}, {b}] <=> Set[{a}, {b}, 9])"),
        17 => format!("{pre}\np (Set[{a}, {b}, 9] <=> Set[{a}, {b}])"),
        18 => format!("{pre}\np (Set[{a}, {b}] <=> Set[{b}, {a}])"),
        // Incomparable unless a == b — the case an ordered fallback gets wrong.
        19 => format!("{pre}\np [Set[{a}, 7] <=> Set[{b}, 8], Set[] <=> Set[{a}]]"),
        // A non-Set operand is `nil`, never an Array comparison.
        _ => format!("{pre}\np [Set[{a}, {b}] <=> [{a}, {b}], Set[{a}] <=> {b}]"),
    })
}

/// `freeze`/`frozen?` and the `FrozenError` a mutation of a frozen object
/// raises, plus how `dup`/`clone` carry (or drop) the frozen flag.
fn gen_frozen(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, w) = (r.range(1, 9), ww(r));
    one(match r.below(16) {
        0 => format!("p [{a}.frozen?, :{w}.frozen?, nil.frozen?, true.frozen?]"),
        1 => format!("p \"{w}\".freeze.frozen?"),
        2 => format!("p [{a}, {a}].freeze.frozen?"),
        3 => format!("p({{ a: {a} }}.freeze.frozen?)"),
        4 => format!("begin\n  [{a}] .freeze << 1\nrescue => e\n  p e.class\nend"),
        5 => format!("begin\n  \"{w}\".freeze << \"x\"\nrescue => e\n  p e.class\nend"),
        6 => format!("begin\n  h = {{ a: {a} }}.freeze\n  h[:b] = 1\nrescue => e\n  p e.class\nend"),
        7 => format!("s = \"{w}\".freeze\np [s.dup.frozen?, s.clone.frozen?]"),
        8 => format!("s = \"{w}\".freeze\np s.clone(freeze: false).frozen?"),
        9 => format!("a = [{a}].freeze\np [a.dup.frozen?, a.frozen?]"),
        10 => format!("class C; end\nc = C.new.freeze\nbegin\n  c.instance_variable_set(:@x, {a})\nrescue => e\n  p e.class\nend"),
        11 => format!("p \"{w}\".frozen?"),
        12 => format!("p [{a}.freeze.equal?({a}), :{w}.freeze.equal?(:{w})]"),
        13 => format!("a = [{a}, {a} + 1].freeze\np a.map {{ |x| x * 2 }}"),
        14 => format!("p [(1..{a}).frozen?, ({a}..).frozen?]"),
        _ => format!("s = \"{w}\"\ns.freeze\np [s.frozen?, s.upcase.frozen?]"),
    })
}

/// `Data.define` value objects (Ruby 3.2+): construction both ways, `with`,
/// equality, deconstruction and the errors a bad call raises.
fn gen_datacls(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b) = (r.range(0, 9), r.range(0, 9));
    let d = "P = Data.define(:x, :y)";
    one(match r.below(14) {
        0 => format!("{d}\np P.new(x: {a}, y: {b}).to_h"),
        1 => format!("{d}\np P.new({a}, {b}).to_h"),
        2 => format!("{d}\np [P.new({a}, {b}).x, P.new({a}, {b}).y]"),
        3 => format!("{d}\np P.new({a}, {b}) == P.new({a}, {b})"),
        4 => format!("{d}\np P.new({a}, {b}).with(y: {b} + 1).to_h"),
        5 => format!("{d}\np P.members"),
        6 => format!("{d}\nbegin\n  P.new({a})\nrescue ArgumentError => e\n  p e.class\nend"),
        7 => {
            format!("{d}\nbegin\n  P.new({a}, {b}, 1)\nrescue ArgumentError => e\n  p e.class\nend")
        }
        8 => format!("{d}\np(case P.new({a}, {b})\n  in {{ x:, y: }} then [x, y]\n  end)"),
        9 => format!("{d}\np P.new({a}, {b}).frozen?"),
        10 => format!("{d}\np P.new({a}, {b}).deconstruct_keys([:x])"),
        11 => format!("{d}\np P.new({a}, {b}).inspect.include?(\"x=\")"),
        12 => format!("{d}\np [P.new({a}, {b}).hash == P.new({a}, {b}).hash]"),
        _ => format!(
            "{d}\nbegin\n  P.new(x: {a}, z: {b})\nrescue ArgumentError => e\n  p e.class\nend"
        ),
    })
}

/// String bytes/chars/encoding: the places a multibyte string makes character
/// count, byte count and index disagree.
fn gen_strenc(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let s = r.pick(&["héllo", "naïve", "日本語", "aéb", "straße", "abc"]);
    let n = r.range(0, 3);
    one(match r.below(16) {
        0 => format!("p \"{s}\".length"),
        1 => format!("p \"{s}\".bytesize"),
        2 => format!("p \"{s}\".bytes.size"),
        3 => format!("p \"{s}\".chars"),
        4 => format!("p \"{s}\".encoding.to_s"),
        5 => format!("p \"{s}\".valid_encoding?"),
        6 => format!("p \"{s}\"[{n}]"),
        7 => format!("p \"{s}\".reverse"),
        8 => format!("p \"{s}\".upcase"),
        9 => format!("p \"{s}\".each_char.to_a.size"),
        10 => format!("p \"{s}\".each_byte.to_a.size"),
        11 => format!("p \"{s}\".b.encoding.to_s"),
        12 => format!("p \"{s}\".codepoints.size"),
        13 => format!("p \"{s}\".sub(/./, \"X\")"),
        14 => format!("p \"{s}\".scan(/./).size"),
        _ => format!("p \"{s}\".slice({n}, 2)"),
    })
}

/// `Complex` arithmetic and conversion, where the real/imaginary parts keep
/// their own numeric types.
fn gen_complexnum(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, b, c, d) = (r.range(-4, 5), r.range(-4, 5), r.range(1, 5), r.range(1, 5));
    one(match r.below(14) {
        0 => format!("p Complex({a}, {b}) + Complex({c}, {d})"),
        1 => format!("p Complex({a}, {b}) - Complex({c}, {d})"),
        2 => format!("p Complex({a}, {b}) * Complex({c}, {d})"),
        3 => format!("p Complex({a}, {b}) / Complex({c}, {d})"),
        4 => format!("p [Complex({a}, {b}).real, Complex({a}, {b}).imaginary]"),
        5 => format!("p Complex({a}, {b}).conjugate"),
        6 => format!("p Complex({a}, {b}).abs2"),
        7 => format!("p Complex({a}, {b}) == Complex({a}, {b})"),
        8 => format!("p Complex({a}, {b}).to_s"),
        9 => format!("p ({a} + {b}i)"),
        10 => format!("p Complex({a}, 0).to_i"),
        11 => format!("p Complex({a}, {b}).rectangular"),
        12 => format!("p [Complex({a}, {b}).zero?, Complex(0, 0).zero?]"),
        _ => format!("p Complex({a}, {b}) * {c}"),
    })
}

/// Object introspection that needs no `ObjectSpace`: class/ancestry queries,
/// instance variables, method lists, and the `tap`/`then` value pipeline.
fn gen_objintro(seed: u64) -> Vec<String> {
    let r = &mut Rng::seed(seed);
    let (a, w) = (r.range(1, 9), ww(r));
    let cls =
        format!("class C\n  def initialize; @x = {a}; @y = \"{w}\"; end\n  def m(z) = z\nend");
    one(match r.below(18) {
        0 => format!("{cls}\np C.new.instance_variables.sort"),
        1 => format!("{cls}\np C.new.instance_variable_get(:@x)"),
        2 => format!("{cls}\np C.new.instance_variable_defined?(:@z)"),
        3 => format!("{cls}\np C.instance_methods(false).sort"),
        4 => format!("{cls}\np C.new.respond_to?(:m)"),
        5 => format!("{cls}\np [C.new.is_a?(C), C.new.is_a?(Object), C.new.kind_of?(Comparable)]"),
        6 => format!("{cls}\np C.ancestors.first(2).map(&:to_s)"),
        7 => format!("{cls}\np C.new.class.name"),
        8 => format!("p [{a}.class, \"{w}\".class, :{w}.class, nil.class, (1..2).class]"),
        9 => format!("p {a}.then {{ |x| x * 2 }}"),
        10 => format!("p [{a}].tap {{ |x| x << {a} }}"),
        11 => format!("{cls}\np C.new.methods.include?(:m)"),
        12 => format!("{cls}\np [C.new.respond_to?(:m), C.new.respond_to?(:nope)]"),
        13 => format!("{cls}\np C.superclass.to_s"),
        14 => "p Integer.ancestors.include?(Comparable)".to_string(),
        15 => format!(
            "{cls}\nc = C.new\nc.instance_variable_set(:@z, {a})\np c.instance_variable_get(:@z)"
        ),
        16 => format!("p [{a}.singleton_class.to_s.start_with?(\"#<Class\"), Integer.name]"),
        _ => format!("{cls}\np C.new.public_methods(false).sort"),
    })
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Arith => "arith",
        Mode::Bignum => "bignum",
        Mode::Floatfmt => "floatfmt",
        Mode::Strings => "strings",
        Mode::Interp => "interp",
        Mode::Ranges => "ranges",
        Mode::Arraymeth => "arraymeth",
        Mode::Hashmeth => "hashmeth",
        Mode::Sorting => "sorting",
        Mode::Formatspec => "formatspec",
        Mode::Blocks => "blocks",
        Mode::Symbols => "symbols",
        Mode::Ternary => "ternary",
        Mode::Comparison => "comparison",
        Mode::Printf => "printf",
        Mode::StringOps => "string_ops",
        Mode::Caseexpr => "caseexpr",
        Mode::Intmeth => "intmeth",
        Mode::Regex => "regex",
        Mode::Enumerable => "enumerable",
        Mode::Exceptions => "exceptions",
        Mode::Struct => "struct",
        Mode::Rational => "rational",
        Mode::Patternmatch => "patternmatch",
        Mode::Kernelconv => "kernelconv",
        Mode::Loopflow => "loopflow",
        Mode::Hashenum => "hashenum",
        Mode::Enumext => "enumext",
        Mode::Kwargs => "kwargs",
        Mode::Metaprog => "metaprog",
        Mode::Mixins => "mixins",
        Mode::Lambda => "lambda",
        Mode::Enumlazy => "enumlazy",
        Mode::Setops => "setops",
        Mode::Frozen => "frozen",
        Mode::Datacls => "datacls",
        Mode::Strenc => "strenc",
        Mode::Complexnum => "complexnum",
        Mode::Objintro => "objintro",
        Mode::Blockflow => "blockflow",
        Mode::Methobj => "methobj",
        Mode::Multiyield => "multiyield",
        Mode::Eqlident => "eqlident",
        Mode::Numwide => "numwide",
        Mode::All => "all",
    }
}

fn mode_from_name(s: &str) -> Option<Mode> {
    if s == "all" {
        return Some(Mode::All);
    }
    ALL_MODES.iter().copied().find(|&m| mode_name(m) == s)
}

fn build_program(stmts: &[String]) -> String {
    stmts.join("\n")
}

/// True iff oracle and rubylang disagree on stdout or exit for `script`. Infra
/// failures (spawn/wait errors, timeouts) are NOT parity gaps.
fn diverges(script: &str, bin: &Path, timeout: Duration) -> bool {
    let o = run_oracle(script, timeout);
    if o.timed_out {
        return false;
    }
    let r = run_ours(script, bin, timeout);
    if r.exit == -999 || r.exit == -998 || r.timed_out || o.exit == -999 || o.exit == -998 {
        return false;
    }
    differs(&o, &r)
}

/// Delta-debug a diverging statement list to a locally-minimal one: repeatedly
/// drop any single statement whose removal preserves the divergence, to a
/// fixpoint.
fn minimize(stmts: Vec<String>, bin: &Path, timeout: Duration) -> Vec<String> {
    let mut cur = stmts;
    loop {
        let mut removed = false;
        let mut i = 0;
        while i < cur.len() {
            let mut cand = cur.clone();
            cand.remove(i);
            if !cand.is_empty() && diverges(&build_program(&cand), bin, timeout) {
                cur = cand;
                removed = true;
            } else {
                i += 1;
            }
        }
        if !removed {
            break;
        }
    }
    cur
}

/// Normalize a reproducer to a stable gap-class signature: keep the last
/// non-empty line (the probe), mask numeric literals and quoted words so many
/// instances of the same gap collapse to one signature.
fn signature(program: &str) -> String {
    let body = program
        .lines()
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let mut s = body;
    for (pat, rep) in [
        (r"[0-9]+\.[0-9]+([eE][-+]?[0-9]+)?", "F"),
        (r"[0-9]+[eE][-+]?[0-9]+", "F"),
        (r"-?[0-9]+", "N"),
        ("\"[^\"]*\"", "W"),
        ("'[^']*'", "W"),
    ] {
        s = regex_lite_replace(&s, pat, rep);
    }
    s
}

fn regex_lite_replace(s: &str, pat: &str, rep: &str) -> String {
    match regex::Regex::new(pat) {
        Ok(re) => re.replace_all(s, rep).into_owned(),
        Err(_) => s.to_string(),
    }
}

// ---------------------------------------------------------------------------
// CLI.
// ---------------------------------------------------------------------------

struct Args {
    count: u64,
    base_seed: u64,
    once: bool,
    timeout_ms: u64,
    out_path: PathBuf,
    max_report: usize,
    jobs: usize,
    mode: Mode,
    verify: usize,
    baseline: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut count = 2000u64;
    let mut base_seed = 1u64;
    let mut once = false;
    let mut timeout_ms = 5000u64;
    let mut max_report = 200usize;
    let mut mode = Mode::Arith;
    let mut verify = 1usize;
    let mut baseline: Option<PathBuf> = None;
    let mut jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut out_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("parity-fuzz")
        .join("divergences.txt");

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--count" | "-c" => {
                i += 1;
                count = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(count);
            }
            "--seed" | "-s" => {
                i += 1;
                base_seed = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(base_seed);
            }
            "--once" => once = true,
            "--timeout-ms" => {
                i += 1;
                timeout_ms = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(timeout_ms);
            }
            "--out" | "-o" => {
                i += 1;
                if let Some(p) = argv.get(i) {
                    out_path = PathBuf::from(p);
                }
            }
            "--max-report" => {
                i += 1;
                max_report = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(max_report);
            }
            "--jobs" | "-j" => {
                i += 1;
                jobs = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .filter(|&j| j >= 1)
                    .unwrap_or(jobs);
            }
            "--mode" | "-m" => {
                i += 1;
                match argv.get(i).and_then(|s| mode_from_name(s)) {
                    Some(m) => mode = m,
                    None => {
                        eprintln!(
                            "unknown --mode '{}'",
                            argv.get(i).map(|s| s.as_str()).unwrap_or("")
                        );
                        std::process::exit(2);
                    }
                }
            }
            a if a.starts_with("--") && mode_from_name(&a[2..]).is_some() => {
                mode = mode_from_name(&a[2..]).unwrap();
            }
            "--verify" => {
                i += 1;
                verify = argv
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .filter(|&k| k >= 1)
                    .unwrap_or(verify);
            }
            "--baseline" => {
                i += 1;
                baseline = argv.get(i).map(PathBuf::from);
            }
            "--stderr" => {
                CMP_STDERR.store(true, Ordering::Relaxed);
            }
            "--help" | "-h" => {
                let modes: Vec<&str> = ALL_MODES.iter().copied().map(mode_name).collect();
                eprintln!(
                    "parity-fuzz — differential ruby/rubylang parity fuzzer\n\
                     \n\
                     --count N        number of cases (default 2000)\n\
                     --seed N         base seed; case i uses seed+i (default 1)\n\
                     --mode M         one of: {}\n\
                     --mode all       round-robin over every mode above\n\
                     (each also accepted as a `--<mode>` shorthand)\n\
                     --stderr         also require the diagnostics to match\n\
                     --once           run a single case (seed) and print both outputs\n\
                     --timeout-ms N   per-interpreter wall-clock timeout (default 5000)\n\
                     --out PATH       divergence corpus file\n\
                     --max-report N   stop after N divergences (default 200)\n\
                     --jobs N         parallel workers (default = CPU count)\n\
                     --verify K       require K consecutive divergences to report (default 1)\n\
                     --baseline FILE  allowlist of known-gap signatures; only a NEW\n\
                                      divergence fails the run (exit 1)\n\
                     \n\
                     env  RUBYLANG_FUZZ_RUBY=PATH  the reference ruby to compare against.\n\
                                      The oracle is part of the result; every run prints it.",
                    modes.join(", ")
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    Args {
        count,
        base_seed,
        once,
        timeout_ms,
        out_path,
        max_report,
        jobs,
        mode,
        verify,
        baseline,
    }
}

fn main() {
    let args = parse_args();
    let bin = ours_bin();
    let timeout = Duration::from_millis(args.timeout_ms);

    if !bin.exists() {
        eprintln!(
            "rubylang binary not found at {}; run `cargo build` first",
            bin.display()
        );
        std::process::exit(2);
    }

    // --once: replay a single seed, minimize if it diverges, dump both sides.
    if args.once {
        let stmts = gen_case(args.base_seed, args.mode);
        let script = build_program(&stmts);
        let o = run_oracle(&script, timeout);
        let r = run_ours(&script, &bin, timeout);
        let diverged = !o.timed_out && differs(&o, &r);
        println!("seed   : {}", args.base_seed);
        println!("mode   : {}", mode_name(args.mode));
        let (show, o, r) = if diverged && stmts.len() > 1 {
            let m = minimize(stmts, &bin, timeout);
            let ms = build_program(&m);
            let mo = run_oracle(&ms, timeout);
            let mr = run_ours(&ms, &bin, timeout);
            (ms, mo, mr)
        } else {
            (script, o, r)
        };
        println!("program:\n  {}", show.replace('\n', "\n  "));
        println!("--- ruby     exit={} timeout={} ---", o.exit, o.timed_out);
        let _ = std::io::stdout().write_all(&o.stdout);
        println!("--- rubylang exit={} timeout={} ---", r.exit, r.timed_out);
        let _ = std::io::stdout().write_all(&r.stdout);
        println!("--- {} ---", if diverged { "DIVERGE" } else { "match" });
        std::process::exit(if diverged { 1 } else { 0 });
    }

    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;

    let next = AtomicU64::new(0);
    let checked = AtomicU64::new(0);
    let timeouts = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let divergences: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
    let start = Instant::now();

    eprintln!(
        "fuzzing {} cases across {} workers (mode {})…",
        args.count,
        args.jobs,
        mode_name(args.mode)
    );

    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= args.count {
                    break;
                }
                let seed = args.base_seed.wrapping_add(idx);
                let stmts = gen_case(seed, args.mode);
                let script = build_program(&stmts);
                let o = run_oracle(&script, timeout);
                let r = run_ours(&script, &bin, timeout);
                let done = checked.fetch_add(1, Ordering::Relaxed) + 1;
                if o.timed_out || r.timed_out {
                    timeouts.fetch_add(1, Ordering::Relaxed);
                }
                // oracle-side timeout ⇒ pathological case; not a parity gap.
                if !o.timed_out && differs(&o, &r) {
                    let minimal = minimize(stmts, &bin, timeout);
                    let mscript = build_program(&minimal);
                    let mo = run_oracle(&mscript, timeout);
                    let mr = run_ours(&mscript, &bin, timeout);
                    // Re-verify: a real gap diverges every time; a transient
                    // won't reproduce. Require `verify` consecutive divergences.
                    let mut confirmed = differs(&mo, &mr);
                    for _ in 1..args.verify.max(1) {
                        if !confirmed {
                            break;
                        }
                        confirmed = diverges(&mscript, &bin, timeout);
                    }
                    if !confirmed {
                        return; // continue loop iteration
                    }
                    let err_of = |o: &RunOut| -> String {
                        if CMP_STDERR.load(Ordering::Relaxed) {
                            format!(
                                "\n  stderr: {}",
                                render(&norm_stderr(&o.stderr)).replace('\n', "\n  ")
                            )
                        } else {
                            String::new()
                        }
                    };
                    let rec = format!(
                        "==== seed {seed} ====\n\
                         program:\n  {}\n\
                         ruby     : exit={} timeout={}{}\n{}\n\
                         rubylang : exit={} timeout={}{}\n{}\n",
                        mscript.replace('\n', "\n  "),
                        mo.exit,
                        mo.timed_out,
                        err_of(&mo),
                        render(&mo.stdout),
                        mr.exit,
                        mr.timed_out,
                        err_of(&mr),
                        render(&mr.stdout),
                    );
                    let mut d = divergences.lock().unwrap();
                    d.push((seed, rec));
                    if d.len() >= args.max_report {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                if done % 500 == 0 {
                    let n = divergences.lock().unwrap().len();
                    eprintln!(
                        "  {done}/{} checked, {n} divergences, {:.0}/s",
                        args.count,
                        done as f64 / start.elapsed().as_secs_f64().max(0.001)
                    );
                }
            });
        }
    });

    let checked = checked.load(Ordering::Relaxed);
    let timeouts = timeouts.load(Ordering::Relaxed);
    let mut divergences: Vec<(u64, String)> = divergences.into_inner().unwrap();
    divergences.sort_by_key(|(seed, _)| *seed);
    let divergences: Vec<String> = divergences.into_iter().map(|(_, r)| r).collect();
    let elapsed = start.elapsed();

    let sig_of = |rec: &str| -> String {
        let prog = rec
            .split("program:\n")
            .nth(1)
            .and_then(|s| s.split("\nruby     :").next())
            .unwrap_or(rec);
        signature(prog)
    };

    let allowed: std::collections::HashSet<String> = match &args.baseline {
        Some(bp) => std::fs::read_to_string(bp)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        None => std::collections::HashSet::new(),
    };
    let mut new_records: Vec<&String> = Vec::new();
    let mut new_sigs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut known = 0usize;
    for rec in &divergences {
        let sig = sig_of(rec);
        if args.baseline.is_some() && allowed.contains(&sig) {
            known += 1;
        } else {
            new_records.push(rec);
            new_sigs.insert(sig);
        }
    }

    let oracle = oracle_id();
    println!(
        "\nfuzzed {checked} cases in {:.1}s ({:.0}/s)\n\
         oracle      : {}\n\
         divergences : {} ({} known / {} new)\n\
         timeouts    : {}",
        elapsed.as_secs_f64(),
        checked as f64 / elapsed.as_secs_f64().max(0.001),
        oracle,
        divergences.len(),
        known,
        new_records.len(),
        timeouts,
    );

    if !divergences.is_empty() {
        if let Some(parent) = args.out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::File::create(&args.out_path) {
            let _ = writeln!(f, "# oracle: {oracle}");
            for d in &divergences {
                let _ = writeln!(f, "{d}");
            }
            println!(
                "wrote {} divergences to {}",
                divergences.len(),
                args.out_path.display()
            );
        }
    }

    if !new_records.is_empty() {
        println!(
            "\n--- {} NEW gap signature(s) (add to baseline once triaged) ---",
            new_sigs.len()
        );
        for s in &new_sigs {
            println!("{s}");
        }
        println!(
            "\n--- first {} new divergence record(s) ---",
            new_records.len().min(5)
        );
        for d in new_records.iter().take(5) {
            println!("{d}");
        }
        std::process::exit(1);
    }
    if known > 0 {
        println!("all {known} divergences are known (in baseline) — OK");
    }
}
