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
        for p in ["/opt/homebrew/bin/ruby", "/usr/local/bin/ruby", "/usr/bin/ruby"] {
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

const INTS: &[&str] = &["0", "1", "2", "7", "10", "-3", "-7", "42", "100", "-1", "5", "9"];
const FLOATS: &[&str] = &[
    "0.1", "0.2", "1.5", "3.14", "2.0", "-1.5", "10.0", "0.0", "100.25", "-0.5", "1e10", "1.0",
];
const WORDS: &[&str] = &["foo", "bar", "baz", "hello", "world", "abc", "xyz", "Ruby", "Lang"];

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
        2 => format!("p \"[#{{[1,2,3].map {{ |x| x * {} }}.join(',')}}]\"", r.range(1, 4)),
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
        9 => format!("p {a}.take({}) + {a}.drop({})", r.range(1, 3), r.range(1, 3)),
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
        (0..4).map(|_| format!("{:?}", ww(r))).collect::<Vec<_>>().join(", ")
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
];

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
    }
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
    }
}

fn mode_from_name(s: &str) -> Option<Mode> {
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
        .filter(|l| !l.is_empty())
        .next_back()
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
                base_seed = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(base_seed);
            }
            "--once" => once = true,
            "--timeout-ms" => {
                i += 1;
                timeout_ms = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(timeout_ms);
            }
            "--out" | "-o" => {
                i += 1;
                if let Some(p) = argv.get(i) {
                    out_path = PathBuf::from(p);
                }
            }
            "--max-report" => {
                i += 1;
                max_report = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(max_report);
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
