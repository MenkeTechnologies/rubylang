//! Ahead-of-time compilation (`ruby --build`).
//!
//! `--build` bundles the whole app: starting from the entrypoint it statically
//! resolves every literal `require`/`require_relative` (see `bundle.rs`), inlines
//! each required file, and lowers the combined program to fusevm bytecode, which
//! it warms into the on-disk cache (`cache.rs`). A subsequent `ruby FILE` then
//! runs the cached bundle directly — skipping lex/parse/lower AND needing none of
//! the required source files on disk. The report below is explicit user-requested
//! output.
//!
//! `--build --native` goes one step further and emits a **standalone native
//! executable** (`build_native`): no `ruby` interpreter, no `.rb` sources, no
//! cache. It uses the `fusevm` crate's `aot` feature — a Cranelift object emitter
//! — plus the rubylang runtime:
//!
//! 1. bundle → `Program` (main chunk + methods/classes/begins/procs), same as
//!    `build`.
//! 2. Every method/class-method/block chunk is tagged with a `native_id`
//!    (1, 2, …) and `fusevm::aot::compile_program_object(&prog.main, extras, app.o)`
//!    Cranelift-lowers *all* of them into one relocatable object: the main driver
//!    `fusevm_aot_entry`, one driver `fusevm_aot_fn_{native_id}` per method/block,
//!    the serialized main chunk (`fusevm_aot_chunk_blob`/`_len`), plus imports for
//!    the runtime shims and the frontend hook `fusevm_aot_register_builtins`.
//! 3. The full `Program` is serialized (`cache::program_to_blob`) and baked into
//!    a generated Rust frontend via `include_bytes!` — the `native_id` on each
//!    chunk rides along in the blob, so at runtime `host::run_chunk_on` reads it
//!    and dispatches the method/block to its native driver instead of interpreting.
//! 4. The frontend defines the blob symbols the runtime hook reads, declares the
//!    `fusevm_aot_fn_{i}` drivers, registers their addresses via
//!    `register_native_fns`, and a `main` that calls
//!    `fusevm::aot::fusevm_aot_run_embedded`. `rustc` compiles it against the
//!    rubylang rlib (which statically links fusevm + the whole runtime) and links
//!    in `app.o`, producing the executable.
//!
//! The runtime hook itself — [`fusevm_aot_register_builtins`] — lives here, in
//! the rubylang library, so it is baked into the rlib the frontend links against.
//! It installs the exact builtins + numeric hook a normal run uses
//! (`host::run_chunk_on`) and loads the embedded program into the thread-local
//! host, so method/block/const dispatch resolves identically to `ruby FILE`.

// ────────────────────────────────────────────────────────────────────────────
// Native method/block dispatch table (`--build --native` full lowering)
// ────────────────────────────────────────────────────────────────────────────

/// The C ABI of a chunk's AOT-lowered native driver, as fusevm emits it: it runs
/// the chunk's ops on `vm` and stores the result via `vm.take_aot_result()`.
type AotEntry = extern "C" fn(*mut fusevm::VM) -> i64;

/// The per-chunk native drivers a `--build --native` binary links in, indexed by
/// `Chunk::native_id - 1`. Populated once at startup by the generated frontend
/// (which alone can name the linked `fusevm_aot_fn_*` symbols) via
/// [`register_native_fns`]; empty in a normal interpreted run, where every chunk
/// has `native_id == 0` anyway.
static NATIVE_FNS: std::sync::OnceLock<Vec<AotEntry>> = std::sync::OnceLock::new();

/// Called once by the AOT frontend's `main` before the driver runs: `addrs` holds
/// the address of each linked `fusevm_aot_fn_{i}` (`fn as usize`), in native-id
/// order. Stored as typed entries so [`native_entry`] can hand one to the runtime.
///
/// # Safety
/// Each address must be a real `extern "C" fn(*mut VM) -> i64` linked into this
/// binary — which is exactly what the generated frontend passes.
pub fn register_native_fns(addrs: Vec<usize>) {
    let fns: Vec<AotEntry> = addrs
        .into_iter()
        .map(|a| unsafe { std::mem::transmute::<usize, AotEntry>(a) })
        .collect();
    let _ = NATIVE_FNS.set(fns);
}

/// The native driver for the chunk tagged `native_id`, if this binary AOT-lowered
/// it (`native_id != 0` and the table is registered). `None` means "interpret it"
/// — the default for a normal `ruby FILE` run.
pub fn native_entry(native_id: u32) -> Option<AotEntry> {
    if native_id == 0 {
        return None;
    }
    let f = NATIVE_FNS.get()?.get((native_id - 1) as usize).copied();
    if std::env::var_os("RUBYLANG_AOT_TRACE").is_some() {
        eprintln!(
            "[aot] dispatch native_id={native_id} -> {}",
            if f.is_some() { "NATIVE" } else { "miss" }
        );
    }
    f
}

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
        let fsrc =
            std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        deps.push((
            p.to_string_lossy().into_owned(),
            crate::cache::key_for(&fsrc),
        ));
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

// ────────────────────────────────────────────────────────────────────────────
// Native standalone executable (`ruby --build --native`)
// ────────────────────────────────────────────────────────────────────────────

/// The AOT frontend hook fusevm's [`fusevm::aot::fusevm_aot_run_embedded`] calls
/// after it deserializes the embedded main chunk and builds the VM. Provided by
/// the frontend per fusevm's contract; here it lives in the rubylang library so
/// the generated frontend links it in via the rubylang rlib.
///
/// It mirrors `crate::host::run_chunk_on`'s VM setup (install every rubylang
/// builtin + the strict numeric hook) so the AOT main chunk dispatches Ruby ops
/// exactly as the interpreter does, then loads the embedded full `Program`
/// (methods/classes/begins/procs) into the thread-local host so a method call,
/// block yield, or namespaced-constant read from main resolves. Each method/block
/// chunk carries a `native_id`; when its body runs, `run_chunk_on` dispatches to
/// the matching `fusevm_aot_fn_{native_id}` native driver (registered by the
/// frontend) instead of interpreting — so loading the program here is what wires
/// the standalone binary's native method/block drivers to their chunk data.
///
/// The full program is carried in two extern symbols the generated frontend
/// defines (`rubylang_aot_program_blob` + `rubylang_aot_program_len`); the main
/// chunk fusevm embedded is ignored here (fusevm already runs it via the driver).
///
/// # Safety
/// `vm` is the valid `&mut VM` fusevm's runtime driver passes; the blob symbols
/// are defined by the linked frontend object.
#[no_mangle]
pub unsafe extern "C" fn fusevm_aot_register_builtins(vm: *mut fusevm::VM) {
    extern "C" {
        static rubylang_aot_program_blob: u8;
        static rubylang_aot_program_len: u64;
    }
    let vm = &mut *vm;
    crate::builtins::install(vm);
    vm.set_numeric_hook(std::sync::Arc::new(|op, a, b| {
        crate::builtins::numeric_hook(op, a, b)
    }));

    // Seed the host exactly as `eval_file` does before a normal run: fresh host,
    // `$LOAD_PATH`/`$LOADED_FEATURES` and the require file-dir stack rooted at the
    // cwd (a bundled binary needs no more, but a leftover dynamic `require` still
    // resolves against where the binary runs).
    crate::host::reset_host();
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::host::with_host(|h| h.init_load_path(&cwd.to_string_lossy()));
    crate::host::push_file_dir(cwd);

    let blob = {
        let len = rubylang_aot_program_len as usize;
        std::slice::from_raw_parts(&rubylang_aot_program_blob as *const u8, len)
    };
    let prog = crate::cache::program_from_blob(blob)
        .unwrap_or_else(|e| panic!("aot: embedded program blob: {e}"));
    // Fresh host: proc/begin ids were compiled from base 0 and nothing is loaded
    // yet, so no rebasing is needed (see `compiler::rebase_program`).
    let crate::compiler::Program {
        main: _,
        methods,
        classes,
        begins,
        procs,
    } = prog;
    crate::host::with_host(|h| h.load_program(methods, classes, begins, procs));
}

/// Locate the rubylang runtime rlib and the deps dir holding fusevm + transitive
/// rlibs, relative to the running `ruby` binary (`.../target/<profile>/ruby` →
/// `librubylang.rlib` + `deps/` in the same dir). Returns an error a linked
/// standalone binary can't be produced without these (e.g. an installed `ruby`
/// stripped of the build tree).
fn locate_runtime() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let exe = std::env::current_exe().map_err(|e| format!("aot: current_exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("aot: cannot find target dir next to the ruby binary")?;
    let rlib = dir.join("librubylang.rlib");
    let deps = dir.join("deps");
    if !rlib.exists() {
        return Err(format!(
            "aot: rubylang rlib not found at {} — --build --native needs the \
             build tree (run from a `cargo build` target dir)",
            rlib.display()
        ));
    }
    Ok((rlib, deps))
}

/// Bundle `file`, emit a native AOT object for its main chunk, bake the full
/// program into a generated frontend, and link a standalone executable next to
/// the source (`app.rb` → `app`). Returns a one-line report. The executable runs
/// the whole app with no `ruby` interpreter and no `.rb` files present.
pub fn build_native(file: &str) -> Result<String, String> {
    use std::path::Path;

    // 1. bundle → Program (identical front half to `build`).
    let (stmts, report) = crate::bundle::bundle(Path::new(file))?;
    let mut prog = crate::compiler::compile(&stmts, false)?;
    let (nmethods, nprocs, nops) = (prog.methods.len(), prog.procs.len(), prog.main.ops.len());

    // Output executable: strip the extension, place it beside the entrypoint.
    let src_abs =
        std::fs::canonicalize(file).map_err(|e| format!("aot: cannot read {file}: {e}"))?;
    let stem = src_abs
        .file_stem()
        .ok_or_else(|| format!("aot: no file stem for {file}"))?
        .to_string_lossy()
        .into_owned();
    let out_exe = src_abs
        .parent()
        .map(|p| p.join(&stem))
        .unwrap_or_else(|| Path::new(&stem).to_path_buf());

    // Scratch build dir (unique per process so concurrent builds don't collide).
    let work = std::env::temp_dir().join(format!("rubylang-aot-{}-{}", stem, std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("aot: mkdir {}: {e}", work.display()))?;

    // 2. Assign a native slot to every method/block chunk (main is the AOT_ENTRY
    // driver fusevm's runtime calls directly). Then Cranelift-lower ALL of them —
    // main + each method/block body — into one object, each method/block under
    // `fusevm_aot_fn_{native_id}`. This is what makes method bodies run as native
    // machine code instead of the interpreter, not just the top-level chunk.
    // Carry each method's `slot_params` into the chunk's `aot_seeded_slots` so the
    // native driver treats the leading param slots as caller-seeded (definite-
    // assignment + integer entry guard). Procs bind their params host-side, so they
    // stay at 0.
    let mut next_id = 1u32;
    for (_, m) in &mut prog.methods {
        m.chunk.native_id = next_id;
        m.chunk.aot_seeded_slots = m.slot_params;
        next_id += 1;
    }
    for (_, c) in &mut prog.classes {
        for (_, m) in &mut c.methods {
            m.chunk.native_id = next_id;
            m.chunk.aot_seeded_slots = m.slot_params;
            next_id += 1;
        }
        for (_, m) in &mut c.class_methods {
            m.chunk.native_id = next_id;
            m.chunk.aot_seeded_slots = m.slot_params;
            next_id += 1;
        }
    }
    for p in &mut prog.procs {
        p.chunk.native_id = next_id;
        next_id += 1;
    }
    let n_native = next_id - 1;

    // Same walk order → `native_id` matches the symbol, so the frontend's table
    // slot `native_id - 1` resolves to this chunk's driver.
    let mut extras: Vec<(String, &fusevm::Chunk)> = Vec::new();
    for (_, m) in &prog.methods {
        extras.push((format!("fusevm_aot_fn_{}", m.chunk.native_id), &m.chunk));
    }
    for (_, c) in &prog.classes {
        for (_, m) in &c.methods {
            extras.push((format!("fusevm_aot_fn_{}", m.chunk.native_id), &m.chunk));
        }
        for (_, m) in &c.class_methods {
            extras.push((format!("fusevm_aot_fn_{}", m.chunk.native_id), &m.chunk));
        }
    }
    for p in &prog.procs {
        extras.push((format!("fusevm_aot_fn_{}", p.chunk.native_id), &p.chunk));
    }

    let obj = work.join("app.o");
    fusevm::aot::compile_program_object(&prog.main, &extras, &obj)?;

    // 3. serialize the full program and write the blob file the frontend embeds.
    let blob = crate::cache::program_to_blob(&prog)?;
    let blob_path = work.join("program.blob");
    std::fs::write(&blob_path, &blob)
        .map_err(|e| format!("aot: write {}: {e}", blob_path.display()))?;

    // 4. generate the frontend: it only uses `extern "C"` FFI + `include_bytes!`,
    // so it needs no `--extern fusevm`. `extern crate rubylang` pulls the whole
    // runtime (rubylang + statically-linked fusevm) into the link, which resolves
    // `fusevm_aot_run_embedded`, `fusevm_aot_register_builtins`, and every shim
    // the object imports. The blob is a fixed-size array so its symbol address is
    // the data itself (a `&[u8]` would export a fat pointer, not the bytes).
    // Declare the linked per-chunk drivers and register their addresses (in
    // native-id order) so the runtime can dispatch a method/block to its native
    // code. Only the frontend can name these symbols, so the table is built here.
    let mut fn_externs = String::new();
    let mut fn_addrs = String::new();
    for i in 1..=n_native {
        fn_externs.push_str(&format!("    fn fusevm_aot_fn_{i}(vm: *mut u8) -> i64;\n"));
        if i > 1 {
            fn_addrs.push_str(", ");
        }
        fn_addrs.push_str(&format!("fusevm_aot_fn_{i} as usize"));
    }

    let frontend = work.join("aot_main.rs");
    let frontend_src = format!(
        "//! Generated by `ruby --build --native` — do not edit.\n\
         #![allow(unused_extern_crates, non_upper_case_globals)]\n\
         extern crate rubylang;\n\n\
         #[no_mangle]\n\
         pub static rubylang_aot_program_blob: [u8; {len}] = *include_bytes!({blob:?});\n\
         #[no_mangle]\n\
         pub static rubylang_aot_program_len: u64 = {len};\n\n\
         extern \"C\" {{\n    fn fusevm_aot_run_embedded() -> i64;\n{fn_externs}}}\n\n\
         fn main() {{\n    \
         // Register every AOT-lowered method/block driver before the program runs.\n    \
         rubylang::aot::register_native_fns(vec![{fn_addrs}]);\n    \
         // SAFETY: resolved from the linked rubylang/fusevm runtime.\n    \
         std::process::exit(unsafe {{ fusevm_aot_run_embedded() }} as i32);\n}}\n",
        len = blob.len(),
        blob = blob_path.to_string_lossy(),
    );
    std::fs::write(&frontend, &frontend_src)
        .map_err(|e| format!("aot: write {}: {e}", frontend.display()))?;

    // 5. link: rustc compiles the frontend against the rubylang rlib (+ deps dir
    // for fusevm & transitive rlibs) and folds in `app.o`.
    let (rlib, deps) = locate_runtime()?;
    let status = std::process::Command::new("rustc")
        .arg("--edition")
        .arg("2021")
        .arg(&frontend)
        .arg("--extern")
        .arg(format!("rubylang={}", rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-C")
        .arg(format!("link-arg={}", obj.display()))
        // cranelift-object doesn't stamp a Mach-O platform load command; the
        // linker assumes macOS and links fine. Silence that benign warning so the
        // build report stays clean (harmless unknown-lint no-op on older rustc).
        .arg("-A")
        .arg("linker_messages")
        .arg("-o")
        .arg(&out_exe)
        .status()
        .map_err(|e| format!("aot: invoke rustc: {e}"))?;
    if !status.success() {
        return Err(format!(
            "aot: rustc link failed (status {status}); frontend + object kept in {}",
            work.display()
        ));
    }

    // 6. report path + size; clean the scratch dir (kept on link failure above).
    let size = std::fs::metadata(&out_exe).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work);
    let nfiles = report.files.len();
    Ok(format!(
        "built {file}: {nfiles} file(s) bundled, {nops} main ops, {nmethods} methods, \
         {nprocs} blocks, {n_native} chunk(s) lowered to native -> {} ({} KiB standalone executable)",
        out_exe.display(),
        size / 1024
    ))
}
