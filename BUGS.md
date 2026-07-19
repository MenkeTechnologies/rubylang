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
  sources are eagerly materialized (faithful for everything except endless
  generators, which are not modeled).
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

## Tooling

- **DAP stepping.** The adapter completes the initialize/launch handshake and
  runs the program to completion with stdout capture, but breakpoints and
  stepping are not wired yet.
- **AOP weave.** `before`/`after`/`around` advice registered via the Ruby-facing
  `intercept(pattern, kind, handler)` builtin now fires from the `run_method`
  dispatch choke point: `before` runs pre-call with the call args, `after` runs
  post-call with the result (observe-only), `around` runs post-call and replaces
  the result. Weaving is gated on an O(1) `intercepts::any()` check, so calls with
  no registered advice are unaffected, and a reentrancy guard prevents handlers
  from advising themselves. Not yet supported: true "call the original from inside
  the handler" wrapping — the registry stores only a handler name and there is no
  native proc that re-invokes the original body, so `around` is a post-transform
  rather than a sandwich. Adding that needs an original-as-block substrate.
