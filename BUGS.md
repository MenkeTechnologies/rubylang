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
chaining; `module` + `include` mixins; class methods (`def self.m`); `begin`/
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
`each_with_object`, `transform_values`, …); blocks/`yield`/closures with lexical
capture.

## Language

- **`extend` / `prepend` / `class << self`.** Only `include` and `def self.m` are
  modeled; other mixin/singleton forms are not. `super` resolves through the
  superclass chain (module-`super` ordering is approximate).
- **Class-body statements.** Only `def`, `attr_*`, and `include` in a class body
  take effect; constants and other executable statements are ignored.
- **Explicit `&block` parameter.** An explicit `&blk` block parameter (and
  calling it via `blk.call`) is not supported; use `yield`/`block_given?`.
  (Keyword params, `**opts` collectors, `**hash` keyword splat, `*rest` splat,
  call-site splat, and `&:sym` block-pass all are.) A block cannot be combined
  with call-site splat in one call yet; paren-less keyword args (`greet name:
  "x"`) are not parsed.
- **Numeric literal / method binding.** `-7.abs` parses as `-(7.abs)` (operator
  precedence) rather than `(-7).abs`; MRI treats `-7` as a literal. Use
  `(-7).abs`.
- **Expression-level modifier `rescue`.** `x = expr rescue fallback` binds the
  rescue at statement level (`(x = expr) rescue fallback`) rather than MRI's
  `x = (expr rescue fallback)`. A standalone `stmt rescue stmt` works.

## Lexer

- **Not lexed:** heredocs (`<<~`, `<<-`), regex literals (`/.../`), `?c` character
  literals, `__END__`. `%w[]` / `%i[]` word/symbol arrays (and the `()`/`{}`/`<>`
  delimiter variants) **are** lexed, as is double-quoted `#{}` interpolation.

## Runtime / methods

- **Regexp.** No `Regexp` type; `String#sub`/`gsub` do literal (non-regex)
  replacement only.
- **Enumerator without block.** `each_with_index.map` (an enumerator returned
  from a block-less call) is not supported. (`&:sym` block-pass IS.)
- **Bignum.** Integers are `i64`; there is no automatic promotion to arbitrary
  precision on overflow (unlike MRI).
- **`rand`.** Seeded from the system clock (no `srand` determinism yet).
- **Method surface.** The Enumerable/String/Hash/Range surface is broad but not
  exhaustive; an unimplemented method raises `undefined method '<name>'`.

## Tooling

- **DAP stepping.** The adapter completes the initialize/launch handshake and
  runs the program to completion with stdout capture, but breakpoints and
  stepping are not wired yet.
- **AOP weave.** The method-intercept registry (`src/intercepts.rs`) matches and
  stores advice, but the dispatch loop does not yet fire it — the fast path stays
  fast until the feature is turned on explicitly.
