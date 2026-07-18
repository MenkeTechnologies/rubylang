```
██████╗ ██╗   ██╗██████╗ ██╗   ██╗██████╗ ███████╗
██╔══██╗██║   ██║██╔══██╗╚██╗ ██╔╝██╔══██╗██╔════╝
██████╔╝██║   ██║██████╔╝ ╚████╔╝ ██████╔╝███████╗
██╔══██╗██║   ██║██╔══██╗  ╚██╔╝  ██╔══██╗╚════██║
██║  ██║╚██████╔╝██████╔╝   ██║   ██║  ██║███████║
╚═╝  ╚═╝ ╚═════╝ ╚═════╝    ╚═╝   ╚═╝  ╚═╝╚══════╝
```

[![CI](https://github.com/MenkeTechnologies/rubylang/actions/workflows/ci.yml/badge.svg)](https://github.com/MenkeTechnologies/rubylang/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-active%20%C2%B7%20in%20development-9b5de5?style=flat-square)

**Ruby in Rust** — a compiled Ruby runtime, hosted on the
[`fusevm`](https://github.com/MenkeTechnologies/fusevm) bytecode VM with a
three-tier Cranelift JIT — the same engine behind `zshrs`, `stryke`, `awkrs`,
and `elisp`.

## What it is

MRI runs Ruby by walking an AST in C. `rubylang` lexes and parses Ruby to an AST,
lowers it to `fusevm` bytecode, and runs it on a compiled VM with a Cranelift
JIT. Arithmetic and comparison operators lower to native VM ops so the JIT can
trace hot loops; Ruby-specific behaviour (method dispatch, blocks, object
construction, `yield`) is served by a thread-local runtime host. rubylang carries
no VM or JIT of its own.

```
Ruby source → lexer → parser (AST) → lower to fusevm bytecode → fusevm VM + Cranelift JIT
                                             │
                                RubyHost heap (methods, blocks, String/Array/Hash)
```

## Try it

```sh
cargo build

# run a file, a one-liner, or the REPL
./target/debug/ruby script.rb
./target/debug/ruby -e 'puts (1..100).sum'
./target/debug/ruby --repl
```

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

## Design

| Piece | How |
| --- | --- |
| **fusevm-hosted** | No local `vm.rs` / `jit.rs`. Ruby lowers to fusevm bytecode and runs on the shared three-tier Cranelift JIT; `jit-disk-cache` persists native code across runs. |
| **Native arithmetic** | Operators lower to native fusevm ops; a strict numeric hook supplies Ruby semantics (String/Array `+`, floored integer division, cross-type `==`) only for non-numeric operands. |
| **Reference-typed objects** | String, Array, and Hash live on the host heap behind `Value::Obj` handles, so `a.push(x)` mutates in place — real Ruby reference semantics. |
| **Scope-sharing blocks** | Every variable access routes through the thread-local host, so a block run as a nested VM captures and mutates its enclosing locals. |
| **Ruby truthiness** | Only `nil` and `false` are falsy — `0` and `""` are true — so conditions normalize through a `TRUTHY` op before a native branch. |

## Command line

| Flag | Effect |
| --- | --- |
| `FILE` | Run a `.rb` script. |
| `-e SRC` | Run a one-liner. |
| `--repl` | Interactive REPL on a persistent host. |
| `--lsp` | Language Server Protocol over stdio. |
| `--dap` | Debug Adapter Protocol over stdio (handshake + run-to-completion). |
| `--build FILE` | AOT-compile the script's bytecode into the on-disk cache. |
| `--dump-bytecode FILE` | Print the lowered fusevm chunk. |

## Status

Implemented: lexer/parser, AST→bytecode lowering, the object heap and method
dispatch (Integer, Float, String, Array, Hash, Symbol, Range, Proc), a growing
core-method and Kernel surface, blocks / `yield` / closures, **classes with
`initialize`/`attr_*`/single inheritance/`super`**, **modules and `include`
mixins**, **class methods (`def self.m`)**, **exceptions (`begin`/`rescue`/
`ensure`, method-body and modifier `rescue`, typed exception classes)**, **splat
parameters, `&:sym` block-pass, parallel assignment, and default arguments**, the
standalone `ruby` binary and REPL, the rkyv bytecode cache, an AOP
method-intercept registry, and an LSP server.

Behaviour is checked against the reference `ruby` by a **differential parity
harness** — `cargo run --bin parity` diffs a 35-snippet corpus live, and
`tests/parity.rs` replays the frozen outputs in CI with no `ruby` installed.

The DAP adapter is partial (handshake + run-to-completion; stepping pending).
`extend`/`prepend`, keyword parameters, regex, and bignum are planned. See
[`BUGS.md`](BUGS.md) for the full known-gaps list.

## Building

rubylang is a standalone Rust crate (an explicit empty `[workspace]` keeps it
independent of the meta repo). `fusevm` is pulled from crates.io with the `jit`,
`jit-disk-cache`, and `aot` features. Run the tests with `cargo test`.

## License

MIT — free and open source. See [`LICENSE`](LICENSE).
