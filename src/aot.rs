//! Ahead-of-time compilation (`ruby --build`).
//!
//! Today this precompiles the script to fusevm bytecode and warms the on-disk
//! cache (`cache.rs`), so subsequent runs skip lex/parse/lower entirely. The
//! `fusevm` crate's `aot` feature (a native-object emitter linked against the
//! rubyrs `staticlib`) is the next wave — emitting a standalone executable — and
//! the `staticlib` crate-type + feature are wired now so that lands without a
//! build-graph change. The report below is explicit user-requested output.

/// Precompile `file` and store its bytecode in the cache. Returns a one-line
/// report of what was built.
pub fn build(file: &str) -> Result<String, String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let prog = crate::compile(&src)?;
    let (nmethods, nprocs, nops) = (prog.methods.len(), prog.procs.len(), prog.main.ops.len());
    crate::cache::store(&src, &prog)?;
    Ok(format!(
        "built {file}: {nops} top-level ops, {nmethods} methods, {nprocs} blocks -> ~/.rubyrs/scripts.rkyv"
    ))
}
