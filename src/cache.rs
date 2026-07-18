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
const SCHEMA: u64 = 1;

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
/// (name, superclass, methods, includes, class methods) — a serde-flat class.
type CClass = (
    String,
    Option<String>,
    Vec<CMethod>,
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
}

/// A stable content key for a source string.
pub fn key_for(src: &str) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    SCHEMA.hash(&mut h);
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

/// Look up a compiled program for `src`, if present and current.
pub fn load(src: &str) -> Option<Program> {
    let key = key_for(src);
    let shard = load_shard();
    let entry = shard.entries.iter().find(|e| e.key == key)?;
    let cp: CProg = bincode::deserialize(&entry.blob).ok()?;
    Some(from_cprog(cp))
}

/// Store `prog` (compiled from `src`) into the shard, replacing any prior entry.
pub fn store(src: &str, prog: &Program) -> Result<(), String> {
    let key = key_for(src);
    let blob = bincode::serialize(&to_cprog(prog)).map_err(|e| format!("cache encode: {e}"))?;
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
            .map(|(n, superclass, methods, includes, class_methods)| {
                let methods = methods.into_iter().map(m_from).collect();
                let class_methods = class_methods.into_iter().map(m_from).collect();
                (
                    n,
                    ClassDef {
                        superclass,
                        methods,
                        includes,
                        class_methods,
                    },
                )
            })
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
