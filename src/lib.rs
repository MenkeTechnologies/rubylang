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

pub use fusevm::Value;

/// Compile a source string to a runnable program.
pub fn compile(src: &str) -> Result<compiler::Program, String> {
    let stmts = parser::parse(src)?;
    compiler::compile(&stmts, false)
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
    host::reset_host();
    let cwd = std::env::current_dir().unwrap_or_default();
    host::with_host(|h| h.init_load_path(&cwd.to_string_lossy()));
    host::push_file_dir(cwd);
    // A `-e` one-liner has no script file; MRI reports `__FILE__` as "-e".
    host::push_file_path("-e".to_string());
    run_compiled(compile(src)?)
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
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    host::reset_host();
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
    let dir = abs
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    host::with_host(|h| h.init_load_path(&dir.to_string_lossy()));
    host::push_file_dir(dir);
    // `__FILE__` for the top-level script is the path exactly as given on the
    // command line (MRI does not canonicalize it).
    host::push_file_path(path.to_string());
    // If `ruby --build FILE` warmed the cache, the stored program is the whole
    // bundled app (every statically-required file inlined). Run it directly —
    // it skips lex/parse/lower AND needs none of the required source files on
    // disk. `cache::load` returns `None` (falls back to a fresh compile + runtime
    // `require`) on a miss or when any still-present bundled file has changed.
    if let Some(prog) = cache::load_file(&abs.to_string_lossy(), &src) {
        return run_compiled(prog);
    }
    run_compiled(compile(&src)?)
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
