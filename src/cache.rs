//! rkyv-backed bytecode cache for compiled Ruby scripts (mirrors
//! zshrs/elisprs/vimlrs). Versioned from day one so a `.rb` that compiled once
//! never breaks on a later run.
//!
//! Layout: a single shard at `~/.rubylang/scripts.rkyv`. The *outer* container is
//! a zero-copy rkyv archive (`Shard`), validated on load; each *inner* entry
//! blob is a bincode-encoded `CProg` (the compiled `fusevm::Chunk`s), because
//! `fusevm::Chunk` is serde-owned, not `rkyv::Archive`. The key is a 64-bit hash
//! of the source plus a schema version, so a source or format change misses
//! cleanly instead of loading stale bytecode.

use crate::compiler::Program;
use crate::host::{BeginDef, ClassDef, MethodDef, ProcDef, RescueDef, Visibility};
use fusevm::Chunk;
use rkyv::{Archive, Deserialize as RkyvDe, Serialize as RkyvSer};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Bump on any incompatible change to `CProg` / the lowering.
const SCHEMA: u64 = 9;

/// The outer, rkyv-archived shard: the [`build_stamp`] of the binary that wrote
/// it, then a flat list of (key, bincode-blob) entries.
#[derive(Archive, RkyvSer, RkyvDe, Default)]
#[archive(check_bytes)]
struct Shard {
    stamp: u64,
    entries: Vec<Entry>,
}

#[derive(Archive, RkyvSer, RkyvDe)]
#[archive(check_bytes)]
struct Entry {
    key: u64,
    blob: Vec<u8>,
}

/// (name, params, splat, kwparams, kwsplat, blockparam, chunk, slot_params) —
/// serde-flat. `slot_params` is the count of leading params bound into frame
/// slots (native-lowerable), 0 for the ordinary host-bound convention.
type CMethod = (
    String,
    Vec<String>,
    Option<usize>,
    Vec<String>,
    Option<String>,
    Option<String>,
    u16,
    u16,
    Vec<String>,
    Chunk,
    u16,
);
/// (name, superclass, methods, includes, prepends, extends, class methods,
/// is_module, non-public method visibilities). `is_module` records `module M`
/// vs `class M`, which nothing else in the tuple implies; the visibility list
/// holds only the private/protected entries (`Visibility::as_u8`), public being
/// the unrecorded default.
type CClass = (
    String,
    Option<String>,
    Vec<CMethod>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<CMethod>,
    bool,
    Vec<(String, u8)>,
);
/// (rescue classes, splat proc id, binding, body proc id) — a serde-flat rescue
/// clause. `splat` is the proc for a `rescue *expr` dynamic class list.
type CRescue = (Vec<String>, Option<usize>, Option<String>, usize);
/// (body proc id, rescues, ensure proc id) — a serde-flat begin block.
type CBegin = (usize, Vec<CRescue>, Option<usize>);
/// (params, splat index, chunk, req, opt, keyword names, required keywords,
/// `**rest` present, `&blk` name) — a serde-flat proc template. The trailing
/// fields are the written parameter shape a lambda is arity-checked against.
type CProc = (
    Vec<String>,
    Option<usize>,
    Chunk,
    u16,
    u16,
    Vec<String>,
    Vec<String>,
    Option<String>,
    Option<String>,
);

/// The inner, serde/bincode form of a compiled program. Tuples keep the shape
/// flat so `fusevm::Chunk`'s serde impl is the only nontrivial dependency.
#[derive(Serialize, Deserialize)]
struct CProg {
    main: Chunk,
    methods: Vec<CMethod>,
    classes: Vec<CClass>,
    begins: Vec<CBegin>,
    procs: Vec<CProc>,
    /// Build-time dependency manifest for a bundled program: `(abs_path, content
    /// key)` for every file inlined by `bundle.rs` (entrypoint first). On load a
    /// still-present file whose content key no longer matches marks the whole
    /// bundle stale (a `require`d file was edited after `--build`); an *absent*
    /// file is trusted, so a bundle runs with its sources deleted. Empty for a
    /// non-bundled single-file entry.
    #[serde(default)]
    deps: Vec<(String, u64)>,
}

/// Serialize a whole compiled `Program` (main chunk + methods/classes/begins/
/// procs) to a self-contained bincode blob, reusing the same serde-flat `CProg`
/// form the on-disk cache uses. `ruby --build --native` bakes this blob into the
/// generated AOT frontend (`include_bytes!`) so the standalone binary carries its
/// full program with no source files and no cache lookup. No dependency manifest
/// is written: the binary IS the program, so staleness never applies.
pub fn program_to_blob(prog: &Program) -> Result<Vec<u8>, String> {
    let cp = to_cprog(prog);
    bincode::serialize(&cp).map_err(|e| format!("aot program encode: {e}"))
}

/// Inverse of [`program_to_blob`]: rebuild a `Program` from an AOT-embedded blob.
/// Called by the AOT runtime hook (`fusevm_aot_register_builtins`) to load the
/// methods/classes/begins/procs into the host before the embedded main chunk runs.
pub fn program_from_blob(bytes: &[u8]) -> Result<Program, String> {
    let cp: CProg = bincode::deserialize(bytes).map_err(|e| format!("aot program decode: {e}"))?;
    Ok(from_cprog(cp))
}

/// A stable content key for a source string.
pub fn key_for(src: &str) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    SCHEMA.hash(&mut h);
    src.hash(&mut h);
    h.finish()
}

/// A stable key for a *bundled* entrypoint: its canonical path plus its source.
/// The path is part of the key because two different apps can share identical
/// entrypoint source while requiring different files (in different directories);
/// keying on source alone would serve one app's bundle for the other.
pub fn key_for_file(abs_path: &str, src: &str) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    SCHEMA.hash(&mut h);
    abs_path.hash(&mut h);
    src.hash(&mut h);
    h.finish()
}

fn shard_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".rubylang");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn shard_path() -> Option<PathBuf> {
    Some(shard_dir()?.join("scripts.rkyv"))
}

/// Identifies the BINARY that wrote the shard: the running executable's size and
/// mtime, plus the schema and crate version.
///
/// Every blob in the shard is bytecode this binary's compiler emitted, so a
/// rebuilt binary must not read them back. Nothing else in the key moved on a
/// rebuild — `SCHEMA` is hand-bumped and `CARGO_PKG_VERSION` only changes at a
/// release — so a dev build that changed lowering silently REPLAYED the previous
/// build's bytecode. Measured rather than reasoned about: with `BinOp::Mul`
/// deliberately lowered to `Op::Add` and the binary rebuilt, a script already in
/// the shard still printed `6 * 7 => 42` while a fresh one printed the broken
/// `13`. A compiler fix would appear to do nothing until the shard was deleted
/// by hand.
///
/// Size and mtime rather than a hash of the executable: the shard is consulted
/// on the startup path of every run, and rubylang's binary is ~58 MB.
fn build_stamp() -> u64 {
    match std::env::current_exe() {
        Ok(p) => exe_stamp(&p),
        // No `current_exe` (an embedder, a stripped environment): the version
        // and schema still separate releases, they just cannot separate two dev
        // builds of the same version.
        Err(_) => exe_stamp(std::path::Path::new("")),
    }
}

/// `build_stamp` for a named executable. Split out so the invalidation test
/// can prove two builds get different stamps without touching the binary it is
/// itself running from.
pub fn exe_stamp(path: &std::path::Path) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    SCHEMA.hash(&mut h);
    env!("CARGO_PKG_VERSION").hash(&mut h);
    if let Ok(md) = std::fs::metadata(path) {
        md.len().hash(&mut h);
        if let Ok(t) = md.modified() {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                d.as_nanos().hash(&mut h);
            }
        }
    }
    h.finish()
}

/// An exclusive advisory lock over the shard, held for the caller's whole
/// read-modify-write.
///
/// `store_keyed` reads the whole shard, adds one entry and writes the whole
/// thing back. Up to 16 rubylang processes share one `~/.rubylang`, so two
/// concurrent stores would each read the same shard and the second write would
/// drop the first's entry. `flock` is advisory and process-wide, which is
/// exactly the scope needed here; a failure to take it is not fatal — the store
/// proceeds unlocked rather than failing the build.
struct ShardLock(Option<std::fs::File>);

impl ShardLock {
    fn acquire() -> ShardLock {
        let Some(dir) = shard_dir() else {
            return ShardLock(None);
        };
        let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("scripts.lock"))
        else {
            return ShardLock(None);
        };
        use std::os::unix::io::AsRawFd;
        // SAFETY: `f` is an open file this process owns for as long as the lock.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return ShardLock(None);
        }
        ShardLock(Some(f))
    }
}

impl Drop for ShardLock {
    fn drop(&mut self) {
        if let Some(f) = &self.0 {
            use std::os::unix::io::AsRawFd;
            // SAFETY: same descriptor, still open until this returns.
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// The stored shard, or an empty one when it was written by a DIFFERENT binary.
///
/// Discarding the whole shard on a stamp mismatch is also how stale entries are
/// pruned: every one of them was emitted by the previous build, and none of
/// their keys will ever be asked for again.
fn load_shard() -> Shard {
    let Some(path) = shard_path() else {
        return Shard::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Shard::default();
    };
    let shard = rkyv::from_bytes::<Shard>(&bytes).unwrap_or_default();
    if shard.stamp != build_stamp() {
        return Shard::default();
    }
    shard
}

/// Write the shard through a temp file and a rename, so a reader never sees a
/// half-written archive. A torn read fails `check_bytes` and lands on
/// `unwrap_or_default()`, which silently discards EVERY entry — a whole-cache
/// loss from an unrelated process's write.
fn write_shard(shard: &mut Shard) -> Result<(), String> {
    let path = shard_path().ok_or("no home dir for cache")?;
    shard.stamp = build_stamp();
    let bytes = rkyv::to_bytes::<_, 4096>(shard).map_err(|e| format!("cache serialize: {e}"))?;
    let tmp = path.with_extension(format!("rkyv.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("cache write: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cache write: {e}")
    })
}

/// Look up a compiled program for the source-only key `src` (single-unit cache;
/// no dependency manifest). See `load_keyed` for the staleness contract.
pub fn load(src: &str) -> Option<Program> {
    load_keyed(key_for(src))
}

/// Look up a bundled program for entrypoint `abs_path` + `src`. Rejected
/// (returns `None`, so the caller recompiles) when any still-present dependency
/// file's content has changed since `--build`.
pub fn load_file(abs_path: &str, src: &str) -> Option<Program> {
    load_keyed(key_for_file(abs_path, src))
}

fn load_keyed(key: u64) -> Option<Program> {
    let shard = load_shard();
    let entry = shard.entries.iter().find(|e| e.key == key)?;
    let cp: CProg = bincode::deserialize(&entry.blob).ok()?;
    for (path, hash) in &cp.deps {
        if let Ok(cur) = std::fs::read_to_string(path) {
            if key_for(&cur) != *hash {
                return None; // a bundled file was edited: stale bundle
            }
        }
    }
    Some(from_cprog(cp))
}

/// Store `prog` (compiled from `src`) under the source-only key, replacing any
/// prior entry.
pub fn store(src: &str, prog: &Program) -> Result<(), String> {
    store_keyed(key_for(src), prog, Vec::new())
}

/// Store a bundled `prog` for entrypoint `abs_path` + `src` together with its
/// dependency manifest so `load_file` can detect a stale bundle. `deps` is
/// `(abs_path, content-key)` for every inlined file.
pub fn store_bundle(
    abs_path: &str,
    src: &str,
    prog: &Program,
    deps: Vec<(String, u64)>,
) -> Result<(), String> {
    store_keyed(key_for_file(abs_path, src), prog, deps)
}

fn store_keyed(key: u64, prog: &Program, deps: Vec<(String, u64)>) -> Result<(), String> {
    let mut cp = to_cprog(prog);
    cp.deps = deps;
    let blob = bincode::serialize(&cp).map_err(|e| format!("cache encode: {e}"))?;
    // Read, modify and write under one lock: a concurrent store would otherwise
    // read the same shard and drop this entry when it wrote its own back.
    let _lock = ShardLock::acquire();
    let mut shard = load_shard();
    shard.entries.retain(|e| e.key != key);
    shard.entries.push(Entry { key, blob });
    write_shard(&mut shard)
}

fn m_to(name: &str, m: &MethodDef) -> CMethod {
    (
        name.to_string(),
        m.params.clone(),
        m.splat,
        m.kwparams.clone(),
        m.kwsplat.clone(),
        m.blockparam.clone(),
        m.req,
        m.opt,
        m.kwreq.clone(),
        (*m.chunk).clone(),
        m.slot_params,
    )
}
fn m_from(
    (name, params, splat, kwparams, kwsplat, blockparam, req, opt, kwreq, chunk, slot_params): CMethod,
) -> (String, MethodDef) {
    (
        name,
        MethodDef {
            chunk_id: crate::host::next_method_chunk_id(),
            params,
            splat,
            kwparams,
            kwsplat,
            blockparam,
            req,
            opt,
            kwreq,
            chunk: std::sync::Arc::new(chunk),
            slot_params,
        },
    )
}

fn to_cprog(prog: &Program) -> CProg {
    CProg {
        deps: Vec::new(),
        main: prog.main.clone(),
        methods: prog.methods.iter().map(|(n, m)| m_to(n, m)).collect(),
        classes: prog
            .classes
            .iter()
            .map(|(n, c)| {
                let methods = c.methods.iter().map(|(mn, m)| m_to(mn, m)).collect();
                let class_methods = c.class_methods.iter().map(|(mn, m)| m_to(mn, m)).collect();
                (
                    n.clone(),
                    c.superclass.clone(),
                    methods,
                    c.includes.clone(),
                    c.prepends.clone(),
                    c.extends.clone(),
                    class_methods,
                    c.is_module,
                    c.visibility
                        .iter()
                        .map(|(n, v)| (n.clone(), v.as_u8()))
                        .collect(),
                )
            })
            .collect(),
        begins: prog
            .begins
            .iter()
            .map(|bd| {
                let rescues = bd
                    .rescues
                    .iter()
                    .map(|r| (r.classes.clone(), r.splat, r.binding.clone(), r.body))
                    .collect();
                (bd.body, rescues, bd.ensure)
            })
            .collect(),
        procs: prog
            .procs
            .iter()
            .map(|p| {
                (
                    p.params.clone(),
                    p.splat,
                    p.chunk.clone(),
                    p.arity.req,
                    p.arity.opt,
                    p.arity.kwnames.clone(),
                    p.arity.kwreq.clone(),
                    p.arity.kwsplat.clone(),
                    p.arity.blockparam.clone(),
                )
            })
            .collect(),
    }
}

fn from_cprog(cp: CProg) -> Program {
    Program {
        main: cp.main,
        methods: cp.methods.into_iter().map(m_from).collect(),
        classes: cp
            .classes
            .into_iter()
            .map(
                |(
                    n,
                    superclass,
                    methods,
                    includes,
                    prepends,
                    extends,
                    class_methods,
                    is_module,
                    visibility,
                )| {
                    let methods = methods.into_iter().map(m_from).collect();
                    let class_methods = class_methods.into_iter().map(m_from).collect();
                    let visibility = visibility
                        .into_iter()
                        .map(|(name, v)| (name, Visibility::from_u8(v)))
                        .collect();
                    (
                        n,
                        ClassDef {
                            superclass,
                            methods,
                            includes,
                            prepends,
                            extends,
                            class_methods,
                            visibility,
                            // Not part of the cached shape: the compiler never
                            // fills it (see `compiler.rs`), so there is nothing
                            // to persist and the rkyv schema is unchanged — a
                            // cache written before this field existed still
                            // loads.
                            class_visibility: Default::default(),
                            is_module,
                        },
                    )
                },
            )
            .collect(),
        begins: cp
            .begins
            .into_iter()
            .map(|(body, rescues, ensure)| {
                let rescues = rescues
                    .into_iter()
                    .map(|(classes, splat, binding, body)| RescueDef {
                        classes,
                        splat,
                        binding,
                        body,
                    })
                    .collect();
                BeginDef {
                    body,
                    rescues,
                    ensure,
                }
            })
            .collect(),
        procs: cp
            .procs
            .into_iter()
            .map(
                |(params, splat, chunk, req, opt, kwnames, kwreq, kwsplat, blockparam)| ProcDef {
                    params,
                    splat,
                    chunk,
                    arity: crate::ast::BlockArity {
                        req,
                        opt,
                        kwnames,
                        kwreq,
                        kwsplat,
                        blockparam,
                    },
                },
            )
            .collect(),
    }
}
