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
use crate::host::{BeginDef, ClassDef, MethodDef, ProcDef, RescueDef};
use fusevm::Chunk;
use rkyv::{Archive, Deserialize as RkyvDe, Serialize as RkyvSer};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Bump on any incompatible change to `CProg` / the lowering.
const SCHEMA: u64 = 3;

/// The outer, rkyv-archived shard: a flat list of (key, bincode-blob) entries.
#[derive(Archive, RkyvSer, RkyvDe, Default)]
#[archive(check_bytes)]
struct Shard {
    entries: Vec<Entry>,
}

#[derive(Archive, RkyvSer, RkyvDe)]
#[archive(check_bytes)]
struct Entry {
    key: u64,
    blob: Vec<u8>,
}

/// (name, params, splat, kwparams, kwsplat, blockparam, chunk) — serde-flat.
type CMethod = (
    String,
    Vec<String>,
    Option<usize>,
    Vec<String>,
    Option<String>,
    Option<String>,
    Chunk,
);
/// (name, superclass, methods, includes, prepends, extends, class methods).
type CClass = (
    String,
    Option<String>,
    Vec<CMethod>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<CMethod>,
);
/// (rescue classes, binding, proc id) — a serde-flat rescue clause.
type CRescue = (Vec<String>, Option<String>, usize);
/// (body proc id, rescues, ensure proc id) — a serde-flat begin block.
type CBegin = (usize, Vec<CRescue>, Option<usize>);
/// (params, splat index, chunk) — a serde-flat proc template.
type CProc = (Vec<String>, Option<usize>, Chunk);

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

fn shard_path() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".rubylang");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("scripts.rkyv"))
}

fn load_shard() -> Shard {
    let Some(path) = shard_path() else {
        return Shard::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Shard::default();
    };
    rkyv::from_bytes::<Shard>(&bytes).unwrap_or_default()
}

fn write_shard(shard: &Shard) -> Result<(), String> {
    let path = shard_path().ok_or("no home dir for cache")?;
    let bytes = rkyv::to_bytes::<_, 4096>(shard).map_err(|e| format!("cache serialize: {e}"))?;
    std::fs::write(&path, &bytes).map_err(|e| format!("cache write: {e}"))
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
    let mut shard = load_shard();
    shard.entries.retain(|e| e.key != key);
    shard.entries.push(Entry { key, blob });
    write_shard(&shard)
}

fn m_to(name: &str, m: &MethodDef) -> CMethod {
    (
        name.to_string(),
        m.params.clone(),
        m.splat,
        m.kwparams.clone(),
        m.kwsplat.clone(),
        m.blockparam.clone(),
        m.chunk.clone(),
    )
}
fn m_from(
    (name, params, splat, kwparams, kwsplat, blockparam, chunk): CMethod,
) -> (String, MethodDef) {
    (
        name,
        MethodDef {
            params,
            splat,
            kwparams,
            kwsplat,
            blockparam,
            chunk,
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
                    .map(|r| (r.classes.clone(), r.binding.clone(), r.body))
                    .collect();
                (bd.body, rescues, bd.ensure)
            })
            .collect(),
        procs: prog
            .procs
            .iter()
            .map(|p| (p.params.clone(), p.splat, p.chunk.clone()))
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
                |(n, superclass, methods, includes, prepends, extends, class_methods)| {
                    let methods = methods.into_iter().map(m_from).collect();
                    let class_methods = class_methods.into_iter().map(m_from).collect();
                    (
                        n,
                        ClassDef {
                            superclass,
                            methods,
                            includes,
                            prepends,
                            extends,
                            class_methods,
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
                    .map(|(classes, binding, body)| RescueDef {
                        classes,
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
            .map(|(params, splat, chunk)| ProcDef {
                params,
                splat,
                chunk,
            })
            .collect(),
    }
}
