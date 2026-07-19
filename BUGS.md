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
  module) reaches the next method correctly. Not modeled: runtime instance
  `obj.extend(M)`, and `super` from inside a class method.
- **Class-level instance variables** (`@n` inside a `def self.m` / an `extend`ed
  method) do not persist across calls — the class object has no per-instance
  variable store yet, so `@n ||= 0; @n += 1` restarts each call.
- **Class-body statements** run at definition time with `self` bound to the
  class, so `def`, `attr_*`, `include`, class variables (`@@x = 0`), constants,
  and other executable statements all take effect. (Constants are stored
  globally rather than namespaced under the class.)
- **Modifier `rescue` inside call-args / array literals.** Numeric-literal
  binding (`-7.abs` → `(-7).abs`, with `-2**2` → `-(2**2)`) and modifier
  `rescue` precedence (`x = a rescue b` → `x = (a rescue b)`, plus grouping
  parens and statement-level) both match MRI now. The one residual divergence:
  MRI rejects a bare modifier `rescue` directly in method-call arguments
  (`p(1/0 rescue 5)`) and array elements (`[1/0 rescue 5]`); this parser accepts
  them (a permissive superset — no valid MRI program breaks).

## Metaprogramming / reflection / eval

Implemented and verified against the reference `ruby`:

- **Singleton methods.** `def obj.m` and `def Klass.m` parse and run: an object
  receiver stores a per-object singleton method; a class receiver registers a
  class method (identical to `def self.m`). `class << obj … end` on an instance
  works the same way. Singleton methods take priority over the class's own
  instance methods in dispatch (matching `Module#ancestors` order).
- **`const_missing`.** `Mod::Const` for an unresolved constant calls
  `Mod.const_missing(:Const)` when the class/module defines it (the hook Rails
  autoloading relies on).
- **Definition hooks.** `inherited(subclass)` fires when a subclass is opened;
  `included`/`extended`/`prepended(base)` fire when the corresponding mixin
  relationship is established. Each fires only if the module/class defines the
  hook as a class method.
- **Constant reflection.** `const_get` / `const_set` / `const_defined?` /
  `constants` on any class or module ref, including builtin class names
  (`Object.const_get(:String)`).
- **Top-level `self`.** Fixed to `main`, an ordinary `Object`, so
  `self.class.name == "Object"` (was `"NilClass"`). Top-level instance variables
  now live on that object.
- **`eval` / `class_eval` / `instance_eval` / `instance_exec`.** `eval("code")`
  compiles and runs the string on the current host (methods/classes/constants it
  defines persist), returning the last value. `Module#class_eval`/`module_eval`
  (block or string) runs with `self` = the class, so a bare `def` defines an
  instance method. `Object#instance_eval`/`instance_exec` (block or string) runs
  with `self` = the receiver: `@ivar` hits the receiver, and a bare `def` defines
  a singleton on it (a class method when the receiver is a class). `instance_exec`
  forwards its arguments to the block.

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
- **Constants remain a flat, global store.** `const_get`/`const_set`/`constants`
  operate on that flat store rather than per-module namespaces, so a nested path
  (`Mod.const_get("A::B")`) resolves by its last segment and `constants` lists all
  user-defined constant names rather than only a given module's.
- **Hook firing order approximates MRI.** Hooks fire from the class-definition
  site in source order (`inherited` before the body, `included`/`extended`/
  `prepended` after), which is correct for observing *which* class triggered the
  hook; exact interleaving with surrounding output can differ because class
  bodies are otherwise hoisted.

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
  the coroutine shares the thread-local object heap with its resumer. `resume`
  threads a value in as `Fiber.yield`'s return (and as the block's first
  parameter on the initial resume); the block's final value is the last
  `resume`'s result, and side effects fire lazily at the real yield boundaries.
  Resuming a fiber whose block has returned raises `FiberError` (dead fiber);
  `Fiber.yield` at the root raises `FiberError`. Each fiber runs its own `VM`
  instance and its own volatile execution context (scope/signal/frames), swapped
  at every resume/suspend boundary, so fibers are isolated and nest correctly.
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
- **`Array#pack` / `String#unpack`** are not implemented: strings are UTF-8
  (`String`, not a byte buffer), so the binary directives (`N`/`n`/`V`/`v`/`H`,
  high `C` bytes) cannot round-trip. This is a durable encoding limitation, not a
  temporary gap.
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
- **Limitations.** `require` does not use `RUBYLIB`, gem paths, or a real stdlib
  tree — only `$LOAD_PATH` (script dir + whatever the program pushes) plus the
  builtin no-op list. Autoload, `require` of a `.so`/`.bundle`, and thread-safe
  concurrent require are out of scope. A required file's top-level *locals* are
  isolated from the caller's (MRI-faithful), but its top-level `self` is the same
  shared main object rather than a per-file binding. The punctuation globals
  `$"` and `$:` can't appear inside a `#{...}` string interpolation (the interp
  scanner reads the quote/colon as a delimiter — the same pre-existing limitation
  noted for `` $` ``/`$'`); reference them outside interpolation.


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
