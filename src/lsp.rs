//! Language Server Protocol over stdio (`ruby --lsp`).
//!
//! Self-contained and read-only: diagnostics come from the same `parser::parse`
//! the runtime uses (a syntax error maps to the reported line); hover and
//! completion draw on the builtin method/kernel corpus. No output ever reaches
//! the terminal — JSON-RPC on stdio only. Structure follows the sibling `-rs`
//! interpreters' `lsp.rs`.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, ExtractError, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

/// The builtin method / kernel corpus: (name, class/module chapter, one-line doc).
/// Single source of truth shared by LSP completion/hover and the offline
/// `gen-docs` reference generator, so the two never drift. Each entry mirrors a
/// real dispatch arm in `builtins.rs`.
const CORPUS: &[(&str, &str, &str)] = &[
    // ── Keyword ── (the reserved words the lexer recognizes: KEYWORDS in lexer.rs, plus `defined?`)
    ("def", "Keyword", "define a method; body runs on call, returns the last expression"),
    ("end", "Keyword", "close a def/class/module/do/if/case/begin block"),
    ("class", "Keyword", "open or reopen a class; `class A < B` sets the superclass"),
    ("module", "Keyword", "open or reopen a module (namespace / mixin)"),
    ("self", "Keyword", "the current receiver object"),
    ("super", "Keyword", "call the same method in the superclass (bare = pass same args)"),
    ("if", "Keyword", "conditional; also the `expr if cond` statement modifier"),
    ("elsif", "Keyword", "additional condition branch inside an if"),
    ("else", "Keyword", "fallback branch of an if/unless/case/begin"),
    ("unless", "Keyword", "negated conditional; also the `expr unless cond` modifier"),
    ("case", "Keyword", "multi-way branch matched by `when` via `===`"),
    ("when", "Keyword", "a case branch; matches its value against the subject with `===`"),
    ("while", "Keyword", "loop while the condition is truthy; also a statement modifier"),
    ("until", "Keyword", "loop until the condition is truthy; also a statement modifier"),
    ("for", "Keyword", "iterate `for x in enumerable` without a new scope"),
    ("in", "Keyword", "the `for x in …` separator and case/in pattern-match clause"),
    ("do", "Keyword", "open a block (`each do |x| … end`) or a while/for body"),
    ("then", "Keyword", "optional separator after an if/when condition"),
    ("yield", "Keyword", "invoke the block passed to the current method"),
    ("return", "Keyword", "return a value from the current method (nil if omitted)"),
    ("break", "Keyword", "exit the nearest loop/block, optionally with a value"),
    ("next", "Keyword", "skip to the next loop/block iteration, optionally with a value"),
    ("retry", "Keyword", "re-run the begin body from a rescue clause"),
    ("begin", "Keyword", "open an exception-handling block (rescue/ensure/else)"),
    ("rescue", "Keyword", "handle a raised exception; also the `expr rescue fallback` modifier"),
    ("ensure", "Keyword", "block that always runs whether or not an exception was raised"),
    ("and", "Keyword", "low-precedence logical AND (short-circuits)"),
    ("or", "Keyword", "low-precedence logical OR (short-circuits)"),
    ("not", "Keyword", "low-precedence logical negation"),
    ("nil", "Keyword", "the sole NilClass instance; the only falsey object besides false"),
    ("true", "Keyword", "the sole TrueClass instance"),
    ("false", "Keyword", "the sole FalseClass instance (falsey)"),
    ("alias", "Keyword", "give a method a second name: `alias new old`"),
    ("defined?", "Keyword", "describe the expression's kind (\"method\"/\"expression\"/…) or nil"),
    // ── Kernel ──
    ("puts", "Kernel", "print each arg (arrays recursed) each on its own line"),
    ("print", "Kernel", "print args joined by $, terminated by $\\ (both nil default)"),
    ("p", "Kernel", "print inspect of each arg; return arg/array/nil"),
    ("pp", "Kernel", "alias of p (no pretty-printing)"),
    ("require", "Kernel", "no-op: always returns true (no file loaded)"),
    ("require_relative", "Kernel", "no-op: always returns true (no file loaded)"),
    ("load", "Kernel", "no-op: always returns true (no file loaded)"),
    ("intercept", "Kernel", "register AOP before/after/around advice by glob pattern"),
    ("raise", "Kernel", "raise by class/message/instance (default RuntimeError)"),
    ("fail", "Kernel", "raise by class/message/instance (default RuntimeError)"),
    ("rand", "Kernel", "random Int in [0,n) or Float in [0,1)"),
    ("sleep", "Kernel", "no-op: returns 0 without sleeping"),
    ("srand", "Kernel", "reseed PRNG; return the previous seed"),
    ("Integer", "Kernel", "convert to Integer, optional base for string args"),
    ("Float", "Kernel", "convert numeric/string to Float, else raise"),
    ("Rational", "Kernel", "build Rational(num,den); raise on zero denominator"),
    ("Complex", "Kernel", "build Complex from real and imaginary args"),
    ("String", "Kernel", "convert arg via to_s to a String"),
    ("Array", "Kernel", "wrap arg as Array (hash->pairs, nil->[], scalar->[x])"),
    ("format", "Kernel", "sprintf-format string; trailing Hash for named refs"),
    ("sprintf", "Kernel", "sprintf-format string; trailing Hash for named refs"),
    ("gets", "Kernel", "read one line from stdin (nil at EOF)"),
    ("proc", "Kernel", "return the given block as a Proc"),
    ("lambda", "Kernel", "return block marked as a lambda"),
    ("loop", "Kernel", "loop block forever; rescue StopIteration; break value"),
    ("catch", "Kernel", "run block with tag; return thrown value if throw matches"),
    ("throw", "Kernel", "unwind to matching catch(tag) with optional value"),
    ("block_given?", "Kernel", "true if a block is present in the current call"),
    ("exit", "Kernel", "exit process (true/nil->0, false->1, n->n)"),
    ("exit!", "Kernel", "exit process (true/nil->0, false->1, n->n)"),
    ("abort", "Kernel", "write optional msg to stderr and exit 1"),
    // ── Object ──
    ("to_s", "Object", "host default string of receiver (only when args are empty)"),
    ("inspect", "Object", "host inspect string of receiver"),
    ("class", "Object", "returns the Class object reference, not its name"),
    ("nil?", "Object", "true only for nil (Value::Undef)"),
    ("to_json", "Object", "JSON-encodes any value; ignores optional generator-state arg"),
    ("==", "Object", "host structural equality"),
    ("!=", "Object", "negation of =="),
    ("===", "Object", "case-equality: Class/Regexp/Range/Float-range membership, else =="),
    ("is_a?", "Object", "true if receiver is an instance of the named class/module"),
    ("kind_of?", "Object", "true if receiver is an instance of the named class/module"),
    ("instance_of?", "Object", "true if receiver's exact class equals the argument"),
    ("itself", "Object", "returns the receiver unchanged"),
    ("freeze", "Object", "records receiver as frozen and returns it; not enforced"),
    ("equal?", "Object", "object identity: same heap handle or same immediate value"),
    ("object_id", "Object", "stable identity integer (2n+1 for ints, fixed immediates)"),
    ("__id__", "Object", "stable identity integer (2n+1 for ints, fixed immediates)"),
    ("dup", "Object", "shallow copy, never frozen"),
    ("clone", "Object", "shallow copy preserving the original's frozen state"),
    ("frozen?", "Object", "reports whether receiver is frozen"),
    ("instance_variable_get", "Object", "reads ivar by name (@ prefix optional)"),
    ("instance_variable_set", "Object", "sets ivar by name and returns the value"),
    ("instance_variables", "Object", "ivar names as symbols"),
    ("tap", "Object", "yields receiver to block, returns receiver"),
    ("then", "Object", "passes receiver to block, returns block result (else receiver)"),
    ("yield_self", "Object", "passes receiver to block, returns block result (else receiver)"),
    ("lazy", "Object", "wraps enumerable in lazy pipeline; non-range/array becomes array"),
    ("methods", "Object", "user-defined instance method names as symbols; builtins omitted"),
    ("send", "Object", "dispatches named method with remaining args (no visibility check)"),
    ("__send__", "Object", "dispatches named method with remaining args (no visibility check)"),
    ("public_send", "Object", "dispatches named method with remaining args (no visibility check)"),
    ("method", "Object", "captures a bound Method object for the named method"),
    ("respond_to?", "Object", "true if class/respond_to_missing? defines it; builtins permissive"),
    ("method_missing", "Object", "wildcard: forwards any undefined method to user method_missing"),
    // ── Comparable ──
    ("<", "Comparable", "true if <=> is negative; incomparable raises ArgumentError"),
    ("<=", "Comparable", "true if <=> is <= 0; incomparable raises ArgumentError"),
    (">", "Comparable", "true if <=> is positive; incomparable raises ArgumentError"),
    (">=", "Comparable", "true if <=> is >= 0; incomparable raises ArgumentError"),
    ("between?", "Comparable", "true if lo <= self <= hi via <=>"),
    ("clamp", "Comparable", "confines self to [lo, hi] via <=>"),
    // ── Enumerable ──
    ("map", "Enumerable", "materialize via each; delegates to Array#map"),
    ("collect", "Enumerable", "materialize via each; delegates to Array#collect"),
    ("flat_map", "Enumerable", "materialize via each; delegates to Array#flat_map"),
    ("collect_concat", "Enumerable", "materialize via each; delegates to Array#collect_concat"),
    ("select", "Enumerable", "materialize via each; delegates to Array#select"),
    ("filter", "Enumerable", "materialize via each; delegates to Array#filter"),
    ("filter_map", "Enumerable", "materialize via each; delegates to Array#filter_map"),
    ("reject", "Enumerable", "materialize via each; delegates to Array#reject"),
    ("reduce", "Enumerable", "materialize via each; delegates to Array#reduce"),
    ("inject", "Enumerable", "materialize via each; delegates to Array#inject"),
    ("to_a", "Enumerable", "materialize via each; delegates to Array#to_a"),
    ("entries", "Enumerable", "materialize via each; delegates to Array#entries"),
    ("find", "Enumerable", "materialize via each; delegates to Array#find"),
    ("detect", "Enumerable", "materialize via each; delegates to Array#detect"),
    ("find_index", "Enumerable", "materialize via each; delegates to Array#find_index"),
    ("count", "Enumerable", "materialize via each; delegates to Array#count"),
    ("min", "Enumerable", "materialize via each; delegates to Array#min"),
    ("max", "Enumerable", "materialize via each; delegates to Array#max"),
    ("minmax", "Enumerable", "materialize via each; delegates to Array#minmax"),
    ("min_by", "Enumerable", "materialize via each; delegates to Array#min_by"),
    ("max_by", "Enumerable", "materialize via each; delegates to Array#max_by"),
    ("sort", "Enumerable", "materialize via each; delegates to Array#sort"),
    ("sort_by", "Enumerable", "materialize via each; delegates to Array#sort_by"),
    ("sum", "Enumerable", "materialize via each; delegates to Array#sum"),
    ("include?", "Enumerable", "materialize via each; delegates to Array#include?"),
    ("member?", "Enumerable", "materialize via each; delegates to Array#member?"),
    ("first", "Enumerable", "materialize via each; delegates to Array#first"),
    ("take", "Enumerable", "materialize via each; delegates to Array#take"),
    ("drop", "Enumerable", "materialize via each; delegates to Array#drop"),
    ("take_while", "Enumerable", "materialize via each; delegates to Array#take_while"),
    ("drop_while", "Enumerable", "materialize via each; delegates to Array#drop_while"),
    ("each_with_index", "Enumerable", "materialize via each; delegates to Array#each_with_index"),
    ("each_with_object", "Enumerable", "materialize via each; delegates to Array#each_with_object"),
    ("group_by", "Enumerable", "materialize via each; delegates to Array#group_by"),
    ("partition", "Enumerable", "materialize via each; delegates to Array#partition"),
    ("tally", "Enumerable", "materialize via each; delegates to Array#tally"),
    ("uniq", "Enumerable", "materialize via each; delegates to Array#uniq"),
    ("zip", "Enumerable", "materialize via each; delegates to Array#zip"),
    ("any?", "Enumerable", "materialize via each; delegates to Array#any?"),
    ("all?", "Enumerable", "materialize via each; delegates to Array#all?"),
    ("none?", "Enumerable", "materialize via each; delegates to Array#none?"),
    ("one?", "Enumerable", "materialize via each; delegates to Array#one?"),
    ("each_slice", "Enumerable", "materialize via each; delegates to Array#each_slice"),
    ("each_cons", "Enumerable", "materialize via each; delegates to Array#each_cons"),
    ("chunk_while", "Enumerable", "materialize via each; delegates to Array#chunk_while"),
    ("to_h", "Enumerable", "materialize via each; delegates to Array#to_h"),
    // ── Class ──
    ("new", "Class", "Struct.new defines a new struct class; other classes instantiate"),
    ("[]", "Class", "Struct/Set/Hash/Array [...] class constructor forms"),
    ("members", "Class", "Struct class: member names as symbols"),
    ("yield", "Class", "Fiber.yield: suspends the running fiber with a value"),
    ("at", "Class", "Time.at: time from epoch seconds (UTC)"),
    ("utc", "Class", "Time.utc: build UTC time from broken-down fields"),
    ("gm", "Class", "Time.gm: build UTC time from broken-down fields"),
    ("local", "Class", "Time.local: build UTC time from fields (local tz not modeled)"),
    ("mktime", "Class", "Time.mktime: build UTC time from fields (local tz not modeled)"),
    ("now", "Class", "Time.now/DateTime.now: current system time"),
    ("civil", "Class", "Date/DateTime.civil: date(-time) from y,m,d[,h,m,s]"),
    ("today", "Class", "Date.today: today's date from the system clock (UTC)"),
    ("jd", "Class", "Date/DateTime.jd: from a Julian Day Number"),
    ("parse", "Class", "Date/DateTime.parse: ISO string, else raises ArgumentError"),
    ("PI", "Class", "Math::PI constant"),
    ("E", "Class", "Math::E constant"),
    ("sqrt", "Class", "Math.sqrt: square root"),
    ("cbrt", "Class", "Math.cbrt: cube root"),
    ("sin", "Class", "Math.sin"),
    ("cos", "Class", "Math.cos"),
    ("tan", "Class", "Math.tan"),
    ("asin", "Class", "Math.asin"),
    ("acos", "Class", "Math.acos"),
    ("atan", "Class", "Math.atan"),
    ("atan2", "Class", "Math.atan2(x, y)"),
    ("sinh", "Class", "Math.sinh"),
    ("cosh", "Class", "Math.cosh"),
    ("tanh", "Class", "Math.tanh"),
    ("asinh", "Class", "Math.asinh"),
    ("acosh", "Class", "Math.acosh"),
    ("atanh", "Class", "Math.atanh"),
    ("exp", "Class", "Math.exp: e**x"),
    ("log", "Class", "Math.log: natural log, or log base y when given 2 args"),
    ("log2", "Class", "Math.log2"),
    ("log10", "Class", "Math.log10"),
    ("hypot", "Class", "Math.hypot(x, y)"),
    ("ldexp", "Class", "Math.ldexp: x * 2**exp"),
    ("gamma", "Class", "Math.gamma"),
    ("erf", "Class", "Math.erf"),
    ("erfc", "Class", "Math.erfc: 1 - erf(x)"),
    ("generate", "Class", "JSON.generate/dump: encode value to JSON string"),
    ("dump", "Class", "JSON.generate/dump: encode value to JSON string"),
    ("pretty_generate", "Class", "JSON.pretty_generate: indented JSON string"),
    ("load", "Class", "JSON.parse/load: decode JSON; symbolize_names option"),
    ("name", "Class", "the class name string"),
    ("superclass", "Class", "direct superclass ref, or nil for BasicObject"),
    ("ancestors", "Class", "ancestor chain as class refs"),
    ("instance_methods", "Class", "instance method names as symbols; visibility not modeled"),
    ("public_instance_methods", "Class", "same as instance_methods; visibility not modeled"),
    ("method_defined?", "Class", "true if method defined on class or ancestor"),
    ("public_method_defined?", "Class", "same as method_defined?; visibility not modeled"),
    ("INFINITY", "Class", "Float::INFINITY"),
    ("NAN", "Class", "Float::NAN"),
    ("MAX", "Class", "Float::MAX"),
    ("MIN", "Class", "Float::MIN (smallest positive normal)"),
    ("EPSILON", "Class", "Float::EPSILON"),
    ("DIG", "Class", "Float::DIG, returns 15"),
    ("MANT_DIG", "Class", "Float::MANT_DIG, returns 53"),
    // ── TrueClass/FalseClass ──
    ("&", "TrueClass/FalseClass", "logical AND by truthiness"),
    ("|", "TrueClass/FalseClass", "logical OR by truthiness"),
    ("^", "TrueClass/FalseClass", "logical XOR by truthiness"),
    ("!", "TrueClass/FalseClass", "logical negation of receiver truthiness"),
    ("to_s", "TrueClass/FalseClass", "host to_s string of true/false/nil"),
    ("inspect", "TrueClass/FalseClass", "host to_s string of true/false/nil (same as to_s)"),
    ("to_a", "TrueClass/FalseClass", "nil.to_a returns [] (guarded to nil only)"),
    ("to_h", "TrueClass/FalseClass", "nil.to_h returns {} (guarded to nil only)"),
    // ── Integer ──
    ("to_s", "Integer", "renders bigint in given radix (default 10)"),
    ("inspect", "Integer", "renders bigint in given radix (default 10)"),
    ("to_i", "Integer", "returns self unchanged"),
    ("to_int", "Integer", "returns self unchanged"),
    ("floor", "Integer", "returns self unchanged (ignores any ndigits arg)"),
    ("ceil", "Integer", "returns self unchanged (ignores any ndigits arg)"),
    ("round", "Integer", "returns self unchanged (ignores any ndigits arg)"),
    ("truncate", "Integer", "returns self unchanged (ignores any ndigits arg)"),
    ("to_f", "Integer", "converts to Float, INFINITY when out of f64 range"),
    ("abs", "Integer", "absolute value"),
    ("magnitude", "Integer", "absolute value"),
    ("-@", "Integer", "arithmetic negation"),
    ("bit_length", "Integer", "number of bits in the value"),
    ("even?", "Integer", "true if divisible by 2"),
    ("odd?", "Integer", "true if not divisible by 2"),
    ("zero?", "Integer", "true if the value is 0"),
    ("positive?", "Integer", "true if greater than 0"),
    ("negative?", "Integer", "true if less than 0"),
    ("integer?", "Integer", "always true"),
    ("hash", "Integer", "i64 value, or bit_length as fallback when out of range"),
    ("succ", "Integer", "returns self + 1"),
    ("next", "Integer", "returns self + 1"),
    ("pred", "Integer", "returns self - 1"),
    ("digits", "Integer", "digits of abs(self) in base (default 10, min 2), low-first"),
    ("/", "Integer", "floored division; ZeroDivisionError on 0, nil if not Integer"),
    ("div", "Integer", "floored division; ZeroDivisionError on 0, nil if not Integer"),
    ("%", "Integer", "floored modulo; ZeroDivisionError on 0, nil if not Integer"),
    ("modulo", "Integer", "floored modulo; ZeroDivisionError on 0, nil if not Integer"),
    ("divmod", "Integer", "[floored quotient, floored modulo]; ZeroDivisionError on 0"),
    ("<=>", "Integer", "compare returning -1/0/1, nil when arg is not Integer"),
    ("coerce", "Integer", "returns [arg, self] without numeric conversion"),
    // ── Numeric ──
    ("times", "Numeric", "yields 0...n; block-less returns an Enumerator"),
    ("**", "Numeric", "exact Integer power (BigInt on overflow), else Float powf"),
    ("pow", "Numeric", "power; 2-arg form is modular exp, neg exp+mod raises RangeError"),
    ("/", "Numeric", "Int/Int floored div, Int/Rational exact, else Float; ZeroDivisionError"),
    ("%", "Numeric", "Int floored mod, else float x - floor(x/y)*y; ZeroDivisionError"),
    ("modulo", "Numeric", "Int floored mod, else float x - floor(x/y)*y; ZeroDivisionError"),
    ("step", "Numeric", "iterates by step; all-Int stays Int else Float; block-less Enumerator"),
    ("upto", "Numeric", "iterates self..limit by 1; block-less returns Enumerator"),
    ("downto", "Numeric", "iterates self down to limit by -1; block-less Enumerator"),
    ("to_s", "Numeric", "Integer in radix 2..=36, else default string form"),
    ("to_i", "Numeric", "truncates to Integer"),
    ("to_int", "Numeric", "truncates to Integer"),
    ("to_f", "Numeric", "converts to Float"),
    ("to_r", "Numeric", "Int as n/1, Float as exact rational; FloatDomainError on NaN/Inf"),
    ("rationalize", "Numeric", "simplest rational within eps (default round-trips the f64)"),
    ("to_c", "Numeric", "returns Complex (self + 0i)"),
    ("coerce", "Numeric", "[b, a] as Int when both Int, else both as Float"),
    ("abs", "Numeric", "absolute value, preserving Int/Float type"),
    ("magnitude", "Numeric", "absolute value, preserving Int/Float type"),
    ("abs2", "Numeric", "self * self, preserving Int/Float type"),
    ("even?", "Numeric", "true if as_i(self) % 2 == 0"),
    ("odd?", "Numeric", "true if as_i(self) % 2 != 0"),
    ("zero?", "Numeric", "true if value equals 0.0"),
    ("nonzero?", "Numeric", "returns self if non-zero, else nil"),
    ("positive?", "Numeric", "true if greater than 0"),
    ("negative?", "Numeric", "true if less than 0"),
    ("integer?", "Numeric", "true for Int, false for Float"),
    ("succ", "Numeric", "returns as_i(self) + 1"),
    ("next", "Numeric", "returns as_i(self) + 1"),
    ("pred", "Numeric", "returns as_i(self) - 1"),
    ("floor", "Numeric", "floor to optional ndigits; Float only when ndigits > 0"),
    ("ceil", "Numeric", "ceil to optional ndigits; Float only when ndigits > 0"),
    ("round", "Numeric", "round to optional ndigits; Float only when ndigits > 0"),
    ("truncate", "Numeric", "truncate to optional ndigits; Float only when ndigits > 0"),
    ("nan?", "Numeric", "true if Float is NaN (only dispatched for Float receiver)"),
    ("infinite?", "Numeric", "1/-1 if infinite Float, else nil"),
    ("finite?", "Numeric", "true if finite Float, true for Int"),
    ("divmod", "Numeric", "Int [floor div, floor mod] else [floor q as Int, Float rem]"),
    ("chr", "Numeric", "1-char string from low byte of self (u8 as char)"),
    ("gcd", "Numeric", "gcd(self, arg); ArgumentError if no arg"),
    ("lcm", "Numeric", "lcm(self, arg), 0 if either is 0; ArgumentError if no arg"),
    ("gcdlcm", "Numeric", "[gcd, lcm]; ArgumentError if no arg"),
    ("ceildiv", "Numeric", "ceiling division -floor_div(-a,b); ZeroDivisionError"),
    ("[]", "Numeric", "bit i of two's-complement; neg i -> 0, i>=64 -> sign bit"),
    ("digits", "Numeric", "digits in base (default 10, min 2); Math::DomainError if negative"),
    ("bit_length", "Numeric", "bit length of self, using ~n for negatives"),
    ("fdiv", "Numeric", "float division of self by arg"),
    ("clamp", "Numeric", "clamps to lo/hi or range bounds, returning bound's own type"),
    ("<=>", "Numeric", "compare as floats: -1/0/1, nil when unordered (NaN)"),
    ("<<", "Numeric", "left shift via BigInt (promotes on overflow); TypeError if non-Int"),
    (">>", "Numeric", "right shift via BigInt; TypeError if arg not Integer"),
    ("&", "Numeric", "bitwise AND via BigInt; TypeError if arg not Integer"),
    ("|", "Numeric", "bitwise OR via BigInt; TypeError if arg not Integer"),
    ("^", "Numeric", "bitwise XOR via BigInt; TypeError if arg not Integer"),
    ("between?", "Numeric", "true if arg0 <= self <= arg1 (compared as floats)"),
    ("+", "Numeric", "addition, Int/Int stays Int else Float"),
    ("-", "Numeric", "subtraction, Int/Int stays Int else Float"),
    ("*", "Numeric", "multiplication, Int/Int stays Int else Float"),
    ("<", "Numeric", "less-than comparison"),
    (">", "Numeric", "greater-than comparison"),
    ("<=", "Numeric", "less-or-equal comparison"),
    (">=", "Numeric", "greater-or-equal comparison"),
    // ── Rational ──
    ("numerator", "Rational", "Returns the numerator as a bigint."),
    ("denominator", "Rational", "Returns the denominator as a bigint."),
    ("to_f", "Rational", "Converts to Float (NAN if unrepresentable)."),
    ("to_i", "Rational", "Truncates toward zero to an integer."),
    ("to_int", "Rational", "Truncates toward zero to an integer."),
    ("truncate", "Rational", "Truncates toward zero to an integer (ignores digits arg)."),
    ("to_r", "Rational", "Returns self unchanged."),
    ("abs", "Rational", "Absolute value as a Rational."),
    ("magnitude", "Rational", "Absolute value as a Rational."),
    ("-@", "Rational", "Negates the rational."),
    ("+@", "Rational", "Returns self unchanged."),
    ("zero?", "Rational", "True if the value is zero."),
    ("positive?", "Rational", "True if the value is positive."),
    ("negative?", "Rational", "True if the value is negative."),
    ("/", "Rational", "Divides; ZeroDivisionError on 0, Float if arg not rational."),
    ("quo", "Rational", "Divides; ZeroDivisionError on 0, Float if arg not rational."),
    ("**", "Rational", "Power; integer exp exact (Rational), else Float via powf."),
    ("pow", "Rational", "Power; integer exp exact (Rational), else Float via powf."),
    ("<=>", "Rational", "Compares; returns -1/0/1 or nil if arg not rational."),
    ("coerce", "Rational", "Returns [other-as-rational, self]."),
    ("hash", "Rational", "Hash as the value's i64 (0 on overflow)."),
    ("integer?", "Rational", "True if the denominator is 1."),
    ("+", "Rational", "Addition via numeric hook."),
    ("-", "Rational", "Subtraction via numeric hook."),
    ("*", "Rational", "Multiplication via numeric hook."),
    ("==", "Rational", "Equality via numeric hook."),
    ("<", "Rational", "Less-than via numeric hook."),
    (">", "Rational", "Greater-than via numeric hook."),
    ("<=", "Rational", "Less-or-equal via numeric hook."),
    (">=", "Rational", "Greater-or-equal via numeric hook."),
    // ── Complex ──
    ("real", "Complex", "Returns the real part."),
    ("imaginary", "Complex", "Returns the imaginary part."),
    ("imag", "Complex", "Returns the imaginary part."),
    ("to_s", "Complex", "String form via host complex_to_s."),
    ("abs", "Complex", "Magnitude sqrt(re^2+im^2) as a Float."),
    ("magnitude", "Complex", "Magnitude sqrt(re^2+im^2) as a Float."),
    ("abs2", "Complex", "Squared magnitude re*re + im*im."),
    ("conjugate", "Complex", "Complex conjugate (negates imaginary part)."),
    ("conj", "Complex", "Complex conjugate (negates imaginary part)."),
    ("rectangular", "Complex", "Returns [real, imaginary] array."),
    ("rect", "Complex", "Returns [real, imaginary] array."),
    ("-@", "Complex", "Negates both real and imaginary parts."),
    ("+@", "Complex", "Returns self unchanged."),
    ("==", "Complex", "Equality via host eq_values."),
    ("+", "Complex", "Addition via numeric hook."),
    ("-", "Complex", "Subtraction via numeric hook."),
    ("*", "Complex", "Multiplication via numeric hook."),
    ("**", "Complex", "Power by repeated mul; raises on non-integer/negative exp."),
    ("pow", "Complex", "Power by repeated mul; raises on non-integer/negative exp."),
    // ── String ──
    ("length", "String", "char count of the string (alias size)"),
    ("size", "String", "char count of the string (alias length)"),
    ("upcase", "String", "uppercase; :ascii option limits to ASCII letters"),
    ("downcase", "String", "lowercase; :ascii option limits to ASCII letters"),
    ("swapcase", "String", "invert case of each char, returning a new String"),
    ("capitalize", "String", "uppercase first char, lowercase the rest"),
    ("reverse", "String", "reverse the characters"),
    ("strip", "String", "trim leading and trailing whitespace"),
    ("lstrip", "String", "trim leading whitespace"),
    ("rstrip", "String", "trim trailing whitespace"),
    ("chomp", "String", "remove trailing separator/newline; empty arg strips all trailing \\r\\n"),
    ("chop", "String", "remove last char; trailing \\r\\n counts as one"),
    ("chars", "String", "array of single-char strings"),
    ("bytes", "String", "array of UTF-8 byte integers"),
    ("bytesize", "String", "number of UTF-8 bytes, not chars"),
    ("each_byte", "String", "with block yield each byte, return self; else Enumerator of bytes"),
    ("getbyte", "String", "byte at index (negatives ok); nil if out of range"),
    ("b", "String", "copy of the string (ASCII-8BIT shim, bytes unchanged)"),
    ("ascii_only?", "String", "true when every byte is 7-bit ASCII"),
    ("valid_encoding?", "String", "always true (only valid UTF-8 is carried)"),
    ("force_encoding", "String", "no-op shim returning self"),
    ("encode", "String", "no-op shim returning a copy"),
    ("lines", "String", "array of lines (keeping newlines)"),
    ("each_line", "String", "with block yield each line, return self; else Enumerator of lines"),
    ("center", "String", "center-pad to width using pad string (default space)"),
    ("tr", "String", "translate chars from spec to spec, ranges/negation supported"),
    ("delete", "String", "remove chars matching the spec(s)"),
    ("count", "String", "count chars matching the spec(s)"),
    ("squeeze", "String", "collapse adjacent duplicate chars (limited to spec if given)"),
    ("empty?", "String", "true if the string has no chars"),
    ("to_i", "String", "parse leading integer in base (default 10); 0 if none"),
    ("hex", "String", "parse leading hex integer; 0 if none"),
    ("oct", "String", "parse leading base-8 int; radix prefix auto-detects base"),
    ("to_f", "String", "parse leading float, 0.0 if none"),
    ("to_r", "String", "parse leading rational (\"3/4\", \"3.14\"); else 0/1"),
    ("to_s", "String", "returns self (alias to_str)"),
    ("to_str", "String", "returns self (alias to_s)"),
    ("to_sym", "String", "the Symbol with this name"),
    ("include?", "String", "true if it contains the substring arg"),
    ("start_with?", "String", "true if any arg matches at the start (Regexp anchored at 0)"),
    ("end_with?", "String", "true if any string arg is a suffix"),
    ("match?", "String", "true if the Regexp/pattern matches; sets no $~"),
    ("=~", "String", "match Regexp, set $~/$1..; return char offset or nil"),
    ("match", "String", "return MatchData for the Regexp, else nil"),
    ("scan", "String", "with block yield each match return self; else array of matches"),
    ("split", "String", "split by pattern/string/awk-mode with optional limit"),
    ("sub", "String", "replace first match of Regexp/string via arg or block"),
    ("gsub", "String", "replace all matches of Regexp/string via arg or block"),
    ("replace", "String", "overwrite receiver contents in place, return self"),
    ("<<", "String", "append arg's to_s in place, return self"),
    ("+", "String", "return new string concatenating self and arg's to_s"),
    ("concat", "String", "append all args in order in place, return self"),
    ("<=>", "String", "compare strings -1/0/1; nil if arg not a String"),
    ("between?", "String", "true if self is between lo and hi inclusive"),
    ("clamp", "String", "clamp to lo/hi or inclusive range; exclusive range errors"),
    ("*", "String", "repeat the string n times"),
    ("%", "String", "sprintf-format self with array/hash/single operand"),
    ("ljust", "String", "left-justify padded to width (default space)"),
    ("rjust", "String", "right-justify padded to width (default space)"),
    ("each_char", "String", "with block yield each char, return self; else Enumerator of chars"),
    ("[]", "String", "substring by index/range/regexp (alias slice)"),
    ("slice", "String", "substring by index/range/regexp (alias [])"),
    ("eql?", "String", "content equality, only another String can be equal"),
    ("slice!", "String", "remove and return the sliced portion in place; nil if none"),
    ("ord", "String", "codepoint of first char; ArgumentError if empty"),
    ("chr", "String", "first char as a string, empty if none"),
    ("partition", "String", "split on first match of sep into [before, sep, after]"),
    ("rpartition", "String", "split on last match of sep into [before, sep, after]"),
    ("casecmp", "String", "case-insensitive compare returning -1/0/1"),
    ("casecmp?", "String", "case-insensitive equality boolean"),
    ("tr_s", "String", "translate like tr then squeeze runs of translated chars"),
    ("succ", "String", "next string in succession (alias next)"),
    ("next", "String", "next string in succession (alias succ)"),
    ("insert", "String", "insert arg before index (negative inserts after) in place"),
    ("prepend", "String", "prepend all args in place, return self"),
    ("index", "String", "char offset of first substring match from optional pos; nil"),
    ("rindex", "String", "char offset of last substring match up to optional pos; nil"),
    ("[]=", "String", "assign to indexed/range/regexp slice in place"),
    // ── Symbol ──
    ("to_s", "Symbol", "the name as a String (alias id2name, name)"),
    ("id2name", "Symbol", "the name as a String (alias to_s, name)"),
    ("name", "Symbol", "the name as a String (alias to_s, id2name)"),
    ("to_sym", "Symbol", "returns self (alias intern)"),
    ("intern", "Symbol", "returns self (alias to_sym)"),
    ("length", "Symbol", "char count of the name (alias size)"),
    ("size", "Symbol", "char count of the name (alias length)"),
    ("empty?", "Symbol", "true if the name is empty"),
    ("upcase", "Symbol", "uppercased name as a Symbol"),
    ("downcase", "Symbol", "lowercased name as a Symbol"),
    ("succ", "Symbol", "next name in succession as a Symbol (alias next)"),
    ("next", "Symbol", "next name in succession as a Symbol (alias succ)"),
    ("swapcase", "Symbol", "case-inverted name as a Symbol"),
    ("capitalize", "Symbol", "capitalized name as a Symbol"),
    ("[]", "Symbol", "index the name, returning a String (alias slice)"),
    ("slice", "Symbol", "index the name, returning a String (alias [])"),
    ("start_with?", "Symbol", "true if any arg is a prefix of the name"),
    ("end_with?", "Symbol", "true if any arg is a suffix of the name"),
    ("match?", "Symbol", "true if the name matches the Regexp/pattern, no $~"),
    ("<=>", "Symbol", "compare names -1/0/1; nil if arg not a Symbol"),
    ("to_proc", "Symbol", "proc that sends the named method to its first arg"),
    // ── Regexp ──
    ("source", "Regexp", "the pattern source string"),
    ("match?", "Regexp", "true if the pattern matches the arg, no $~"),
    ("=~", "Regexp", "match arg, set $~/$1..; return char offset or nil"),
    ("match", "Regexp", "return MatchData for the arg string"),
    ("scan", "Regexp", "array of all matches in the arg string"),
    ("to_s", "Regexp", "string form \"(?-mix:source)\""),
    ("inspect", "Regexp", "literal form \"/source/\""),
    // ── MatchData ──
    ("[]", "MatchData", "group string at integer index; nil if absent"),
    ("pre_match", "MatchData", "text before the match"),
    ("post_match", "MatchData", "text after the match"),
    ("to_a", "MatchData", "array of whole match plus all groups"),
    ("captures", "MatchData", "array of capture groups excluding the whole match"),
    ("to_s", "MatchData", "the whole matched string"),
    ("size", "MatchData", "number of groups including group 0 (alias length)"),
    ("length", "MatchData", "number of groups including group 0 (alias size)"),
    // ── Array ──
    ("&", "Array", "set intersection vs arg array (deduped)"),
    ("intersection", "Array", "set intersection vs arg array (deduped)"),
    ("|", "Array", "set union vs arg array (deduped)"),
    ("union", "Array", "set union vs arg array (deduped)"),
    ("-", "Array", "set difference vs arg array"),
    ("difference", "Array", "set difference vs arg array"),
    ("length", "Array", "element count"),
    ("size", "Array", "element count"),
    ("push", "Array", "append args to end in place, returns self"),
    ("append", "Array", "append args to end in place, returns self"),
    ("<<", "Array", "append args to end in place, returns self"),
    ("pop", "Array", "remove and return last element, nil if empty, mutates"),
    ("shift", "Array", "remove and return first element, nil if empty, mutates"),
    ("unshift", "Array", "insert args at front in place, returns self"),
    ("prepend", "Array", "insert args at front in place, returns self"),
    ("first", "Array", "first element, or first n elements as array if arg given"),
    ("last", "Array", "last element, or last n elements as array if arg given"),
    ("each_cons", "Array", "yield each n-wide window; no block returns windows array"),
    ("dig", "Array", "recursively index into nested arrays/hashes"),
    ("empty?", "Array", "true if array has no elements"),
    ("reverse", "Array", "new array with elements reversed"),
    ("to_a", "Array", "return a shallow copy of the array"),
    ("to_ary", "Array", "return a shallow copy of the array"),
    ("dup", "Array", "return a shallow copy of the array"),
    ("clone", "Array", "return a shallow copy of the array"),
    ("deconstruct", "Array", "return a shallow copy of the array"),
    ("include?", "Array", "true if any element == arg"),
    ("index", "Array", "first index where block truthy or element == arg, else nil"),
    ("find_index", "Array", "first index where block truthy or element == arg, else nil"),
    ("rindex", "Array", "last index where block truthy or element == arg, else nil"),
    ("bsearch", "Array", "binary search sorted array via block; no block returns self"),
    ("values_at", "Array", "elements at given indices/ranges, nil for out-of-range"),
    ("each_index", "Array", "yield each index; no block returns indices array"),
    ("rotate!", "Array", "rotate elements left by n (default 1) in place, returns self"),
    ("flatten!", "Array", "flatten nested arrays in place; nil if nothing was nested"),
    ("compact!", "Array", "remove nil elements in place; nil if none removed"),
    ("join", "Array", "concatenate elements to string with optional separator"),
    ("sort", "Array", "sort by <=> or block; returns new sorted array"),
    ("sort!", "Array", "sort by <=> or block in place, returns self"),
    ("minmax", "Array", "[min, max] pair by <=> or block; [nil, nil] if empty"),
    ("min", "Array", "min by <=> or block, or n smallest sorted if arg given"),
    ("max", "Array", "max by <=> or block, or n largest sorted if arg given"),
    ("sum", "Array", "sum elements (block-mapped) starting from arg init (default 0)"),
    ("uniq", "Array", "new array with duplicates removed (block form unsupported)"),
    ("compact", "Array", "new array with nil elements removed"),
    ("flatten", "Array", "new fully-flattened array, or up to n levels if arg given"),
    ("concat", "Array", "append elements of arg arrays in place, returns self"),
    ("each", "Array", "yield each element, returns self; no block returns Enumerator"),
    ("each_with_index", "Array", "yield element and index; no block returns Enumerator of pairs"),
    ("map", "Array", "map elements via block; no block returns Enumerator"),
    ("collect", "Array", "map elements via block; no block returns Enumerator"),
    ("flat_map", "Array", "map via block, flattening array results; no block returns Enumerator"),
    ("select", "Array", "keep block-truthy elements; no block returns Enumerator"),
    ("filter", "Array", "keep block-truthy elements; no block returns Enumerator"),
    ("find_all", "Array", "keep block-truthy elements; no block returns Enumerator"),
    ("reject", "Array", "drop block-truthy elements; no block returns Enumerator"),
    ("filter_map", "Array", "map elements via block, keep only truthy results"),
    ("transpose", "Array", "transpose array of equal-length rows; IndexError if ragged"),
    ("find", "Array", "first element where block truthy, else nil"),
    ("detect", "Array", "first element where block truthy, else nil"),
    ("any?", "Array", "block truthy for any; block-less: any element truthy"),
    ("all?", "Array", "block truthy for all elements"),
    ("none?", "Array", "block truthy for no element"),
    ("count", "Array", "count block-truthy elements, or == arg, else total length"),
    ("reduce", "Array", "fold via block or symbol op with optional initial value"),
    ("inject", "Array", "fold via block or symbol op with optional initial value"),
    ("min_by", "Array", "min element by block key"),
    ("max_by", "Array", "max element by block key"),
    ("sort_by", "Array", "sort by block key"),
    ("minmax_by", "Array", "[min, max] by block key"),
    ("[]", "Array", "index or range access"),
    ("fetch", "Array", "element at index, else arg default, else block, else IndexError"),
    ("[]=", "Array", "set element at index, padding with nil, returns assigned value"),
    ("take", "Array", "first n elements as new array"),
    ("drop", "Array", "all but first n elements as new array"),
    ("partition", "Array", "[matching, non-matching] arrays split by block"),
    ("group_by", "Array", "hash grouping elements by block return value"),
    ("tally", "Array", "hash counting occurrences of each element"),
    ("each_with_object", "Array", "yield element and memo object, returns the memo"),
    ("zip", "Array", "merge with arg arrays into rows; block yields rows, returns nil"),
    ("product", "Array", "cartesian product of self with arg arrays"),
    ("combination", "Array", "n-element combinations; block yields each else returns array"),
    ("permutation", "Array", "n-element permutations (default all); block yields each else array"),
    ("assoc", "Array", "first sub-array whose first element == arg, else nil"),
    ("rassoc", "Array", "first sub-array whose second element == arg, else nil"),
    ("fill", "Array", "fill with value or block result over optional range, mutates"),
    ("insert", "Array", "insert args at index (neg inserts after), padding, returns self"),
    ("delete_at", "Array", "remove and return element at index, nil if out of range"),
    ("delete_if", "Array", "remove block-truthy elements in place, returns self"),
    ("reject!", "Array", "remove block-truthy elements in place, returns self"),
    ("take_while", "Array", "leading elements while block truthy"),
    ("drop_while", "Array", "elements after the leading block-truthy run"),
    ("rotate", "Array", "new array rotated left by n (default 1)"),
    ("each_slice", "Array", "yield n-length slices; no block returns slices array"),
    ("chunk_while", "Array", "split into runs; new run when block falsey for adjacent pair"),
    ("slice_when", "Array", "split into runs; new run when block truthy for adjacent pair"),
    ("to_h", "Array", "build hash from [k,v] pairs, block maps each element first"),
    ("cycle", "Array", "repeat elements n times or endlessly to block"),
    ("chunk", "Array", "consecutive equal-key runs as [key, elems] pairs"),
    // ── Hash ──
    ("size", "Hash", "number of key-value pairs"),
    ("length", "Hash", "number of key-value pairs"),
    ("count", "Hash", "count block-truthy pairs, or total pairs without block"),
    ("empty?", "Hash", "true if hash has no pairs"),
    ("deconstruct_keys", "Hash", "return self for pattern matching (keys arg is only a hint)"),
    ("keys", "Hash", "array of keys"),
    ("values", "Hash", "array of values"),
    ("key?", "Hash", "true if hash has the given key"),
    ("has_key?", "Hash", "true if hash has the given key"),
    ("include?", "Hash", "true if hash has the given key"),
    ("member?", "Hash", "true if hash has the given key"),
    ("value?", "Hash", "true if any value == arg"),
    ("has_value?", "Hash", "true if any value == arg"),
    ("[]", "Hash", "value for key, else default proc call, else default value (nil)"),
    ("fetch", "Hash", "value for key, else block, else arg default, else KeyError"),
    ("[]=", "Hash", "set value for key, returns the value"),
    ("store", "Hash", "set value for key, returns the value"),
    ("delete", "Hash", "remove key and return its value, nil if absent"),
    ("merge", "Hash", "new hash merging arg hashes; block resolves key collisions"),
    ("to_a", "Hash", "array of [key, value] pairs"),
    ("each", "Hash", "yield key and value, returns self"),
    ("each_pair", "Hash", "yield key and value, returns self"),
    ("map", "Hash", "array of block results, block gets each [k, v] pair"),
    ("select", "Hash", "new hash keeping block-truthy pairs"),
    ("filter", "Hash", "new hash keeping block-truthy pairs"),
    ("reject", "Hash", "new hash dropping block-truthy pairs"),
    ("transform_values", "Hash", "new hash with each value replaced by block result"),
    ("transform_keys", "Hash", "new hash with keys remapped via mapping hash then block"),
    ("filter_map", "Hash", "array of truthy block results over pairs"),
    ("each_with_object", "Hash", "yield [k, v] pair and memo, returns the memo"),
    ("sum", "Hash", "sum of block results over pairs from arg init (default 0)"),
    ("any?", "Hash", "block truthy for any pair; without block returns false"),
    ("all?", "Hash", "block truthy for all pairs; without block returns true"),
    ("none?", "Hash", "block truthy for no pair; without block returns true"),
    ("min_by", "Hash", "min [k, v] pair by block key"),
    ("max_by", "Hash", "max [k, v] pair by block key"),
    ("sort_by", "Hash", "sort [k, v] pairs by block key"),
    ("reduce", "Hash", "fold over [k, v] pairs"),
    ("inject", "Hash", "fold over [k, v] pairs"),
    ("group_by", "Hash", "group [k, v] pairs by block return"),
    ("partition", "Hash", "partition [k, v] pairs by block"),
    ("find_all", "Hash", "keep block-truthy [k, v] pairs"),
    ("invert", "Hash", "new hash with keys and values swapped"),
    ("default", "Hash", "the hash's default value for missing keys"),
    ("default=", "Hash", "set the hash's default value, returns it"),
    ("to_h", "Hash", "return self"),
    ("except", "Hash", "new hash without the given keys, order preserved"),
    ("slice", "Hash", "new hash with only the given keys, in argument order"),
    ("compact", "Hash", "new hash without nil-valued pairs"),
    ("flat_map", "Hash", "map pairs via block, flattening array results"),
    ("collect_concat", "Hash", "map pairs via block, flattening array results"),
    ("each_with_index", "Hash", "yield [k,v] pair and index; no block returns pairs array"),
    ("find", "Hash", "first [k, v] pair where block truthy, else nil"),
    ("detect", "Hash", "first [k, v] pair where block truthy, else nil"),
    ("dig", "Hash", "recursively index into nested hashes/arrays"),
    // ── Set ──
    ("add", "Set", "add element to set, returns self"),
    ("<<", "Set", "add element to set, returns self"),
    ("add?", "Set", "add element; self if newly added, nil if already present"),
    ("delete", "Set", "remove element from set, returns self"),
    ("delete?", "Set", "remove element; self if it was present, else nil"),
    ("include?", "Set", "true if set contains arg"),
    ("member?", "Set", "true if set contains arg"),
    ("===", "Set", "true if set contains arg"),
    ("contain?", "Set", "true if set contains arg"),
    ("size", "Set", "number of elements"),
    ("length", "Set", "number of elements"),
    ("count", "Set", "number of elements"),
    ("empty?", "Set", "true if set has no elements"),
    ("clear", "Set", "remove all elements, returns self"),
    ("to_a", "Set", "elements as a new array"),
    ("to_ary", "Set", "elements as a new array"),
    ("to_set", "Set", "return a copy as a new set"),
    ("dup", "Set", "return a copy as a new set"),
    ("clone", "Set", "return a copy as a new set"),
    ("merge", "Set", "add all elements of arg set/array in place, returns self"),
    ("union", "Set", "new set with elements of self and arg"),
    ("|", "Set", "new set with elements of self and arg"),
    ("+", "Set", "new set with elements of self and arg"),
    ("merge_new", "Set", "new set with elements of self and arg"),
    ("intersection", "Set", "new set of elements in both self and arg"),
    ("&", "Set", "new set of elements in both self and arg"),
    ("difference", "Set", "new set of elements in self but not arg"),
    ("-", "Set", "new set of elements in self but not arg"),
    ("^", "Set", "new set of symmetric difference (elements in exactly one)"),
    ("subset?", "Set", "true if every element of self is in arg"),
    ("<=", "Set", "true if every element of self is in arg"),
    ("superset?", "Set", "true if every element of arg is in self"),
    (">=", "Set", "true if every element of arg is in self"),
    ("proper_subset?", "Set", "true if self is subset of arg and strictly smaller"),
    ("<", "Set", "true if self is subset of arg and strictly smaller"),
    ("proper_superset?", "Set", "true if self is superset of arg and strictly larger"),
    (">", "Set", "true if self is superset of arg and strictly larger"),
    ("disjoint?", "Set", "true if self and arg share no elements"),
    ("intersect?", "Set", "true if self and arg share any element"),
    ("each", "Set", "yield each element, returns self (block required)"),
    // ── Range ──
    ("to_a", "Range", "materialize integer range into Array"),
    ("to_ary", "Range", "materialize integer range into Array"),
    ("entries", "Range", "materialize integer range into Array"),
    ("first", "Range", "lower bound, or first n elements as Array with count arg"),
    ("last", "Range", "upper bound, or last n elements as Array with count arg"),
    ("min", "Range", "lower bound lo"),
    ("begin", "Range", "lower bound lo"),
    ("max", "Range", "upper bound (hi-1 if exclusive)"),
    ("end", "Range", "raw upper bound hi (not exclusive-adjusted)"),
    ("size", "Range", "element count (end-lo, min 0)"),
    ("count", "Range", "element count (end-lo, min 0)"),
    ("length", "Range", "element count (end-lo, min 0)"),
    ("sum", "Range", "sum of integer elements"),
    ("include?", "Range", "true if arg in [lo, end)"),
    ("cover?", "Range", "true if arg in [lo, end)"),
    ("member?", "Range", "true if arg in [lo, end)"),
    ("===", "Range", "true if arg in [lo, end)"),
    ("step", "Range", "step by Int n or Float; yields to block or returns Array/enum"),
    ("each", "Range", "yield each integer to block"),
    ("exclude_end?", "Range", "true if range excludes its end"),
    ("%", "Range", "alias of step: float step by arg"),
    // ── Struct ──
    ("to_a", "Struct", "array of member values"),
    ("values", "Struct", "array of member values"),
    ("deconstruct", "Struct", "array of member values (for array pattern matching)"),
    ("members", "Struct", "member names as symbols"),
    ("size", "Struct", "number of members"),
    ("length", "Struct", "number of members"),
    ("to_h", "Struct", "hash of member => value"),
    ("deconstruct_keys", "Struct", "hash of requested (or all) members for pattern matching"),
    ("[]", "Struct", "index by int position or by symbol/string member name"),
    ("each", "Struct", "yields each member value, returns receiver (block form only)"),
    ("each_pair", "Struct", "yields member symbol and value, returns receiver (block form)"),
    ("==", "Struct", "true if same struct class and all members equal"),
    ("eql?", "Struct", "true if same struct class and all members equal (same as ==)"),
    ("to_s", "Struct", "#<struct Name m=v, ...> representation"),
    ("inspect", "Struct", "#<struct Name m=v, ...> representation"),
    // ── Exception ──
    ("message", "Exception", "the message ivar, or the class name if unset"),
    ("to_s", "Exception", "the message ivar, or the class name if unset"),
    // ── Enumerator ──
    ("next", "Enumerator", "Returns next buffered value; StopIteration at end."),
    ("peek", "Enumerator", "Returns next buffered value without advancing; StopIteration at end."),
    ("rewind", "Enumerator", "Resets the buffer cursor; returns self."),
    ("size", "Enumerator", "Returns buffer length as Int (nil for generator source)."),
    ("length", "Enumerator", "Returns buffer length as Int."),
    ("with_index", "Enumerator", "Re-runs source method with index; returns elements array."),
    ("with_object", "Enumerator", "Threads memo through block per element; returns memo."),
    ("each_with_object", "Enumerator", "Threads memo through block per element; returns memo."),
    ("first", "Enumerator", "Drives source for first value, or n as array."),
    ("take", "Enumerator", "Drives source collecting up to n values as array."),
    ("to_a", "Enumerator", "Drives source to completion into array."),
    ("force", "Enumerator", "Drives source to completion into array."),
    ("entries", "Enumerator", "Drives source to completion into array."),
    ("each", "Enumerator", "Drives source to completion, calls block per value; returns self."),
    ("lazy", "Enumerator", "Wraps the enumerator as a lazy pipeline source."),
    // ── Enumerator::Lazy ──
    ("map", "Enumerator::Lazy", "Appends a lazy Map op; returns new lazy enumerator."),
    ("collect", "Enumerator::Lazy", "Appends a lazy Map op; returns new lazy enumerator."),
    ("select", "Enumerator::Lazy", "Appends a lazy Select op; returns new lazy enumerator."),
    ("filter", "Enumerator::Lazy", "Appends a lazy Select op; returns new lazy enumerator."),
    ("reject", "Enumerator::Lazy", "Appends a lazy Reject op; returns new lazy enumerator."),
    ("filter_map", "Enumerator::Lazy", "Appends a lazy FilterMap op; returns new lazy enumerator."),
    ("flat_map", "Enumerator::Lazy", "Appends a lazy FlatMap op; returns new lazy enumerator."),
    ("collect_concat", "Enumerator::Lazy", "Appends a lazy FlatMap op; returns new lazy enumerator."),
    ("take_while", "Enumerator::Lazy", "Appends a lazy TakeWhile op; returns new lazy enumerator."),
    ("drop_while", "Enumerator::Lazy", "Appends a lazy DropWhile op; returns new lazy enumerator."),
    ("take", "Enumerator::Lazy", "Appends a lazy Take(n) op; returns new lazy enumerator."),
    ("drop", "Enumerator::Lazy", "Appends a lazy Drop(n) op; returns new lazy enumerator."),
    ("zip", "Enumerator::Lazy", "Appends a lazy Zip op over array-coerced args."),
    ("lazy", "Enumerator::Lazy", "Returns self unchanged."),
    ("first", "Enumerator::Lazy", "Pulls first element, or first n as array if arg given."),
    ("force", "Enumerator::Lazy", "Pulls the whole pipeline into an array."),
    ("to_a", "Enumerator::Lazy", "Pulls the whole pipeline into an array."),
    ("entries", "Enumerator::Lazy", "Pulls the whole pipeline into an array."),
    ("each", "Enumerator::Lazy", "With block: pulls all and calls block per element; returns self."),
    // ── Enumerator::Yielder ──
    ("<<", "Enumerator::Yielder", "Pushes value into sink; raises break at limit; returns self."),
    ("yield", "Enumerator::Yielder", "Pushes value into sink; raises break at limit; returns self."),
    // ── Fiber ──
    ("resume", "Fiber", "Resumes the fiber with args (nil/one/array)."),
    ("alive?", "Fiber", "True if the fiber is still alive."),
    ("inspect", "Fiber", "Returns \"#<Fiber (created)>\" with \" (dead)\" if not alive."),
    ("to_s", "Fiber", "Returns \"#<Fiber (created)>\" with \" (dead)\" if not alive."),
    // ── Proc ──
    ("call", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc."),
    ("[]", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc."),
    ("yield", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc."),
    ("to_proc", "Proc", "Returns self unchanged."),
    ("arity", "Proc", "Returns the proc's arity as Int (0 if unknown)."),
    ("lambda?", "Proc", "True if the proc is a lambda."),
    ("curry", "Proc", "Returns a curried version of the proc."),
    (">>", "Proc", "Composition: (f >> g).call(x) == g.call(f.call(x))."),
    ("<<", "Proc", "Composition: (f << g).call(x) == f.call(g.call(x))."),
    // ── Method ──
    ("call", "Method", "Routes back through dispatch on the captured receiver."),
    ("[]", "Method", "Routes back through dispatch on the captured receiver."),
    ("yield", "Method", "Routes back through dispatch on the captured receiver."),
    ("===", "Method", "Routes back through dispatch on the captured receiver."),
    ("arity", "Method", "Returns the bound method's arity as Int."),
    ("name", "Method", "Returns the method name as a Symbol."),
    ("receiver", "Method", "Returns the captured receiver."),
    ("to_proc", "Method", "Returns self unchanged (Method is callable)."),
    // ── Time ──
    ("year", "Time", "year field (UTC breakdown of epoch secs)"),
    ("month", "Time", "month 1-12"),
    ("mon", "Time", "month 1-12"),
    ("day", "Time", "day of month"),
    ("mday", "Time", "day of month"),
    ("hour", "Time", "hour 0-23"),
    ("min", "Time", "minute 0-59"),
    ("sec", "Time", "second 0-59"),
    ("wday", "Time", "weekday, 0=Sunday"),
    ("yday", "Time", "day of year"),
    ("to_i", "Time", "epoch seconds floored to Integer"),
    ("tv_sec", "Time", "epoch seconds floored to Integer"),
    ("to_f", "Time", "epoch seconds as Float"),
    ("sunday?", "Time", "true if wday == 0"),
    ("monday?", "Time", "true if wday == 1"),
    ("tuesday?", "Time", "true if wday == 2"),
    ("wednesday?", "Time", "true if wday == 3"),
    ("thursday?", "Time", "true if wday == 4"),
    ("friday?", "Time", "true if wday == 5"),
    ("saturday?", "Time", "true if wday == 6"),
    ("utc", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)"),
    ("getutc", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)"),
    ("gmtime", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)"),
    ("localtime", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)"),
    ("getlocal", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)"),
    ("utc?", "Time", "always true (UTC-only model)"),
    ("gmt?", "Time", "always true (UTC-only model)"),
    ("to_s", "Time", "formatted time string"),
    ("inspect", "Time", "formatted time string (inspect form)"),
    ("strftime", "Time", "format time by strftime pattern arg"),
    ("<=>", "Time", "compare epoch secs; nil if arg not a Time"),
    ("==", "Time", "true if arg has equal epoch secs"),
    ("+", "Time", "new Time shifted forward by arg seconds"),
    ("-", "Time", "Float secs diff if arg is Time, else Time minus arg secs"),
    ("hash", "Time", "epoch secs bit pattern as Integer"),
    // ── Date ──
    ("year", "Date", "year field"),
    ("month", "Date", "month 1-12"),
    ("mon", "Date", "month 1-12"),
    ("day", "Date", "day of month"),
    ("mday", "Date", "day of month"),
    ("wday", "Date", "weekday, 0=Sunday"),
    ("yday", "Date", "day of year"),
    ("cwday", "Date", "ISO weekday, Monday=1..Sunday=7"),
    ("jd", "Date", "Julian day number"),
    ("leap?", "Date", "true if year is a leap year"),
    ("sunday?", "Date", "true if wday == 0"),
    ("monday?", "Date", "true if wday == 1"),
    ("tuesday?", "Date", "true if wday == 2"),
    ("wednesday?", "Date", "true if wday == 3"),
    ("thursday?", "Date", "true if wday == 4"),
    ("friday?", "Date", "true if wday == 5"),
    ("saturday?", "Date", "true if wday == 6"),
    ("to_s", "Date", "ISO date string YYYY-MM-DD"),
    ("iso8601", "Date", "ISO date string YYYY-MM-DD"),
    ("inspect", "Date", "inspect string form"),
    ("strftime", "Date", "format date by strftime pattern arg"),
    ("next_day", "Date", "date plus arg days (default 1)"),
    ("succ", "Date", "date plus arg days (default 1)"),
    ("prev_day", "Date", "date minus arg days (default 1)"),
    ("next_month", "Date", "add arg months, clamping day to month end (default 1)"),
    ("prev_month", "Date", "subtract arg months, clamping day to month end (default 1)"),
    (">>", "Date", "add arg months, clamping day to month end"),
    ("<<", "Date", "subtract arg months, clamping day to month end"),
    ("next_year", "Date", "add 12*arg months (default 1 year)"),
    ("prev_year", "Date", "subtract 12*arg months (default 1 year)"),
    ("+", "Date", "date plus arg days"),
    ("-", "Date", "Rational day diff if arg is Date, else date minus arg days"),
    ("<=>", "Date", "compare day counts; nil if arg not a Date"),
    ("==", "Date", "true if arg is the same date"),
    ("hash", "Date", "day count as Integer"),
    // ── DateTime ──
    ("year", "DateTime", "year field"),
    ("month", "DateTime", "month 1-12"),
    ("mon", "DateTime", "month 1-12"),
    ("day", "DateTime", "day of month"),
    ("mday", "DateTime", "day of month"),
    ("hour", "DateTime", "hour 0-23"),
    ("min", "DateTime", "minute 0-59"),
    ("sec", "DateTime", "second 0-59"),
    ("wday", "DateTime", "weekday, 0=Sunday"),
    ("yday", "DateTime", "day of year"),
    ("cwday", "DateTime", "ISO weekday, Monday=1..Sunday=7"),
    ("jd", "DateTime", "Julian day number of the day part"),
    ("leap?", "DateTime", "true if year is a leap year"),
    ("sunday?", "DateTime", "true if wday == 0"),
    ("monday?", "DateTime", "true if wday == 1"),
    ("tuesday?", "DateTime", "true if wday == 2"),
    ("wednesday?", "DateTime", "true if wday == 3"),
    ("thursday?", "DateTime", "true if wday == 4"),
    ("friday?", "DateTime", "true if wday == 5"),
    ("saturday?", "DateTime", "true if wday == 6"),
    ("to_s", "DateTime", "ISO8601 datetime string"),
    ("iso8601", "DateTime", "ISO8601 datetime string"),
    ("inspect", "DateTime", "inspect string form"),
    ("strftime", "DateTime", "format datetime by strftime pattern arg"),
    ("to_date", "DateTime", "Date of the day part"),
    ("to_time", "DateTime", "Time at the same epoch secs"),
    ("next_day", "DateTime", "datetime plus arg days, keeping time (default 1)"),
    ("succ", "DateTime", "datetime plus arg days, keeping time (default 1)"),
    ("prev_day", "DateTime", "datetime minus arg days, keeping time (default 1)"),
    ("next_month", "DateTime", "add arg months, clamping day, keeping time (default 1)"),
    (">>", "DateTime", "add arg months, clamping day, keeping time of day"),
    ("prev_month", "DateTime", "subtract arg months, clamping day, keeping time (default 1)"),
    ("<<", "DateTime", "subtract arg months, clamping day, keeping time of day"),
    ("next_year", "DateTime", "add 12*arg months, keeping time (default 1 year)"),
    ("prev_year", "DateTime", "subtract 12*arg months, keeping time (default 1 year)"),
    ("+", "DateTime", "new DateTime plus arg days"),
    ("-", "DateTime", "Rational day diff if arg is DateTime, else minus arg days"),
    ("<=>", "DateTime", "compare epoch secs; nil if arg not a DateTime"),
    ("==", "DateTime", "true if arg has equal epoch secs"),
    ("hash", "DateTime", "epoch secs bit pattern as Integer"),
];

/// The builtin corpus, exposed for the offline docs generator (`gen-docs`).
pub fn corpus() -> &'static [(&'static str, &'static str, &'static str)] {
    CORPUS
}

type Docs = HashMap<String, String>;

/// Entry point for `ruby --lsp`.
pub fn run() -> Result<(), String> {
    spawn_orphan_guard();
    let (conn, io_threads) = Connection::stdio();
    let (init_id, _params) = conn
        .initialize_start()
        .map_err(|e| format!("lsp initialize: {e}"))?;
    let init_result = serde_json::json!({
        "capabilities": server_capabilities(),
        "serverInfo": { "name": "rubylang", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.sender
        .send(Response::new_ok(init_id, init_result).into())
        .map_err(|e| format!("lsp send: {e}"))?;

    let mut docs: Docs = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn
                    .handle_shutdown(&req)
                    .map_err(|e| format!("lsp shutdown: {e}"))?
                {
                    break;
                }
                dispatch_request(&conn, req);
            }
            Message::Notification(not) => dispatch_notification(&conn, &mut docs, not),
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io_threads.join().map_err(|_| "lsp io join".to_string())?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

fn handle<P, R>(conn: &Connection, req: Request, f: impl FnOnce(P) -> R)
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let method = req.method.clone();
    let id = req.id.clone();
    match req.extract::<P>(&method) {
        Ok((id, params)) => {
            let value = serde_json::to_value(f(params)).unwrap_or(serde_json::Value::Null);
            let _ = conn.sender.send(Response::new_ok(id, value).into());
        }
        Err(ExtractError::JsonError { error, .. }) => {
            let _ = conn.sender.send(
                Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()).into(),
            );
        }
        Err(ExtractError::MethodMismatch(_)) => unreachable!("method matched before extract"),
    }
}

fn dispatch_request(conn: &Connection, req: Request) {
    match req.method.as_str() {
        Completion::METHOD => handle(conn, req, |_p: CompletionParams| completions()),
        HoverRequest::METHOD => handle(conn, req, |_p: HoverParams| hover()),
        _ => {
            let _ = conn.sender.send(
                Response::new_err(req.id, ErrorCode::MethodNotFound as i32, "unhandled".into())
                    .into(),
            );
        }
    }
}

fn dispatch_notification(conn: &Connection, docs: &mut Docs, not: lsp_server::Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.insert(uri.as_str().to_string(), p.text_document.text.clone());
                publish_diagnostics(conn, &uri, &p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params) {
                if let Some(change) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.as_str().to_string(), change.text.clone());
                    publish_diagnostics(conn, &uri, &change.text);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.remove(uri.as_str());
                publish_diagnostics(conn, &uri, "");
            }
        }
        _ => {}
    }
}

fn completions() -> CompletionResponse {
    let items = CORPUS
        .iter()
        .map(|(name, _chapter, doc)| CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some((*doc).to_string()),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

fn hover() -> Hover {
    let body = "**rubylang** — Ruby on the fusevm bytecode VM + Cranelift JIT.".to_string();
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: None,
    }
}

fn publish_diagnostics(conn: &Connection, uri: &Uri, text: &str) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: compute_diagnostics(text),
        version: None,
    };
    let not = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    let _ = conn.sender.send(not.into());
}

/// Compile the whole document; a syntax error maps to a single diagnostic on the
/// line named in its `line N:` prefix (or line 0 if unlabeled).
fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    match crate::parser::parse(text) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let line = parse_error_line(&e).saturating_sub(1);
            vec![Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position {
                        line,
                        character: 200,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e,
                ..Default::default()
            }]
        }
    }
}

/// Extract the `line N` number from a parser error message.
fn parse_error_line(e: &str) -> u32 {
    e.strip_prefix("line ")
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Exit if reparented to pid 1 (the editor died) so we never leak.
fn spawn_orphan_guard() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) only registers a signal disposition.
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // SAFETY: getppid takes no arguments and never fails.
            if unsafe { libc::getppid() } == 1 {
                std::process::exit(0);
            }
        }
    });
}
