//! Builtin/extension opcode ids are hand-assigned `pub const`s in
//! `src/host.rs`. Two independently written changes can pick the same number:
//! the files merge with no conflict marker, and `VM::register_builtin` keeps the
//! *last* registration for an id — silently replacing the earlier handler, with
//! nothing in the build to catch it. These tests read the constants back out of
//! the source and fail on any duplicate, so a collision is a red test rather
//! than a runtime mystery.
//!
//! Two independent id namespaces exist and are checked separately, because a
//! number reused across them is legal:
//!   * `host::ops`  — `Op::CallBuiltin(id, argc)`, dispatched via the
//!     `builtin_table` that `register_builtin` writes.
//!   * `host::ext`  — `Op::Extended(id, arg)`, dispatched via
//!     `set_extension_handler`.

use std::collections::BTreeMap;

const HOST_RS: &str = include_str!("../src/host.rs");
const BUILTINS_RS: &str = include_str!("../src/builtins.rs");

/// Extract `NAME = value` for every `pub const NAME: u16 = value;` inside
/// `pub mod <module> { … }` in `src/host.rs`, in source order.
fn consts_in_mod(src: &str, module: &str) -> Vec<(String, u16)> {
    let header = format!("pub mod {module} {{");
    let start = src
        .find(&header)
        .unwrap_or_else(|| panic!("`{header}` not found in src/host.rs — module renamed?"))
        + header.len();
    // The module bodies are top-level items, so the first line that is exactly
    // `}` at column 0 closes them.
    let body_len = src[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("unterminated `pub mod {module}` in src/host.rs"));
    let body = &src[start..start + body_len];

    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once(": u16 = ") else {
            continue;
        };
        let Some((num, _)) = rest.split_once(';') else {
            continue;
        };
        let value: u16 = num
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("`{name}` has a non-literal id `{num}`: {e}"));
        out.push((name.to_string(), value));
    }
    assert!(
        !out.is_empty(),
        "no `pub const NAME: u16 = N;` found in `pub mod {module}` — parser out of date"
    );
    out
}

/// Group ids that map to more than one constant name.
fn collisions(consts: &[(String, u16)]) -> BTreeMap<u16, Vec<&str>> {
    let mut by_id: BTreeMap<u16, Vec<&str>> = BTreeMap::new();
    for (name, id) in consts {
        by_id.entry(*id).or_default().push(name.as_str());
    }
    by_id.retain(|_, names| names.len() > 1);
    by_id
}

#[test]
fn builtin_op_ids_are_unique() {
    let consts = consts_in_mod(HOST_RS, "ops");
    let dups = collisions(&consts);
    assert!(
        dups.is_empty(),
        "duplicate `host::ops` builtin ids — `register_builtin` keeps only the \
         LAST handler for an id, so the earlier op silently stops working:\n{}",
        dups.iter()
            .map(|(id, names)| format!("  id {id}: {}", names.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn extension_op_ids_are_unique() {
    let consts = consts_in_mod(HOST_RS, "ext");
    let dups = collisions(&consts);
    assert!(
        dups.is_empty(),
        "duplicate `host::ext` extension ids — the single extension handler \
         cannot tell them apart:\n{}",
        dups.iter()
            .map(|(id, names)| format!("  id {id}: {}", names.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A fresh id is only half the contract: the op also has to be wired to exactly
/// one handler. A copy-pasted `register_builtin(ops::X, …)` line whose constant
/// was not updated registers `X` twice — the second call overwrites the first,
/// and the op the author *meant* to add never gets a handler at all.
#[test]
fn every_builtin_op_is_registered_exactly_once() {
    let consts = consts_in_mod(HOST_RS, "ops");

    let mut counts: BTreeMap<&str, usize> = consts.iter().map(|(n, _)| (n.as_str(), 0)).collect();
    for line in BUILTINS_RS.lines() {
        let Some(rest) = line.trim_start().strip_prefix("vm.register_builtin(ops::") else {
            continue;
        };
        let Some((name, _)) = rest.split_once(',') else {
            continue;
        };
        let name = name.trim();
        let slot = counts.get_mut(name).unwrap_or_else(|| {
            panic!("src/builtins.rs registers `ops::{name}`, which is not declared in `host::ops`")
        });
        *slot += 1;
    }

    let unregistered: Vec<&str> = counts
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(name, _)| *name)
        .collect();
    assert!(
        unregistered.is_empty(),
        "declared in `host::ops` but never registered in src/builtins.rs \
         (the op would trap as an unknown builtin at run time): {unregistered:?}"
    );

    let doubly: Vec<String> = counts
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(name, n)| format!("{name} ({n}x)"))
        .collect();
    assert!(
        doubly.is_empty(),
        "registered more than once in src/builtins.rs — only the last handler \
         survives: {doubly:?}"
    );
}

/// The ids are a dense `1..=N` block. Holding that invariant makes "what is the
/// next free number?" answerable by reading one line (the highest id) instead of
/// scanning the whole module, which is how the duplicates get picked in the
/// first place.
#[test]
fn builtin_op_ids_are_dense_and_start_at_one() {
    let consts = consts_in_mod(HOST_RS, "ops");
    let mut ids: Vec<u16> = consts.iter().map(|(_, id)| *id).collect();
    ids.sort_unstable();
    let expected: Vec<u16> = (1..=consts.len() as u16).collect();
    assert_eq!(
        ids,
        expected,
        "`host::ops` ids must be a dense 1..={} block; a gap means an op was \
         deleted without renumbering (fine to fix by lowering the tail) and a \
         value out of range means a hand-picked number skipped the sequence",
        consts.len()
    );
}
