# Known gaps

rubylang is in active development. The pipeline (lex → parse → lower to fusevm
bytecode → run) is solid for the implemented surface, verified against the
reference `ruby` by the parity harness (`cargo run --bin parity`, replayed in CI
by `tests/parity.rs`). This file tracks what is deliberately not done yet, so the
gaps are honest rather than surprising. Nothing is faked as working — an
unimplemented method raises `undefined method`.

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
`.call`/`.()`/`[]`), keyword args + `**opts`, `Integer#step`, `?c` char literals,
`String#center`/`tr`/`lines`/`delete`/`count`/`to_i(base)`, `Integer#to_s(base)`,
`Array#dig`/`first(n)`/`last(n)`/`min(n)`/`max(n)`/`each_cons`/`sum { }`,
`Hash#dig`.

## Language

- **`extend` / `prepend` / `class << self`.** `extend M` in a class/module body
  mixes `M`'s instance methods in as class methods; `prepend M` inserts `M`
  ahead of the class in the ancestor chain (so its methods override and `super`
  reaches the class); `class << self … end` defs register as class methods, the
  same as `def self.x`. `super` resolves through the receiver's linearized
  `Module#ancestors` order, so module-`super` (from a prepended or included
  module) reaches the next method correctly. Runtime instance `obj.extend(M, …)`
  mixes each module's instance methods (following `M`'s own `include` chain, plus
  `define_method` blocks) into the object's singleton table. Not modeled: `super`
  from inside a class method.
- **Class-level instance variables** (`@n` inside a `def self.m` / an `extend`ed
  method) do not persist across calls — the class object has no per-instance
  variable store yet, so `@n ||= 0; @n += 1` restarts each call.
- **Class-body statements** run at definition time with `self` bound to the
  class, so `def`, `attr_*`, `include`, class variables (`@@x = 0`), constants,
  and other executable statements all take effect. Constants are namespaced
  under their enclosing module/class (see "Namespaces" below), not global.
- **Modifier `rescue` inside call-args / array literals.** Numeric-literal
  binding (`-7.abs` → `(-7).abs`, with `-2**2` → `-(2**2)`) and modifier
  `rescue` precedence (`x = a rescue b` → `x = (a rescue b)`, plus grouping
  parens and statement-level) both match MRI now. The one residual divergence:
  MRI rejects a bare modifier `rescue` directly in method-call arguments
  (`p(1/0 rescue 5)`) and array elements (`[1/0 rescue 5]`); this parser accepts
  them (a permissive superset — no valid MRI program breaks).
- **Splat-only / anonymous params.** A bare `*` splat, a `*name` splat, and a
  bare `**` keyword-splat parse as the sole (or any) parameter of a method,
  block, or lambda: `def f(*)`, `def f(**)`, `def f(*, **)`, `->(*) { }`,
  `->(*a) { a }`, `proc { |*| }`, `proc { |*x| x }`. A splat block/lambda's
  `arity` is `-(required + 1)`, negative like MRI.
- **Parallel assignment with a leading splat.** `*x, y = 1, 2, 3` and `*x = 1, 2`
  (splat as the first target) parse now, alongside the already-supported trailing
  (`a, *b =`) and middle (`a, *b, c =`) splat positions.
- **Lambda literal as a command argument.** `p ->(x) { x }` (a `->` lambda
  directly after a spaced command name) parses. Still not parsed: nested
  destructuring targets in a parallel assignment LHS (`(a, (b, c)), d = …`).
- **Numbered / `it` implicit block params.** `_1`.`_9` and Ruby 3.4's `it`
  are not bound (they read as `nil`); implementing them needs a nested-block-aware
  body walk. Use explicit `{ |x| … }` params.

## Metaprogramming / reflection / eval

Implemented and verified against the reference `ruby`:

- **Singleton methods.** `def obj.m` and `def Klass.m` parse and run: an object
  receiver stores a per-object singleton method; a class receiver registers a
  class method (identical to `def self.m`). `class << obj … end` on an instance
  works the same way. Singleton methods take priority over the class's own
  instance methods in dispatch (matching `Module#ancestors` order). A bare
  self-call inside a singleton method (or inside a block whose `self` is a
  receiver carrying singletons) resolves those singletons.
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
  now live on that object.
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
  `respond_to_missing?`, then Struct/OpenStruct attributes. For builtin receivers
  (`String`, `Integer`, `Array`, `Hash`, `Symbol`, …) `respond_to?` returns
  `true` for any name except the pattern-match deconstruction protocol, because
  there is no enumerable registry of the builtin method surface — so
  `"s".respond_to?(:no_such)` is `true` where MRI is `false`. Accurate builtin
  `respond_to?` needs a per-type method-name registry (deep substrate, not yet
  built).

## Lexer

- **Not lexed:** (nothing outstanding here). Heredocs (`<<END`, `<<~SQL`, `<<-EOT`, `<<'RAW'`),
  `%w[]` / `%i[]` word/symbol arrays (and the `()`/`{}`/`<>` delimiter variants),
  double-quoted `#{}` interpolation, `?c` character literals, regex literals
  (`/pat/flags`, with `i`/`m`/`x` flags), radix integer literals
  (`0b1010` binary, `0o17`/`017` octal, `0xff` hex, `0d99` decimal, with `_`
  separators), and the `%q`/`%Q`/`%r`/`%s` percent literals (single/double-
  quoted string, Regexp, Symbol — any punctuation delimiter, with `()`/`{}`/
  `[]`/`<>` nesting) **are** lexed. Double-quoted string escapes cover
  `\a\b\t\n\v\f\r\e\s\0\\\"\#`, `\xHH` (hex byte), and `\uHHHH`/`\u{H…}`
  (Unicode). `String#inspect` renders these Ruby-faithfully — named escapes,
  `\uXXXX` (uppercase) for other control chars, and `\#` before `{`/`@`/`$`. The
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

- **Regexp.** Supported: `/pat/flags` literals, `=~`/`!~`, `String#match`
  (returns `MatchData` with `[n]`/`pre_match`/`post_match`/`to_a`/`captures`),
  `match?`, `scan`, `split(re)`, `sub`/`gsub` with a Regexp (backrefs `\1`..`\9`
  in the replacement and a block form), and `Regexp#{source,match,scan,match?}`
  plus `case`/`when /re/` case-equality. A successful match sets the globals
  `$~` (MatchData), `$&` (whole match), `` $` ``/`$'` (pre/post text), `$+`
  (last group), and `$1`..`$9` (numbered groups) — visible after `=~`/`match`
  and inside a `sub`/`gsub` block. (The punctuation globals `` $` `` and `$'`
  can't yet appear inside a `#{...}` interpolation — the interp scanner reads the
  quote as a string delimiter; reference them outside interpolation.) Backed by
  the Rust `regex` crate, so Ruby's Onigmo-only constructs (backreferences within
  the pattern, lookaround) are unavailable.
- **`Object#class` returns a Class object** (a class reference): `p obj.class`
  prints the bare name, `obj.class == SomeClass` and `Integer == Integer`
  compare by class identity, and `obj.class.name` / `.to_s` give the name.
  `Class#superclass` and `Module#ancestors` walk the class chain (builtin types
  use a fixed table; user classes follow their superclass + included modules),
  and the `<`/`<=`/`>`/`>=` class relations return `true`/`false`/`nil` like
  Ruby (`Integer < Numeric` → true, `String < Numeric` → nil). A Class object
  is usable as a Hash key or Set member (keyed by class name), so
  `group_by(&:class)` and counting-by-class work.
- **Class/module reflection.** `Module#instance_methods([inherited])`,
  `#public_instance_methods`, `#method_defined?`/`#public_method_defined?`, and
  the instance-side `Object#methods` return method names as symbols.
  `instance_methods(false)` is the class's own methods (including
  `attr_accessor`/`attr_reader`/`attr_writer` accessors and `define_method`
  methods); `instance_methods` / `instance_methods(true)` add every user-defined
  ancestor (included modules and superclasses) via the ancestor chain. Builtin
  ancestors (`Object`/`Kernel`/`Comparable`/`Enumerable`) are NOT enumerated —
  the inherited set is bounded to the user-defined portion of the chain, so it
  omits MRI's builtin Kernel methods. Method visibility (public/private/
  protected) is not modeled, so `public_instance_methods` equals
  `instance_methods` and `public_method_defined?` equals `method_defined?`. The
  synthetic `__class_body__` (and any `__`-prefixed internal name) is excluded.
- **Composite Hash keys.** Arrays (`{[1, 2] => v}`, nested), Ranges
  (`{(1..3) => v}`, Integer/String/Float endpoints), and class objects work as
  Hash keys and Set members — keyed structurally by value, so equal keys hash
  together and round-trip through `.keys`/`.inspect`. (Only a user object with a
  custom `hash`/`eql?` still keys by heap identity.)
- **Enumerator.** A block-less `each`/`map`/`select`/`reject`/`each_with_index`
  (on arrays), `String#each_char`/`each_byte`/`each_line`, and
  `Integer#times`/`upto`/`downto`/`step`
  returns a concrete `Enumerator` supporting external iteration (`next`, `peek`,
  `rewind`, `size`, raising `StopIteration` at the end), the full Enumerable
  surface (`to_a`, `map`, `select`, `each_with_index.map { … }`, …) delegated to
  the materialized buffer, and re-attachable blocks via `with_index(offset=0)`
  and `with_object(memo)`. `with_index` honors the source method — `map`/
  `flat_map` collect the block's results, `select`/`reject` filter, `each`
  returns the elements; `with_object` threads the memo and returns it. Finite
  block-less sources are eagerly materialized.
- **Block-based generators.** `Enumerator.new { |y| ... }` drives the block with
  a native `Enumerator::Yielder`; `y << v` and its alias `y.yield(v)` push
  yielded values. `to_a`/`first(n)`/`take(n)`/`each`/`lazy` re-run the block on
  demand. Infinite generators (`loop { y << ... }`) are bounded by `first(n)`,
  `take(n)`, and lazy pipelines (`gen.lazy.map { ... }.first(n)`): the yielder
  raises a break signal once the requested count is reached, unwinding the loop
  (the same early-stop mechanism as endless-range `.lazy`). `Array#cycle(n)`
  (block-less) returns a finite Enumerator over the elements repeated `n` times.
  Limits: external iteration (`next`/`peek`) materializes the block to completion
  on first use, so it works only for *finite* generators — an infinite
  generator's `.next` still runs forever (routing it through a `Fiber` is a
  planned follow-on, now that the Fiber engine exists — see below); and
  block-less endless `cycle` (no count) stays `nil` (it would need an infinite
  external-iteration enumerator).
- **Fiber (stackful coroutines).** `Fiber.new { |first| ... }`, `#resume(*args)`,
  `Fiber.yield(v)`, and `#alive?` are implemented on `corosensei` same-thread
  stackful coroutines: a fiber freezes its entire native stack — including the
  in-flight fusevm `VM::run()` driving its block — onto an alternate stack, so
  `Fiber.yield` suspends *below* the VM (fusevm needs no suspend/resume API) and
  the coroutine shares the process-global object heap with its resumer. `resume`
  threads a value in as `Fiber.yield`'s return (and as the block's first
  parameter on the initial resume); the block's final value is the last
  `resume`'s result, and side effects fire lazily at the real yield boundaries.
  Resuming a fiber whose block has returned raises `FiberError` (dead fiber);
  `Fiber.yield` at the root raises `FiberError`. Each fiber runs its own `VM`
  instance and its own volatile execution context (scope/signal/frames), swapped
  at every resume/suspend boundary, so fibers are isolated and nest correctly.
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
  on a raise). Not yet: `Queue`/`ConditionVariable` and blocking-op safepoints (a
  `Queue#pop` on empty cannot yet block-and-wake a producer), and
  `report_on_exception`'s stderr warning is not emitted. Fibers remain
  thread-owned (a fiber is resumed only on its creating thread, as in MRI).
- **Bignum.** Integers auto-promote to arbitrary precision on overflow, like
  MRI: values that fit stay `i64` immediates, and only the overflow path
  allocates a `BigInt` heap object (backed by `num-bigint`). Arithmetic, bit
  ops, `**`, comparison, `to_s(base)`, `bit_length`, and `digits` all cross the
  boundary transparently.
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
- **`Time` is UTC-only.** `Time.at`, `Time.utc`/`Time.gm`, and `Time.now`
  construct times; the field readers (`year`/`month`/`day`/`hour`/`min`/`sec`/
  `wday`/`yday`), `to_i`/`to_f`, `to_s`/`inspect`, `strftime` (common directives
  plus the `-`/`_`/`0` padding flags), arithmetic (`Time - Time → Float`,
  `Time ± Numeric → Time`) and comparison/sort all work, with a dependency-free
  proleptic-Gregorian calendar (valid for negative epochs too). The
  local-timezone offset is **not** modeled — there is no tz database, so
  `.utc`/`.getutc` are exact and `.localtime`/`Time.local` behave as UTC.
  Timezone-aware `strftime` is not modeled (`%Z` always prints `UTC`, `%z` always
  `+0000`).
- **`Date`** (available without `require "date"`, which is accepted as a no-op).
  `Date.new`/`civil`, `Date.today`, `Date.jd`, and `Date.parse` (ISO
  `YYYY-MM-DD` / `YYYY/MM/DD` only — MRI's lenient free-form parsing is not
  modeled) construct dates. Field readers (`year`/`month`/`day`/`wday`/`yday`/
  `cwday`/`jd`/`leap?`), `to_s`/`iso8601`, `inspect` (Julian-day form),
  `strftime`, day/month/year arithmetic (`+`/`-`/`next_day`/`prev_day`/
  `next_month`/`prev_month`/`>>`/`<<` with last-day clamping), `Date - Date →
  Rational`, and comparison/sort all work over the same proleptic-Gregorian
  calendar as `Time`. `Date#>>`-with-fractional and locale-aware formatting are
  not implemented.
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
  first). Because strings are UTF-8 (`String`, not a byte buffer), a binary string
  is modeled with the Latin-1 convention: a "byte" is a code point in
  `U+0000..=U+00FF` (its low 8 bits). `pack` produces such a string and `unpack`
  reads it back the same way, so any `pack`-produced binary string round-trips
  (`bytes.pack("C*").unpack("C*")`, `(0..255).to_a.pack("C*").unpack("C*")`), and
  `Integer#chr` (`n & 0xff → U+00nn`) round-trips through `unpack("C*")` too. Two
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
  `atan2`/`sinh`/`cosh`/`tanh`/`exp`/`log`(with optional base)/`log2`/`log10`/
  `hypot`/`ldexp` and the constants `Math::PI` / `Math::E` are implemented over
  `f64`. `Math.gamma` (Lanczos) and `Math.erf`/`erfc` (Abramowitz-Stegun
  7.1.26) are implemented approximately: gamma to ~1e-9, erf to ~1.5e-7. These
  do NOT match MRI's libm output in the trailing digits, so they are excluded
  from the parity corpus and tested only with a tolerance. `Math.class` reports
  `Class` rather than `Module` (modules aren't distinguished from classes yet).
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
  arrays/scalars; malformed input raises `JSON::ParserError` (catchable via bare
  `rescue` / `rescue => e`; the `JSON::ParserError` constant is not registered for
  explicit-constant rescue). `Rational`/`Complex`/`Time` encode as their quoted
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
  `==`, so `case`/`when` over ranges and classes works.
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
(`Digest::MD5.new.update(...).hexdigest`) is not implemented — only the
class-level one-shot `hexdigest`/`digest`/`base64digest`. (3) A setter-symbol
literal (`:x=`) does not parse (pre-existing lexer gap); use a String
(`os.respond_to?("a=")`) or a non-setter symbol.

## File / IO / Dir

Backed by `std::fs`/`std::io`, verified method-by-method against the reference
`ruby`. The `std::fs::File` is stored in a host side table (`io_handles`),
indexed by an `RObj::IoHandle` — the same non-`Clone`-in-a-`Clone`-enum pattern
`Fiber` uses for its coroutine. `STDOUT`/`STDERR`/`STDIN` (and the matching
`$stdout`/`$stderr`/`$stdin` globals) are pre-seeded stream handles.

**Working, output-matched to MRI:** `File.read`/`write` (write returns the byte
count), `exist?`/`exists?`/`file?`/`directory?`/`size`/`delete`/`unlink`,
`readlines`/`foreach`, `open(path, mode)` (block form yields the IO and closes
it on exit, returning the block value; block-less returns the open IO); the
pure path helpers `basename` (incl. `suffix` and `.*` extension strip),
`dirname`, `extname` (MRI edge cases: leading-dot names, trailing dot →`"."`,
all-dots →`""`), `join`, `expand_path` (lexical `~`/`.`/`..` resolution against
an explicit base or the cwd). IO instance: `read`/`write`/`gets`/`puts`/
`print`/`<<`/`each_line`/`each`/`readlines`/`close`/`closed?`/`flush`/`inspect`
(`#<File:/path>`, `#<File:/path (closed)>`, `#<IO:<STDOUT>>`). `Dir.pwd`,
`glob`/`[]` (sorted per MRI ≥3.0; leading-dot files excluded from `*`;
`{a,b}` brace alternation expanded and concatenated in brace order),
`entries` (incl. `.`/`..`), `exist?`/`exists?`, `mkdir`/`rmdir`, `chdir`
(block form restores the cwd), `home`. `Kernel#open(path, mode)` delegates to
`File.open`.

**Known gaps / divergences:**
- **No `Errno` hierarchy.** Filesystem failures raise a single
  `SystemCallError` carrying the OS message, not the specific `Errno::ENOENT` /
  `Errno::EEXIST` MRI raises. The error is still a rescuable `StandardError`
  descendant; only the class name differs.
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
- **No `Errno` hierarchy.** Connect/bind failures raise a single `SocketError`
  carrying the OS message, not the specific `Errno::ECONNREFUSED` /
  `Errno::EADDRINUSE` MRI raises. Still a rescuable `StandardError` descendant;
  only the class name differs.
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
- **`__dir__`** returns the directory of the file currently running (from the
  same file-dir stack), a String; under `-e`/stdin it returns the seeded current
  directory (MRI returns the relative `"."` there).
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
  shared main object rather than a per-file binding. The punctuation globals
  `$"` and `$:` can't appear inside a `#{...}` string interpolation (the interp
  scanner reads the quote/colon as a delimiter — the same pre-existing limitation
  noted for `` $` ``/`$'`); reference them outside interpolation.


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
