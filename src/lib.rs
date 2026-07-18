//! rubyrs — Ruby as a fusevm frontend.
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
    compiler::compile(&stmts)
}

/// Parse, compile, load, and run a Ruby source string on a fresh host; return
/// the value of the last top-level expression.
pub fn eval_str(src: &str) -> Result<Value, String> {
    host::reset_host();
    run_compiled(compile(src)?)
}

/// Run an already-compiled program on the current (freshly reset) host.
pub fn run_compiled(prog: compiler::Program) -> Result<Value, String> {
    let compiler::Program {
        main,
        methods,
        procs,
    } = prog;
    host::with_host(|h| h.load_program(methods, procs));
    host::run_main(main)
}

/// Read and run a `.rb` file.
pub fn eval_file(path: &str) -> Result<Value, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    eval_str(&src)
}

/// Evaluate `src` and return the `inspect` form of the last expression's value.
/// The convenience entry point for tests and `--eval`-style checks.
pub fn eval_to_string(src: &str) -> Result<String, String> {
    let v = eval_str(src)?;
    Ok(host::with_host(|h| h.inspect(&v)))
}
