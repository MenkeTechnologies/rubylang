# Known gaps

rubylang is in active development. The pipeline (lex → parse → lower to fusevm
bytecode → run) is solid for the implemented surface, verified against the
reference `ruby` by the parity harness (`cargo run --bin parity`, replayed in CI
by `tests/parity.rs`). This file tracks what is deliberately not done yet, so the
gaps are honest rather than surprising. Unimplemented METHODS raise
`undefined method` rather than answering a plausible value. The guarantee stops
at methods: some unimplemented CONSTANTS answer `nil` instead of raising
`NameError` (`FileUtils`, `Socket`, `UNIXServer`, `Addrinfo`, and `Errno::*`
through it), and a few flag-valued APIs answer a wrong number rather than
refusing (`Regexp::EXTENDED`/`MULTILINE` read back as `1`; `~/re/` answers
`-1`). Those are listed in their own sections below.

## The oracle is never bare `ruby`

rubylang installs its own binary under the name `ruby`. Wherever it is
installed, every `ruby` on `PATH` — including `/opt/homebrew/bin/ruby`, a
symlink into `…/Cellar/rubylang/<ver>/bin/ruby` — IS rubylang. A differential
harness that spawns bare `ruby` therefore compares rubylang to itself: every
snippet agrees, the fuzzer reports zero divergences on a broken build, and
`--freeze` writes rubylang's own stdout into `tests/data/` as the reference
answer. Nothing fails, so nothing reveals it.

`src/oracle.rs` is the single resolver every harness uses. It never falls back
to a `PATH` lookup, and a candidate must pass four independent proofs before it
is accepted: it must not canonicalize into a `rubylang` install or a `target/`
build dir; **its executable must not contain `oracle::SELF_MARKER`**; its
`--version` must have MRI's `ruby X.Y.Z (… revision …) [platform]` shape; and
`RUBY_ENGINE` must be `"ruby"`. An unresolvable oracle is a hard error, not a
skip. `RUBYLANG_ORACLE_RUBY` names one explicitly and is never silently
replaced.

**Three of those four proofs decay as rubylang matures, and the decay is
silent.** They ask the candidate to describe itself, and rubylang's job is to
answer the way MRI does; nothing fails when a clause stops discriminating, it
just stops contributing. It has already happened to the `--version` shape:
rubylang prints `ruby 3.4.0 (rubylang 0.1.4) [aarch64-macos]`, which now matches
the leading `ruby ` and the bracketed platform, so `revision` is the only clause
of that proof still separating the two. `RUBY_ENGINE` is one compatibility shim
away from going the same way — it answers `"rubylang"` only because rubylang
chooses to.

The content proof is the one that cannot decay, because it asks nothing: a
rubylang binary carries the marker and an MRI binary cannot, however close the
mimicry gets. It catches a rubylang renamed, installed outside a `rubylang`
path, given MRI's exact `--version` and made to answer `RUBY_ENGINE` as
`"ruby"` — a case the path proof cannot see, pinned by
`tests/entry_points.rs::the_oracle_refuses_a_rubylang_that_no_path_check_could_catch`.

Two consequences worth knowing:

- `tools/gen_arity_table.rb` aborts unless `RUBY_ENGINE == "ruby"`. Regenerating
  the reference table from rubylang would freeze rubylang's own answers as the
  reference it is measured against.
- `--freeze-examples` LEAVES ALONE any example the oracle cannot run, and
  records what each frozen output actually is in
  `tests/data/examples/PROVENANCE.tsv`. Three examples (`orm_app`, `orm_blog`,
  `sqlite_persistence`) require libraries rubylang embeds and a stock MRI does
  not have installed, so MRI exits with a `LoadError`; their `.out` files are
  rubylang self-baselines and are labelled `NOT-REFERENCE`. The other fifteen
  were re-verified byte-for-byte against ruby 4.0.6.

## What the harnesses structurally cannot report

Gating the oracle answers *which reference* is consulted. It says nothing about
which questions get asked, or which parts of the answer are compared. Both are
bounded, and a bug inside a blind spot is not "not yet found" — it is
unfindable by that harness at any case count. The census below is per harness,
so a gap can be closed on purpose rather than stumbled into.

| Harness | Cannot report | Why | State |
| --- | --- | --- | --- |
| `tests/parity.rs` | Any stderr difference | Compares `stdout` only; stderr is never captured | Open — by design; it is the CI replay, and stderr carries paths |
| `tests/parity.rs` | The exact *reason* a rejected snippet was rejected | A frozen `<error>` records no class or message, so a snippet that fails for a different Ruby-level reason still passes | Open — narrowed: it now also requires empty stdout, no Rust panic, and a `… (SomeError)` line on stderr, so a PANIC and a silent non-zero exit no longer satisfy it |
| `bin/parity` | Exit-code differences | Compares captured stdout; a rubylang error becomes the same `<error>` marker regardless of class or message | Open |
| `bin/parity` | Anything about a snippet MRI rejects | Both sides collapse to `<error>` | Open |
| `parity-fuzz` | Stderr, unless `--stderr` | `differs` compares stdout + exit; stderr is opt-in | Open — deliberate, the wording is noisier than the behaviour |
| `parity-fuzz` | The LINE NUMBER in any diagnostic | `norm_stderr` strips the leading `-e:LINE:` prefix — so the line-0 bug below is invisible even WITH `--stderr` | Open — stripping is what makes wording comparable at all |
| `parity-fuzz` | The FRAME NAME in any diagnostic | `norm_stderr` strips through the first `": "`, taking `in 'Integer#/'` with it | Open — same mechanism |
| `parity-fuzz` | Output produced before a timeout | `run_with_timeout` discards stdout on the timeout path and reports an empty buffer | Open |
| `parity-fuzz` | A hang that both sides share | Two timeouts compare equal and count as parity | Open — cannot be distinguished from agreement without a reference runtime |
| `parity-fuzz` | Locale- or TZ-dependent behaviour | Neither `run_oracle` nor `run_ours` pins `LANG`/`LC_ALL`/`TZ`; both inherit the developer's ambient environment, so a divergence that only appears under another locale cannot appear here | Open — see the locale note below |
| all harnesses | A PANIC or ABORT on a degenerate input | Every generator and every frozen corpus builds a WELL-FORMED program, so a self-referential container, an out-of-range radix and a past-`u16::MAX` precision were all unreachable — the nine abort shapes above were found by hand-built degenerate input, not by any harness | Open — the shapes are now pinned as regression tests, but nothing GENERATES new ones |
| all harnesses | The exception CLASS and its attribute readers | `bin/parity` and `tests/parity.rs` compare stdout, so a snippet that prints nothing and raises is invisible; `parity-fuzz --stderr` compares the message TEXT, which is identical when only the class or a missing `#key`/`#errno` differs | Open — the Theme-C findings above came from probes that print `e.class`/`e.key`/`e.errno` explicitly |
| all generators | Type-mismatched operands | Every generator built a WELL-TYPED program, so the whole implicit-conversion surface was unreachable | **Closed** — `typeerr` mode; it found 21 real divergences on its first outing |
| all generators | Anything involving `Time` | The determinism invariant bans `Time`, and the ban was read as covering the class rather than the clock — but `Time.at(<const>)` is deterministic | **Closed** — `timefmt` mode; ten `strftime` directives were unimplemented behind it |

The two closures share a shape worth naming: in both, the COMPARISON was
already strong enough to catch the bug — wrong stdout AND wrong exit status —
and only the generators stood between the fuzzer and the finding. A blind-spot
census that only audits the comparison would have missed both.

### The line-0 bug, and why no harness sees it

An exception raised from inside a builtin operation reports line 0:

```console
$ ./target/debug/ruby -e $'x=1\ny=2\n1/0'
-e:0:in '<main>': divided by 0 (ZeroDivisionError)
$ /opt/homebrew/opt/ruby/bin/ruby -e $'x=1\ny=2\n1/0'
-e:3:in 'Integer#/': divided by 0 (ZeroDivisionError)
```

**Fixed, and the diagnosis above was wrong.** Nothing was lost inside fusevm and
no upstream change was needed. `**`, `/`, `%`, `<<`, `>>`, `<=>`, `===`, `=~`,
`&`, `|` and `^` are Ruby METHODS, so `compile_binary` routes them through method
dispatch instead of the native op — and that one dispatch site emitted its
`CallBuiltin` with a literal `0` where every other call site in the compiler
passes `self.cur_line`. Passing the line there fixes every case listed above:
`1/0`, `7 % 0`, `(2**70)/0`, `[].freeze << 1` and `"a".freeze << "b"` each now
report the line they are written on, at top level, inside a method and inside a
block. Regression: `tests/uncaught.rs::an_operator_that_raises_reports_its_own_line`.

The FRAME is still `<main>` where MRI names the builtin that raised
(`in 'Integer#/'`, plus a `from` line for the caller). That is the separate
frame-name gap listed further down, and it is why these cases still differ under
`parity-fuzz --stderr` even though the line is now right.

A SECOND, distinct defect lives next to it: an error raised by the native
operator path returns a bare `Err(String)` and never sets a pending exception,
so `format_uncaught` has nothing to decorate and the diagnostic prints with no
location and no class tag at all:

```console
$ ./target/debug/ruby -e 'nil + 1'
undefined method '+' for nil
$ /opt/homebrew/opt/ruby/bin/ruby -e 'nil + 1'
-e:1:in '<main>': undefined method '+' for nil (NoMethodError)
```

Both are invisible to `parity-fuzz` for the same reason: `norm_stderr` strips
exactly the part that differs.

### Locale

Checked against the reference under `LC_ALL` of `C`, `en_US.UTF-8`,
`tr_TR.UTF-8` and `de_DE.UTF-8`, with a pinned `TZ`:

- **`Encoding.default_external` is the one real locale divergence.** MRI reports
  `US-ASCII` under `LC_ALL=C` and `UTF-8` under a `.UTF-8` locale; rubylang
  always answers `UTF-8`. Related: under `LC_ALL=C`, MRI rejects non-ASCII
  source as `invalid multibyte character`, while rubylang accepts it.
- `String#upcase`/`downcase` are NOT locale-sensitive in either (`"istanbul"`
  upcases to `ISTANBUL` even under `tr_TR.UTF-8`, per Ruby's Unicode default).
- `sort` on non-ASCII is codepoint-ordered in both — no collation.
- `Integer#to_s` never groups in either.
- `strftime` day/month names are English in both regardless of locale (Ruby does
  not consult `LC_TIME`).

No frozen record depends on the locale, because rubylang's own output is
locale-invariant on all five axes — the risk is one-sided: running
`parity --freeze` under a non-UTF-8 locale would capture MRI's
locale-dependent answers as the baseline. The harnesses do not pin the
environment, so that remains possible.

### Frozen-record provenance

Every frozen expectation was re-run through the reference and compared
mechanically, asking two questions: does the current reference still produce
this string, and could ANY reference have produced it (a pin that fails the
second test was written from memory, not captured, and no oracle gate can
detect that).

- `tests/data/parity_expected.txt` — 570 pins, **570 reproduce byte-for-byte**
  against ruby 4.0.6. No stale pins, no fabricated pins.
- `tests/data/examples/*.out` — 15 rows labelled `reference` in
  `PROVENANCE.tsv`, **all 15 reproduce**. The 3 rows labelled `NOT-REFERENCE`
  are rubylang self-baselines and were not compared, which is what the label is
  for.

### Hardcoded-wording provenance (the other direction)

The frozen records above are data files, and a fabricated pin in one is caught
by re-running the reference. A diagnostic sentence hardcoded in `src/` is not:
no oracle gate, no version gate and no frozen record can see a string frozen in
the implementation source. Every diagnostic rubylang EMITS was written by a
person, so the same two questions have to be asked of those too.

Method: 144 literals were extracted mechanically from the `raise_exc`/`abort`
call sites and matched against the string constants in `libruby.4.0.dylib`,
every bundled extension and the whole installed stdlib, with `{…}` placeholders
and digit runs treated as argument slots. 43 had no MRI wording behind them; each
was then put to the reference interpreter directly. **51 wording families
measured, 17 agreed, 34 did not; 23 corrected, 11 left as measured gaps below.**

Four failure shapes turned up, and only the first is the obvious one:

- **Never any MRI's words.** `tried to create Enumerator without a block`, `index
  0 outside of array bounds: -0...0` (a minus sign written as literal text next
  to a placeholder), `can't convert to Rational`.
- **An older MRI's words, frozen where no gate can see them.** `for GC:Module` is
  what MRI said before it started saying `for module GC`. `step can't be
  negative` is a real message applied to `step(0)` as well, so it gave the wrong
  reason for zero and raised at all for a negative step, which MRI answers `[]`.
- **A real MRI string on the wrong feature.** `no receiver is available` exists
  in `libruby`; the message for `:sym.to_proc.call` is `no receiver given`.
- **Right words, wrong class, or right in one place and wrong in another.**
  `no block given (yield)` was a RuntimeError from `b_yield` and a LocalJumpError
  from `catch`. `can't modify frozen Integer: 1` was already correct in
  `frozen_guard` and wrong in the `instance_variable_set` arm four thousand lines
  away.

Remaining measured gaps in this family, all left deliberately: the frame name in
a diagnostic (`in 'Kernel#require'` vs `in '<main>'`), already listed above --
the line-0 bug beside it is now fixed; `ENV`'s address-bearing receiver phrase; `Module.new`'s
`#<Module:0x…>` address; `NoMatchingPatternError` omitting which clause failed
and why (`99: 1 === 99 does not return true`); MRI's `Socket::ResolutionError`
class and its `getaddrinfo` wording; `Zlib::GzipFile::Error`; and MRI's
multi-line Prism syntax-error rendering.

### `inspect` escapes a smaller set than MRI

MRI escapes any character its encoding calls unprintable, which is a full
Unicode general-category table — 1044 codepoints below U+3000 alone. rubylang
escapes the C0 controls, DEL, the C1 controls and U+2028/U+2029, which covers
every character that occurs in practice, and prints assigned-but-unusual
codepoints raw where MRI would escape them. Closing this needs a Unicode
category table, which is a dependency decision rather than a bug fix.

## An abort is not an exception

MRI's failures are all catchable. rubylang's were not: eight shapes ended the
process with `fatal runtime error: stack overflow, aborting` (rc=134) or a Rust
`panicked at …` (rc=101 / rc=1) rather than raising. A `rescue` cannot see
either, so no Ruby program could defend against them, and the happy path
matching hid every one. All eight are closed; the census below is the record of
where they were, because the same shape recurs wherever a walk has no base case.

| Shape | Was | MRI | Closed by |
| --- | --- | --- | --- |
| `a=[1];a<<a; p a` (also `to_s`, Hash, Set, Struct) | stack-overflow abort | `[1, [...]]` | `RubyHost::cycle_marker` + the `rendering` stack |
| `a=[1];a<<a; a.join(",")` | stack-overflow abort | `ArgumentError: recursive array join` | `join_into` |
| `a=[1];a<<a; a.flatten` | stack-overflow abort | `ArgumentError: tried to flatten recursive array` | `flatten_depth_into` |
| `a=[1];a<<a; a.hash` / `h[a]=1` | stack-overflow abort | a finite Integer | `to_key_seen` + `RKey::Recursive` |
| `a=[1];a<<a; a == a.dup` | stack-overflow abort | `true` | `EQ_PAIRS` |
| `a=[1];a<<a; a.eql?(a)` / `uniq` | stack-overflow abort | `true` | `EQL_PAIRS` |
| `h = Hash.new { \|hh,k\| hh[k] }; h[:a]` (also `Proc#call`, `define_method`) | stack-overflow abort | `SystemStackError` | `ProcDepth` in `call_proc_self_ctx` |
| `(10**30).to_s(1)` | `panicked at num-bigint … The radix must be within 2...36` | `ArgumentError: invalid radix 1` | `check_radix` |
| `sprintf("%.65536f", 1.0)` (also `%e`, `%g`) | `panicked at … Formatting argument out of range` | the formatted string | `fixed` / `sci` |

Three things about the fixes are worth stating, because each was wrong once:

- **The elision marker is for RE-ENTRY, not repetition.** `rendering` is a
  STACK, not a set: `x = [1]; [x, x].inspect` is `[[1], [1]]` because the first
  render pops before the second begins. A set would have printed `[[1], [...]]`.
- **A BOUNDED `flatten` cannot fail to terminate, so MRI does not guard it.**
  The first fix raised for `a.flatten(1)`, where MRI answers
  `[1, 1, [1, [...]]]`. MRI's detection is armed only for the unbounded form;
  `a.flatten(100)` walks the cycle a hundred times and answers a 102-element
  array. The guard now arms only when `depth < 0`.
- **`==` and `eql?` need SEPARATE pair stacks.** Sharing one made
  `class Z; def ==(o); true; end; end; Z.new.eql?(Z.new)` answer true where MRI
  answers false: `eql?` falls through to `==` for a non-container, so an
  in-flight `eql?` was satisfying the very `==` it delegated to. Caught by
  `tests/eval.rs::eql_is_equality_with_numeric_class_strictness`, which is the
  reason that test exists.

The block-recursion ceiling is 2000 nested activations, the same number
`run_method` applies to `def` bodies. Blocks do not push a `Frame` — they swap
`active_scope` — which is exactly why the frame-depth guard could not see them.
Native exhaustion is between 8000 and 16000 on the interpreter's 1 GB worker
thread (`src/main.rs`), measured, so the guard sits an order of magnitude clear
of it. Pinned at the BINARY entry point by
`tests/entry_points.rs::runaway_recursion_raises_a_rescuable_error_rather_than_aborting_the_process`:
an in-process `eval_to_string` test would measure the test harness's much
smaller thread stack and prove nothing about `ruby -e`.

### Still aborts nothing, but still diverges

- **`proc { break }.call` is swallowed.** MRI raises
  `LocalJumpError: break from proc-closure` with `#reason == :break`; rubylang
  evaluates it to nil and prints nothing. The raise site exists and now records
  `#reason`, but the `break` signal from a stored proc never reaches it. The
  other `LocalJumpError` shape (`def m; yield; end; m`) does raise, with
  `#reason == :noreason`, and is pinned.
- **A bare undefined CONSTANT answers nil instead of raising.** `p Nope` is
  `nil` where MRI raises `NameError`. This is the constant half of the "some
  unimplemented constants answer nil" note at the top of this file, and it stays
  open on purpose: the nil is what lets a program name a library constant
  rubylang does not model (`FileUtils`, `Socket`, `Errno::*`) without stopping.
  The lowercase half is CLOSED — `p nope` now raises
  `NameError: undefined local variable or method 'nope' for main`, with `#name`
  and `#receiver` set. Closing it needed the compiler, as predicted here: a name
  the scope assigns at or before the read is a local (Ruby hoists it to nil from
  that parse position, so `y = (y || 0) + 1` and a name assigned only inside an
  `if` that did not run both read nil) and lowers to `GETLOCAL_DECLARED`, while
  every other bareword lowers to `GETLOCAL` and raises when nothing answers it.
  One MRI strictness remains unmatched at the top level, where locals are
  slot-lowered: `p y; y = 1` reads nil there rather than raising, since the slot
  exists for the whole scope.
- **`p obj` does not dispatch a user-defined `#inspect`**, and `Array#join` does
  not dispatch a user-defined `#to_s`. `Class.new { def inspect; "Y"; end }.new`
  inspects as `#<#<Class:1>>` rather than `Y`; `[obj].join(",")` stringifies the
  default form rather than calling the object's `to_s`.

## Exception SHAPE, not exception wording

A right message under the wrong class — or under an object that answers no
`#key`, `#errno`, `#receiver` or `#result` — is a divergence no message audit
can see, because those readers are how a `rescue` body branches. Closed here:

| Reader | Was | Now |
| --- | --- | --- |
| `Exception#inspect` | the bare message (`"x"`) | `#<RuntimeError: x>`; a bare class name for an empty message; the message INSPECTED (and the space dropped) when it is multi-line |
| `KeyError#key` / `#receiver` | `NoMethodError` | the missed key and the collection |
| `StopIteration#result` | `NoMethodError` | what the underlying `each` answered |
| `LoadError#path` | `NoMethodError` | the path that would not load |
| `SystemCallError#errno` | `NoMethodError` | the OS error number |
| `FrozenError#receiver` | `nil` | the object that could not be modified |
| `NoMethodError#args` / `#private_call?` | `NoMethodError` | `[]` / `false` |
| `LocalJumpError#reason` | `NoMethodError` | `:noreason` / `:break` |
| `NameError#name` with no name | `:""` | `nil`, as MRI answers for `raise NameError, "x"` |
| the `Errno::*` family | one flat `SystemCallError` | the specific class, `rescue`-able by name (see File/IO) |

`raise_exc_with` is the mechanism: it records the structured fields on the
exception object at the raise site, where they are known. A hand-constructed
exception (`KeyError.new("m")`) answers nil for them, as MRI's does.

**Not closed, and why:**

- **`NoMethodError#receiver` and `#args` are empty for a native miss.** The
  dispatcher reports an undefined method by returning a bare `Err(String)`;
  the exception object is synthesized later by `infer_exc_class`, which has only
  the message. `#name` survives because it is parseable out of the message text;
  the receiver and the argument list are not. Converting those sites to
  `raise_exc_with` is the fix, and it touches every `undefined method` raise in
  the dispatcher.
- **`break` in an orphaned proc raises `LocalJumpError` — fixed.** A block's
  `break` targets the method invocation the block was passed to; when that
  invocation has already returned, MRI raises `LocalJumpError: break from
  proc-closure` AT THE CALL, so a `rescue` catches it and the program continues.
  The signal used to escape unclaimed, which silently abandoned the rest of the
  statement and exited 0 — a wrong answer rather than an error.

  The boundary is a property of the PROC, not of the stack, which is why a count
  of break-owners on the stack cannot be the rule: `[1].each { pr.call }` raises
  even though `each` owns a `break` of its own. `RObj::Proc` therefore carries
  the id of the invocation that ADOPTED it (`host::enter_block_home`, stamped by
  the four block-carrying call ops), and `BLOCK_HOMES` holds the ids still on the
  stack. Stamping at adoption rather than at creation is what gets both columns
  right: the literal in `m { break 2 }` is stamped with `m`'s id and `m` is still
  running when `b.call` reaches the break, while the literal in
  `proc { break 1 }` is stamped with the `proc` call's id, which dies when
  `proc` returns the object. A proc no call ever adopted keeps a `None` home and
  behaves as it always did — `None` is "unknown", not "dead".

  Only the `break` KEYWORD can be an orphan. The generator yielder raises a break
  of its own to stop a block once `first(n)`/`take(n)` has enough, so
  `b_sig_break` marks the lexical one and the check asks for the mark;
  `Enumerator.new { |y| … }.first(2)` is an ordinary answer, as in MRI.

  Verified against ruby 4.0.6 on thirteen shapes — the five that raise
  (`proc{}.call`, `each(&pr)`, `each { pr.call }`, a block captured with `&b` and
  returned, a proc returned from a method), the four that must not (a lambda, a
  block whose call is live, `map { break 9 }`, a lazy pipeline), and four
  generator/enumerator drivers that must stay ordinary answers.

- **A block frame is named `block in X` — fixed.** MRI names the frame a failure
  was raised in, and inside a block that is `block in <enclosing>`, with
  `block (N levels) in <enclosing>` once blocks nest. N counts the block literals
  the code is WRITTEN inside — it is lexical, not a call depth — and `<enclosing>`
  is the method the outermost of them was written in (`K#m`, `K.s`, `<main>`).
  A lambda body counts as a block. `ProcDef::block_depth` carries N from the
  compiler, and the label is built from the proc's CAPTURED scope, so a proc
  written at the top level and called from inside a method is still
  `block in <main>`.

  A method CALLED from inside a block is named for the method, not the block, so
  the label is pushed together with `frames.len()` at entry and only wins while
  no deeper frame exists — `[1].each { where }` is `Object#where`.

  One residue: the `LocalJumpError` an orphaned `break` raises is built after the
  block body has returned, so it still reports the enclosing frame
  (`-e:1:in '<main>'` where MRI says `block in <main>`). The class, message,
  `reason` and exit status all agree; only that label does not.
- **`Exception#backtrace` is `[]`, never `nil` and never populated.** MRI
  answers `nil` for an exception that was never raised and a real frame list for
  one that was. rubylang retains no per-exception Ruby backtrace (see the line-0
  note above), so both cases answer the same empty Array. Callers splat it or
  call `.first` on it, so `[]` is the safer of the two wrong answers.
- **`Exception#full_message` takes no keyword arguments.**
  `full_message(highlight: false, order: :top)` raises
  `ArgumentError: wrong number of arguments (given 1, expected 0)`.
- **`eval` reports a syntax error as a `RuntimeError`.** MRI raises
  `SyntaxError`, which is a `ScriptError` and therefore NOT caught by a bare
  `rescue`; rubylang's is caught by one. The class tree is right —
  `builtin_exception_parent` already puts `SyntaxError` under `ScriptError` —
  but the eval path does not raise it.
- **`exit(3)` is not rescuable as `SystemExit`**, and an uncaught `throw` is not
  rescuable as `UncaughtThrowError`. Both terminate instead of raising an object
  a handler can inspect.
- **MRI's `DidYouMean::Correctable` / `ErrorHighlight::CoreExt` do not appear in
  `ancestors`.** These are gems MRI injects into `NameError`/`KeyError`/
  `TypeError`. Deliberately absent — rubylang emits no "did you mean"
  suggestions at all — so the ancestor lists are one or two entries shorter than
  MRI's while the class itself and its real superclass chain match.

## A test that can pass having asserted nothing

Two sibling frontends shipped tests that reported PASS while executing zero
assertions. A census of all 439 `#[test]` functions here found 17 with the same
shape; every one is now either loud or visibly reported. The rule applied: a
test may skip only through a mechanism a reader of the output can SEE, and it
may not skip at all when the condition it gates on cannot legitimately be false.

| File | Was | Now |
| --- | --- | --- |
| `tests/ffi.rs` (2) | `if !rustc_available() { return }` — one of them with no message at all | `require_rustc()` ASSERTS. This file is compiled and linked by the very `rustc` it was checking for, so a missing one is a broken environment, not an unsupported one |
| `tests/aot_native.rs` (2) | `if !toolchain_ready() { eprintln!; return }` | `require_toolchain()` asserts both halves. `librubylang.rlib` is produced by the same `cargo test` that runs the test, so its absence is a build-layout regression |
| `tests/aot_bundle.rs::mri_parity_for_bundled_app` | no frozen literal at all: with no MRI installed it was `assert_eq!(direct, bundled)` — rubylang against rubylang | a frozen `"CB\n"` measured from ruby 4.0.6, asserted unconditionally, plus a `SKIPPED WITNESS:` line when the oracle resolves nothing |
| `tests/aot_native.rs::native_binary_matches_mri_when_available` | silent when the oracle was absent | same `SKIPPED WITNESS:` line; its frozen literal already fired |
| `tests/name_keys.rs` (6) | `assert_sorted` is `for pair in keys.windows(2)`, which yields NOTHING for a 0- or 1-element slice | `assert_sorted` requires `len() > 1`; `lsp_corpus_…` requires a non-empty corpus |
| `tests/examples.rs::provenance_covers_every_example` | two empty-tolerant loops — an all-comment TSV checked no label, an empty `examples/` compared no coverage | both sources pinned non-empty first |
| `tests/parity.rs::corpus_matches_reference_ruby` | no non-empty guard on the corpus; a corpus that stopped parsing passes | `!snips.is_empty()` |
| `tests/fiddle.rs` (4) | `assert!(out == "5" \|\| out == ":unresolved")` — nothing distinguished a verified C call from the escape | `assert_call` prints `SKIPPED C CALL:` when the sentinel fires, and a new `fiddle_core_libc_symbols_resolve` fails if `strlen` stops resolving, so all four cannot silently skip together |

The `ffi.rs` fix was verified to FIRE rather than being decorative: with
`RUSTC=/nonexistent/rustc-xyz`, the two tests that previously both passed now
both fail with `rustc (/nonexistent/rustc-xyz) must be runnable`.

Shapes the census looked for and did NOT find anywhere in `tests/`: `#[ignore]`,
`#[should_panic]`, `cfg!(target_os)` gating, `TMPDIR`/`HOME`/`CI`/locale skip
guards, network-availability skips, `assert!(true)`, and `let _ = <the Result
under test>`. `tests/io.rs::dir_home_matches_env` reads `HOME` with `.unwrap()`,
which PANICS on an unset `HOME` rather than skipping — the right shape.

## Working (for reference)

Classes with `initialize`/`attr_*`/instance methods, single inheritance, `super`
(bare-forwarding and explicit-args), method resolution through the ancestor chain
(own → included modules → superclass), `self`, instance variables, method
chaining; endless method definitions (`def square(x) = x * x`, including
`def self.m(x) = …`); `module` + `include` mixins; `extend`/`prepend`/`class << self`;
class methods (`def self.m`); `begin`/
`rescue`/`ensure`, method-body and statement-modifier `rescue`, `raise` with a
message or an exception class, typed `ZeroDivisionError`/`NoMethodError`/
`ArgumentError`; default arguments; splat parameters (`def f(a, *rest)`); `&:sym`
block-pass (`map(&:upcase)`); keyword arguments (`def f(name:, x: 1)` + `f(name:
"a")`); `%w[]`/`%i[]` word/symbol arrays; operator method definitions (`def +`,
`def <=>`, `def [](i)`) and `Comparable` (`< > <= >= == between?` derived from
`<=>`); block comparators (`sort { |a, b| … }`, `min`/`max` with a block);
call-site/array/target splat (`f(*a)`, `[1, *a]`, `a, *b = …`); parallel
assignment (`a, b = 1, 2`, swap); `case`/`when Class`
(`when Integer`) and `is_a?`; `sprintf`/`format`/`String#%` with width/precision
flags; a broad Enumerable/Hash surface (`partition`, `group_by`, `tally`, `zip`,
`each_with_object`, `transform_values`, Hash `reduce`/`inject`/`find_all` over
`[k, v]` pairs, `Hash#default`/`default=`, …); blocks/`yield`/closures with lexical
capture, `&block` params + `block_given?`/`__method__`, block-parameter
destructuring (`|(a, b), i|`, nested `|(a, (b, c))|`, `|(a, *rest)|`, and the
`->((a, b)) { }` lambda form), lambdas (`->(x) { }`,
`.call`/`.()`/`[]`), keyword args + `**opts`; block-pass-by-value (`&proc`,
`&method(:m)`, and `&nil` = no block, so a forwarded block keeps `block_given?`
faithful) and Ruby 3 argument forwarding (`def m(...)` / `m(...)`, including a
leading positional before `...`); `Integer#step`, `?c` char literals,
`String#center`/`tr`/`lines`/`delete`/`count`/`to_i(base)`/`encoding`, `Integer#to_s(base)`,
`Array#dig`/`first(n)`/`last(n)`/`min(n)`/`max(n)`/`each_cons`/`sum { }`/
`sample`/`shuffle`/`shuffle!`/`repeated_combination`/`repeated_permutation`,
`Hash#dig`/`assoc`/`rassoc`, `String#upto`; `eql?` on every value (`==` plus
MRI's numeric class-strictness at every depth, so `1.eql?(1.0)` and
`[1].eql?([1.0])` are both false), and the set-like operations that Ruby defines
in terms of `hash`/`eql?` rather than `==` — `uniq`, `|`, `&`, `-`,
`intersect?` and every `Set` predicate — agreeing with it, so `[1, 1.0].uniq`
keeps both.

- **`Set#<=>` is the subset relation, not an ordering.** `0` for equal sets,
  `-1`/`1` for a proper subset/superset, and `nil` when neither set contains the
  other. Unlike the algebra methods it does NOT accept an Array operand:
  anything that is not a Set answers nil. Both halves used to be wrong in
  opposite directions — a Set operand fell through to `Array#<=>`, whose own
  guard rejects a Set, so every pair including equal ones answered nil, while an
  Array operand passed that guard and got an ordered element-wise answer. The
  subset PREDICATES (`subset?`/`<=`/`superset?`/`>=`/`proper_*`) were correct
  throughout, which is how it went unnoticed: they can all agree with MRI while
  `<=>` disagrees. Still absent from Set, in two different ways —
  the distinction matters, because one is silent and one is not.

  Reached through the Array delegate, which builds a COPY, so the call succeeds
  and mutates the temporary, leaving the Set unchanged: `map!`, `collect!`,
  `select!`/`filter!`, `reject!`, `delete_if`, `keep_if`, `flatten!` and
  `replace`. These are the dangerous ones — no error, wrong result.

  Not reached at all, raising `NoMethodError: undefined method '<name>' for an
  instance of Set`: `subtract`, `classify`, `divide`, `reset` and
  `compare_by_identity`. These fail loudly.
- **Ordering operators derive from `<=>`, so an unrankable pair raises.** Ruby
  gets `< <= > >=` from `Comparable`: when `<=>` answers nil the operator raises
  `ArgumentError`, it does not answer false. This is ASYMMETRIC and the asymmetry
  is load-bearing — `Rational(1, 2) < Float::NAN` raises, while
  `Float::NAN < Rational(1, 2)` is `Float#<`, plain IEEE, and answers false. So
  the rule is about the RECEIVER, never about "an operand is NaN". The message
  is MRI's `rb_cmperr`, which names the operand by `inspect` when it is a Float
  or a special constant (`NaN`, `nil`, `:sym`, a small Integer) and by its CLASS
  otherwise (`String`, `Array`, `Object`) — so a NaN reads `NaN`, not `Float`.
  The same rule reaches a user `Comparable` class through the native operator
  path, where reading a nil `<=>` as 0 previously made `<` answer false and, more
  dangerously, made `==` answer TRUE for two values that cannot be compared.
  `==`/`!=` are the exception that does not raise: an unrankable pair is simply
  not equal.
- **One receiver phrasing for `NoMethodError`.** MRI names the receiver exactly
  one way: `an instance of C`, the bare literal for `nil`/`true`/`false`, or
  `class C`/`module M` for a class or module reference. `Host::receiver_phrase`
  is the only renderer, and every site routes through it — method dispatch, the
  numeric hook's `-@` arm and its binary-operator fallthrough, the user-method
  lookup, and `Proc#curry`. Those four used to build the sentence by hand and
  print the bare class name, which is a phrasing MRI uses for nothing
  (`-[]` said `for Array`, `{} + 1` said `for Hash`, `-nil` said `for
  NilClass`); they were fixed together, because fixing one of four only makes
  the wording inconsistent.
  A fifth site was found in the round-6 audit and joined them: the terminal
  `Kernel` arm dropped the clause entirely, so a top-level `no_such(1)` said
  `undefined method 'no_such'` where MRI says `undefined method 'no_such' for
  main`.
- **`GC`, `ObjectSpace`, `Random` and `SecureRandom` now use MRI's wording.**
  These four build their own message rather than reaching `receiver_phrase`
  (the receiver is not a class-reference value), and each had the phrasing MRI
  used BEFORE it changed to `for module X` / `for class X` — `GC.zzz` said `for
  GC:Module`. A version gate cannot see a string frozen in the implementation
  source, so nothing flagged it. Corrected against ruby 4.0.6.
- **Divergence — `ENV` still names itself.** `ENV.zzz` says `for ENV`; MRI
  answers `for #<Object:0x…>`, an address. Left alone deliberately: the MRI
  form embeds a heap address, which is not reproducible and not comparable.

## Language

- **`extend` / `prepend` / `class << self`.** `extend M` in a class/module body
  mixes `M`'s instance methods in as class methods; `prepend M` inserts `M`
  ahead of the class in the ancestor chain (so its methods override and `super`
  reaches the class); `class << self … end` defs register as class methods, the
  same as `def self.x`. `super` resolves through the receiver's linearized
  `Module#ancestors` order, so module-`super` (from a prepended or included
  module) reaches the next method correctly. `super` from inside a class method
  (`def self.m`) resolves through the singleton-class chain — the superclass's
  `def self.m`, or a module's method contributed via `extend` — so it reaches the
  next class method in ancestry order. Runtime instance `obj.extend(M, …)` mixes
  each module's instance methods (following `M`'s own `include` chain, plus
  `define_method` blocks) into the object's singleton table.
- **Class-level instance variables** (`@n` inside a `def self.m` / an `extend`ed
  method) persist across calls: the class object has its own variable store, so
  `@n ||= 0; @n += 1` counts up (`1`, `2`, `3`) as in MRI.
- **Class-body statements** run at definition time with `self` bound to the
  class, so `def`, `attr_*`, `include`, class variables (`@@x = 0`), constants,
  and other executable statements all take effect. Constants are namespaced
  under their enclosing module/class (see "Namespaces" below), not global.
- **Modifier `rescue` inside call-args / array literals.** Numeric-literal
  binding (`-7.abs` → `(-7).abs`, with `-2**2` → `-(2**2)`) and modifier
  `rescue` precedence (`x = a rescue b` → `x = (a rescue b)`, plus grouping
  parens and statement-level) both match MRI now. A bare modifier `rescue` directly in
  method-call arguments (`p(1/0 rescue 5)`) or array elements
  (`[1/0 rescue 5]`) is a syntax error here as it is in MRI, so the parser is no
  longer a permissive superset on this point.
- **Splat-only / anonymous params.** A bare `*` splat, a `*name` splat, and a
  bare `**` keyword-splat parse as the sole (or any) parameter of a method,
  block, or lambda: `def f(*)`, `def f(**)`, `def f(*, **)`, `->(*) { }`,
  `->(*a) { a }`, `proc { |*| }`, `proc { |*x| x }`. A splat block/lambda's
  `arity` is `-(required + 1)`, negative like MRI.
- **Parallel assignment with a leading splat.** `*x, y = 1, 2, 3` and `*x = 1, 2`
  (splat as the first target) parse now, alongside the already-supported trailing
  (`a, *b =`) and middle (`a, *b, c =`) splat positions.
- **Lambda literal as a command argument.** `p ->(x) { x }` (a `->` lambda
  directly after a spaced command name) parses. So does `yield` in the same
  position (`p yield`, `puts yield`, `forwards yield`) — it is an expression and
  never a modifier or a binary operator, so it is unambiguous there; left out of
  the argument-start set the argument was silently dropped and the call printed
  nothing at all. Still not parsed: nested destructuring targets in a parallel
  assignment LHS (`(a, (b, c)), d = …`).
- **Numbered / `it` implicit block params.** Implemented. A block that declares
  no `|params|` records the highest `_1`.`_9` it mentions and synthesizes exactly
  that many required params when it closes; a bare `it` (Ruby 3.4) synthesizes
  one, bound under the reserved name `__it__`. `it` resolves to that parameter
  only after the ordinary local chain misses, which is MRI's precedence — so
  `[1, 2].map { it }` is `[1, 2]` while `it = 5; [1, 2].map { it }` is `[5, 5]`,
  and a *method* named `it` never wins over the implicit parameter. Not rejected
  yet: MRI's SyntaxErrors for mixing `it` with ordinary params or with `_1`.
- **Loop control flow through `begin`/`rescue`.** A `begin` body compiles to its
  own chunk, so a `break`/`next` inside one cannot jump to the enclosing native
  `while`/`until`/`for` label directly; it raises a signal that the loop picks
  back up immediately after the nested run (`BEGIN_IN_LOOP` plus the
  `TAKE_LOOP_NEXT`/`PEEK_LOOP_BREAK`/`TAKE_LOOP_BREAK` handoff). `return`,
  `throw` and `retry` still unwind past the loop untouched, and a `break` from a
  *block* keeps belonging to the method it was passed to. `while`/`until` also
  evaluate to their `break` operand (`(while true do break 7 end) == 7`), not
  always nil.
- **`break` out of a block — who owns it.** MRI targets a `break` at the
  invocation the block LITERAL was written on: that invocation ends immediately
  and the break value becomes its value. There is exactly one place that ends the
  signal. `MKPROC` (emitted only for a block literal, always as the last operand
  before its call op) marks the call as the owner, and the block-carrying call
  handlers consume a pending `Break` there. A forwarded `&expr` block never sets
  the marker, so an inner forwarding call cannot steal an outer block's break —
  `def m(&b); r = [1, 2].each(&b); p :after; r; end; m { break 7 }` answers `7`
  and never prints `:after`, as in MRI. The signal also survives a user-defined
  method boundary (`def y; yield; :after; end; y { break 3 }` is `3`), so it can
  reach that owner. Consequently the iterating builtins do NOT consume `break`
  themselves; they only stop iterating and leave the signal pending. Before this,
  each block-taking builtin had to remember to handle `break` and about fifty did
  not — they returned their receiver, or let the signal unwind far enough to
  discard the enclosing statement's output entirely (`p([1, 2, 3].find { break 99 })`
  printed nothing). A block a LAZY pipeline stored is the case with no owner
  left: `[1, 2].lazy.map { break 7 }` runs the block only once `to_a`/`first`
  pulls it, long after the `.map` it was written on returned, so MRI raises
  `LocalJumpError: break from proc-closure` there instead of unwinding — as
  rubylang now does, where the signal used to escape and swallow the statement.
- **Block scoping.** A block parameter is block-local and gets a fresh binding
  per iteration, so `3.times { |i| procs << -> { i } }` closes over `0`,`1`,`2`;
  a local first assigned inside a block does not leak out (`1.times { x = 1 };
  defined?(x)` is nil); and explicit block-locals (`{ |i; tmp| … }`) shadow an
  outer name of the same spelling. `for` is the deliberate exception — it
  introduces no scope, so its loop variable AND every local its body assigns are
  enclosing locals that outlive the loop and are shared by every closure the body
  makes (`[2, 2, 2]`, where the `each` form gives `[0, 1, 2]`). The body walk
  that pre-declares those locals follows control flow (`if`/`while`/`case`/
  `case-in` clause bindings/`begin` + its `rescue => e` binding/a nested `for`)
  and stops at a real new scope (a block or lambda body, a `def`, a `class`/
  `module` body), so `for i in 1..3; sq = i * i; end` leaves `sq` behind while
  `[1].each { q = 1 }` does not leave `q`. `for k, v in pairs` destructures each
  element into enclosing locals the same way.
- **Method arity is enforced.** A call with the wrong number of positional
  arguments, an unknown keyword, or a missing required keyword raises
  `ArgumentError` before the body runs, with MRI's exact message —
  `wrong number of arguments (given 1, expected 2)`, `expected 1..2` for a
  signature with optional params, `expected 1+` for one with a splat, plus the
  `; required keyword: x` suffix MRI appends when the signature has required
  keywords, and `unknown keyword(s): :z` / `missing keyword(s): :x`. (Previously
  a short call silently left the parameter unbound, so the body ran with a stray
  nil.)
- **Lambda arity is enforced too.** A block template now carries the parameter
  shape as written (`req`/`opt`/keyword names/required keywords/`**rest`/`&blk`),
  so a lambda — `->`, `lambda { }`, a `define_method` body, `Method#to_proc` —
  runs the same pre-body check a `def` does and raises the identical
  `ArgumentError`. A lambda also does NOT auto-splat a single array argument, so
  `[[1, 2]].map(&->(x, y) { x + y })` raises where the block form binds both. A
  plain `proc` stays lenient in both respects, which is MRI's rule, and
  `Proc#lambda?` is the discriminator. `Proc#arity`/`Method#arity` are computed
  from the same shape via MRI's `rb_iseq_min_max_arity` formula, so an optional
  positional, a splat, a `**rest` or an optional keyword each make it negative
  for a lambda (`->(x, y = 1) {}.arity == -2`) while a proc goes negative only
  for a splat. `->(&b) { }` binds the block passed to `Proc#call`, and
  `->(x; t) { }` declares block-locals like `{ |x; t| }`.

- **`redo`.** Implemented. Inside a native `while`/`until` (and the post-test
  `begin … end while` form) it compiles to a backward jump to the body start, so
  the loop condition is not re-tested. Anywhere else — a block body, or a `begin`
  nested in either — it raises a Redo signal: `call_proc` re-runs the block body
  on it (with the same scope, so the params keep their values and a local the
  body already assigned survives the re-run, as in MRI), and a loop picks it back
  up through the `TAKE_LOOP_REDO` handoff alongside break/next. `redo` outside
  both a loop and a block is rejected at compile time with
  `Invalid redo (SyntaxError)`, matching MRI's parse-time rejection rather than
  leaving a signal nothing consumes.

## Metaprogramming / reflection / eval

Implemented and verified against the reference `ruby`:

- **Singleton methods.** `def obj.m` and `def Klass.m` parse and run: an object
  receiver stores a per-object singleton method; a class receiver registers a
  class method (identical to `def self.m`). `class << obj … end` on an instance
  works the same way. Singleton methods take priority over the class's own
  instance methods in dispatch (matching `Module#ancestors` order). A bare
  self-call inside a singleton method (or inside a block whose `self` is a
  receiver carrying singletons) resolves those singletons. `Object#singleton_class`
  itself is NOT implemented — the singleton is a per-object method table, not a
  reified anonymous `Class`, so `obj.singleton_class` raises `undefined method`
  and `obj.singleton_class.instance_methods(false)` has no equivalent. Use
  `obj.singleton_methods`, which is implemented.
- **`Method#arity` / `#owner` / `#parameters` on builtins.** A builtin has no
  declared shape to read — the dispatch functions match on an `args` slice — so
  the three answers come out of `src/arity_table.rs`, a table of what the
  reference interpreter declares for every method on the core classes,
  regenerated by `ruby tools/gen_arity_table.rb`. The arity encoding is MRI's:
  the count when every parameter is required (`3.method(:*).arity` → `1`), and
  `-(required + 1)` as soon as anything is optional or variadic
  (`3.method(:round).arity` → `-1`, `[].method(:pack).arity` → `-2`). `#owner`
  resolves through the receiver's ancestor chain, so it names the DEFINING module
  (`3.method(:between?).owner` → `Comparable`, `3.method(:puts).owner` →
  `Kernel`, `[].method(:each_slice).owner` → `Enumerable`), and a class receiver
  resolves against the singleton chain (`Integer.method(:sqrt).owner` →
  `#<Class:Integer>`, `Integer.method(:name).owner` → `Module`). Measured over
  2248 `(receiver, method)` pairs across 21 receivers: 1395 arities, 2248 owners
  and 1543 parameter lists disagreed with ruby 4.0.6 before the table; none do
  after.
- **`method` / `instance_method` on an undefined name.** Both raise `NameError`
  (`undefined method 'x' for class 'C'`, `for module 'M'` on a module) rather
  than handing back a `Method`
  object that fails only when called. The table alone cannot decide this — it
  lists what MRI defines, not what rubylang implements — so the check is the union
  of every place a definition can live: written `def`s, `define_method` bodies,
  per-object singletons, `alias`es whose target is a built-in, runtime `attr_*`
  accessors, `Struct`/`Data` members and their generated class methods, top-level
  `def`s (private on `Object`, so a class receiver sees them), and the table.
  A class that answers `respond_to_missing?` owns the name even though nothing
  defines it; a `method_missing` without one does not, matching MRI. A bound
  lookup on a class names a CLASS method, so the instance side does not answer it
  (`String.method(:upcase)` raises, `String.instance_method(:upcase)` does not).
  Measured over 38,864 `(receiver, name)` pairs across 28 receivers and a further
  49,490 across 35 user-defined receiver shapes (every definition form above):
  33,960 names wrongly answered a `Method` before; 24 and 199 remain, and no name
  MRI answers is refused except the six on `main` (below).
- **`UnboundMethod`.** Its own class, not `Method`: it answers `bind`/`bind_call`
  and raises `NoMethodError` for `#receiver`, while `#name`/`#arity`/`#owner`/
  `#parameters` describe the class it was looked up on. `#inspect` is
  `#<UnboundMethod: Owner#name>` — MRI also appends the written parameter list and
  the definition's source location, neither of which rubylang retains (the same
  reason `Exception#backtrace` is empty).
  Still open:
  - `Object#public_method` and `Object#singleton_method` are not implemented at
    all, so they raise `undefined method` rather than the `NameError` (or the
    `Method`) MRI answers. `method` and `Module#instance_method` /
    `#public_instance_method` are the implemented spellings.
  - **`Object#singleton_class` — on a plain object only.** A CLASS or module
    answers its metaclass, and that chain is exact: it interleaves each
    `#<Class:X>` with the modules X was `extend`ed with, walks the superclass
    chain of metaclasses, and closes with `Class, Module, Object, Kernel,
    BasicObject`, so `C.singleton_class.include?(M)` and `.ancestors` both agree
    with MRI. A non-class receiver still raises `undefined method`. Modelling it
    needs a per-object metaclass IDENTITY — MRI names one
    `#<Class:#<Foo:0x000000010488>>`, so two objects of the same class have
    DIFFERENT singleton classes, and a name built without the address would
    collide them into one. `obj.extend(M)` and `def obj.m` themselves work: both
    register in the object's singleton table, which is what dispatch reads.
  - `main`'s singleton methods (`include`, `private`, `public`, `define_method`,
    `using`, `ruby2_keywords`) are not modeled, so `self.method(:include)` at the
    top level raises. The object itself now exists and names itself: top-level
    `self` is heap slot 0, an ordinary `Object` (so `self.class` is `Object`)
    whose `to_s`/`inspect` answer `"main"` and which a `NoMethodError` calls
    `main` rather than `an instance of Object` — it is only the singleton method
    set that is still missing.
  - MRI `undef`s the `Comparable`/`Numeric` methods `Complex` inherits (`<`,
    `clamp`, `between?`, `round`, …); the generated table records no
    "undefined here" column, so the ancestor's row is still found — 19 of the 24
    residual cases above.
  - `NameError#receiver` is nil for these; MRI answers the class.
- **`define_method`.** `define_method(:m) { … }` in a class body and the explicit
  receiver form `Klass.define_method(:m) { … }` both register an instance method
  whose body is the block. When invoked, the block rebinds `self` to the calling
  instance: `@ivar` reads/writes hit that instance and bare-name calls dispatch on
  it, while the block's closed-over locals stay visible. `obj.define_singleton_method`
  is the per-object analogue.
- **`const_missing`.** `Mod::Const` for an unresolved constant calls
  `Mod.const_missing(:Const)` when the class/module defines it (the hook Rails
  autoloading relies on).
- **Definition hooks.** `inherited(subclass)` fires when a subclass is opened;
  `included`/`extended`/`prepended(base)` fire when the corresponding mixin
  relationship is established. Each fires only if the module/class defines the
  hook as a class method.
- **`singleton_methods`.** Lists the names defined on the object alone
  (`def obj.m`, `class << obj`, `define_singleton_method`); on a class or module
  it lists that class's own class methods, since a class's singleton methods ARE
  its class methods. Returned sorted.
- **Constant reflection.** `const_get` / `const_set` / `const_defined?` /
  `constants` on any class or module ref, including builtin class names
  (`Object.const_get(:String)`). `const_get`/`const_set`/`const_defined?`
  resolve a name relative to the receiver's namespace (`A.const_get("B")` →
  `A::B`) and accept a qualified string (`Object.const_get("A::B")`).
- **Namespaces (nested modules / classes).** `module A; module B; … end; end`,
  the compact forms `class A::B::C` / `module A::B`, nested `class`es inside a
  class body, a namespaced superclass (`class D < Foo::Base`), and
  `include`/`prepend`/`extend Namespaced::Mod` all resolve and mix in. A
  constant is stored under its fully-qualified name (`A::B::X`), and a class's
  `name`/`inspect` reports that path. Constant lookup follows Ruby's rule: a
  qualified path (`A::B::X`) resolves through each namespace, and a bare `Const`
  inside a namespace body walks the lexical nesting (innermost first) then the
  top level. The nesting is captured at compile time (each class/module body
  pushes its qualified name), so methods and constants resolve against their
  definition-site nesting. Approximations, all lenient supersets of MRI: the
  compact form does not require the intermediate parent to pre-exist (MRI raises
  `uninitialized constant` when it is missing); the lexical chain for a bare
  read is derived by stripping segments of the innermost qualified name rather
  than tracking a separate `[A::B, A]` nesting list (identical for the common
  `module A; module B` shape); and `Module.nesting` is best-effort — it returns
  `[]` (correct at the top level) because the runtime does not carry the
  lexical nesting of the call site.
- **Class/module reopening merges.** A second `class A … end` (or `module M … end`)
  adds to the existing definition instead of replacing it: new instance methods,
  class methods, and constants are merged in, a redefined name replaces the
  earlier body, and `include`/`prepend`/`extend` mixins accumulate. Each opening's
  class body runs (they are stored under distinct synthetic `__class_body__N`
  names, so side effects from every reopening fire). Caveat: rubylang installs all
  class/method definitions at load time (methods and classes are usable before
  their textual position — a pre-existing hoisting deviation from MRI), so when a
  later reopening *redefines* a method the last definition wins for the whole run
  rather than only after its textual point. Additive reopenings (distinct method
  names) match MRI exactly.
- **Top-level `self`.** Fixed to `main`, an ordinary `Object`, so
  `self.class.name == "Object"` (was `"NilClass"`). Top-level instance variables
  now live on that object, and it prints as `main` (see the `Method` section).
- **Divergence — a `class`/`module` definition evaluates to `nil`.** In MRI the
  body is an expression and the definition answers its last statement, so
  `p(class Foo; 42; end)` prints `42`; rubylang prints `nil`. It matters mainly
  for the one-liner idiom of reading a directive's result straight off the body
  (`x = class C; private :m; end`); assigning inside the body and reading after
  is the working spelling.
- **`eval` / `class_eval` / `instance_eval` / `instance_exec`.** `eval("code")`
  compiles and runs the string on the current host (methods/classes/constants it
  defines persist), returning the last value. `Module#class_eval`/`module_eval`
  (block or string) runs with `self` = the class, so a bare `def` defines an
  instance method. `Object#instance_eval`/`instance_exec` (block or string) runs
  with `self` = the receiver — a full rebind, not just for ivar writes: a
  bare-name call inside the block dispatches on the receiver (reaching its
  instance and singleton methods), `@ivar` reads and writes hit the receiver,
  `self` is the receiver, and a bare `def` defines a singleton on it (a class
  method when the receiver is a class). `instance_exec` forwards its arguments to
  the block; the closed-over locals of the block stay visible.

Honest limitations of this surface:

- **Singleton storage is keyed by heap id.** Per-object singleton methods live in
  a `heap-id → name → MethodDef` map. This is stable for the object's lifetime,
  but object identity is the heap slot, so it does not survive `dup`/`clone`
  (a shallow copy gets a new id and none of the original's singletons) — matching
  MRI, which also does not copy the singleton class on `dup`.
- **`eval` binds to the current scope only.** The top-level / current-`self`
  binding is supported; an explicit `Binding` argument (`eval(str, some_binding)`)
  is not modeled. String `class_eval`/`instance_eval` rebind `self` but share the
  caller's local scope.
- **A bare `def` is still hoisted globally in addition to registering on an eval
  target.** Because top-level/`in-block` `def`s are hoisted into the method table
  at compile time, a `def` inside `class_eval`/`instance_eval` also leaves a
  same-named top-level method behind (harmless pollution; the eval-target
  registration is what dispatch uses). A `def` inside an *ordinary* method body
  called from within an eval is correctly isolated (hoists, does not hit the eval
  target).
- **Hook firing order approximates MRI.** Hooks fire from the class-definition
  site in source order (`inherited` before the body, `included`/`extended`/
  `prepended` after), which is correct for observing *which* class triggered the
  hook; exact interleaving with surrounding output can differ because class
  bodies are otherwise hoisted.
- **`respond_to?` on builtin receivers is permissive.** A user object reports
  accurately: it consults the receiver's class method table (own methods,
  inherited methods, included/prepended-module methods, `define_method` blocks,
  `alias_method` aliases, and per-object singleton methods), then
  `respond_to_missing?` (an override can call `super` for the default `false`, the
  same as `Object#respond_to_missing?`), then Struct/OpenStruct attributes. For builtin receivers
  (`String`, `Integer`, `Array`, `Hash`, `Symbol`, …) `respond_to?` returns
  `true` for any name except the pattern-match deconstruction protocol, because
  there is no enumerable registry of the builtin method surface — so
  `"s".respond_to?(:no_such)` is `true` where MRI is `false`. Accurate builtin
  `respond_to?` needs a per-type method-name registry (deep substrate, not yet
  built). A *class or module* receiver is the exception and reports accurately:
  its registered class methods, singleton methods and aliases, the `Class`/
  `Module` reflection surface, then `respond_to_missing?` on its singleton class.
  `Object`/`BasicObject` report an unknown name as absent (Rails walks
  `ancestors` calling `respond_to?(:initializers)` on each), and `Kernel` answers
  for exactly the module functions it dispatches — `Kernel.respond_to?(:puts)` is
  `true`, `Kernel.respond_to?(:initializers)` is `false`, as in MRI.
- **Refinements (`refine` / `using`) are not implemented.** `refine Klass do … end`
  raises `undefined method 'refine'`. Scoped, lexically-activated monkey-patching
  needs a whole activation-scope substrate (per-lexical-scope method-table
  overlays) that does not exist yet; global reopening (`class String; … end`)
  covers the common patch case in the meantime.

## Lexer

- **Not lexed:** non-ASCII identifiers (`é = 1`, `def ünf`, `:é`). The scanner is
  byte-based and starts an identifier only on an ASCII letter or `_`, so a
  UTF-8 name is rejected with `unexpected character`. Everything else below **is**
  lexed. Heredocs (`<<END`, `<<~SQL`, `<<-EOT`, `<<'RAW'`),
  `%w[]` / `%i[]` word/symbol arrays (and the `()`/`{}`/`<>` delimiter variants,
  plus the interpolating `%W[]` / `%I[]` forms),
  double-quoted `#{}` interpolation, `?c` character literals, regex literals
  (`/pat/flags`, with `i`/`m`/`x` flags), radix integer literals
  (`0b1010` binary, `0o17`/`017` octal, `0xff` hex, `0d99` decimal, with `_`
  separators), and the `%q`/`%Q`/`%r`/`%s` percent literals (single/double-
  quoted string, Regexp, Symbol — any punctuation delimiter, with `()`/`{}`/
  `[]`/`<>` nesting) **are** lexed. Double-quoted string escapes cover
  `\a\b\t\n\v\f\r\e\s\0\\\"\#`, `\xHH` (hex byte), and `\uHHHH`/`\u{H…}`
  (Unicode). `String#inspect` renders these Ruby-faithfully — named escapes,
  `\uXXXX` (uppercase) for other control chars, and `\#` before `{`/`@`/`$`.
  MRI's sigil-shorthand interpolation is supported everywhere `#{}` is (strings,
  heredocs, `:"…"` symbols, regex literals, `%W[]`/`%I[]`) — `#@ivar`,
  `#@@cvar`, `#$gvar`, including the punctuation and numbered globals (`#$!`,
  `` #$` ``, `#$'`, `#$&`, `#$1`) — and a sigil not followed by a variable name
  stays a literal `#`, as in MRI (`"#$ x"`, `"#@ x"`, `"#$-0"`).
  `Symbol#inspect` quotes any
  name that would not round-trip bare (`:"weird sym"`, `:""`, `:"1a"`), and a
  Hash symbol key that needs quoting prints as `{"a b": 1}`. Symbol literals
  cover the sigil forms (`:@x`, `:@@x`, `:$x`) and every operator name
  (including `` :` ``). The
  bare `%(…)` / `%{…}` / `%[…]` / `%<…>` string form is lexed (double-quoted,
  like `%Q`); it reads as a string at an expression start or after a spaced bare
  method name (`p %(x)`), and as the modulo operator after a value (`10 %(3)`,
  `a % b`). A bare local variable that MRI would treat as modulo (`foo %(3)`
  where `foo` is a local) is read as a string command arg here, since this lexer
  has no local-variable table. `__END__` alone on a line stops the program (the
  trailing DATA section is out of scope). Not yet: an unknown escape like `"\d"`
  keeps its backslash rather than dropping it as MRI does (deliberate, for
  regex-source strings).
- **`<<` heredoc vs left-shift.** A `<<` glued to the right of a value with no
  preceding space (`s<<"b"`, `arr<<CONST`) is the shift/append operator, not a
  heredoc — the quoted (`<<"X"`) and bare-uppercase (`<<END`) heredoc forms are
  recognized only at an expression start or as a command argument (`puts <<"EOF"`:
  space before, none after). `<<~`/`<<-` are always heredocs.
- **`key:value` label vs symbol.** A `:` glued to the right of a value where the
  value that follows is a keyword or constant (`x:true`, `k:String`,
  `keyword_init:true`) is a label colon, not a symbol start. Expression-start and
  spaced-command-arg symbols (`:foo`, `p :bar`, `[:a, :b]`) are unaffected.

## Runtime / methods

- **Argument counts on built-ins are checked once, from a measured table.** Every
  built-in dispatch arm reads its arguments positionally (`args[0]`), so a call
  with too few of them panicked the interpreter (`index out of bounds`) where MRI
  raises a rescuable `ArgumentError`. `builtins::check_builtin_arity` now rejects
  such a call at the single point where dispatch has decided the call belongs to a
  built-in, so the whole surface is covered at once rather than arm by arm. The
  accepted range comes from `arity_table::BUILTIN_ARG_SHAPES`: a `(min, max)` per
  built-in for each call shape (with and without a block), the `expected …` wording
  MRI prints, and whether the method takes keyword arguments.
  A sweep of 48,816 cases — 904 method names from the table × 18 receivers × 3
  argument modes (none / too many / wrong type), restarting the interpreter after
  each crash — goes from **635 panics to 90**.
  The 90 that remain are not argument-count misses; every one is a different bug:
  44 are methods the table deliberately does not measure (`instance_exec` and
  `define_method` forward their arguments elsewhere; `print`/`printf`/`throw` are
  unsafe to probe), 41 are calls MRI answers with `NoMethodError` — rubylang
  dispatches an Array or String arm on a receiver that has no such method
  (`Set.new([1]).fill`, `[1, 2].each.pack`), so no table row resolves and nothing
  is checked — and 5 are type errors rather than count errors (`"abc".gsub(obj)`
  passes the 1..2 count and then panics on a pattern that is neither Regexp nor
  String; `1.step` is a legal zero-argument Enumerator call MRI accepts and the arm
  does not implement).
- **The built-in shape table is measured, not derived.** `Method#arity` cannot
  express what a C function accepts — `String#center` reports -1 and takes 1..2 —
  so `tools/gen_arity_table.rb` CALLS each method with a deliberately wrong number
  of arguments and reads the range out of the `ArgumentError` MRI raises. Three
  properties of that measurement are worth stating because each one was a wrong
  answer first:
  - **MRI's `expected …` clause is a message, not a range.** `(1..3).min(*12)`
    prints `expected 1` and yet `(1..3).min` is valid, so a table that reads the
    minimum off the clause rejects a correct call. Both ends are confirmed by
    probing — downward from the stated minimum until a count is really refused,
    and one past the stated maximum to check a maximum exists at all — and the
    clause is carried verbatim only because it is what the raised message has to
    say.
  - **A forwarding method has no shape of its own.** `Class#new` hands its
    arguments to the receiver's `initialize`, so probing it on `String` measures
    `String#initialize` and yields a row saying `new` takes 0..1 — which then
    rejects `Range.new(1, 3)` and every other multi-argument constructor. Those
    methods are excluded by name (`UNMEASURABLE`) rather than measured; an
    unmeasured row is one nothing checks, which is the right answer for a method
    whose shape belongs to its target.
  - **Keyword acceptance is measured too.** `Data#with` declares `[[:rest]]` and
    accepts nothing but keywords, so a column read off `Method#parameters` rejects
    `point.with(x: 9)` — rubylang's parser desugars keyword arguments into one
    trailing Hash positional, which then overflows a maximum of 0. The generator
    asks MRI directly, with an unknown keyword: a method that takes keywords
    complains about the keyword, one that does not counts the Hash as one more
    positional and complains about the count.
- **Blocks and procs.** `Proc#parameters` is implemented, including the
  `lambda:` keyword and the rule that a NON-lambda proc reports its required
  positionals as `:opt`; a destructuring parameter (`->(a, (b, c)) { }`) is a
  one-element entry, since it has no written name. A destructuring parameter in
  a `def` list (`def m(a, (b, c))`) still does not parse — the block and lambda
  forms do. An anonymous keyword collector in a block parameter list
  (`{ |**| }`) does not parse; the named form (`{ |**rest| }`) does.
- **Named captures suppress the unnamed ones.** A pattern holding ANY named
  group stops numbering its plain `(…)` groups: `/(?<a>b)(c)/.match("bc")` has
  size 2, `to_a` `["bc", "b"]`, and `$2`/`m[2]` nil. This is Onigmo's
  `ONIG_SYN_CAPTURE_ONLY_NAMED_GROUP` — `regcomp.c` splices the unnamed node out
  of the AST and sets `num_mem = num_named`, so the group COUNT shrinks rather
  than the slot going nil — and Ruby documents it as "When a regexp contains a
  named capture, there are no unnamed captures". There is no way to switch it
  off: `ONIG_OPTION_CAPTURE_GROUP` exists in the C API but `re.c` never sets it.
  It is implemented by rewriting the source `(` to `(?:` before compiling, so
  `fancy-regex`'s own numbering matches Onigmo's and `to_a`/`captures`/`size`/
  `[]`/`values_at`/`scan`/`split`/`$1`..`$9`/`$+` all follow from one place. The
  same rule applies to a REPLACEMENT string: `\1`..`\9` expand to nothing when
  the pattern has a named capture, even where the number would have resolved.
- **Regexp.** Supported: `/pat/flags` literals, `=~`/`!~`, `String#match`
  (returns `MatchData` with `[n]`/`pre_match`/`post_match`/`to_a`/`captures`/
  `values_at`/`regexp`/`begin`/`end`/`offset` and the byte-addressed
  `bytebegin`/`byteend`/`byteoffset` — the character forms and the byte forms
  part company on any multi-byte subject), `match?`, `scan`, `split(re)`,
  `sub`/`gsub` with a Regexp (a block form, and a replacement string with the
  full escape set `\0`, `\1`..`\9`, `\&`, `` \` ``, `\'`, `\+`, `\\` and
  `\k<name>`; an unrecognized escape keeps its backslash, and `\k<name>` naming
  no group is an `IndexError`), and `Regexp#{source,match,scan,match?}`
  plus `case`/`when /re/` case-equality. A successful match sets the globals
  `$~` (MatchData), `$&` (whole match), `` $` ``/`$'` (pre/post text), `$+`
  (last group), and `$1`..`$9` (numbered groups) — visible after `=~`/`match`
  and inside a `sub`/`gsub` block. (`` $` `` reads correctly inside a *brace*
  `#{...}` interpolation; `$'` still cannot, because the interp scanner reads
  its quote as a string delimiter and the read fails with
  `unterminated string`. The sigil shorthand works for both:
  `` "pre=#$` post=#$'" ``.) Backed by `fancy-regex`, a backtracking engine, so
  the Onigmo constructs the `regex` crate rejects DO work: backreferences within
  the pattern (`/(ab)\1/`) and lookaround (`/foo(?=bar)/`, `/(?<=foo)bar/`).
  Ruby's flag spellings differ from fancy-regex's Perl ones and are translated
  before compiling: `^`/`$` are LINE anchors unconditionally (`\A`/`\z` are the
  string anchors), and an inline `(?m)`/`(?im:…)`/`(?-mix:…)` keeps Ruby's
  meaning of `m` — dot-matches-newline — rather than Perl's line-anchor switch.
  Matching also follows MRI's `rb_reg_search` stepping, where a zero-width match
  landing on the previous match's end is a match rather than being skipped, so
  `"aaa".gsub(/a*/, "X")` is `"XX"` and `"abc".split(//)` is one field per
  character.
  A Regexp is also a VALUE, not just a matcher: `==`/`eql?`/`hash` compare its
  source and its normalized option bitmask, so `/a/ == /a/` and
  `/a/im == /a/mi` are true while `/a/ == /a/i` is false, and a Regexp works as
  a Hash key and collapses under `uniq`/`-`/`include?`/`Set`. (MRI also compares
  the encoding; there is no per-Regexp encoding modelled here.) Still missing on
  Regexp, though the arity table declares them: `encoding`, `fixed_encoding?`,
  `initialize_copy` and `~`. `Regexp.new(/a/i)` also drops the source regexp's
  flags, and its integer-options argument only decodes IGNORECASE, never
  EXTENDED or MULTILINE.
- **`Object#class` returns a Class object** (a class reference): `p obj.class`
  prints the bare name, `obj.class == SomeClass` and `Integer == Integer`
  compare by class identity, and `obj.class.name` / `.to_s` give the name.
  `Class#superclass` and `Module#ancestors` walk the class chain (builtin types
  use a fixed table; user classes follow their superclass + included modules),
  and the `<`/`<=`/`>`/`>=` class relations return `true`/`false`/`nil` like
  Ruby (`Integer < Numeric` → true, `String < Numeric` → nil). A Class object
  is usable as a Hash key or Set member (keyed by class name), so
  `group_by(&:class)` and counting-by-class work. `Class` itself sits under
  `Module` (`Class.superclass` → `Module`), matching MRI. A module IS
  distinguished from a class at runtime: `ClassDef` carries an `is_module` flag
  set by the opening keyword (and by `Module.new`), threaded through the
  bytecode cache, so `M.class` is `Module`, `M.is_a?(Class)` is `false`,
  `M.instance_of?(Module)` is `true`, `M.ancestors` is the module chain with no
  `Object`/`Kernel`/`BasicObject` tail (`module B; include A; end` → `[B, A]`),
  `M.superclass` raises `NoMethodError` instead of answering `Object`, and the
  error wording is MRI's `for module M` rather than `for class M`. Built-in
  modules (`Comparable`, `Enumerable`, `Kernel`, `Math`, …) come from the
  generated table's module list and answer the same way.
  A module's singleton class is created LAZILY, as in MRI: `module M; end`
  leaves `M` an ordinary instance of `Module`, so `M.method(:undefined)` reports
  the lookup class as `Module`, not `#<Class:M>`. It materialises on the first
  thing that has to live in it — a `def self.m`, a `def M.m`, an `extend`, a
  `define_singleton_method`, a `class << M` body, or `module_function` — after
  which the same miss names `#<Class:M>`. A CLASS is never lazy this way
  (`#<Class:C>` holds `new`/`allocate` from the start), and the unbound
  `M.instance_method(:undefined)` names `module 'M'` either way.
- **Class/module reflection.** `Module#instance_methods([inherited])`, its three
  visibility-specific siblings, the `#*method_defined?` predicates, and the
  instance-side `Object#methods` return method names as symbols (see "Method
  visibility" below for which name lands in which set).
  `instance_methods(false)` is the class's own methods (including
  `attr_accessor`/`attr_reader`/`attr_writer` accessors and `define_method`
  methods); `instance_methods` / `instance_methods(true)` add every user-defined
  ancestor (included modules and superclasses) via the ancestor chain. Builtin
  ancestors (`Object`/`Kernel`/`Comparable`/`Enumerable`) are NOT enumerated —
  the inherited set is bounded to the user-defined portion of the chain, so it
  omits MRI's builtin Kernel methods. The
  synthetic `__class_body__` (and any `__`-prefixed internal name) is excluded.
  The modifier-with-a-`def` forms (`private def m …`, and `public`/`protected`/
  `module_function` likewise) still DEFINE on the class: the class-body compiler
  recognises them as definitions, where deferring them to the runtime class body
  registered the `def` in the top-level method table instead — making a
  `private def helper` reachable as a bare `helper` from anywhere.
  `module_function def m` also promotes to a module method.
- **Method visibility.** Enforced, not just parsed. `ClassDef` carries a
  per-name `visibility` map (public is the unrecorded default) filled by the
  class-body compiler from a bare `private`/`protected`/`public` mode, from the
  `private def m` / `private :a, :b` forms, and from `module_function` (whose
  instance copy is private); the runtime `private :m` spelling writes the same
  map, so a directive reached through `class_eval` works too. It rides the
  bytecode cache.
  An explicit-receiver call (`obj.m`) is gated: a private method is callable
  only when the receiver IS the current `self` (Ruby 2.7's `self.priv`), a
  protected one when `self` is a kind of the class owning the entry. The miss
  raises MRI's `private method 'm' called for an instance of C`. Implicit-self
  calls and `send`/`__send__` bypass the check, as in MRI.
  The modifier applies to `attr_accessor`/`attr_reader`/`attr_writer` accessors
  and to a `define_method` in the same body, not only to a following `def`.
  `initialize`, `initialize_copy`/`_clone`/`_dup`, `respond_to_missing?` and
  `method_missing` are private however they were declared.
  Reflection follows: `instance_methods` is public + protected,
  `public_`/`private_`/`protected_instance_methods` select one visibility each,
  `method_defined?` excludes private while `public_`/`private_`/
  `protected_method_defined?` test for exactly one, and `respond_to?` answers
  false for a private method unless the second argument is true.
  `public_send` is NOT a synonym for `send`: it refuses a private or protected
  method, and it is stricter than an ordinary `obj.m` call — the self-receiver
  exemption does not apply, so `public_send(:priv)` on your own receiver raises
  too. A name nothing defines still reports `undefined method`, not `private`.
  **Class-method visibility.** `private_class_method` / `public_class_method`
  write a real store: `ClassDef` carries a `class_visibility` map separate from
  the instance one, because the two namespaces are independent in MRI (the
  instance entry lives on the class, the class entry on its singleton class), so
  `private :run` cannot hide `self.run`. Both the class-body and
  explicit-receiver spellings write it, including the `private_class_method def
  self.m …` expression form. Lookup walks the superclass chain — a private
  `new` on a base class stays private on every subclass, and the error names the
  RECEIVER class (`private method 'mk' called for class Sub`) — while a subclass
  that redefines the class method makes it public again. `respond_to?` hides it
  unless the second argument asks for the private surface, and `public_send`
  refuses it. The compiler never fills the map (`private_class_method` is an
  ordinary runtime call), so the rkyv cache shape is unchanged and a cache
  written before the field existed still loads.
  Still open, one shared limitation for both instance and class methods: MRI's
  self-receiver exemption is SYNTACTIC — only the literal keyword `self` counts
  — while rubylang tests object identity. So `z = self; z.priv` and
  `def self.go; C.priv_cm; end` are accepted where MRI raises. Distinguishing
  them needs the compiler to mark a literal-`self` receiver on the call op.
  Still open: the private surface of the BUILT-IN Kernel methods is not modelled
  (`Kernel#puts`/`print`/`require`/`raise`/… are private instance methods in
  MRI), so `5.public_send(:puts, "x")` reports `undefined method 'puts'` where
  MRI reports `private method 'puts'`. Only user-defined classes and modules
  record visibility; closing this needs a generated visibility column in
  `src/arity_table.rs`.
  Still open: the singleton-class reflection surface is empty —
  `C.singleton_class.instance_methods(false)` and its `private_` sibling both
  answer `[]` where MRI lists the class methods, and `C.methods` omits class
  methods entirely (even public ones). Enforcement above does not depend on it;
  it needs `#<Class:C>` to be populated from the owning class's `class_methods`.
  **What the directives ANSWER.** Three shapes, and which one applies is a
  property of the directive rather than of its argument count. The name-list
  spellings (`private`, `public`, `protected`, `module_function`) echo their own
  arguments — `nil` bare, the single argument itself by identity (so
  `private "a"` answers the String `"a"`, not `:a`), an Array for several. The
  constant- and class-method-level spellings (`private_constant`,
  `public_constant`, `deprecate_constant`, `private_class_method`,
  `public_class_method`) answer the RECEIVER, so `private_constant :X` inside
  `module M` is `M`. `ruby2_keywords` answers `nil`.
  Still open: `private_constant` records nothing, so the constant stays readable
  from outside (`M::X` answers the value where MRI raises
  `NameError: private constant M::X referenced`). Only the return value is
  faithful today.
- **Integer-to-Float comparison is exact everywhere, including the bare
  operator.** Ruby compares an Integer to a Float exactly, never by rounding the
  Integer, so `3**34 == (3**34).to_f` is false and `7**53 <= (7**53).to_f` is
  false. Every path rubylang owns did that from the start: `Integer#<=>`,
  `eql?`, Hash keys, `uniq`, `include?`/`index`, `max`/`min`/`sort`, and all of
  `<` `<=` `>` `>=` `==` once either side is a BigInt, Rational or Complex. The
  one path it did NOT own was the bare operator with BOTH sides still native —
  an `i64` Integer and a Float — which fusevm compared inline from a rounded
  `f64`, so the numeric hook never saw the operands and the answer went wrong
  above 2^53. That is fixed upstream and rubylang now pins fusevm 0.22.0, which
  carries it; rubylang itself needed no change, its hook already dispatched
  generically. Verified against the reference:

  ```console
  $ ./target/debug/ruby -e 'p(3**34 == (3**34).to_f)'      # false
  $ ./target/debug/ruby -e 'p(7**53 <= (7**53).to_f)'      # false
  $ ./target/debug/ruby -e 'p(2**60+1 == (2**60+1).to_f)'  # false
  ```
- **Composite Hash keys.** Arrays (`{[1, 2] => v}`, nested), Hashes
  (`{{a: 1} => v}`), Sets, Ranges (`{(1..3) => v}`, Integer/String/Float
  endpoints), `BigInt`/`Rational`/`Complex` numbers, and class objects work as
  Hash keys and Set members — keyed structurally by value, so equal keys hash
  together and round-trip through `.keys`/`.inspect`. Hash and Set keys are
  order-independent, matching MRI (`{a: 1, b: 2}.hash == {b: 2, a: 1}.hash`),
  and `0.0`/`-0.0` are one key. A `Struct`/`Data` instance
  keys by VALUE too — its class plus its members — so two `P.new(1, 2)` are the
  same Hash key and report the same `#hash`, as in MRI. (Only a plain user object
  with a custom `hash`/`eql?` still keys by heap identity.)
- **`Struct`/`Data` class ancestry.** A `Struct.new` class keeps `Struct` — and
  the `Enumerable` it mixes in — in its chain, and a `Data.define` class keeps
  `Data` and deliberately does NOT get `Enumerable`, matching MRI. `#is_a?` reads
  the same chain, so `Trio.new(1, 2).is_a?(Struct)` and `is_a?(Enumerable)` hold
  where a struct class is not in the class table at all. The class methods the
  generated class carries itself (`[]`, `members`, `keyword_init?`; `[]`, `new`,
  `members` for `Data`) are defined on it rather than on `Struct`/`Data`, so
  dumping `Struct.methods` never sees them and `src/arity_table.rs` has no row —
  they are named explicitly in the method-existence check.
- **`Data.define` is strict about its members.** Every member is mandatory and no
  extra one is accepted, in either construction form: `P.new(1)` raises
  `ArgumentError: missing keyword: :y`, `P.new(1, 2, 3)` raises `wrong number of
  arguments (given 3, expected 0..2)`, and `P.new(x: 1, z: 2)` raises `unknown
  keyword: :z`. A `Struct` stays lenient (missing members read as nil), which is
  also MRI's rule.
- **`Complex` arithmetic.** `+`/`-`/`*` combine componentwise through the numeric
  tower, and `/`/`quo` divide by the conjugate: two exact integer parts give an
  Integer when the division is exact and a reduced `Rational` otherwise
  (`Complex(1, 2) / Complex(3, 4)` is `((11/25)+(2/25)*i)`), while a Float
  anywhere in the operands divides as a Float. Unary minus negates both parts, so
  the `-2i` literal form works. `Complex#inspect` follows MRI's shape: each part
  is inspected (a Rational part is parenthesized), the sign comes from the value
  rather than its rendering, and the imaginary unit is `*i` whenever the
  magnitude does not end in a digit.
- **Private-by-definition hooks.** `initialize`, `initialize_copy`/`_clone`/
  `_dup`, `method_missing` and `respond_to_missing?` are private in Ruby however
  they were written, so they are excluded from `instance_methods`,
  `public_instance_methods`, `methods` and `public_methods`.
- **`Object#clone(freeze:)`.** `clone` carries the frozen flag over (unlike
  `dup`); `freeze: false` thaws the copy and `freeze: true` freezes it regardless
  of the source.
- **Frozen string literals.** A non-interpolated literal freezes when the file
  asks for it. Precedence is MRI's: a `# frozen_string_literal:` magic comment
  always wins, and `--enable`/`--disable-frozen-string-literal` only decides
  what a file with NO comment compiles as — so `--enable` cannot freeze a file
  whose comment says `false`. A comment value that is neither `true` nor `false`
  is not a setting at all and leaves the switch in charge. The switch is
  invocation-wide (an `AtomicBool`, so a file required from a spawned thread
  compiles the same way) while the per-file flag stays thread-local. All four
  MRI spellings work (`--enable-X`, `--enable=X`, `-` or `_` in the name), as do
  `all` and comma lists.
- **Divergence — an error raised by a builtin operation reports line 0.** The
  `file:LINE:in '<main>'` prefix is right for `raise` and for a `NoMethodError`,
  but an exception thrown from inside an operation carries no line: `1/0`,
  `[].freeze << 1` and a frozen-literal mutation all print `:0:` where MRI
  prints the real line. It is the op dispatch that never records a line, so
  this is independent of which error is raised and of the entry point.
- **Enumerator.** A block-less `each`/`map`/`select`/`reject`/`each_with_index`
  (on arrays), `String#each_char`/`each_byte`/`each_codepoint`/`each_line`, and
  `Integer#times`/`upto`/`downto`/`step`
  returns a concrete `Enumerator` supporting external iteration (`next`, `peek`,
  `rewind`, `size`, raising `StopIteration` at the end), the full Enumerable
  surface (`to_a`, `map`, `select`, `each_with_index.map { … }`, …) delegated to
  the materialized buffer, and re-attachable blocks via `with_index(offset=0)`
  and `with_object(memo)`. `with_index` honors the source method — `map`/
  `flat_map` collect the block's results, `select`/`reject` filter, `each`
  returns the elements; `with_object` threads the memo and returns it. Finite
  block-less sources are eagerly materialized. The grouping methods answer an
  Enumerator too: `each_slice(n)`/`each_cons(n)` without a block, and
  `chunk`/`chunk_while`/`slice_when` with one. `chunk_while`/`slice_when` need a
  block — MRI turns it into a Proc up front, so a block-less call raises
  `ArgumentError: tried to create Proc object without a block` rather than
  answering an Enumerator. An Enumerator built by the Array dispatcher records
  the object it iterates, which `each` answers and `inspect` shows
  (`[1, 2, 3].each_cons(2).each { }` is `[1, 2, 3]`, and it inspects as
  `#<Enumerator: [1, 2, 3]:each_cons(2)>`) — neither is reconstructible from the
  buffer, since `each_cons` windows overlap. A receiver that had to be
  materialized to reach the Array implementation (a Range, a Hash, a Set, another
  Enumerator) is re-pointed afterwards, so `(1..3).each_with_index` inspects as
  `#<Enumerator: 1..3:each_with_index>` and `[1, 2].each.map` nests.
  `Hash#each_with_index` answers one as well, and `Enumerator#each` on a GENERATOR
  answers what the generator body evaluated to, as MRI does — usually the
  `Yielder`, since `y << v` answers the yielder so `y << 1 << 2` chains.
- **Multi-value Enumerator yields.** `each_with_index`, `each_with_object` and a
  generator's `y.yield a, b` yield TWO values per iteration, not one packed pair.
  The buffer keeps them packed (that is what `to_a` and `each_entry` see) and the
  block is called with them SPREAD, so Ruby's own binding rules reshape them: a
  one-parameter block binds the first, `{ |x, i| }` binds both, `{ |*a| }`
  collects `[x, i]`, and a strict `->(x){}` raises `ArgumentError` exactly as MRI
  does. The two reshapes are therefore independent, which is what MRI's
  `rb_yield_values2`-vs-`rb_enum_values_pack` split needs and what a single
  projection of the buffer could not express: `[10, 20].each_with_index
  .take_while { |x| x == 10 }` hands the block `10` and keeps `[[10, 0]]`.
  Which consumers get the spread is measured, not derived — `map`, `collect`,
  `flat_map`, `collect_concat`, `filter_map`, `each`, `take_while`, `count`,
  `find_index`, `any?`, `all?`, `none?` and `one?` do; `select`, `reject`,
  `sort_by`, `group_by`, `partition`, `drop_while`, `find`, `min_by`/`max_by`,
  `sum`, `uniq` and `inject` see the packed pair, as does `each_entry` always. A
  generator's packs are marked when `y.yield a, b` builds them, so `y << [a, b]`
  (one value that happens to be an array) is left alone. The LAZY pipeline has
  its own split, measured the same way: `map`, `flat_map`, `filter_map`,
  `take_while` and `drop_while` spread, `select` and `reject` pack — note lazy
  `drop_while` differs from eager `drop_while`.
- **`.lazy` sources.** A lazy enumerator remembers the object `.lazy` was called
  on, so it inspects the way MRI's does — one `#<Enumerator::Lazy: …>` wrapper
  per stage, tagged with the operation and its argument
  (`#<Enumerator::Lazy: #<Enumerator::Lazy: [1, 2]>:take(2)>`). A Range (possibly
  endless) and a generator stay the pipeline's source, so they are still pulled
  on demand; anything else is materialized first. That materialization goes through `to_a`, not an
  "is it an Array" test — a Hash, a Set and an Enumerator are all enumerable but
  none of them IS an array, and reading them as one answered an EMPTY pipeline
  (`{a: 1}.lazy.map { … }.to_a` was `[]`). A user `Enumerable` with an infinite
  `each` therefore hangs where MRI stays lazy.
- **Lazy `uniq` is the one stateful stage.** Every other stage decides an
  element on its own, so its state is a counter at most; `uniq` has to remember
  the keys it has already emitted. That seen-set belongs to the PULL, not to the
  stored op — the op is shared by every pull of the same pipeline, so
  accumulating there makes the second `to_a` of one pipeline answer empty — and
  it has to be reset again inside the generator re-drive loop, which replays the
  source from the start in growing batches. Keys go through the same
  `hash`/`eql?` key `Array#uniq` and Hash keys use, so `1` and `1.0` stay
  distinct; a block supplies the key while the ORIGINAL element is what passes
  through. Still missing on `Enumerator::Lazy`, though the arity table declares
  them: `compact`, `grep`/`grep_v`, `with_index`, `eager`, `chunk`/`chunk_while`,
  `slice_before`/`slice_after`/`slice_when`, block-less `each`, and `size` (MRI
  answers nil for a lazy enumerator of unknown length).
- **Tie order in `min(n)`/`max(n)`/`min_by(n)`/`max_by(n)` is source order, not
  MRI's.** The n-argument forms answer the right n ELEMENTS, and agree with the
  reference whenever the keys are distinct. They can differ in the ORDER MRI
  reports among EQUAL keys: `[5,4,3,2,1].min_by(2) { 0 }` answers `[5, 4]` here
  and `[3, 5]` in MRI. That is not a rule MRI documents — `enum.c` reaches these
  through `nmin_filter`'s median-of-three quickselect and then a final
  `ruby_qsort`, neither of which is stable, so the order among ties is an
  artifact of that particular partitioning. Reproducing it means porting
  `ruby_qsort` itself; a stable sort by key is used instead, which keeps ties in
  source order. The tie cases are deliberately kept OUT of the parity corpus
  rather than frozen as though they matched.
- **`sort`/`min`/`max` raise on a pair they cannot rank.** They are DEFINED by
  `<=>`, so when `<=>` answers nil the caller raises `ArgumentError: comparison
  of X with Y failed`. `cmp_values` answers `Option<Ordering>`, where `None` IS
  that nil, and each of its callers decides what it means — they disagree:
  `Array#<=>` answers nil (`[1, "a"] <=> [1, 2]`), while `sort`, `sort!`,
  `min`, `max`, `minmax`, `sort_by`, `min_by`, `max_by`, `minmax_by`, `min(n)`,
  `max(n)` and the block forms of all of them raise. A block that answers nil is
  the same unrankable pair and raises too. All of these used to ANSWER: the
  comparator had no error channel, so it invented a ranking by comparing `as_f`
  values, and `[1, "a"].sort` came back `["a", 1]` from `0.0` against `1.0`.
  The ranking mirrors MRI's `<=>` — Int/Int, a user `<=>`, Time/Date/DateTime,
  Symbol, Array elementwise, String, number (a Complex counts only when its
  imaginary part is zero), any other class's own `<=>`, and finally
  `Object#<=>`: 0 when the pair is `==`, nil otherwise, which is what makes
  `[nil, nil]` and `[{}, {}]` sortable while `[true, false]` is not.
  **Known limit — which of the two operands the message names first.** MRI
  raises from inside whichever loop it selects by the receiver's length and
  element kinds, and the loops disagree about both which pair they reach and
  which operand leads: `[1, 2, "a", 3].min` names `"a"` and `1`, `.max` names
  `"a"` and `2`, `.min(2)` names `"a"` and `2`. At TWO elements the rule IS
  knowable: 1,399 of the 1,400 ordered pairs of
  `1 "a" nil :s 2.5 [9] true 1r Float::NAN 2**70`, across every entry point,
  match ruby 4.0.6 byte for byte (the one that does not is
  `[1, Float::NAN].sort_by`). At three or more the operand order is not
  reproducible outside MRI. `min(n)` also
  selects rather than sorts, so its order among `==`-equal but distinct values
  (`1` and `1r`) is its quickselect's, not a stable sort's.
- **Divergence — `sort`/`sort_by` are STABLE here and unstable in MRI.**
  `(1..20).sort_by { |x| x % 3 }` returns a different permutation of the
  equal-keyed elements in each; below roughly a dozen elements they agree,
  because MRI's `ruby_qsort` only switches strategy above a size threshold.
  Ruby documents `sort` as not stable, so neither answer is wrong — matching MRI
  means reproducing its qsort exactly in order to agree on an ordering the
  language explicitly does not specify, and being stable is the stronger
  guarantee. Left as-is on purpose; it is the one open row of the
  name-lookalike sweep.
- **A size argument is CHECKED, not clamped.** `first(-1)`, `last(-1)`,
  `take(-1)`, `drop(-1)`, `each_slice(0)`, `each_cons(0)`, `min(-1)`, `max(-1)`,
  `Array.new(-1)`, `"x" * -1` and `[1] * -1` each raise, and MRI uses six
  different sentences across them (`negative array size`, `attempt to take
  negative size`, `attempt to drop negative size`, `invalid slice size`,
  `invalid size`, `negative size (-1)`, `negative argument`). Clamping with
  `.max(0)` had answered an empty collection for all eleven.
- **Operand type checks.** `values_at` converts its index rather than reading a
  non-integer as 0; `zip` refuses an operand that is not enumerable rather than
  padding with nil; `Array#sum` propagates a type mismatch instead of falling
  back to `as_f`; `to_h` and `Hash[]` name the offending element's position;
  `String.new` refuses a non-String; `Struct#new` refuses more values than the
  struct has members; `Integer#chr` refuses anything outside 0..255 instead of
  wrapping through `as u8`; the `Math` functions raise `Math::DomainError`
  rather than answering IEEE's NaN, and `Integer.sqrt` exists and shares that
  rule.
- **Collection searches use `rb_equal`, not `==`.** `include?`, `index`,
  `rindex`, `count(obj)` and element-wise `Array#==` answer true for two
  operands that are the same value before asking `==`, which is what MRI does.
  One value distinguishes the two rules: `[Float::NAN].include?(Float::NAN)` is
  true while `Float::NAN == Float::NAN` stays false.
- **`between?` and `clamp` are one implementation for every receiver.**
  Comparable, Integer, Float, String and Rational all reach them through
  `cmp_int` — MRI's `cmpint`, which ranks through the receiver's own `<=>`.
  They disagree by design and the difference is the point: `clamp` rejects a min
  above its max BEFORE ranking the receiver against either (`5.clamp(3, 1)` is
  `ArgumentError: min argument must be less than or equal to max argument`) and
  treats a nil bound as NO bound (`5.clamp(nil, 9)` is 5); `between?` has no
  min-vs-max rule at all (`5.between?(3, 1)` is false), ranks against a nil
  bound and so raises on it, and short-circuits to false below min without ever
  ranking max. Each comparison reports its OWN operand — blaming min for a max
  that failed names the wrong value. Every receiver used to carry its own copy:
  none had the min-vs-max check, the numeric copies compared `as_f` values (so
  `5.clamp("a", "z")` answered `"z"` and `Float::NAN.clamp(1, 3)` answered NaN),
  the String copy coerced its bounds with `arg_str`, and Rational had neither
  method at all. `clamp(range)` accepts every range flavour, rejects an
  exclusive one only when it HAS an end (`5.clamp(1...)` is legal,
  `5.clamp(...9)` is not), and names a non-Range argument MRI's way
  (`wrong argument type Integer (expected Range)`).
- **Block-based generators.** `Enumerator.new { |y| ... }` drives the block with
  a native `Enumerator::Yielder`; `y << v` and its alias `y.yield(v)` push
  yielded values. `to_a`/`first(n)`/`take(n)`/`each`/`lazy` re-run the block on
  demand. Infinite generators (`loop { y << ... }`) are bounded by `first(n)`,
  `take(n)`, and lazy pipelines (`gen.lazy.map { ... }.first(n)`): the yielder
  raises a break signal once the requested count is reached, unwinding the loop
  (the same early-stop mechanism as endless-range `.lazy`). `Array#cycle(n)`
  (block-less) returns a finite Enumerator over the elements repeated `n` times;
  block-less endless `cycle` (no count) returns an infinite Enumerator backed by a
  native cycling generator, so `first(n)`/`take(n)`/`.lazy` draw as many repeats as
  needed and `next`/`peek` round-robin the elements forever (materializing one
  cycle, then wrapping). External iteration (`next`/`peek`) on any other generator
  runs the block on a `Fiber`, as MRI's `enumerator.c` does: the block advances
  exactly one `y << v` per `next`, so an endless `loop { y << ... }` generator
  answers `next` instead of running forever, a side effect between two yields
  happens only once the consumer reaches it, and a raise surfaces on the `next`
  that would have reached it (not on the first). `peek` parks the pulled value
  without advancing; `rewind` drops the fiber so the next `next` re-runs the block
  from the top. A block-less enumerator-returning method on an infinite source no
  longer materializes it: `each`/`map`/`select`/`each_entry`/`to_enum` and
  friends, plus `each_slice(n)`/`each_cons(n)`/`each_with_index`/`with_index`/
  `each_with_object(o)`, answer a DERIVED generator that pulls the source in
  growing batches and reshapes it on demand, so `gen.each_slice(2).first(2)`
  draws four elements instead of hanging. An endless Range (`(1..)`,
  `(1..Float::INFINITY)`) gets the same treatment through a native counting
  generator, where it used to raise `RangeError`. A call WITH a block, or one
  that genuinely needs every element (`sort`, `sum`, `to_a`), still materializes
  and so still runs forever on an infinite source — exactly as MRI does.
- **Hash through Enumerable.** MRI derives Hash's Enumerable surface from
  `Hash#each`, and `each` yields the whole `[k, v]` pair as ONE value
  (`hash.c` `each_pair_i`) — so a one-parameter block sees the pair, not the key.
  `each`/`each_pair` yield that way here, and every derived method
  (`find`/`detect`, `count`, `sum`, `flat_map`, `filter_map`, `any?`/`all?`/
  `none?`, `min`/`max`/`sort`, `take_while`/`drop_while`, `find_index`,
  `collect`, `tally`, `zip`, `first`, `reverse_each`, `each_entry`, `cycle`,
  `grep`, …) is delegated to the array of pairs, which is the same model. A
  `{ |k, v| }` block still binds both, because a multi-parameter block auto-splats
  the pair. Hash's OWN methods — `select`/`reject`, `delete_if`/`keep_if`,
  `each_key`/`each_value`, `transform_keys`/`transform_values`, and `to_h` with a
  block — yield key and value separately, as in MRI. `Struct#each_pair` and
  `ENV.each` follow `Hash#each`. `map`/`collect`/`find`/`detect` are the
  exception MRI carves out (`rb_block_pair_yield_optimizable`): they hand the
  block two separate values when its arity is a FIXED count above one, and the
  packed pair otherwise — so `hash.map(&->(k, v) { … })`, `hash.map(&->(kv) { … })`
  and `hash.map(&:first)` all bind correctly, which a single fixed yield shape
  cannot do now that lambdas are strict.
- **Exception hierarchy.** The builtin exception classes carry MRI's real tree,
  not a flat `X < Exception`: `ArgumentError`/`TypeError`/`RuntimeError`/… derive
  from `StandardError`, and the ones with an intermediate parent keep it
  (`NoMethodError < NameError`, `KeyError`/`StopIteration < IndexError`,
  `FloatDomainError < RangeError`, `FrozenError < RuntimeError`,
  `LoadError`/`NotImplementedError`/`SyntaxError < ScriptError`).
  `Exception#cause` is recorded by `raise`: raising inside a `rescue` links the
  new exception to the one being handled (`$!`), so `rescue => e; raise Wrapper`
  keeps the original reachable and `e.cause.cause` walks the chain. A bare
  re-raise is not its own cause, and an already-set cause is never overwritten.
  So `#is_a?`,
  `Class#superclass` and `Module#ancestors` agree with MRI, and a bare `rescue`
  catches `StandardError` and nothing above it — `SystemExit`, `ScriptError` and
  friends fall through it as they should. An error class the host has no record
  of (a Rust-side failure reported by message alone) is still treated as a plain
  `StandardError` so a bare `rescue` keeps working.
- **Fiber (stackful coroutines).** `Fiber.new { |first| ... }`, `#resume(*args)`,
  `Fiber.yield(v)`, and `#alive?` are implemented on `corosensei` same-thread
  stackful coroutines: a fiber freezes its entire native stack — including the
  in-flight fusevm `VM::run()` driving its block — onto an alternate stack, so
  `Fiber.yield` suspends *below* the VM (fusevm needs no suspend/resume API) and
  the coroutine shares the process-global object heap with its resumer. `resume`
  threads a value in as `Fiber.yield`'s return (and as the block's first
  parameter on the initial resume); the block's final value is the last
  `resume`'s result, and side effects fire lazily at the real yield boundaries.
  Resuming a fiber whose block has returned raises `FiberError` (`attempt to
  resume a terminated fiber`); `Fiber.yield` at the root raises `FiberError`
  (`attempt to yield on a not resumed fiber`). Both wordings were rubylang's own
  until the round-6 audit measured them. Each fiber runs its own `VM`
  instance and its own volatile execution context (scope/signal/frames), swapped
  at every resume/suspend boundary, so fibers are isolated and nest correctly.
  That swap includes the pending-exception slot, so a raise inside the body has
  its exception object handed back across the boundary on the way out — without
  that the resumer would see only the message string and rebuild it as a bare
  `RuntimeError`, losing both the class and any user subclass identity.
- **Thread (real OS threads under a GVL).** `Thread.new`/`start`/`fork` spawn a
  real OS thread that runs on the one process-global `Mutex<RubyHost>` heap. A
  Global VM Lock serializes execution — only the lock-holder runs Ruby, exactly
  like MRI — so shared-heap read-modify-write (`x += 1` across threads) stays
  atomic. Because the spawner holds the GVL, the child does not start until the
  spawner releases it (at `join`/`value`), giving one-thread-at-a-time ordering;
  a thread swaps in its own execution context (frames/scope/signal) so call
  stacks never collide. `Thread#join`/`#value` release the GVL, wait for the OS
  thread, reacquire, and `value` re-raises the thread's real exception object.
  `#alive?`/`#status`, `Thread.current`/`main`/`pass`/`list` are present.
  `Mutex`/`Thread::Mutex`/`Monitor` (`lock`/`unlock`/`try_lock`/`locked?`/
  `synchronize`) work: under the GVL a critical section with no blocking call runs
  uninterrupted, so `synchronize` holds a lock flag around the block (cleared even
  on a raise). `Queue`/`SizedQueue` (`push`/`<<`/`pop`/`size`/`empty?`/`close`/…)
  and `ConditionVariable` (`wait`/`signal`/`broadcast`) block for real: each has
  its own mutex+condvar (independent of the GVL), and a blocking `pop`/`wait`
  releases the GVL so a producer/signaller can run, then reacquires it — a
  consumer thread that `pop`s an empty queue correctly parks until a `push`.
  `ConditionVariable` uses a generation counter so a signal delivered after the
  waiter starts waiting is never lost (bare non-predicate signals can still race,
  as in MRI). Not yet: `report_on_exception`'s stderr warning, and MRI's fatal
  deadlock detector (a program that waits on a queue no thread will ever feed
  hangs rather than aborting). Fibers remain thread-owned (a fiber is resumed only
  on its creating thread, as in MRI).
- **Bignum.** Integers auto-promote to arbitrary precision on overflow, like
  MRI: values that fit stay `i64` immediates, and only the overflow path
  allocates a `BigInt` heap object (backed by `num-bigint`). Arithmetic, bit
  ops, `**`, comparison, `to_s(base)`, `bit_length`, and `digits` all cross the
  boundary transparently. Five operations used to CRASH at the `i64` boundary
  rather than promote — `(-2**63)` under `abs`, `/ -1`, `% -1`, `divmod(-1)` and
  `pred` each panicked the interpreter, because Rust's `i64` arithmetic does not
  overflow, it aborts. Division and modulo are taken in `i128`, where the `i64`
  quotient always fits, and every result leaves through one promotion point.
- **`Integer#/` floors and `#%` takes the divisor's sign** — neither is Rust's.
  Rust's `/` truncates toward zero and its `%` takes the DIVIDEND's sign, which
  is Ruby's `remainder`, not Ruby's `%`. Both Ruby spellings are implemented:
  `-7 / 2` is `-4`, `-7 % 3` is `2`, `(-7).remainder(3)` is `-1`.
- **`Float#round(half:)`.** Ruby's default tie rule is half-UP (away from zero),
  and `half: :even` and `half: :down` are real modes that change only values
  sitting exactly on the halfway point. All three go through the f64 path, the
  exact-integer path for a negative `ndigits`, and the rational path past
  `DBL_DIG`. `half:` belongs to `round` alone; `floor`/`ceil`/`truncate` take
  the keyword Hash as their `ndigits` positional and fail converting it, as MRI
  does.
- **`Float#%` and `#divmod` by zero raise.** `1.0 % 0` is a `ZeroDivisionError`,
  not IEEE's NaN; only `/` and `fdiv` answer Infinity.
- **Ruby's whitespace is not Rust's, and there are three different sets.**
  `String#strip`/`lstrip`/`rstrip` remove a FIXED ASCII set plus NUL; Rust's
  `str::trim` shares the name but uses the Unicode `White_Space` property, so it
  removed the NBSP and ideographic space MRI keeps and kept the NUL MRI strips.
  Awk-mode `split(" ")` uses the same fixed set, so an NBSP is a field and not a
  separator. The numeric scanners (`to_i`, `to_f`, `to_r`, `Kernel#Integer`,
  `Kernel#Float`) skip C `isspace`, which INCLUDES the vertical tab that Rust's
  `is_ascii_whitespace` omits and EXCLUDES the NUL that `strip` removes — so
  `"\v12".to_i` is 12 and `Integer("\0" + "12")` is refused. Each set is a named
  constant in `src/builtins.rs` rather than a call to a Rust predicate that
  happens to be close.
- **`Integer#pow(e, m)`.** Modular exponentiation for `e >= 0`. A negative
  exponent with a modulus raises `RangeError` (`Integer#pow() 1st argument
  cannot be negative when 2nd argument specified`), matching MRI — no modular
  inverse is computed.
- **Numeric conversions.** `Integer#to_r` / `Float#to_r` (the *exact* rational
  an f64 represents), `String#to_r` (leading `a/b` or decimal), `#to_c`
  (`(n+0i)`), and `Float#rationalize([eps])` (simplest rational within a
  tolerance) are supported, backed by `num-rational`. `nil.to_a`/`to_h` return
  the empty collection. `Array#sum` (and `reduce(:+)`) stay exact for Rational,
  BigInt, String, and Array elements.
  `Kernel#Rational` takes the whole surface MRI takes — Integer, Float
  (exactly), Rational, a Complex whose imaginary part is zero, and the string
  grammar with exponents, digit-grouping underscores and a decimal denominator
  — and rejects with MRI's four different classes (`ArgumentError` naming the
  string, `TypeError` naming the class, `ZeroDivisionError`, `FloatDomainError`,
  and `RangeError` for a Complex with an imaginary part). `Integer`, `Float`,
  `Rational`, `Complex`, `String`, `Array` and `Hash` check their argument count
  against the measured table and honour `exception: false`.
- **Gap — `String#to_c` is not implemented.** `"12".to_c` raises
  `NoMethodError`; MRI answers `(12+0i)`. `Integer#to_c` and `Float#to_c` are
  present, so this is the String half only.
- **`Time` is UTC-only.** `Time.at`, `Time.utc`/`Time.gm`, and `Time.now`
  construct times; the field readers (`year`/`month`/`day`/`hour`/`min`/`sec`/
  `wday`/`yday`), `to_i`/`to_f`, `to_s`/`inspect`, `strftime` (every directive
  MRI documents except a numeric field WIDTH such as `%6N`, including the
  composites `%c`/`%x`/`%r`/`%v`, the week numbers `%U`/`%W`, the ISO-8601
  week-based `%V`/`%G`/`%g`, `%N`, and the `-`/`_`/`0` padding flags),
  arithmetic (`Time - Time → Float`,
  `Time ± Numeric → Time`) and comparison/sort all work, with a dependency-free
  proleptic-Gregorian calendar (valid for negative epochs too). The
  local-timezone offset is **not** modeled — there is no tz database, so
  `.utc`/`.getutc` are exact and `.localtime`/`Time.local` behave as UTC.
  Timezone-aware `strftime` is not modeled (`%Z` always prints `UTC`, `%z` always
  `+0000`).

  **Divergence — `Time#to_s`/`#inspect` print UTC where MRI prints local.** With
  `TZ=America/New_York`, MRI renders `Time.at(0)` as
  `1969-12-31 19:00:00 -0500` and answers `false` to `utc?`; here it is
  `1970-01-01 00:00:00 UTC` and `true`. This is NOT an `inspect`-local fix, and
  that is why it is still open: `RObj::Time` stores a bare `secs: f64` with no
  offset field, and the whole surface decomposes it through one UTC
  `time_fields` — the field readers, `strftime`, `<=>`, `+`/`-`, and the
  `Time.local`/`Time.mktime` constructors (which currently share the `Time.utc`
  arm and so silently shift the instant by the local offset). Rendering local in
  `inspect` alone would leave `t.hour` disagreeing with `t.inspect`, which is
  worse than being consistently UTC. Closing it properly means giving
  `RObj::Time` an offset, filling it from `localtime_r` (libc is already a
  dependency, so no tz crate is needed), and threading it through every one of
  those sites — plus pinning `TZ` in the fuzzer, since otherwise the mode's
  output depends on the machine's zone. `Time#inspect` is also implemented
  twice, in `dispatch_time` and in `Host::inspect`, and only the latter fires
  (the universal `inspect` arm wins first), so both would have to move together.
- **`Date`** (available without `require "date"`, which is accepted as a no-op).
  `Date.new`/`civil`, `Date.today`, `Date.jd`, and `Date.parse` (ISO
  `YYYY-MM-DD` / `YYYY/MM/DD` only — MRI's lenient free-form parsing is not
  modeled) construct dates. Field readers (`year`/`month`/`day`/`wday`/`yday`/
  `cwday`/`jd`/`leap?`), `to_s`/`iso8601`, `inspect` (Julian-day form),
  `strftime`, day/month/year arithmetic (`+`/`-`/`next_day`/`prev_day`/
  `next_month`/`prev_month`/`>>`/`<<` with last-day clamping), `Date - Date →
  Rational`, and comparison/sort all work over the same proleptic-Gregorian
  calendar as `Time`. `Date#>>` accepts a fractional argument
  (`Date.new(2020,1,31) >> 1.5` is 2020-02-29, as in MRI). Locale-aware
  formatting is not implemented — see the `strftime` note under Time.
- **`DateTime`** (also available without `require "date"`; it is a `Date`
  subclass carrying a time of day). `DateTime.new`/`civil` (year through second),
  `DateTime.now` (UTC here), `DateTime.jd`, and `DateTime.parse` (ISO8601
  `YYYY-MM-DDTHH:MM:SS` only) construct values. Field readers add `hour`/`min`/
  `sec` to the `Date` set; `to_s`/`iso8601`/`inspect` use the ISO8601 form
  (`2020-01-01T12:30:45+00:00`); `strftime`, day/month/year arithmetic (keeping
  the time of day), `DateTime - DateTime → Rational` (in days), `to_date`,
  `to_time`, and comparison/sort all work over the same proleptic-Gregorian,
  UTC-only calendar. Because the model is UTC-only, `DateTime#to_time.to_s`
  renders the zone as `UTC` rather than MRI's `+0000`, and fractional-second /
  `DateTime.now` sub-second values are not bit-for-bit faithful (f64 storage).
- **`Array#pack` / `String#unpack` / `#unpack1`** are implemented for the common
  web/crypto directives — `C`/`c` (bytes), `a`/`A` (string, NUL/space pad), `N`/
  `n`/`V`/`v` (big/little-endian 16/32-bit ints), `H`/`h` (hex, high/low nibble
  first), the fixed-width integers `s`/`S`/`l`/`L`/`q`/`Q`/`i`/`I`/`j`/`J` (with
  the `<`/`>` byte-order modifiers; an unsigned 64-bit value past `i64::MAX`
  unpacks to a Bignum), the floats `D`/`d`/`E`/`G` (double) and `F`/`f`/`e`/`g`
  (single), and the cursor moves `x`/`X`/`@`. Because strings are UTF-8 (`String`,
  not a byte buffer), a binary string
  is modeled with the Latin-1 convention: a "byte" is a code point in
  `U+0000..=U+00FF` (its low 8 bits). `pack` produces such a string and `unpack`
  reads it back the same way, so any `pack`-produced binary string round-trips
  (`bytes.pack("C*").unpack("C*")`, `(0..255).to_a.pack("C*").unpack("C*")`), and
  `Integer#chr` (`n → U+00nn`, and only for `0..255` — outside that MRI raises
  `RangeError: N out of char range`, where masking with `& 0xff` used to answer
  a wrapped character) round-trips through `unpack("C*")` too. Two
  documented divergences remain from the lack of a true ASCII-8BIT type: (1)
  `unpack` on a *genuine* multibyte-UTF-8 text string reads code points, not the
  raw UTF-8 bytes MRI would — `"é".unpack("C*")` is `[233]` here vs `[195, 169]`
  in MRI; (2) `String#bytes`/`#ord` keep real-UTF-8 semantics, so `255.chr.bytes`
  is `[195, 191]` here vs `[255]` in MRI, and `255.chr.inspect` is the `U+00FF`
  code point rather than MRI's `"\xFF"`. For ASCII and every `pack`-produced
  binary string the two models coincide.
- **`defined?`.** The `defined?(expr)` / `defined? expr` operator returns the
  Ruby description string (`"local-variable"`, `"instance-variable"`,
  `"global-variable"`, `"constant"`, `"method"`, `"assignment"`, `"expression"`,
  `"nil"`/`"true"`/`"false"`/`"self"`/`"yield"`) or `nil`, without evaluating the
  operand. Kernel methods (`puts`, `require`, …) report `"method"`. Two edges
  differ from MRI: an instance/class variable *set to `nil`* reads as undefined
  (nil and unset are indistinguishable in the object model), and the lexical
  local-declaration quirk (`x = 1 unless defined?(x)` — MRI treats `x` as an
  already-declared local from the unexecuted assignment) is not modeled.
- **`Math` module.** `Math.sqrt`/`cbrt`/`sin`/`cos`/`tan`/`asin`/`acos`/`atan`/
  `atan2`/`sinh`/`cosh`/`tanh`/`asinh`/`acosh`/`atanh`/`exp`/`log`(with optional
  base)/`log2`/`log10`/`hypot` and the constants `Math::PI` / `Math::E` are
  implemented over `f64`. `Math.erf`/`erfc`/`lgamma`/`log1p`/`expm1`/`frexp`/
  `ldexp`/`gamma` bind the C library's own entries, which is what MRI's `math.c`
  does — each of those is a one-line wrapper there — so they agree with the
  reference to the last bit and are covered by the parity corpus rather than
  tested with a tolerance. `Math.gamma` additionally carries MRI's exact
  factorial table for integral arguments in `1..23`, because Γ(n) has an exact
  double representation that `tgamma` is not required to land on.
  `Math.class` reports `Module`, as MRI does.
- **`JSON` (dependency-free).** `require "json"` is a no-op; the module is always
  available. `JSON.generate`/`JSON.dump` and `#to_json` (on any value — Array,
  Hash, String, Symbol, Integer, Float, `true`/`false`/`nil`, Bignum, and a
  generic quoted-`to_s` fallback for other objects) hand-encode over the host
  value model, matching MRI byte-for-byte: symbol hash keys become string keys,
  non-string keys stringify via `to_s`, `nil`→`null`, floats use `Float#to_s`,
  and string escaping names only `" \ \b \t \n \f \r` with other C0 controls as
  lowercase `\uXXXX` (DEL and non-ASCII pass through raw, `/` is not escaped).
  `JSON.pretty_generate` uses 2-space indent. `JSON.parse`/`JSON.load` is a
  hand-written recursive-descent decoder producing Hashes with string keys (or
  symbol keys under `symbolize_names: true`), Integer/Bignum/Float numbers, and
  arrays/scalars; malformed input raises `JSON::ParserError`, catchable by bare
  `rescue`, by `rescue => e`, and by naming the constant
  (`rescue JSON::ParserError => e`). `Rational`/`Complex`/`Time` encode as their quoted
  `to_s` (MRI's default `Object#to_json`), so those are excluded from the exact
  parity corpus.
- **`ERB` (dependency-free templating).** `require "erb"` is a no-op; the class is
  always available. `ERB.new(template, trim_mode: "-")` compiles the template into
  a `_erbout`-building Ruby program (kept in the `@src` ivar, exposed by `#src`
  like MRI) using a hand-written scanner over `<% %>` tags: `<%= e %>` →
  `_erbout << (e).to_s`, `<% c %>` emits `c` verbatim (so loops/conditionals
  wrap the appends), `<%# … %>` is dropped, literal text is embedded in a
  double-quoted Ruby string, and `<%%` yields a literal `<%`. Because text is
  embedded in a real Ruby double-quoted string, `#{…}` in template text
  interpolates — matching MRI 6.x byte-for-byte, not "literal passthrough".
  `#result` / `#result(binding)` evaluates the compiled program via the host
  `eval_in_place` machinery **in the caller's current scope**, so the template
  sees the caller's top-level locals, instance variables (`<%= @x %>`), and
  methods; this matches MRI at top level (where the default binding's `self` is
  also `main`). `#result_with_hash(hash)` evaluates in a fresh, isolated scope
  with the hash keys pre-bound as template locals (self is a blank object, so it
  does not see or pollute caller state). Trim mode: `"-"` is implemented —
  `-%>` chomps the immediately following newline, `<%-` strips leading blanks on
  its line; `dash_trim` is enabled when the mode string contains `-`. All the
  above are verified byte-for-byte against MRI (`ruby -rerb`). **Limitations:**
  (1) an explicit `Binding` argument to `#result` is accepted but not modeled —
  evaluation always uses the current scope, so `#result(some_other_binding)` does
  not switch scopes (same limit as the `eval` builtin). Inside a method body,
  `#result` sees that method's scope rather than a fresh top-level binding, which
  is broader access than MRI's default `new_toplevel_binding`. (2) The other MRI
  trim modes (`">"`, `"<>"`, `"%"`) are not implemented — only `"-"` (and the
  default no-trim). (3) The `%%>` → `%>` escape is not special-cased: `%%>` in
  template text stays literal (which is what MRI 6.x does in text); inside a tag
  the first `%>` closes it, so a literal `%>` cannot be embedded in a tag body.
  (4) `#result_with_hash` binds arbitrary runtime values directly (no
  serialization), so any object works as a template local. (5) Legacy positional
  `safe_level`/`eoutvar` args to `ERB.new` are ignored; the deprecated positional
  trim-mode (3rd arg) is still honored.
- **`StringIO` (dependency-free).** `require "stringio"` is a no-op; the class is
  always available. `StringIO.new(initial = "")` is a String-backed IO: the
  buffer and read cursor live in the object's `buf`/`pos` ivars. `#string`
  returns the accumulated buffer; `#write`/`#<<`/`#print`/`#puts` append (the
  common output/log-sink and input-buffer patterns only ever append or read),
  with `#write` returning the byte count and `#<<` returning self; `#read([len])`,
  `#gets`, and `#each_line` read from the cursor; `#rewind`, `#pos`/`#pos=`,
  `#tell`/`#seek`, `#eof?` track it. Used by Rack for input and log sinks.
- **`rand`.** Backed by a thread-local SplitMix64. `srand(seed)` reseeds it so
  `rand`/`rand(n)` are reproducible within a run and returns the previous seed
  (MRI semantics); the MRI-exact sequence and MRI's random startup seed are not
  matched. `srand` with no argument reseeds from the system clock.
- **Method surface.** The Enumerable/String/Hash/Range surface is broad but not
  exhaustive; an unimplemented method raises a `NoMethodError` whose message uses
  the Ruby-4.0 form — `undefined method '<name>' for an instance of <Class>` for
  ordinary receivers, `for nil`/`for true`/`for false` for those values, and
  `for class <Name>` for a class/module reference.
- **Ranges.** Integer, Float (`1.0..2.0`), and String (`'a'..'e'`) endpoints are
  supported, plus endless (`1..`) and beginless (`..5`). A Float range can't be
  iterated directly (`each`/`to_a`/`map` raise `TypeError` like Ruby) but
  supports `step`, `min`/`max`/`begin`/`end`, and the containment predicates.
  `==` compares endpoints and exclusivity; `===` is proper case-equality (Range
  covers, `Class` matches instances, `Regexp` matches a string) rather than
  `==`, so `case`/`when` over ranges and classes works. `step` covers String
  endpoints as well as numeric ones (`('a'..'e').step(2)` → `["a", "c", "e"]`;
  a non-positive step yields only the first element, as MRI does for String
  ranges). A String range whose endpoints are all digits counts *numerically*
  and zero-pads to the beginning string's width, like MRI's `rb_str_upto_each`:
  `("9".."11").to_a` is `["9", "10", "11"]` and `("08".."11").to_a` is
  `["08", "09", "10", "11"]`. `Range.new(lo, hi[, exclusive])` builds the same
  value the `..`/`...` literal does.
- **Pattern matching (`case/in`).** Array/hash/find-by-key patterns, class
  patterns (`Integer`, `Point[...]`), variable/`_` binding, `=> name` (chained
  and binding a whole `|` alternation), `^pin`, `|` alternatives (a bare `|` in a
  value pattern is alternation, not bitwise-or), `*rest` splats, the two-sided
  find pattern (`[*, x, *]`), `**nil` exact-key enforcement, and `if`/`unless`
  guards work. As in MRI, a variable binding inside an `|` branch is rejected
  ("variable capture in alternative pattern"). Array and hash patterns honour the
  `deconstruct` / `deconstruct_keys` protocol: an array pattern matches any object
  responding to `deconstruct` (called once, must return an Array), a hash pattern
  any object responding to `deconstruct_keys` (passed the requested symbol keys,
  or `nil` for `**rest`/`**nil`/`{}`); binding a hash `**rest` name is supported.
  Struct instances participate fully: they report `respond_to?` true for both
  protocol methods, `deconstruct` yields the member values, and
  `deconstruct_keys` honours a requested-key filter (returning only the named
  members, in the requested order) or all members when passed `nil`.

  **What a failed match REPORTS is thinner than MRI's.** The class is right
  (`NoMatchingPatternError`) and so is the matchee, but the message is the
  matchee alone where MRI appends the specific check that failed, and there is no
  `NoMatchingPatternKeyError` subclass:

  ```console
  $ /opt/homebrew/opt/ruby/bin/ruby -e 'case 9; in String then 1; end'
  9: String === 9 does not return true (NoMatchingPatternError)
  $ /opt/homebrew/opt/ruby/bin/ruby -e '{a: 1} => {b:}'
  {a: 1}: key not found: :b (NoMatchingPatternKeyError)
  ```

  rubylang reports `9` and `{a: 1}`. MRI's detail is one of six forms —
  `<pat> === <val> does not return true`, `guard clause does not return true`,
  `<val> length mismatch (given N, expected M)`, `does not respond to
  #deconstruct`, `does not respond to #deconstruct_keys`, and `key not found:
  :k` — and it appears ONLY when the `case` has a single `in` clause; with two or
  more, MRI also reports the matchee alone.

  This is not a message-formatting gap. `lower_pattern` compiles a pattern to a
  boolean expression tree (`pcall(v, "===", [subj])` and friends) that the
  compiler tests with `JumpIfFalse`, so at the point of failure nothing knows
  WHICH sub-check said no — the information is not merely unformatted, it is
  never produced. Reporting it means giving the lowering a failure-reason
  channel, which also has to survive alternatives (`|`) and nesting. The same
  channel is what `NoMatchingPatternKeyError` needs, since a missing key and a
  present-but-non-matching value are the same `false` today.

## `min(n)` / `max(n)` tie order past 7 survivors

`Enumerable#min(n)`, `#max(n)`, `#min_by(n)` and `#max_by(n)` answer the right
`n` elements always. The ORDER among elements that rank EQUAL but are
distinguishable — `1`, `1.0` and `1r` all compare equal and all `inspect`
differently — matches MRI exactly while at most **seven** elements survive the
selection, and is not guaranteed to match beyond that.

MRI runs `enum.c`'s `rb_nmin_run`: elements stream into a buffer of `4 * n`,
`nmin_filter` quickselects it down whenever it fills, and the survivors are
sorted at the end by `ruby_qsort`. `nmin_filter` is deterministic and portable,
and rubylang ports it faithfully — that is what decides the tie order for the
sizes that matter, and it is why sorting the whole array and taking the first
`n` (which is what rubylang used to do) was a different permutation of the same
elements:

```console
$ /opt/homebrew/opt/ruby/bin/ruby -e 'p [1, 1r, 2.5].min(2)'
[(1/1), 1]
```

`ruby_qsort` is the part that does not port. On this platform it is not MRI's
code at all — the oracle's own
`include/ruby-4.0.0/arm64-darwin25/ruby/config.h` defines
`HAVE_BSD_QSORT_R 1`, so `util.c` compiles `ruby_qsort` to a direct call to the
C library's `qsort_r`. MRI's bundled `mm.c` quicksort runs only on a platform
that has neither `qsort_r` nor `qsort_s`, and a glibc build calls glibc's
`qsort_r` — a third algorithm again. The final tie order is therefore a property
of the host C library, not of Ruby, and the same program orders ties differently
on macOS and on Linux.

Measured against MRI 4.0.6, over tie-heavy inputs drawn from
`[1, 1.0, 1r, 2, 2.0, 2r, 3, 3.0, 3r]`:

| survivors `n` | cases | reordered by `ruby_qsort` |
| --- | --- | --- |
| 1–7 | 62,770 | 0 |
| 8 | 3,990 | 2,961 |
| 9 | 3,124 | 2,425 |
| 12 | 1,814 | 1,590 |
| 16 | 296 | 294 |

Zero below eight, because libc's quicksort finishes runs of seven or fewer with
insertion sort, which is stable. From eight it enters the unstable partitioning,
and 2,500 randomly generated `min(n)`/`max(n)` calls with `n <= 7` are
byte-identical between rubylang and MRI while 1,498 of 2,000 with `n >= 8`
differ — in every one of those 1,498 by ORDER only, never by which elements were
selected.

Chasing the rest would mean hard-coding one C library's permutation of output
Ruby documents no order for, and it would then be wrong on the other platforms
rubylang targets. It is left alone deliberately rather than approximated: a
stable sort would agree with neither libc for `n >= 8`.

## Stdlib modules (SecureRandom / Digest / Base64 / OpenStruct)

Dependency-free, verified against the reference `ruby`.

- **`Digest::MD5`/`Digest::SHA1`/`Digest::SHA256`** — hand-written MD5 (RFC 1321),
  SHA-1 and SHA-256 (FIPS 180-4), no crate. `hexdigest`, `digest` (raw bytes),
  and `base64digest` are supported and match MRI byte-for-byte on ASCII/UTF-8
  input (`Digest::SHA256.hexdigest("abc")` → the reference vector). `Digest`
  resolves the `::MD5`/`::SHA1`/`::SHA256` sub-module refs; `Digest.hexencode`
  hex-encodes a string.
- **`Base64`** — `encode64` (60-char line wrap + trailing `\n`, matching
  `[str].pack("m")`), `strict_encode64`, `urlsafe_encode64` (padding defaults to
  true; `padding: false` drops the `=`), `decode64`/`strict_decode64`/
  `urlsafe_decode64` (lenient: both alphabets accepted, whitespace skipped, `=`
  terminates). Byte-exact vs MRI on ASCII/UTF-8.
- **`SecureRandom`** — `hex`, `base64`, `urlsafe_base64`, `uuid` (v4, correct
  version/variant nibbles), `alphanumeric`, `bytes`, and `random_number`
  (`Integer` → `[0, n)` Integer, `Float` → `[0, n)` Float, no-arg → `[0, 1)`
  Float). **Not cryptographically secure**: it draws from the same thread-local
  SplitMix64 that backs `Kernel#rand`, so outputs are the right shape, length,
  and format but not CSPRNG-grade (there is no OS entropy source wired in). This
  is a deliberate dependency-free tradeoff, not a silent one.
- **`OpenStruct`** — dynamic attributes stored as the object's ivars.
  `OpenStruct.new(a: 1)`, reader `os.a`, writer `os.a = 2` (creates the field),
  unknown reader → `nil`, `os[:a]`/`os[:a] = v`, `to_h`, `each_pair`/`each`,
  `members`, `dig`, `respond_to?` (true for a set attribute's reader and writer
  plus the container methods; a writer for an unset field is not reported, as in
  MRI), `inspect`/`to_s` (`#<OpenStruct a=1, b=2>`), and
  attribute-wise `==` (order-independent, also inside Arrays/Hashes).

**Limitations.** (1) `Digest`/`Base64`/`SecureRandom` operate on a String's UTF-8
bytes. rubylang stores a `"\xNN"` source escape as the Unicode code point U+00NN
(UTF-8-encoded), not a single raw byte like MRI's ASCII-8BIT, so hashing/encoding
a string built from non-ASCII byte escapes differs from MRI. Pure ASCII/UTF-8
text is byte-exact. (2) The streaming `Digest` instance API
(`Digest::MD5.new.update(...).hexdigest`) is implemented alongside the
class-level one-shot `hexdigest`/`digest`/`base64digest`.

## File / IO / Dir

Backed by `std::fs`/`std::io`, verified method-by-method against the reference
`ruby`. The `std::fs::File` is stored in a host side table (`io_handles`),
indexed by an `RObj::IoHandle` — the same non-`Clone`-in-a-`Clone`-enum pattern
`Fiber` uses for its coroutine. `STDOUT`/`STDERR`/`STDIN` (and the matching
`$stdout`/`$stderr`/`$stdin` globals) are pre-seeded stream handles.

**Working, output-matched to MRI:** `File.read`/`write` (write returns the byte
count), `exist?`/`exists?`/`file?`/`directory?`/`size`/`delete`/`unlink`,
`readlines`/`foreach` (both honour `chomp: true`), `open(path, mode)`
(block form yields the IO and closes
it on exit, returning the block value; block-less returns the open IO); the
pure path helpers `basename` (incl. `suffix` and `.*` extension strip),
`dirname`, `extname` (MRI edge cases: leading-dot names, trailing dot →`"."`,
all-dots →`""`), `join`, `expand_path` (lexical `~`/`.`/`..` resolution against
an explicit base or the cwd). IO instance: `read`/`write`/`gets`/`puts`/
`print`/`<<`/`each_line`/`each`/`readlines` (the last two take `chomp: true`
too)/`close`/`closed?`/`flush`/`inspect`
(`#<File:/path>`, `#<File:/path (closed)>`, `#<IO:<STDOUT>>`). `Dir.pwd`,
`glob`/`[]` (sorted per MRI ≥3.0; leading-dot files excluded from `*`;
`{a,b}` brace alternation expanded and concatenated in brace order),
`entries` (incl. `.`/`..`), `exist?`/`exists?`, `mkdir`/`rmdir`, `chdir`
(block form restores the cwd), `home`. `Kernel#open(path, mode)` delegates to
`File.open`.

**Known gaps / divergences:**
- ~~**No `Errno` hierarchy.**~~ **Closed.** A filesystem failure now raises the
  specific `Errno::*` class under `SystemCallError`, carrying `#errno`, and the
  constant resolves by name so `rescue Errno::ENOENT` matches:

  ```console
  $ ruby -e 'begin; File.read("/nope/xyz"); rescue => e; p [e.class, e.class.superclass, e.errno]; end'
  [Errno::ENOENT, SystemCallError, 2]
  ```

  The errno NUMBERS come from `libc` at compile time, never from a literal —
  `ENOTEMPTY` is 66 on macOS and 39 on Linux, `ECONNREFUSED` 61 and 111, so a
  hardcoded table would name the wrong class on one of the two platforms
  rubylang targets and would do it silently. `errno_class` covers 46 names; an
  errno outside that set still raises a plain `SystemCallError` with the number
  attached. Pinned by
  `tests/eval.rs::a_filesystem_failure_raises_its_specific_errno_class`.
- **`Errno::ENOENT::Errno`** (the per-class errno constant) is unimplemented;
  read the number off an instance's `#errno` instead.
- **`IO#to_s` returns the `inspect` form** (`#<IO:<STDOUT>>`, `#<File:/path>`)
  rather than MRI's non-deterministic address form (`#<IO:0x0000…>`). Chosen for
  a deterministic, testable string; `#inspect` itself is exact.
- **`Dir.glob` drops a literal `./` prefix.** The `glob` crate normalizes
  `./a.txt` to `a.txt`, so a pattern like `{sub,.}/*.txt` loses the `./` MRI
  keeps. `*`, `*.ext`, `**`, `[..]`, and brace patterns all match exactly.
- **`gets` reads byte-at-a-time** (one syscall per byte) — correct, not tuned
  for large-file line iteration.
- **No pipe/command IO** (`open("|cmd")`), no `File.chmod`/`symlink`/`stat`
  struct, no `IO.select`/`seek`/`pos`/`rewind`/`tell`, no separator/limit args
  to `gets`/`readlines`. `File`/`IO`/`Dir` are not user-subclassable.
- **`.localtime`/timezones** are unmodeled elsewhere (see Runtime); file mtimes
  are not surfaced as `Time` objects.

## Sockets (`TCPServer` / `TCPSocket`)

Backed by `std::net` (`TcpListener`/`TcpStream`) — no C extension, no external
crate. A socket value is an `RObj::IoHandle` into the same `io_handles` side
table as `File`/`IO`, with two new `IoCell` cases (`TcpListener`/`TcpStream`);
the non-`Clone` OS handles live in the table exactly like `File`'s
`std::fs::File` and `Fiber`'s coroutine. Every blocking syscall (`accept`,
`read`, `write`) is issued on a `try_clone`d handle *after* the host borrow is
released, so a blocked socket never holds the interpreter lock. `TCPSocket`
reads are buffered (a 4 KiB read-ahead `VecDeque`) so `gets`/`read` don't issue
one syscall per byte. This is enough to serve HTTP: a pure-Ruby `TCPServer`
accept loop reading a request and writing an `HTTP/1.1 200` response is verified
end-to-end (`tests/socket.rs`, incl. a raw `std::net` client and `curl`).

**Working, output-matched to MRI:**
- `TCPServer.new(port)` / `TCPServer.new(host, port)` — bind + listen; `host`
  defaults to `0.0.0.0`; `port` `0` gets an OS-assigned ephemeral port. `.open`
  with a block yields the server and closes it on return.
- `TCPServer#accept` (blocking) → a connected `TCPSocket`; `#addr`
  (`["AF_INET", port, ip, ip]`, the ephemeral-port readback path), `#close`,
  `#closed?`.
- `TCPSocket.new(host, port)` client; `.open` with a block.
- `TCPSocket#gets` (line, buffered), `#read(n)` (exactly `n`, or all with
  `read`/`read(nil)`), `#readpartial(n)`, `#write`/`#<<`/`#print`/`#puts`,
  `#each_line`/`#each`, `#peeraddr`/`#remote_address`, `#addr`/`#local_address`,
  `#close`, `#closed?`. `#inspect`/`#to_s` → `#<TCPServer:127.0.0.1:PORT>` /
  `#<TCPSocket:PEER>`.
- `require 'socket'` is a no-op returning true (the classes are always present).

**Known gaps / divergences:**
- **TCP only.** No `UDPSocket`, no `UNIXSocket`/`UNIXServer`, no `Socket`
  (the low-level BSD-socket class), no `Addrinfo`. `#local_address`/
  `#remote_address` return the `#addr` array, not a real `Addrinfo` object.
- **Blocking only.** No `IO.select`, no `IO#wait_readable`/`wait_writable`, no
  event loop. `#read_nonblock` is best-effort: it toggles `O_NONBLOCK` for one
  read and raises `IO::EAGAINWaitReadable` on `WouldBlock` / `EOFError` at EOF,
  but `TCPServer#accept_nonblock` falls back to a blocking `accept` (the
  buffered model has no pending-queue to peek). No `IO.select`-based
  multiplexing means one connection at a time unless the caller threads.
- **No TLS/SSL.** No `OpenSSL::SSL::SSLSocket` — plaintext HTTP only, not HTTPS.
- **No `Errno` hierarchy on the SOCKET path.** Connect/bind failures still raise
  a single `SocketError` carrying the OS message, not the specific
  `Errno::ECONNREFUSED` / `Errno::EADDRINUSE` MRI raises. The `Errno::*` classes
  themselves now exist and the FILESYSTEM path uses them (see File/IO above);
  only the socket raise sites have not been converted, because they do not go
  through `sys_err`. Still a rescuable `StandardError` descendant; only the
  class name differs.
- **No socket options / timeouts.** No `setsockopt`/`getsockopt`,
  `SO_REUSEADDR`, `TCP_NODELAY`, `#recv`/`#send` with flags, connect/read
  timeouts, or `#shutdown`. `#close_read`/`#close_write` close the whole socket.
- **No separator/limit args** to `gets`/`readlines`; `gets` is `\n`-terminated.
- `TCPServer`/`TCPSocket` are not user-subclassable.

## Database persistence (`SQLite3::Database`)

`require "sqlite3"` is a no-op returning true (the classes are always present).
`SQLite3::Database` is backed by the `rusqlite` crate with the `bundled` feature,
so SQLite is compiled in-tree — no external `sqlite3` gem, no libsqlite3/FFI, no
system package. This is real on-disk persistence: rows written to a file DB
survive the connection closing and are read back by a fresh process (verified by
`tests/sqlite.rs` reopening a tempfile in a reset interpreter, and by
`examples/sqlite_persistence.rb`). Each open `rusqlite::Connection` (not `Clone`)
lives in the host `db_handles` side table, exactly like `File`/`TCPServer` do in
`io_handles`; the Ruby value is an `RObj::Db` handle.

Implemented (the core sqlite3-gem shape):
- `SQLite3::Database.new(path)` / `.open` (alias), `":memory:"` (and `""`) for an
  in-memory DB; the block form `SQLite3::Database.new(path) { |db| … }` yields the
  handle, closes it afterward (even on error), and returns the block's value. An
  options hash honors `results_as_hash: true`.
- `db.execute(sql[, bind])` — a SELECT returns an Array of rows (each an Array of
  column values); DDL/DML returns `[]`. A block yields each row and returns nil.
  Binds follow the gem's `execute(sql, bind_vars = [])` signature: a single Array
  (`["a", 1]`), or a lone scalar auto-wrapped; a placeholder with no supplied
  bind is left NULL (the gem's lenient behavior).
- `db.execute2(sql[, bind])` — same, with a header row of column names prepended.
- `db.query(sql[, bind])` — returns rows like `execute` (no streaming `ResultSet`).
- `db.get_first_row(sql, *binds)`, `db.get_first_value(sql, *binds)` (varargs binds).
- `db.last_insert_row_id`, `db.changes`.
- `db.results_as_hash = true` / `db.results_as_hash` — rows become Hashes keyed by
  column name (String keys).
- `db.close`, `db.closed?`, `db.open?`. `SQLite3::SQLITE_VERSION` (the linked lib).
- Type map: INTEGER→Integer, REAL→Float, TEXT→String, NULL→nil, BLOB→String.
- SQL errors raise `SQLite3::SQLException` (a `StandardError` — caught by a bare
  `rescue` and by `rescue SQLite3::SQLException`), carrying the sqlite message.

**Known gaps / divergences:**
- **BLOB → String is lossy for non-UTF-8 bytes.** Host Strings are Rust `String`
  (UTF-8), so a BLOB is decoded with `from_utf8_lossy` — exact for text/UTF-8
  blobs, but raw binary with invalid sequences gets U+FFFD replacement. There is
  no separate `SQLite3::Blob` type, and bind values are typed by their Ruby class
  (String→TEXT), so a String bind is stored as TEXT, not BLOB.
- **Positional binds only.** No named parameters (`:name` / `$name` /
  `db.execute(sql, "name" => v)`); only `?` placeholders bound by position.
- **No streaming `ResultSet` / prepared-statement object.** `db.prepare` and the
  `Statement`/`ResultSet` API are not implemented — `execute`/`query` prepare,
  run, and materialize all rows eagerly.
- **One exception class.** Every SQL error is a `SQLite3::SQLException` carrying
  the raw sqlite message; the gem's finer subclasses (`CantOpenException`,
  `BusyException`, `ConstraintException`, …) resolve as class refs but are not
  raised distinctly. The message text comes from `rusqlite`/SQLite and differs
  verbatim from the gem's (which appends the offending SQL) — assert on content,
  not the exact string.
- **No transaction/pragma helpers.** No `db.transaction`/`commit`/`rollback`
  block API, `db.busy_timeout`, `db.trace`, `db.function` (custom SQL functions),
  or `db.type_translation`. Raw `BEGIN`/`COMMIT` via `execute` work.
- `SQLite3::Database` is not user-subclassable.

## FFI (`Fiddle`)

`require "fiddle"` is a no-op returning true (the classes are always present).
Fiddle is real foreign-function calling: `libloading` provides `dlopen`/`dlsym`
and `libffi` builds a call interface from types decided at runtime, so Ruby code
invokes actual C functions. Both crates are vendored/compiled in-tree (libffi
builds its own C via `libffi-sys`), so this needs no system package and builds on
macOS aarch64 + Linux x86_64/aarch64. The `Fiddle::Handle` library, and the
owned buffers behind `Fiddle::Pointer`, live in host side tables (the
`fiddle_libs` / `fiddle_mem` pattern, like `db_handles`/`io_handles`).

Implemented (enough to call C):
- `Fiddle.dlopen(path)` / `Fiddle::Handle.new(path)` → a `Fiddle::Handle`.
  `Fiddle.dlopen(nil)` opens the current process' global scope (`dlopen(NULL)`),
  so already-loaded libc symbols resolve with no library path. `handle[sym]` /
  `handle.sym(name)` → the symbol's address (Integer); `handle.close`.
- Type codes `Fiddle::TYPE_VOID`/`TYPE_VOIDP`/`TYPE_CHAR`/`TYPE_SHORT`/`TYPE_INT`/
  `TYPE_LONG`/`TYPE_LONG_LONG`/`TYPE_FLOAT`/`TYPE_DOUBLE`/`TYPE_SIZE_T` (and the
  unsigned negatives), with MRI's exact integer values.
- `Fiddle::Function.new(addr, [arg_types], ret_type)` and `#call(*args)`:
  Integer/Float/String arguments marshal to C through libffi and the result
  marshals back (integer/size_t → Integer, float/double → Float, `TYPE_VOIDP`
  result → a `Fiddle::Pointer`). A String argument passes as a NUL-terminated C
  `char*` for `TYPE_VOIDP`.
- `Fiddle::Pointer[str]` / `.to_ptr(str)` (wrap bytes), `.malloc(n)`, `#to_s`,
  `#to_str(len)`, `#[]` (byte / slice read), `#size`, `#null?`, `#to_i`, `#free`.
  A returned `char*` reads back as a Ruby String via `#to_s`.
- `Fiddle::DLError` (rescuable) for dlopen/dlsym/type failures.

Not modeled / boundaries:
- **Not a libruby C ABI.** Fiddle calls arbitrary C functions in a shared
  library; it does not expose `libruby`'s `VALUE`/`rb_*` C-API. MRI-C-API
  extension gems (nokogiri, the C `mysql2`, etc.) link against that ABI, so they
  do not load. Pure-Ruby gems and anything expressible as direct C calls work.
- **FFI is unsafe by construction.** A signature that does not match the real C
  function can crash the process — this matches MRI Fiddle's low-level contract
  and is not guarded.
- **Unix only.** Backed by `os::unix` `dlopen` (macOS + Linux, the crate's
  target set). No Windows `LoadLibrary` path.
- **No closures/callbacks.** `Fiddle::Closure` (passing a Ruby block as a C
  function pointer) is not implemented, nor `Fiddle::Pointer#ptr`/`#ref`
  dereferencing, struct/`CStruct` layouts, or `Fiddle::Function::STDCALL` (only
  the default C calling convention).
- **Unsigned results above `i64::MAX`** promote to a bignum, but such values do
  not arise from the common libc surface.

## Loading files (`require` / `require_relative` / `load`)

- **`require`, `require_relative`, and `load` actually read, parse, compile, and
  run files** on the live host, so a required file's constants, classes,
  methods, and globals persist into the caller. `require(path)` resolves against
  `$LOAD_PATH` (`$:`) trying `path` then `path.rb` (absolute paths and a
  current-directory fallback too); `require_relative(path)` resolves against the
  directory of the file currently running (an internal file-dir stack pushed
  before a required/loaded file runs, popped after); `load(path)` searches
  `$LOAD_PATH` without appending `.rb`. `require`/`require_relative` dedup on the
  resolved absolute path — first load returns `true`, an already-loaded feature
  returns `false` without re-running — and record the path in `$LOADED_FEATURES`
  (`$"`) *before* running the body, so a circular `require` sees it loaded and
  returns `false` instead of recursing. `load` always re-runs and never dedups.
  A missing file raises a catchable `LoadError` (`cannot load such file --
  <path>`); a syntax error in the target raises `SyntaxError`.
- **`$LOAD_PATH`/`$:` and `$LOADED_FEATURES`/`$"`** are real, pushable Arrays,
  seeded with the running script's directory (or the current directory for `-e`
  / stdin). Each alias pair (`$LOAD_PATH`/`$:`, `$LOADED_FEATURES`/`$"`) points
  at the *same* Array object, so `$LOAD_PATH.equal?($:)` is true and a push
  through either name is visible through the other. Re-*assigning* one alias
  (`$: = [...]`) does not repoint the other (only mutation is shared); read the
  canonical name after such a reassignment.
- **Proc/begin id merge.** Each file compiles to its own program with `procs` /
  `begins` Vecs indexed from 0; merging a second program onto the host would
  collide those ids with the first program's. Before merge, every proc-id and
  begin-id operand in the new program (in the main chunk, method chunks, class
  method chunks, proc chunks, and the `BeginDef` body/ensure/rescue fields) is
  rebased above the host's current `procs.len()`/`begins.len()`, and the vecs are
  appended (never replaced). This also fixes a latent REPL bug where each line
  replaced the proc/begin tables, dangling ids captured by earlier-line
  closures. A required method whose body uses a block or `begin`/`rescue` now
  dispatches to its own body, not a same-id body from another file.
- **Builtin libraries stay no-ops.** `require` of a known standard-library name
  the runtime provides natively or ignores (`set`, `json`, `date`, `time`,
  `securerandom`, and a fixed list of common stdlib names) returns `true`
  without a file search, so those names never map to a `.rb` on disk.
- **Bundled pure-Ruby stdlib (`uri`, `csv`, `optparse`, `yaml`).** These are real
  pure-Ruby libraries embedded in the binary (`embedded_stdlib`, via
  `include_str!`), compiled and run on the host the first time they are required
  — so `require "uri"` actually defines `URI` (etc.) with no external file, and
  the installed `ruby` stays self-contained. They dedup on the require name in
  host state, not through `$LOADED_FEATURES` (a repeat `require` still returns
  `false`): a bundled library has no path on disk, and the prelude requires one
  of them (`rubygems/version`) before user code runs, so a synthetic entry there
  would show a script a feature it never required. Coverage is the common surface, not
  the full API: `URI` parse/join/`encode_www_form` (HTTP/HTTPS/FTP + Generic, no
  scheme registry); `CSV` parse/generate with RFC-4180 quoting (no converters or
  `:headers` Row objects); `OptionParser` short/long/`--[no-]`/valued flags with
  Integer/Float coercion, `parse!`/`parse`/`to_s` (no required-arg enforcement or
  completion); `YAML` block-style emit/parse for Hash/Array/String/Symbol/
  Integer/Float/bool/nil plus inline flow collections on load, `dump` output
  round-trips through `load` (no anchors/tags/multi-doc/block scalars/custom
  objects). `module_function` (both the bare-directive and
  `module_function :m` forms) and `to_yaml` on builtin receivers both work.
  Not yet: `FileUtils`/similar native pseudo-modules whose *constant* is unbound
  (`FileUtils.mkdir_p` works, but `FileUtils.class` is `NilClass` where MRI
  raises `NameError: uninitialized constant FileUtils`).
- **`__dir__`** returns the directory of the file currently running (from the
  same file-dir stack), a String. MRI computes `File.dirname(File.realpath(
  __FILE__))`, so under `-e`/stdin — where `__FILE__` is the literal `"-e"` /
  `"-"` and names no file — the realpath step drops out and the answer is the
  relative `"."`; rubylang matches. The dir stack is still seeded with the
  current directory there, so `require_relative` from a one-liner resolves
  against the real directory; only what `__dir__` *reports* is `"."`.
- **`__FILE__`** returns the path of the file currently running (a parallel
  file-path stack): the script path exactly as given on the command line for the
  top-level script (not canonicalized, matching MRI), the required file's own
  absolute path inside a `require`d file, and `"-e"` for a one-liner. So
  `File.dirname(__FILE__)` behaves like `__dir__`.
- **Limitations.** `require` does not use `RUBYLIB`, gem paths, or a real stdlib
  tree — only `$LOAD_PATH` (script dir + whatever the program pushes) plus the
  builtin no-op list. Autoload, `require` of a `.so`/`.bundle`, and thread-safe
  concurrent require are out of scope. A required file's top-level *locals* are
  isolated from the caller's (MRI-faithful), but its top-level `self` is the same
  shared main object rather than a per-file binding. `$:` reads correctly inside a `#{...}` string
  interpolation; `$"` still cannot (the interp scanner reads the quote as a
  string delimiter, and the read fails with `unterminated string`). Reference
  `$"` outside interpolation, or use the sigil shorthand.


## AOT bundling (`ruby --build`)

- **Whole-app build-time merge.** `ruby --build FILE` compiles the entrypoint
  *and everything it statically requires* into one program and warms it into the
  cache (`~/.rubylang/scripts.rkyv`). A build-time pass (`bundle.rs`) walks the
  entrypoint AST; for every `require "..."` / `require_relative "..."` whose
  argument is a **literal** string it resolves the path with the *same* resolver
  the runtime uses (`builtins::resolve_in` / `resolve_require_in`, shared code —
  not a reimplementation), reads + parses the target, recursively bundles *its*
  requires (deduped by absolute path, cycle-safe), and inlines each file **at the
  require site** wrapped in `begin … end`, so a required file's top level runs
  exactly where the `require` sat and after the requires preceding it. A second
  `require` of an already-bundled path becomes `false` (MRI's already-loaded
  return). The combined statement list lowers through the normal `compiler`
  path — proc/begin ids are assigned natively in one pass, so no id rebasing is
  needed for the static bundle (rebasing still governs the runtime
  `require`/`load`/REPL merge). A subsequent `ruby FILE` runs the cached bundle
  directly: it skips lex/parse/lower **and needs none of the required source
  files on disk**.
- **Stale-bundle detection.** The stored bundle carries a dependency manifest
  (`(abs_path, content-key)` for every inlined file). On run, a still-present
  dependency whose content changed since `--build` marks the whole bundle stale,
  so the run silently recompiles from source instead of executing an outdated
  artifact; an *absent* dependency is trusted (that is the "ship the bundle, drop
  the sources" case). The cache key is the canonical entrypoint path plus its
  source, so two apps that share identical entrypoint source but require
  different files never collide.
- **Dynamic requires stay runtime (honest).** A `require` whose argument is not a
  literal string (computed / interpolated) cannot be resolved at build time and
  is left as a runtime call — it still works when the source is present, and the
  build report counts it under "runtime require(s) left dynamic." A builtin-lib
  name (`json`, `socket`, …) is likewise never bundled (it stays a runtime
  no-op), and a literal path that does not resolve is left in place to raise
  `LoadError` at run time exactly as MRI would. Requires inside a `def` / block /
  lambda body are **not** bundled either: those run when the method is *called*,
  not at load time, so inlining them would change semantics. Class/module/`begin`
  bodies and top-level `if` branches *are* load-time, so their literal requires
  are bundled (a conditional require is inlined in-branch — it runs only if the
  branch runs; the required file's `def`/`class`/constant definitions are hoisted
  globally by the compiler regardless, matching Ruby's "defined but maybe unused"
  load semantics).
- **Divergence — top-level local shadowing.** A bundled file's top level runs
  inside the `begin … end` wrapper, which is a block closure over the requiring
  file's scope, not the fresh top-level binding MRI (and the rubylang *runtime*
  `require`) gives it. New top-level locals in a bundled file stay block-local
  (they neither leak downward into the requirer nor upward out of the block), but
  if the requirer *already has* a live local of the same name, an assignment to
  that name in the bundled file reassigns the requirer's local instead of
  shadowing it. Example: entry has `secret = 1`, then requires a file whose top
  level does `secret = 42`; run directly this prints `1` (isolated), but built
  with `--build` it prints `42`. This only bites when a required file assigns a
  bare top-level local whose name collides with a live local in the requirer —
  essentially never in real libraries, which define constants/classes/methods,
  not shared top-level locals. Related: a top-level `return` in a bundled file
  propagates out of the `begin` wrapper rather than merely ending that file's
  load (the runtime `require` path swallows it). Use the runtime `require` path
  (don't `--build`) for the rare code that depends on either behavior.

## AOT native executable (`ruby --build --native`)

- **Standalone binary, no interpreter, no sources.** `ruby --build --native FILE`
  bundles the app exactly as `--build` does, then emits a native executable next
  to the entrypoint (`app.rb` → `app`). It runs the whole program with **no `ruby`
  interpreter and no `.rb` files present** — verified end-to-end (multi-file app
  with a namespaced class, a block-taking method, and a namespaced constant; run
  after deleting every source; stdout and exit code match both `ruby FILE` and
  MRI). `file app` reports a native Mach-O / ELF executable.
- **How it links.** `fusevm::aot::compile_object` lowers the **main** chunk to a
  relocatable object that exports the native driver `fusevm_aot_entry`, the
  serialized main chunk, and imports the runtime shims plus the frontend hook
  `fusevm_aot_register_builtins`. Because `compile_object` embeds only the main
  chunk, the rest of the program (methods/classes/blocks/constants) is serialized
  with the same serde-flat form the on-disk cache uses (`cache::program_to_blob`)
  and baked into a generated Rust frontend via `include_bytes!` as a fixed-size
  byte array (so the symbol's address *is* the data). The frontend's `main` calls
  `fusevm::aot::fusevm_aot_run_embedded`; `rustc` links it against the rubylang
  rlib (which statically links fusevm and the whole runtime) plus the object.
  The frontend hook (`aot::fusevm_aot_register_builtins`, in the rubylang library)
  installs the same builtins + numeric hook `host::run_chunk_on` uses and loads
  the embedded program into the thread-local host before main runs.
- **Only the top-level chunk is native (today).** Method and block bodies are
  **not** AOT-compiled — each runs through the interpreter (`host::run_chunk_on`
  spins a fresh VM per call), reading the body from the host the frontend hook
  loaded. So the "native executable" is a native top-level driver over an
  interpreted method/block core, not a fully native-compiled program. This is
  correct and self-contained; it is not yet the "every method compiled to
  machine code" endpoint.
- **Needs `rustc` + the build tree.** The link step shells out to `rustc` and
  resolves the rubylang rlib + its dependency rlibs from the `target/<profile>`
  dir next to the running `ruby` binary. An installed `ruby` stripped of its build
  tree (no `librubylang.rlib`) reports a clear error instead of a broken binary;
  the integration test (`tests/aot_native.rs`) skips cleanly when `rustc` is
  absent. Cross-compilation is not supported — the object targets the host ISA
  (`cranelift_native`), so it builds for the machine it runs on.
- **Binary size + per-app link cost.** The executable statically links the whole
  rubylang + fusevm + Cranelift runtime, so it is large (~30 MiB, unstripped debug
  profile) and each build pays a `rustc` link (~1–2 s). No attempt is made yet to
  shrink via dead-code stripping or a release-profile runtime. On macOS the linker
  prints a benign "no platform load command" note for the cranelift-object output
  (it assumes macOS and links fine); it is silenced in the build report.

## Tooling

- **DAP debugger (`ruby --dap`).** Source-line breakpoints fire inside method,
  block, and loop bodies (per-statement line markers, emitted only in `--dap`
  mode — normal runs carry zero extra ops and keep the tracing JIT; the debug
  path runs the pure interpreter so every marker fires). Supports
  `setBreakpoints` with real marker-based verification (a breakpoint on a
  blank/`end`/comment line reports unverified and never fires),
  `stackTrace`/`scopes`/`variables` (locals of the stopped frame), and
  `continue`/`next`/`stepIn`/`stepOut` with `stopped`/`output`/`terminated`
  events. It is single-threaded: it services requests only while stopped at a
  marker, so an async `pause` of a free-running program is not supported. Also
  not yet: `evaluate`/watch expressions, conditional/hit-count breakpoints,
  `setVariable`, exception breakpoints, and non-innermost-frame variable
  inspection.
- **AOP weave.** `before`/`after`/`around` advice registered via the Ruby-facing
  `intercept(pattern, kind, handler)` builtin fires from the `run_method` dispatch
  choke point: `before` runs pre-call with the call args, `after` runs post-call
  with the final result (observe-only), and `around` is a true sandwich — the
  handler runs INSTEAD of the body, receiving the original call args plus a block
  that, when yielded, runs the real body once. The block is a native
  `ProcKind::Around` proc backed by the host `around_stack`; re-entering
  `run_method` under the `IN_ADVICE` guard runs the original un-advised (no
  infinite recursion). The handler's return value is the call's result whether or
  not it yielded (MRI around semantics); stacked around handlers nest. Weaving is
  gated on an O(1) `intercepts::any()` check, so calls with no registered advice
  are unaffected. Limitations: yield args to the around block are ignored (the
  original always runs with its captured args), and the native block is valid only
  during the weave that created it (capturing it and calling it after the
  intercepted call returns is unsupported — stale `around_stack` index).
