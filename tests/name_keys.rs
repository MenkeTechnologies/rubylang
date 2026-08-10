//! Duplicate-key guards for the registries keyed by NAME, the companion to
//! `tests/op_ids.rs` (which guards the numeric opcode space).
//!
//! # The hazard
//!
//! A name-keyed registry that maps a key to a VALUE has the same last-write-wins
//! failure the opcode ids do, and it is quieter. Two rows with the same key
//! merge without a conflict marker, one of them wins, and the loser's data is
//! simply never read again. What surfaces later is a wrong answer attributed to
//! an unrelated name — `Method#arity` reporting some other method's shape, an
//! `ArgumentError` quoting some other method's accepted range.
//!
//! Rust does not catch it. `match`-arm registries at least draw an
//! `unreachable_patterns` WARNING, but a warning does not fail a build and is
//! invisible in the output of compiling an 18k-line file. The static tables in
//! `arity_table` and `lsp` draw NOTHING: they are slice literals, and to the
//! compiler two rows with the same first field are just two rows.
//!
//! # Second failure of the same registries: sort order
//!
//! Every table here is read with `binary_search_by`. That is only correct on a
//! sorted slice; on an unsorted one the probe walks the wrong half and reports
//! "not found" for a row that is present, which reads exactly like a missing
//! entry. `--freeze`-style regeneration keeps the order, but a hand-added row
//! appended to the end does not, so the ordering is asserted too.
//!
//! # What is deliberately NOT guarded
//!
//! Membership-only lists (`STRING_MUTATORS`, `KEYWORDS`, `INTROSPECT_CALLS`,
//! `host::PRIVATE_HOOKS`, …) are `&[&str]` tested with `contains`. A repeated
//! entry there answers the same question twice and cannot change a result, so a
//! guard on them would fail builds over a harmless typo. `BUILTIN_MODULES` is
//! membership-only too and appears below for its sort order only.

use std::collections::BTreeMap;
use std::fmt::Debug;

/// Report every key carrying more than one row, with the rows it collided over.
fn duplicates<K: Ord + Debug + Clone, V: Debug>(rows: impl Iterator<Item = (K, V)>) -> Vec<String> {
    let mut by_key: BTreeMap<K, Vec<V>> = BTreeMap::new();
    for (k, v) in rows {
        by_key.entry(k).or_default().push(v);
    }
    by_key
        .into_iter()
        .filter(|(_, vs)| vs.len() > 1)
        .map(|(k, vs)| format!("  {k:?} appears {} times: {vs:?}", vs.len()))
        .collect()
}

fn assert_no_duplicates(table: &str, consequence: &str, dups: Vec<String>) {
    assert!(
        dups.is_empty(),
        "duplicate keys in `{table}` — the table is read with `binary_search_by`, \
         which returns an ARBITRARY one of the matching rows, so {consequence}. \
         The build is clean either way:\n{}",
        dups.join("\n")
    );
}

/// `binary_search_by` on an unsorted slice reports "not found" for rows that are
/// present. Assert the exact ordering each lookup relies on.
fn assert_sorted<K: Ord + Debug>(table: &str, keys: Vec<K>) {
    // `windows(2)` yields NOTHING for a 0- or 1-element slice, so without this
    // precondition an emptied table passes every sortedness check in this file
    // having compared no pair at all — a green run that measured nothing. Two
    // rows is the smallest input that can actually be out of order.
    assert!(
        keys.len() > 1,
        "`{table}` has {} row(s): the sortedness check compares adjacent pairs, \
         so fewer than two rows means this assertion examined nothing. Either \
         the table was emptied or its generator stopped emitting.",
        keys.len()
    );
    for pair in keys.windows(2) {
        assert!(
            pair[0] < pair[1],
            "`{table}` is not sorted ascending: {:?} precedes {:?}. Every read \
             is a `binary_search_by`, so rows past the first inversion become \
             unreachable — a present row looks missing.",
            pair[0],
            pair[1]
        );
    }
}

// ---------------------------------------------------------------------------
// arity_table — five tables, all keyed by name, all binary searched.
// ---------------------------------------------------------------------------

#[test]
fn builtin_methods_have_one_row_per_name() {
    use rubylang::arity_table::BUILTIN_METHODS;
    assert_no_duplicates(
        "arity_table::BUILTIN_METHODS",
        "`Method#arity`, `#owner` and `#parameters` would report whichever row won",
        duplicates(
            BUILTIN_METHODS
                .iter()
                .map(|(o, m, arity, params)| ((*o, *m), (*arity, *params))),
        ),
    );
    assert_sorted(
        "arity_table::BUILTIN_METHODS",
        BUILTIN_METHODS.iter().map(|(o, m, ..)| (*o, *m)).collect(),
    );
}

#[test]
fn builtin_arg_shapes_have_one_row_per_name() {
    use rubylang::arity_table::BUILTIN_ARG_SHAPES;
    assert_no_duplicates(
        "arity_table::BUILTIN_ARG_SHAPES",
        "a call would be accepted or rejected against another method's measured \
         argument range, and the `wrong number of arguments` message would quote it",
        duplicates(
            BUILTIN_ARG_SHAPES
                .iter()
                .map(|(o, m, min, max, expected, ..)| ((*o, *m), (*min, *max, *expected))),
        ),
    );
    assert_sorted(
        "arity_table::BUILTIN_ARG_SHAPES",
        BUILTIN_ARG_SHAPES
            .iter()
            .map(|(o, m, ..)| (*o, *m))
            .collect(),
    );
}

#[test]
fn builtin_zero_arg_errors_have_one_row_per_name() {
    use rubylang::arity_table::BUILTIN_ZERO_ARG_ERRORS;
    assert_no_duplicates(
        "arity_table::BUILTIN_ZERO_ARG_ERRORS",
        "a zero-argument call would raise with another method's bespoke message",
        duplicates(
            BUILTIN_ZERO_ARG_ERRORS
                .iter()
                .map(|(o, m, msg, msg_blk)| ((*o, *m), (*msg, *msg_blk))),
        ),
    );
    assert_sorted(
        "arity_table::BUILTIN_ZERO_ARG_ERRORS",
        BUILTIN_ZERO_ARG_ERRORS
            .iter()
            .map(|(o, m, ..)| (*o, *m))
            .collect(),
    );
}

#[test]
fn builtin_ancestry_has_one_row_per_class() {
    use rubylang::arity_table::BUILTIN_ANCESTRY;
    assert_no_duplicates(
        "arity_table::BUILTIN_ANCESTRY",
        "`#owner` resolution would walk another class's ancestor chain",
        duplicates(BUILTIN_ANCESTRY.iter().map(|(c, chain)| (*c, chain.len()))),
    );
    assert_sorted(
        "arity_table::BUILTIN_ANCESTRY",
        BUILTIN_ANCESTRY.iter().map(|(c, _)| *c).collect(),
    );
}

#[test]
fn builtin_modules_are_sorted_and_unique() {
    use rubylang::arity_table::BUILTIN_MODULES;
    // Membership-only, so a duplicate cannot change an answer — but it is still
    // a merge artifact, and the sort order is load-bearing for `binary_search`.
    assert_sorted("arity_table::BUILTIN_MODULES", BUILTIN_MODULES.to_vec());
}

// ---------------------------------------------------------------------------
// lsp::CORPUS — the shared LSP-completion / `gen-docs` table.
// ---------------------------------------------------------------------------

/// `(name, chapter)` is the key: `gen-docs` deliberately allows a name to repeat
/// ACROSS chapters (`to_s` on several classes) and disambiguates the anchor with
/// a per-chapter counter. Repeating it WITHIN one chapter is the defect — the
/// reference page grows two identical entries, LSP completion offers the name
/// twice, and `gen-docs` prints an inflated `"{n} methods"` line into
/// `docs/reference.html`.
#[test]
fn lsp_corpus_has_one_entry_per_name_per_chapter() {
    let corpus = rubylang::lsp::corpus();
    // An empty corpus has no duplicates, so the assertion below would pass
    // having inspected nothing. The corpus is the source for LSP completion and
    // `docs/reference.html`; empty means the generator broke, not that the
    // invariant holds.
    assert!(
        !corpus.is_empty(),
        "lsp::corpus() is empty — the duplicate check below would pass vacuously"
    );
    let dups = duplicates(
        corpus
            .iter()
            .map(|(name, chapter, doc, _ex)| ((*chapter, *name), *doc)),
    );
    assert!(
        dups.is_empty(),
        "duplicate (chapter, name) in `lsp::CORPUS` — the reference page renders \
         the entry twice, LSP completion offers it twice, and the published \
         method count in docs/reference.html is inflated:\n{}",
        dups.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Source-text registries — the `match`/`matches!` name lists in builtins.rs.
// ---------------------------------------------------------------------------

const BUILTINS_RS: &str = include_str!("../src/builtins.rs");

/// The body of `fn <name>` up to the first line that is exactly `}` at column 0.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("`{sig}` not found in src/builtins.rs — renamed?"));
    let len = src[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("unterminated `{sig}` in src/builtins.rs"));
    &src[start..start + len]
}

/// Every double-quoted literal on the pattern side of the arms in `body`.
///
/// A pattern line is one whose first non-space character is `"` (the head of an
/// arm) or `|` (its continuation) — the shape `cargo fmt` gives both a `match`
/// and a multi-line `matches!`. Requiring that prefix is what keeps ordinary
/// code out: `let n = name.strip_suffix(".rb")…` in the same function bodies
/// also holds a string literal, and counting it reported a bogus `".rb"`
/// collision between the two registries.
///
/// On a pattern line the value side is dropped at `=>`, so the bundled path in
/// `include_str!("../stdlib/uri.rb")` is not collected; the `/stdlib/` check
/// keeps that true if an arm is ever reflowed onto one line.
fn pattern_literals(body: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        let line = raw.trim_start();
        if !(line.starts_with('"') || line.starts_with('|')) {
            continue;
        }
        let patterns = line.split("=>").next().unwrap_or("");
        for piece in patterns.split('"').skip(1).step_by(2) {
            if piece.contains("/stdlib/") || piece.is_empty() {
                continue;
            }
            out.push((lineno + 1, piece.to_string()));
        }
    }
    assert!(
        !out.is_empty(),
        "no quoted patterns extracted — the scanner is out of date with the source"
    );
    out
}

/// `embedded_stdlib` maps a `require` name to the bundled source that is
/// evaluated for it. A name listed twice takes whichever body comes first and
/// the other library never loads under that name; `rustc` reports it as an
/// `unreachable pattern` WARNING, which does not fail a build.
#[test]
fn embedded_stdlib_names_are_unique() {
    let body = fn_body(BUILTINS_RS, "pub(crate) fn embedded_stdlib(");
    let dups = duplicates(pattern_literals(&body).into_iter().map(|(l, n)| (n, l)));
    assert!(
        dups.is_empty(),
        "duplicate require name in `embedded_stdlib` — only the first arm is \
         reachable, so the second library is unloadable under that name (lines \
         listed):\n{}",
        dups.join("\n")
    );
}

/// `is_builtin_lib` is the list of `require` names satisfied by the runtime
/// itself. A name repeated here is a merge artifact; a name that ALSO appears in
/// `embedded_stdlib` is the load-bearing conflict, because it decides whether a
/// `require` evaluates the bundled Ruby source or silently succeeds with nothing
/// defined — the failure then surfaces at the first use of a constant the
/// library was supposed to define.
#[test]
fn builtin_lib_names_are_unique_and_disjoint_from_embedded_stdlib() {
    let no_op = fn_body(BUILTINS_RS, "pub(crate) fn is_builtin_lib(");
    let no_op_names = pattern_literals(&no_op);

    let dups = duplicates(no_op_names.iter().map(|(l, n)| (n.clone(), *l)));
    assert!(
        dups.is_empty(),
        "duplicate name in `is_builtin_lib` (lines listed):\n{}",
        dups.join("\n")
    );

    let embedded = fn_body(BUILTINS_RS, "pub(crate) fn embedded_stdlib(");
    let embedded_names: std::collections::BTreeSet<String> = pattern_literals(&embedded)
        .into_iter()
        .map(|(_, n)| n)
        .collect();

    let both: Vec<&str> = no_op_names
        .iter()
        .map(|(_, n)| n.as_str())
        .filter(|n| embedded_names.contains(*n))
        .collect();
    assert!(
        both.is_empty(),
        "these names are in BOTH `is_builtin_lib` and `embedded_stdlib`: {both:?}. \
         `require` consults one of them; whichever loses, the bundled Ruby source \
         either runs or is skipped for a `require` that reports success either way, \
         and the mismatch only shows up at the first missing constant."
    );
}
