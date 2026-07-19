//! Ahead-of-time compilation (`ruby --build`).
//!
//! `--build` bundles the whole app: starting from the entrypoint it statically
//! resolves every literal `require`/`require_relative` (see `bundle.rs`), inlines
//! each required file, and lowers the combined program to fusevm bytecode, which
//! it warms into the on-disk cache (`cache.rs`). A subsequent `ruby FILE` then
//! runs the cached bundle directly — skipping lex/parse/lower AND needing none of
//! the required source files on disk. The `fusevm` crate's `aot` feature (a
//! native-object emitter linked against the rubylang `staticlib`) is the next
//! wave — emitting a standalone executable — and the `staticlib` crate-type +
//! feature are wired now so that lands without a build-graph change. The report
//! below is explicit user-requested output.

/// Bundle `file` with everything it statically requires, compile the whole app to
/// bytecode, and store it in the cache with a dependency manifest. Returns a
/// one-line report of what was built.
pub fn build(file: &str) -> Result<String, String> {
    let (stmts, report) = crate::bundle::bundle(std::path::Path::new(file))?;
    let prog = crate::compiler::compile(&stmts, false)?;
    let (nmethods, nprocs, nops) = (prog.methods.len(), prog.procs.len(), prog.main.ops.len());

    // Dependency manifest: every inlined file's absolute path + content key, so a
    // later run detects (and recompiles past) a stale bundle after any bundled
    // file is edited. The cache is keyed on the canonical entrypoint path + its
    // source — what a plain `ruby FILE` run recomputes to find this bundle.
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let abs = std::fs::canonicalize(file)
        .map_err(|e| format!("cannot read {file}: {e}"))?
        .to_string_lossy()
        .into_owned();
    let mut deps = Vec::with_capacity(report.files.len());
    for p in &report.files {
        let fsrc = std::fs::read_to_string(p)
            .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        deps.push((p.to_string_lossy().into_owned(), crate::cache::key_for(&fsrc)));
    }
    crate::cache::store_bundle(&abs, &src, &prog, deps)?;

    let nfiles = report.files.len();
    let dynamic = if report.dynamic > 0 {
        format!(", {} runtime require(s) left dynamic", report.dynamic)
    } else {
        String::new()
    };
    Ok(format!(
        "built {file}: {nfiles} file(s) bundled, {nops} top-level ops, {nmethods} methods, {nprocs} blocks{dynamic} -> ~/.rubylang/scripts.rkyv"
    ))
}
