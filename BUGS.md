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
`def self.m(x) = …`); `module` + `include` mixins; class methods (`def self.m`); `begin`/
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

- **`extend` / `prepend` / `class << self`.** Only `include` and `def self.m` are
  modeled; other mixin/singleton forms are not. `super` resolves through the
  superclass chain (module-`super` ordering is approximate).
- **Class-body statements** run at definition time with `self` bound to the
  class, so `def`, `attr_*`, `include`, class variables (`@@x = 0`), constants,
  and other executable statements all take effect. (Constants are stored
  globally rather than namespaced under the class.)
- **Keyword param named after a reserved word** (`def f(class: "x")`) is not
  parsed — the parser rejects `class`/`def`/`end`/etc. as a parameter name. A
  paren-less call *carrying* keyword args (`greet name: "x"`, `opts 9, **h`,
  `total *a`) does parse.
- **Numeric literal / method binding.** `-7.abs` parses as `-(7.abs)` (operator
  precedence) rather than `(-7).abs`; MRI treats `-7` as a literal. Use
  `(-7).abs`.
- **Expression-level modifier `rescue`.** `x = expr rescue fallback` binds the
  rescue at statement level (`(x = expr) rescue fallback`) rather than MRI's
  `x = (expr rescue fallback)`. A standalone `stmt rescue stmt` works.

## Lexer

- **Not lexed:** `__END__`. Heredocs (`<<END`, `<<~SQL`, `<<-EOT`, `<<'RAW'`),
  `%w[]` / `%i[]` word/symbol arrays (and the `()`/`{}`/`<>` delimiter variants),
  double-quoted `#{}` interpolation, `?c` character literals, regex literals
  (`/pat/flags`, with `i`/`m`/`x` flags), radix integer literals
  (`0b1010` binary, `0o17`/`017` octal, `0xff` hex, `0d99` decimal, with `_`
  separators), and the `%q`/`%Q`/`%r`/`%s` percent literals (single/double-
  quoted string, Regexp, Symbol — any punctuation delimiter, with `()`/`{}`/
  `[]`/`<>` nesting) **are** lexed. Not yet: the bare `%(…)` string form (its
  disambiguation from the modulo operator is skipped; use `%Q(…)`), and
  `#{`/`#@`-escaping in `String#inspect` for a literal `#{` in a string.

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
  `.utc`/`.getutc` are exact and `.localtime`/`Time.local` behave as UTC. `Date`/
  `DateTime` and timezone-aware `strftime` (`%Z` always prints `UTC`, `%z` always
  `+0000`) are not implemented.
- **`Date`** (available without `require "date"`, which is accepted as a no-op).
  `Date.new`/`civil`, `Date.today`, `Date.jd`, and `Date.parse` (ISO
  `YYYY-MM-DD` / `YYYY/MM/DD` only — MRI's lenient free-form parsing is not
  modeled) construct dates. Field readers (`year`/`month`/`day`/`wday`/`yday`/
  `cwday`/`jd`/`leap?`), `to_s`/`iso8601`, `inspect` (Julian-day form),
  `strftime`, day/month/year arithmetic (`+`/`-`/`next_day`/`prev_day`/
  `next_month`/`prev_month`/`>>`/`<<` with last-day clamping), `Date - Date →
  Rational`, and comparison/sort all work over the same proleptic-Gregorian
  calendar as `Time`. `DateTime`, `Date#>>`-with-fractional, and locale-aware
  formatting are not implemented.
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
  `f64`. `Math.gamma`/`erf` are not (they need approximations that would not
  match MRI bit-for-bit). `Math.class` reports `Class` rather than `Module`
  (modules aren't distinguished from classes yet).
- **`rand`.** Seeded from the system clock (no `srand` determinism yet).
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
  patterns (`Integer`, `Point[...]`), variable/`_` binding, `=> name`, `^pin`,
  `|` alternatives, `*rest` splats, and `if`/`unless` guards work. Not yet: the
  two-sided find pattern (`[*, x, *]`), `**nil` exact-key enforcement, and
  variable bindings *inside* an `|` alternative.

## Tooling

- **DAP stepping.** The adapter completes the initialize/launch handshake and
  runs the program to completion with stdout capture, but breakpoints and
  stepping are not wired yet.
- **AOP weave.** The method-intercept registry (`src/intercepts.rs`) matches and
  stores advice, but the dispatch loop does not yet fire it — the fast path stays
  fast until the feature is turned on explicitly.
