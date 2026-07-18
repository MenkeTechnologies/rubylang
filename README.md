```
██████╗ ██╗   ██╗██████╗ ██╗   ██╗██╗      █████╗ ███╗   ██╗ ██████╗
██╔══██╗██║   ██║██╔══██╗╚██╗ ██╔╝██║     ██╔══██╗████╗  ██║██╔════╝
██████╔╝██║   ██║██████╔╝ ╚████╔╝ ██║     ███████║██╔██╗ ██║██║  ███╗
██╔══██╗██║   ██║██╔══██╗  ╚██╔╝  ██║     ██╔══██║██║╚██╗██║██║   ██║
██║  ██║╚██████╔╝██████╔╝   ██║   ███████╗██║  ██║██║ ╚████║╚██████╔╝
╚═╝  ╚═╝ ╚═════╝ ╚═════╝    ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝ ╚═════╝
```

[![CI](https://github.com/MenkeTechnologies/rubylang/actions/workflows/ci.yml/badge.svg)](https://github.com/MenkeTechnologies/rubylang/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
[![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://menketechnologies.github.io/rubylang/)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-active%20%C2%B7%20in%20development-9b5de5?style=flat-square)

### `[RUBY, COMPILED TO BYTECODE — JIT-COMPILED, NOT TREE-WALKED]`

> *"MRI walks the tree. rubylang compiles it."*

**Ruby in Rust** — a compiled Ruby runtime, hosted on the
[`fusevm`](https://github.com/MenkeTechnologies/fusevm) bytecode VM with a
three-tier Cranelift JIT — the same engine behind `zshrs`, `stryke`, `awkrs`,
and `elisp`.

### [`Read the Docs`](https://menketechnologies.github.io/rubylang/) &middot; [`Engineering Report`](https://menketechnologies.github.io/rubylang/report.html) &middot; [`Builtin Reference`](https://menketechnologies.github.io/rubylang/reference.html)

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] Install](#0x01-install)
- [\[0x02\] Usage](#0x02-usage)
- [\[0x03\] Language Features](#0x03-language-features)
- [\[0x04\] Command-Line Flags](#0x04-command-line-flags)
- [\[0x05\] Architecture](#0x05-architecture)
- [\[0x06\] Parity Harness](#0x06-parity-harness)
- [\[0x07\] Status & Roadmap](#0x07-status--roadmap)
- [\[0x08\] Documentation](#0x08-documentation)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

MRI runs Ruby by walking an AST in C. `rubylang` lexes and parses Ruby to an AST,
lowers it to `fusevm` bytecode, and runs it on a compiled VM with a Cranelift
JIT. rubylang carries no VM or JIT of its own. Highlights:

- **Compiled, not tree-walked** — arithmetic and comparison operators lower to
  native fusevm ops so the JIT can trace hot loops.
- **fusevm-hosted** — no local `vm.rs` / `jit.rs`; the shared engine behind
  `zshrs`, `stryke`, `awkrs`, and `elisp`. `jit-disk-cache` persists native code
  across runs.
- **Native arithmetic** — a strict numeric hook supplies Ruby semantics
  (String/Array `+`, floored integer division, cross-type `==`) only for
  non-numeric operands the VM can't compute directly.
- **Reference-typed objects** — String, Array, and Hash live on the host heap
  behind `Value::Obj` handles, so `a.push(x)` mutates in place — real Ruby
  reference semantics.
- **Scope-sharing blocks** — every variable access routes through the
  thread-local host, so a block run as a nested VM captures and mutates its
  enclosing locals.
- **Ruby truthiness** — only `nil` and `false` are falsy (`0` and `""` are
  true); conditions normalize through a `TRUTHY` op before a native branch.
- **AOP intercepts** — a glob-matched before/after/around method-intercept
  registry, the same design as zshrs's function intercepts.
- **Editor-ready** — an LSP server, a partial DAP adapter, and an `irb`-style
  REPL on a persistent host, all over stdio.
- **Differential parity** — a 35-snippet corpus diffed live against the
  reference `ruby`, frozen and replayed in CI with no `ruby` installed.

---

## [0x01] INSTALL

```sh
# Via Homebrew tap (bumped by each release; formula is `rubylang`)
brew tap MenkeTechnologies/menketech
brew install rubylang

# Or from source
git clone https://github.com/MenkeTechnologies/rubylang
cd rubylang
cargo build

# run a file, a one-liner, or the REPL
./target/debug/ruby script.rb
./target/debug/ruby -e 'puts (1..100).sum'
./target/debug/ruby --repl
```

`rubylang` is a standalone Rust crate (an explicit empty `[workspace]` keeps it
independent of the meta repo). `fusevm` is pulled from crates.io with the `jit`,
`jit-disk-cache`, and `aot` features. Run the tests with `cargo test`.

#### Zsh tab completion

```sh
cp completions/_ruby /usr/local/share/zsh/site-functions/_ruby
# or: fpath=(/path/to/rubylang/completions $fpath) in .zshrc
autoload -Uz compinit && compinit
```

---

## [0x02] USAGE

```ruby
def fib(n)
  n < 2 ? n : fib(n - 1) + fib(n - 2)
end

puts (0..10).map { |i| fib(i) }.join(", ")
# => 0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55

sum = 0
[1, 2, 3, 4].each { |x| sum += x }
puts sum                                   # => 10

puts({ a: 1, b: 2 }.merge({ c: 3 }).keys.length)   # => 3
```

---

## [0x03] LANGUAGE FEATURES

Implemented and checked against the reference `ruby`:

- **Core objects & dispatch** — Integer, Float, String, Array, Hash, Symbol,
  Range, Proc, with a growing core-method and Kernel surface.
- **Blocks / `yield` / closures** — scope-sharing blocks with `break` / `next` /
  `return` control flow.
- **Classes** — `initialize`, `attr_*`, instance variables, single inheritance,
  `super` (bare-forwarding and explicit-args), ancestor-chain method resolution,
  method chaining.
- **Modules** — `module` + `include` mixins searched before the superclass; class
  methods via `def self.m`.
- **Exceptions** — `begin` / `rescue` / `ensure`, method-body and
  statement-modifier `rescue`, `raise` with a message or an exception class,
  typed `ZeroDivisionError` / `NoMethodError` / `ArgumentError` and custom
  classes.
- **Parameters & assignment** — splat parameters (`def f(a, *rest)`), `&:sym`
  block-pass (`map(&:upcase)`), parallel assignment (`a, b = 1, 2`, array
  destructuring, swap), default arguments.
- **String interpolation** — double-quoted `#{}` interpolation.

---

## [0x04] COMMAND-LINE FLAGS

| Flag | Effect |
| --- | --- |
| `FILE` | Run a `.rb` script. |
| `-e SRC` | Run a one-liner. |
| `--repl` | Interactive REPL on a persistent host. |
| `--lsp` | Language Server Protocol over stdio. |
| `--dap` | Debug Adapter Protocol over stdio (handshake + run-to-completion). |
| `--build FILE` | AOT-compile the script's bytecode into the on-disk cache. |
| `--dump-bytecode FILE` | Print the lowered fusevm chunk. |

---

## [0x05] ARCHITECTURE

rubylang contains no virtual machine or JIT of its own. The execution path
mirrors how `zshrs` hosts zsh and `elisp` hosts Emacs Lisp:

```
Ruby source → lexer → parser (AST) → lower to fusevm bytecode → fusevm VM + Cranelift JIT
                                             │
                                RubyHost heap (methods, blocks, String/Array/Hash)
```

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`. Ruby lowers to fusevm bytecode and runs on the shared three-tier Cranelift JIT; `jit-disk-cache` persists native code across runs. |
| **Native arithmetic** | Operators lower to native fusevm ops; a strict numeric hook supplies Ruby semantics (String/Array `+`, floored integer division, cross-type `==`) only for non-numeric operands. |
| **Reference-typed objects** | String, Array, and Hash live on the host heap behind `Value::Obj` handles, so `a.push(x)` mutates in place — real Ruby reference semantics. |
| **Scope-sharing blocks** | Every variable access routes through the thread-local host, so a block run as a nested VM captures and mutates its enclosing locals. |
| **Ruby truthiness** | Only `nil` and `false` are falsy — `0` and `""` are true — so conditions normalize through a `TRUTHY` op before a native branch. |

---

## [0x06] PARITY HARNESS

Behaviour is checked against the reference `ruby` by a **differential parity
harness** — `cargo run --bin parity` diffs the snippet corpus
(`tests/data/parity_corpus.rb`) live against the system `ruby`, and
`tests/parity.rs` replays the frozen outputs in CI with no `ruby` installed.
Nothing is faked as working: an unimplemented method raises `undefined method`.

The `examples/` directory holds runnable programs that double as tests: the
`test_*.rb` scripts embed `check` assertions that abort on any divergence from
Ruby, and `tests/examples.rs` runs every example through the binary in CI,
asserting a clean exit and stdout matching the frozen reference output
(`cargo run --bin parity -- --freeze-examples` regenerates it).

---

## [0x07] STATUS & ROADMAP

The standalone `ruby` binary, the REPL, the rkyv bytecode cache, the AOP
method-intercept registry, and the LSP server are all in the tree.

The DAP adapter is partial (handshake + run-to-completion; stepping pending).
Regex literals (`/pat/flags`), `=~`/`!~`, `String#{match,scan,match?,sub,gsub}`
with `Regexp`, and `MatchData` are supported. `extend` / `prepend` and bignum are
planned. See [`BUGS.md`](BUGS.md) for the full known-gaps list.

---

## [0x08] DOCUMENTATION

- **[Read the Docs](https://menketechnologies.github.io/rubylang/)** — the HUD
  documentation site.
- **[Engineering Report](https://menketechnologies.github.io/rubylang/report.html)**
  — architecture, value model, roadmap, dependency posture.
- **[Builtin Reference](https://menketechnologies.github.io/rubylang/reference.html)**
  — Kernel and core methods, generated from the language-server corpus.
- **[`BUGS.md`](BUGS.md)** — the honest known-gaps list.

---

## [0xFF] LICENSE

MIT — free and open source. See [`LICENSE`](LICENSE).
