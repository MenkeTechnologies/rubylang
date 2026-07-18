# Known gaps

rubyrs is in active development. The pipeline (lex → parse → lower to fusevm
bytecode → run) is solid for the implemented surface, but Ruby is large. This
file tracks what is deliberately not done yet, so the gaps are honest rather than
surprising. None of these are faked as working — an unimplemented method raises
`undefined method`.

## Language

- **User-defined classes / modules.** `class`/`module` keywords are lexed but not
  yet lowered; there is no user object model, inheritance, `initialize`, `self`
  binding, or `attr_*`. Instance variables currently resolve against a single
  top-level object.
- **Exceptions.** `begin`/`rescue`/`ensure`/`raise` — `raise` produces a runtime
  error that aborts the program, but `rescue` does not yet catch it; `begin`
  bodies run but rescue/ensure clauses are parsed and dropped.
- **Numeric literal / method binding.** `-7.abs` parses as `-(7.abs)` (operator
  precedence) rather than `(-7).abs`; MRI treats `-7` as a literal. Use `(-7).abs`
  for the literal form.
- **Command-call with a leading `-`/`+`.** `puts -5` parses as `puts - 5`
  (binary) because the space heuristic excludes unary sign tokens as command
  arguments. Use `puts(-5)`.
- **Method default arguments.** Parsed, but a missing argument binds `nil` rather
  than evaluating the default expression.
- **Multiple assignment / splat / keyword args.** `a, b = 1, 2` (multiple
  assignment), `*rest`/`**kwargs` parameters, and `&block` capture are not yet
  supported. (A single Array passed to a multi-param *block* IS auto-splatted.)

## Lexer

- **Not lexed:** heredocs (`<<~`, `<<-`), `%w[]` / `%i[]` word/symbol arrays,
  regex literals (`/.../`), `?c` character literals, `__END__`. Double-quoted
  `#{}` interpolation **is** supported.

## Runtime / methods

- **Regexp.** No `Regexp` type; `String#sub`/`gsub` do literal (non-regex)
  replacement only.
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
