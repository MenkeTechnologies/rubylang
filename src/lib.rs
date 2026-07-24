//! rubylang — Ruby as a fusevm frontend.
//!
//! Pipeline: `lexer` → `parser` builds a Ruby AST → `compiler` lowers it to a
//! `fusevm::Chunk` (plus a table of method/block sub-chunks) → fusevm executes
//! it, calling back into the `host` (through registered builtins and the strict
//! numeric hook) for every Ruby-specific operation. There is no bespoke VM or
//! JIT here — execution and codegen live in fusevm.

pub mod aot;
pub mod ast;
pub mod banner;
pub mod builtins;
pub mod bundle;
pub mod cache;
pub mod cli;
pub mod compiler;
pub mod dap;
pub mod host;
pub mod intercepts;
pub mod lexer;
pub mod lsp;
pub mod parser;
pub mod repl;
pub mod rust_ffi;

pub use fusevm::Value;

/// The MRI language level rubylang targets — the value reported as `RUBY_VERSION`
/// and in `ruby --version`. Gems test this against `required_ruby_version`.
pub const RUBY_COMPAT_VERSION: &str = "3.4.0";
/// `RUBY_ENGINE` — rubylang is its own engine (like `jruby`/`truffleruby`), so
/// engine-sniffing code can tell it apart from MRI.
pub const RUBY_ENGINE: &str = "rubylang";
/// `RUBY_ENGINE_VERSION` — this crate's own version.
pub const RUBY_ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `RUBY_PLATFORM` string, built from the host arch/OS.
pub fn ruby_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The `ruby --version` / `-v` banner. Starts with `ruby <X.Y.Z>` so tools that
/// parse the version line (rbenv/chruby/bundler) read the compat level, then
/// names the real engine so nothing is misrepresented as MRI.
pub fn version_banner() -> String {
    format!(
        "ruby {} (rubylang {}) [{}]",
        RUBY_COMPAT_VERSION,
        RUBY_ENGINE_VERSION,
        ruby_platform()
    )
}

/// Program-invocation configuration threaded in from the command line: `ARGV`,
/// `-I` load-path prepends, `-r` requires, and the `$0` script name.
#[derive(Default)]
pub struct RunConfig {
    pub argv: Vec<String>,
    pub includes: Vec<String>,
    pub requires: Vec<String>,
    pub script_name: String,
    /// `-w`/`-W` level: 0 → `$VERBOSE = nil`, 1 → `false`, 2 → `true`.
    pub warn_level: u8,
    /// `-d`/`--debug` → `$DEBUG`.
    pub debug: bool,
}

/// Seed `$VERBOSE`/`$DEBUG` from the warning level and debug flag (MRI: `-W0`
/// silences to `nil`, `-w`/`-W2` set `$VERBOSE = true`, default is `false`).
fn seed_verbosity(cfg: &RunConfig) {
    // Ruby `nil` is `Value::Undef` in this runtime.
    let verbose = match cfg.warn_level {
        0 => Value::Undef,
        1 => Value::Bool(false),
        _ => Value::Bool(true),
    };
    host::with_host(|h| {
        h.set_global("VERBOSE", verbose);
        h.set_global("DEBUG", Value::Bool(cfg.debug));
    });
}

/// Compile a source string to a runnable program.
pub fn compile(src: &str) -> Result<compiler::Program, String> {
    compiler::set_frozen_string_literals(has_frozen_string_literal(src));
    let stmts = parser::parse(src)?;
    compiler::compile(&stmts, false)
}

/// Detect a `# frozen_string_literal: true` magic comment. MRI honors it only in
/// the leading comment block (after an optional shebang) and stops at the first
/// line of code. The value must be `true`; anything else (incl. `false`) is off.
fn has_frozen_string_literal(src: &str) -> bool {
    for (i, line) in src.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if i == 0 && t.starts_with("#!") {
            continue; // shebang
        }
        let Some(rest) = t.strip_prefix('#') else {
            break; // first non-comment line ends the magic-comment region
        };
        // Accept `# frozen_string_literal: true` and `-*- … -*-` emacs form.
        if let Some(after) = rest.split("frozen_string_literal:").nth(1) {
            return after
                .trim_start()
                .trim_end_matches("-*-")
                .trim()
                .starts_with("true");
        }
    }
    false
}

/// Compile with per-statement DAP line markers enabled (`ruby --dap`).
pub fn compile_debug(src: &str) -> Result<compiler::Program, String> {
    let stmts = parser::parse(src)?;
    compiler::compile(&stmts, true)
}

/// Parse, compile, load, and run a Ruby source string on a fresh host; return
/// the value of the last top-level expression. `$LOAD_PATH`/`$LOADED_FEATURES`
/// and the `require` file-dir stack are seeded with the current directory so a
/// `require`/`require_relative` from a `-e` one-liner resolves against it.
pub fn eval_str(src: &str) -> Result<Value, String> {
    eval_str_cfg(src, &RunConfig::default())
}

/// `eval_str` with command-line configuration: seeds `-I` includes, `ARGV`/`$0`,
/// runs `-r` requires, then the one-liner. `$0`/`__FILE__` default to `-e`.
pub fn eval_str_cfg(src: &str, cfg: &RunConfig) -> Result<Value, String> {
    // Install a fresh per-run VM BEFORE taking the GVL: `reset_host` swaps the
    // thread's current VM, which must not happen while a guard into the old one
    // is live (see the invariant on `gvl_enter`).
    host::reset_host();
    // Hold the GVL for the whole run so heap access stays exclusive and any
    // `Thread` spawned inside contends on the same lock (MRI's model).
    host::with_gvl(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        host::with_host(|h| h.init_load_path(&cwd.to_string_lossy()));
        host::with_host(|h| h.prepend_load_path(&cfg.includes));
        host::push_file_dir(cwd);
        let name = if cfg.script_name.is_empty() {
            "-e".to_string()
        } else {
            cfg.script_name.clone()
        };
        // A `-e` one-liner has no script file; MRI reports `__FILE__` as "-e".
        host::push_file_path(name.clone());
        host::with_host(|h| h.set_program_args(&cfg.argv, &name));
        seed_verbosity(cfg);
        run_prelude()?;
        run_requires(&cfg.requires)?;
        run_compiled(compile(src)?)
    })
}

/// Core classes that MRI provides in C but rubylang defines in Ruby, run on the
/// fresh host before any user code. `ObjectSpace::WeakMap` is a weak-key hash in
/// MRI; here it is an ordinary hash (no weak semantics), which is enough for the
/// callers that use it as a registry/set (activesupport DescendantsTracker).
const PRELUDE: &str = r#"
module RbConfig
  CONFIG = {
    "host_os" => "linux-gnu",
    "arch" => "x86_64-linux",
    "ruby_version" => "3.4.0",
    "MAJOR" => "3",
    "MINOR" => "4",
    "TEENY" => "0",
    "ruby_install_name" => "ruby",
    "RUBY_INSTALL_NAME" => "ruby",
    "UNICODE_VERSION" => "15.0.0",
    "UNICODE_EMOJI_VERSION" => "15.0",
    "bindir" => "/usr/bin",
    "rubylibdir" => "/usr/lib/ruby",
    "EXEEXT" => "",
  }
end

class ObjectSpace::WeakMap
  def initialize; @__wm = {}; end
  def [](k); @__wm[k]; end
  def []=(k, v); @__wm[k] = v; end
  def delete(k); @__wm.delete(k); end
  def key?(k); @__wm.key?(k); end
  def include?(k); @__wm.key?(k); end
  def member?(k); @__wm.key?(k); end
  def each(&b); @__wm.each(&b); end
  def each_key(&b); @__wm.each_key(&b); end
  def each_value(&b); @__wm.each_value(&b); end
  def each_pair(&b); @__wm.each_pair(&b); end
  def keys; @__wm.keys; end
  def values; @__wm.values; end
  def size; @__wm.size; end
  def length; @__wm.size; end
end
"#;

/// Compile and run [`PRELUDE`] on the current host. Silent and fast; runs once per
/// fresh VM before requires and the program.
fn run_prelude() -> Result<(), String> {
    run_compiled(compile(PRELUDE)?).map(|_| ())
}

/// Run each `-r LIB` on the current host before the program. Requires share the
/// program's host (globals, classes), matching MRI's `-r`.
fn run_requires(requires: &[String]) -> Result<(), String> {
    if requires.is_empty() {
        return Ok(());
    }
    // `{lib:?}` quotes+escapes the name into a Ruby string literal for `require`.
    let src: String = requires
        .iter()
        .map(|lib| format!("require {lib:?}\n"))
        .collect();
    run_compiled(compile(&src)?).map(|_| ())
}

/// Merge an already-compiled program onto the current host: rebase its
/// proc/begin ids above what is already loaded (so ids never collide — see
/// `compiler::rebase_program`), install its methods/classes/begins/procs, and
/// return the (rebased) main chunk for the caller to run. Shared by the initial
/// script run, each REPL line, and every `require`/`load`.
pub fn load_merged(mut prog: compiler::Program) -> fusevm::Chunk {
    let (proc_off, begin_off) = host::with_host(|h| h.program_offsets());
    compiler::rebase_program(&mut prog, proc_off, begin_off);
    let compiler::Program {
        main,
        methods,
        classes,
        begins,
        procs,
    } = prog;
    host::with_host(|h| h.load_program(methods, classes, begins, procs));
    main
}

/// Run an already-compiled program on the current host (rebasing + merging it
/// first, then running its main chunk at the top level).
pub fn run_compiled(prog: compiler::Program) -> Result<Value, String> {
    host::run_main(load_merged(prog))
}

/// Read and run a `.rb` file. Seeds `$LOAD_PATH`/`$LOADED_FEATURES` and the
/// `require` file-dir stack with the script's own directory (MRI seeds the
/// running script's dir), so `require`/`require_relative` resolve against it.
pub fn eval_file(path: &str) -> Result<Value, String> {
    eval_file_cfg(path, &RunConfig::default())
}

/// `eval_file` with command-line configuration (`-I`/`-r`/`ARGV`/`$0`).
pub fn eval_file_cfg(path: &str, cfg: &RunConfig) -> Result<Value, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    // Fresh per-run VM before the GVL (see `eval_str_cfg`).
    host::reset_host();
    // Hold the GVL for the whole run (see `eval_str_cfg`).
    host::with_gvl(|| {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
        let dir = abs
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        host::with_host(|h| h.init_load_path(&dir.to_string_lossy()));
        host::with_host(|h| h.prepend_load_path(&cfg.includes));
        host::push_file_dir(dir);
        // `__FILE__` for the top-level script is the path exactly as given on the
        // command line (MRI does not canonicalize it). `$0` is the same path.
        host::push_file_path(path.to_string());
        host::with_host(|h| h.set_program_args(&cfg.argv, path));
        seed_verbosity(cfg);
        run_prelude()?;
        run_requires(&cfg.requires)?;
        // If `ruby --build FILE` warmed the cache, the stored program is the whole
        // bundled app (every statically-required file inlined). Run it directly —
        // it skips lex/parse/lower AND needs none of the required source files on
        // disk. `cache::load` returns `None` (falls back to a fresh compile +
        // runtime `require`) on a miss or a changed bundled file.
        if let Some(prog) = cache::load_file(&abs.to_string_lossy(), &src) {
            return run_compiled(prog);
        }
        run_compiled(compile(&src)?)
    })
}

/// Read and run a `.rb` file under the DAP debugger: compile with per-statement
/// line markers and run with the debug hook installed (`ruby --dap`).
pub fn eval_file_debug(path: &str) -> Result<Value, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let prog = compile_debug(&src)?;
    host::reset_host();
    host::set_debug_mode(true);
    let r = run_compiled(prog);
    host::set_debug_mode(false);
    r
}

/// Evaluate `src` and return the `inspect` form of the last expression's value.
/// The convenience entry point for tests and `--eval`-style checks.
pub fn eval_to_string(src: &str) -> Result<String, String> {
    let v = eval_str(src)?;
    Ok(host::with_host(|h| h.inspect(&v)))
}
