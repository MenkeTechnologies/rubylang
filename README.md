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
- **Scope-sharing blocks** — every variable access routes through the shared
  host, so a block run as a nested VM captures and mutates its enclosing locals.
- **Real threads under a GVL** — `Thread.new` spawns an OS thread that runs on
  the one process-global object heap, serialized by a Global VM Lock exactly like
  MRI: only one thread executes Ruby at a time, so shared-heap mutation stays
  atomic. `Thread#join`/`#value` release the GVL while waiting and re-raise a
  thread's exception; each thread swaps in its own call stack. `Mutex`, `Queue`/
  `SizedQueue`, and `ConditionVariable` are provided — a blocking `Queue#pop`/
  `ConditionVariable#wait` releases the GVL and parks on its own condvar until a
  producer/signaller wakes it.
- **Ruby truthiness** — only `nil` and `false` are falsy (`0` and `""` are
  true); conditions normalize through a `TRUTHY` op before a native branch.
- **AOP intercepts** — a glob-matched before/after/around method-intercept
  registry, the same design as zshrs's function intercepts.
- **Editor-ready** — an LSP server, a DAP debugger (source-line breakpoints
  inside methods, stepping, stack + locals), and an `irb`-style REPL on a
  persistent host, all over stdio.
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
  destructuring, swap), block-parameter destructuring (`map { |(a, b), i| … }`,
  nested and splat groups, `->((a, b)) { }`), default arguments.
- **String interpolation** — double-quoted `#{}` interpolation.
- **Standard library** — `require`-able builtin libs including `json`, `set`,
  `date`/`time`, `securerandom`, `digest`, `base64`, `ostruct`, `socket`
  (`TCPServer`/`TCPSocket`), `sqlite3`, and `fiddle`.
- **Database persistence** — `require "sqlite3"` gives a real, on-disk
  `SQLite3::Database` backed by a bundled SQLite (compiled in-tree, no external
  gem or FFI). The core sqlite3-gem shape: `SQLite3::Database.new(path)` /
  `.open` (and `":memory:"`), block form that auto-closes, `execute` /
  `execute2` with positional binds, rows as Arrays or (with
  `results_as_hash = true`) Hashes, `get_first_row` / `get_first_value`,
  `last_insert_row_id` / `changes`, and rescuable `SQLite3::SQLException`. The
  sqlite→Ruby type map is INTEGER→Integer, REAL→Float, TEXT→String, NULL→nil,
  BLOB→String. See [`examples/sqlite_persistence.rb`](examples/sqlite_persistence.rb).
- **ActiveRecord-style ORM** — [`stdlib/active_record.rb`](stdlib/active_record.rb)
  is a real, pure-Ruby subset of Rails' ActiveRecord built on the bundled
  `SQLite3::Database` (no stubs, runs identically on rubylang and MRI). Covers
  connection management + table-name inference + PRAGMA schema introspection with
  dynamically-defined column accessors; a lazy chainable query `Relation`
  (`where` / `order` / `limit` / `offset` / `pluck` / `first` / `last` / `count`
  / `exists?` / `find_each` / `where.not`, every value `?`-bound); finders
  (`find` / `find_by` / `find_by!`); persistence (`create!` / `save!` / `update` /
  `destroy_all` / `update_all` / `reload`, dirty tracking, `new_record?`);
  presence + query-backed uniqueness validations with an `errors` object
  (`save` returns `false` when invalid, `save!`/`create!` raise
  `AR::RecordInvalid`); `before_save`/`after_save`/`before_create`/`after_create`/
  `before_destroy` callbacks; and `belongs_to` / `has_many` associations. See
  [`examples/orm_app.rb`](examples/orm_app.rb) (CRUD) and
  [`examples/orm_blog.rb`](examples/orm_blog.rb) (associations, validations,
  callbacks, query chaining).
- **FFI via Fiddle** — `require "fiddle"` gives real foreign-function calls:
  `Fiddle.dlopen` a shared library (or `dlopen(nil)` for the current process),
  resolve a symbol's address with `handle[sym]`, then build a
  `Fiddle::Function.new(addr, [arg_types], ret_type)` and `#call` it. Arguments
  (Integer/Float/String) marshal to C and the result marshals back through
  libffi, with a runtime-determined signature — this is genuine C execution, not
  a shim. `Fiddle::Pointer` wraps bytes, reads a returned `char*` back into a
  Ruby String, and supports `ptr[i]`/`ptr[i, len]` reads plus `ptr[i] = byte` /
  `ptr[i, len] = str` writes (clamped to the allocation). The MRI type codes are
  provided (`TYPE_VOID`/`TYPE_INT`/
  `TYPE_LONG`/`TYPE_SIZE_T`/`TYPE_VOIDP`/`TYPE_DOUBLE`/`TYPE_CHAR`/`TYPE_SHORT`/
  `TYPE_LONG_LONG` and their unsigned negatives), and `Fiddle::DLError` is
  rescuable. FFI is inherently unsafe — a wrong signature can crash the process,
  exactly as MRI Fiddle documents.

  ```ruby
  require "fiddle"
  libc = Fiddle.dlopen(nil)                                   # current process
  strlen = Fiddle::Function.new(libc["strlen"],
                                [Fiddle::TYPE_VOIDP], Fiddle::TYPE_SIZE_T)
  strlen.call("hello")                                        # => 5
  sqrt = Fiddle::Function.new(libc["sqrt"],
                              [Fiddle::TYPE_DOUBLE], Fiddle::TYPE_DOUBLE)
  sqrt.call(16.0)                                             # => 4.0
  dup = Fiddle::Function.new(libc["strdup"],
                             [Fiddle::TYPE_VOIDP], Fiddle::TYPE_VOIDP)
  dup.call("world").to_s                                      # => "world"
  ```

  **Boundary:** Fiddle calls *arbitrary C functions in a shared library* — it is
  not a libruby C ABI. MRI-C-API extension gems (nokogiri, the C `mysql2`, etc.)
  link against `libruby` and its `VALUE`/`rb_*` internals; that ABI is a
  separate, much larger surface Fiddle does not provide, so those native gems do
  not load. Pure-Ruby gems and anything expressible as direct C calls do.

---

## [0x04] COMMAND-LINE FLAGS

The `ruby` binary parses the `ruby(1)` option grammar (repeatable `-e`, bundled
short switches, glued value switches like `-Idir`/`-rlib`, `--` end-of-options,
and program-file-vs-`ARGV` handling), so shebang scripts and tools that shell out
to `ruby` work unchanged.

**MRI-compatible switches**

| Flag | Effect |
| --- | --- |
| `FILE [args…]` | Run a `.rb` script; `args` become `ARGV`, `$0`/`$PROGRAM_NAME`/`__FILE__` are set. Runs a matching `--build` bundle from the cache if one is current, else compiles fresh. |
| `-e SRC [args…]` | Run a one-liner (repeatable; multiple `-e` are joined with newlines). Trailing `args` become `ARGV`; `$0` is `-e`. |
| `-I DIR` | Prepend `DIR` to `$LOAD_PATH` (repeatable; glued `-Idir` or detached). |
| `-r LIB` | `require LIB` before the program runs (repeatable). |
| `-c` | Check syntax only — print `Syntax OK`, do not run. |
| `-w` / `-W[level]` | Warning level → `$VERBOSE` (`-W0` → `nil`, `-w`/`-W1` → `false`, `-W2` → `true`). |
| `-d` / `--debug` | Set `$DEBUG`. |
| `-S` | Search `$PATH` for the program file. |
| `-v` | Print the version banner, then run any program. |
| `--version` | Print the version banner and exit. |
| `-h` / `--help` | Print usage and exit. |
| `--` | End of options; the next token is the program file, the rest is `ARGV`. |

`RUBY_VERSION` reports `3.4.0` (the targeted MRI language level, so gems'
`required_ruby_version` checks pass); `RUBY_ENGINE` is `rubylang`,
`RUBY_ENGINE_VERSION` the crate version, `RUBY_PLATFORM` the host triple. The
text-processing line-loop switches (`-n`/`-p`/`-a`/`-l`/`-F`) are parsed but not
yet implemented (they need ARGF plus the Kernel `$_` method family) and error out
rather than silently mis-running.

**rubylang extensions**

| Flag | Effect |
| --- | --- |
| `--repl` | Interactive REPL on a persistent host. |
| `--lsp` | Language Server Protocol over stdio. |
| `--dap` | Debug Adapter Protocol over stdio: source-line and function breakpoints, stepping, call stack, locals, and expression `evaluate`. |
| `--build FILE` | AOT-bundle the whole app — the entrypoint plus every file it statically `require`s / `require_relative`s — into one program in the on-disk cache. A later `ruby FILE` runs it directly, needing none of the required sources on disk. |
| `--build --native FILE` | Emit a **standalone native executable** next to the script (`app.rb` → `app`). It runs the whole app with no `ruby` interpreter and no `.rb` sources present. `fusevm`'s Cranelift AOT emitter compiles the main chunk to a native object; the full program (methods/classes/blocks/constants) is baked in and linked against the rubylang runtime (`rustc` + the crate rlib). Needs `rustc` and the build tree present. |
| `--dump-tokens FILE` | Print the lexer token stream and exit. |
| `--dump-ast FILE` | Print the parsed AST and exit. |
| `--dump-bytecode FILE` | Print the lowered fusevm chunk and exit. |
| `--disasm FILE` | Print a fusevm bytecode disassembly and exit. |

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
| **Scope-sharing blocks** | Every variable access routes through the shared host, so a block run as a nested VM captures and mutates its enclosing locals. |
| **GVL threading** | One process-global `Mutex<RubyHost>` heap (MRI's single-VM model). `Thread`s share it; a Global VM Lock serializes execution (only the lock-holder runs Ruby), released around blocking waits. Local-var envs are `Arc<Mutex>` and fibers are thread-owned so the heap is `Send`. |
| **Ruby truthiness** | Only `nil` and `false` are falsy — `0` and `""` are true — so conditions normalize through a `TRUTHY` op before a native branch. |

---

## [0x06] PARITY HARNESS

Behaviour is checked against the reference `ruby` by a **differential parity
harness** — `cargo run --bin parity` diffs the snippet corpus
(`tests/data/parity_corpus.rb`) live against the system `ruby`, and
`tests/parity.rs` replays the frozen outputs in CI with no `ruby` installed.
Nothing is faked as working: an unimplemented method raises `undefined method`.

Alongside the fixed corpus, a **differential parity fuzzer** — `cargo run --bin
parity-fuzz` — generates thousands of seed-deterministic Ruby snippets across
grammar-driven modes (arithmetic, float shortest-repr, slicing, enumerables,
format specs, `case`/`when`, …) and diffs stdout + exit code of the reference
`ruby` against rubylang. Every divergence is delta-debugged to a minimal
reproducer and replays exactly with `parity-fuzz --seed N --once`. It runs
subprocesses only (never links the library) and needs a reference `ruby`, so CI
does not run it. `--baseline FILE` allowlists known gaps so only a new
divergence fails the run; `RUBYLANG_FUZZ_RUBY=PATH` selects the oracle.

The `examples/` directory holds runnable programs that double as tests: the
`test_*.rb` scripts embed `check` assertions that abort on any divergence from
Ruby, and `tests/examples.rs` runs every example through the binary in CI,
asserting a clean exit and stdout matching the frozen reference output
(`cargo run --bin parity -- --freeze-examples` regenerates it).

---

## [0x07] STATUS & ROADMAP

The standalone `ruby` binary, the REPL, the rkyv bytecode cache, the LSP server,
and the AOP method-intercept engine are all in the tree. `before`/`after`/
`around` advice registered via `intercept(pattern, kind, handler)` fires from the
method-dispatch choke point, gated on an O(1) check so unadvised calls are
unaffected.

`ruby --build --native FILE` compiles an app to a **standalone native
executable** — no interpreter, no `.rb` sources at run time. `fusevm`'s Cranelift
AOT emitter lowers the main chunk to a relocatable object exporting a native
driver; the full program (methods/classes/blocks/constants) is serialized and
baked into a generated frontend, which `rustc` links against the rubylang runtime
(the crate rlib, itself statically linking fusevm) plus that object. At startup
the frontend hook installs the same builtins + numeric hook a normal run uses and
loads the embedded program into the host, so method dispatch, block yields, and
namespaced-constant reads resolve exactly as under `ruby FILE`. Method/block
bodies run through the interpreter from that host; only the top-level chunk is
native today.

The DAP debugger (`ruby --dap`) sets source-line and function breakpoints (break on method entry), steps (next/stepIn/stepOut), evaluates expressions against the paused frame, and inspects the call stack and locals; markers are emitted only in --dap mode, so normal runs are unaffected.
Regex literals (`/pat/flags`), `=~`/`!~`, `String#{match,scan,match?,sub,gsub}`
with `Regexp`, `MatchData`, and arbitrary-precision `Integer` (auto-promotion on
overflow) are supported. `extend` / `prepend` / `class << self`, the full
`case/in` pattern surface (two-sided find patterns, `**nil` exact-key matching,
alternation binding, the `deconstruct`/`deconstruct_keys` protocol on user
objects), reserved-word keyword labels (`def f(class:)`), `Date`/`Time`/
`DateTime`, bare `%(…)` strings, and `__END__` are all implemented. See
[`BUGS.md`](BUGS.md) for the full known-gaps list.

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
