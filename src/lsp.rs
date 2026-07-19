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
const CORPUS: &[(&str, &str, &str, &str)] = &[
    // ── Keyword ──
    ("def", "Keyword", "define a method; body runs on call, returns the last expression", "def greet; \"hi\"; end; greet   # => \"hi\""),
    ("end", "Keyword", "close a def/class/module/do/if/case/begin block", "class A; end   # 'end' closes the class body"),
    ("class", "Keyword", "open or reopen a class; `class A < B` sets the superclass", "class B < Object; end; B.superclass   # => Object"),
    ("module", "Keyword", "open or reopen a module (namespace / mixin)", "module M; end; M.class   # => Module"),
    ("self", "Keyword", "the current receiver object", "class A; def me; self; end; end; A.new.me.class   # => A"),
    ("super", "Keyword", "call the same method in the superclass (bare = pass same args)", "class B<A; def f; super+1; end; end   # calls A#f"),
    ("if", "Keyword", "conditional; also the `expr if cond` statement modifier", "x = 5 if true; x   # => 5"),
    ("elsif", "Keyword", "additional condition branch inside an if", "if false then 1 elsif true then 2 end   # => 2"),
    ("else", "Keyword", "fallback branch of an if/unless/case/begin", "if false then 1 else 2 end   # => 2"),
    ("unless", "Keyword", "negated conditional; also the `expr unless cond` modifier", "\"y\" unless false   # => \"y\""),
    ("case", "Keyword", "multi-way branch matched by `when` via `===`", "case 2; when 2 then \"b\"; end   # => \"b\""),
    ("when", "Keyword", "a case branch; matches its value against the subject with `===`", "case 2; when 1..3 then \"in\"; end   # => \"in\""),
    ("while", "Keyword", "loop while the condition is truthy; also a statement modifier", "i=0; i+=1 while i<3; i   # => 3"),
    ("until", "Keyword", "loop until the condition is truthy; also a statement modifier", "i=0; i+=1 until i>=3; i   # => 3"),
    ("for", "Keyword", "iterate `for x in enumerable` without a new scope", "for x in [1,2,3]; print x; end   # prints 123"),
    ("in", "Keyword", "the `for x in …` separator and case/in pattern-match clause", "case [1,2]; in [a,b]; a+b; end   # => 3"),
    ("do", "Keyword", "open a block (`each do |x| … end`) or a while/for body", "[1,2].each do |n| print n end   # prints 12"),
    ("then", "Keyword", "optional separator after an if/when condition", "if true then 1 else 2 end   # => 1"),
    ("yield", "Keyword", "invoke the block passed to the current method", "def f; yield 5; end; f { |x| p x }   # prints 5"),
    ("return", "Keyword", "return a value from the current method (nil if omitted)", "def f; return 9; end; f   # => 9"),
    ("break", "Keyword", "exit the nearest loop/block, optionally with a value", "[1,2,3].each { |n| break n if n==2 }   # => 2"),
    ("next", "Keyword", "skip to the next loop/block iteration, optionally with a value", "[1,2,3].map { |n| next 0 if n==2; n }   # => [1, 0, 3]"),
    ("retry", "Keyword", "re-run the begin body from a rescue clause", "a=0; begin; a+=1; raise if a<2; rescue; retry if a<2; end; a   # => 2"),
    ("begin", "Keyword", "open an exception-handling block (rescue/ensure/else)", "begin; raise \"e\"; rescue => e; e.message; end   # => \"e\""),
    ("rescue", "Keyword", "handle a raised exception; also the `expr rescue fallback` modifier", "1/0 rescue \"safe\"   # => \"safe\""),
    ("ensure", "Keyword", "block that always runs whether or not an exception was raised", "begin; 1; ensure; p \"always\"; end   # prints always"),
    ("and", "Keyword", "low-precedence logical AND (short-circuits)", "(true and false)   # => false"),
    ("or", "Keyword", "low-precedence logical OR (short-circuits)", "(nil or 2)   # => 2"),
    ("not", "Keyword", "low-precedence logical negation", "(not true)   # => false"),
    ("nil", "Keyword", "the sole NilClass instance; the only falsey object besides false", "nil.nil?   # => true"),
    ("true", "Keyword", "the sole TrueClass instance", "(true && 1)   # => 1"),
    ("false", "Keyword", "the sole FalseClass instance (falsey)", "(false || 1)   # => 1"),
    ("alias", "Keyword", "give a method a second name: `alias new old`", "class A; def old; 1; end; alias new old; end; A.new.new   # => 1"),
    ("defined?", "Keyword", "describe the expression's kind (\"method\"/\"expression\"/…) or nil", "defined?(String)   # => \"constant\""),
    // ── Kernel ──
    ("puts", "Kernel", "print each arg (arrays recursed) each on its own line", "puts \"hi\"   # => nil (prints hi)"),
    ("print", "Kernel", "print args joined by $, terminated by $\\ (both nil default)", "print \"hi\"   # => nil (prints hi)"),
    ("p", "Kernel", "print inspect of each arg; return arg/array/nil", "p 42   # => 42 (also prints 42)"),
    ("pp", "Kernel", "alias of p (no pretty-printing)", "pp [1,2]   # => [1, 2] (also prints it)"),
    ("require", "Kernel", "no-op: always returns true (no file loaded)", "require \"json\"   # => true (nothing loaded)"),
    ("require_relative", "Kernel", "no-op: always returns true (no file loaded)", "require_relative \"x\"   # => true (no-op)"),
    ("load", "Kernel", "no-op: always returns true (no file loaded)", "load \"x.rb\"   # => true (no-op)"),
    ("intercept", "Kernel", "register AOP before/after/around advice by glob pattern", "intercept(\"foo\", :before, :log)   # => :log"),
    ("raise", "Kernel", "raise by class/message/instance (default RuntimeError)", "raise \"boom\" rescue \"caught\"   # => \"caught\""),
    ("fail", "Kernel", "raise by class/message/instance (default RuntimeError)", "fail \"boom\" rescue \"caught\"   # => \"caught\""),
    ("rand", "Kernel", "random Int in [0,n) or Float in [0,1)", "rand(1)   # => 0"),
    ("sleep", "Kernel", "no-op: returns 0 without sleeping", "sleep(5)   # => 0 (no actual sleep)"),
    ("srand", "Kernel", "reseed PRNG; return the previous seed", "srand(0)   # => previous seed (Int)"),
    ("Integer", "Kernel", "convert to Integer, optional base for string args", "Integer(\"ff\", 16)   # => 255"),
    ("Float", "Kernel", "convert numeric/string to Float, else raise", "Float(\"3.5\")   # => 3.5"),
    ("Rational", "Kernel", "build Rational(num,den); raise on zero denominator", "Rational(6, 4)   # => (3/2)"),
    ("Complex", "Kernel", "build Complex from real and imaginary args", "Complex(1, 2)   # => (1+2i)"),
    ("String", "Kernel", "convert arg via to_s to a String", "String(123)   # => \"123\""),
    ("Array", "Kernel", "wrap arg as Array (hash->pairs, nil->[], scalar->[x])", "Array(nil)   # => []"),
    ("format", "Kernel", "sprintf-format string; trailing Hash for named refs", "format(\"%05.2f\", 3.1)   # => \"03.10\""),
    ("sprintf", "Kernel", "sprintf-format string; trailing Hash for named refs", "sprintf(\"%d-%d\", 1, 2)   # => \"1-2\""),
    ("gets", "Kernel", "read one line from stdin (nil at EOF)", "line = gets   # reads one line from stdin; nil at EOF"),
    ("proc", "Kernel", "return the given block as a Proc", "proc { |x| x+1 }.call(4)   # => 5"),
    ("lambda", "Kernel", "return block marked as a lambda", "lambda { |x| x*2 }.call(3)   # => 6"),
    ("loop", "Kernel", "loop block forever; rescue StopIteration; break value", "i=0; loop { i+=1; break i if i>2 }   # => 3"),
    ("catch", "Kernel", "run block with tag; return thrown value if throw matches", "catch(:x) { throw :x, 42 }   # => 42"),
    ("throw", "Kernel", "unwind to matching catch(tag) with optional value", "catch(:t) { throw :t }   # => nil"),
    ("block_given?", "Kernel", "true if a block is present in the current call", "def f; block_given?; end; f { }   # => true"),
    ("exit", "Kernel", "exit process (true/nil->0, false->1, n->n)", "exit   # terminates the process with status 0"),
    ("exit!", "Kernel", "exit process (true/nil->0, false->1, n->n)", "exit!(1)   # terminates the process with status 1"),
    ("abort", "Kernel", "write optional msg to stderr and exit 1", "abort(\"fatal\")   # writes fatal to stderr, exits 1"),
    // ── Object ──
    ("to_s", "Object", "host default string of receiver (only when args are empty)", "5.to_s   # => \"5\""),
    ("inspect", "Object", "host inspect string of receiver", "[1, 2].inspect   # => \"[1, 2]\""),
    ("class", "Object", "returns the Class object reference, not its name", "1.class   # => Integer"),
    ("nil?", "Object", "true only for nil (Value::Undef)", "nil.nil?   # => true"),
    ("to_json", "Object", "JSON-encodes any value; ignores optional generator-state arg", "[1, 2].to_json   # => \"[1,2]\""),
    ("==", "Object", "host structural equality", "(1 == 1)   # => true"),
    ("!=", "Object", "negation of ==", "(1 != 2)   # => true"),
    ("===", "Object", "case-equality: Class/Regexp/Range/Float-range membership, else ==", "((1..5) === 3)   # => true"),
    ("is_a?", "Object", "true if receiver is an instance of the named class/module", "1.is_a?(Integer)   # => true"),
    ("kind_of?", "Object", "true if receiver is an instance of the named class/module", "1.kind_of?(Numeric)   # => true"),
    ("instance_of?", "Object", "true if receiver's exact class equals the argument", "1.instance_of?(Integer)   # => true"),
    ("itself", "Object", "returns the receiver unchanged", "5.itself   # => 5"),
    ("freeze", "Object", "records receiver as frozen and returns it; not enforced", "5.freeze   # => 5 (frozen flag, not enforced)"),
    ("equal?", "Object", "object identity: same heap handle or same immediate value", "1.equal?(1)   # => true"),
    ("object_id", "Object", "stable identity integer (2n+1 for ints, fixed immediates)", "1.object_id   # => 3"),
    ("__id__", "Object", "stable identity integer (2n+1 for ints, fixed immediates)", "1.__id__   # => 3"),
    ("dup", "Object", "shallow copy, never frozen", "[1, 2].dup   # => [1, 2]"),
    ("clone", "Object", "shallow copy preserving the original's frozen state", "[1, 2].clone   # => [1, 2]"),
    ("frozen?", "Object", "reports whether receiver is frozen", "\"x\".frozen?   # => false"),
    ("instance_variable_get", "Object", "reads ivar by name (@ prefix optional)", "Object.new.instance_variable_get(:@x)   # => nil (unset)"),
    ("instance_variable_set", "Object", "sets ivar by name and returns the value", "Object.new.instance_variable_set(:@x, 7)   # => 7"),
    ("instance_variables", "Object", "ivar names as symbols", "Object.new.instance_variables   # => [] (explicit receiver)"),
    ("tap", "Object", "yields receiver to block, returns receiver", "5.tap { |x| p x }   # => 5 (prints 5)"),
    ("then", "Object", "passes receiver to block, returns block result (else receiver)", "5.then { |x| x+1 }   # => 6"),
    ("yield_self", "Object", "passes receiver to block, returns block result (else receiver)", "5.yield_self { |x| x*2 }   # => 10"),
    ("lazy", "Object", "wraps enumerable in lazy pipeline; non-range/array becomes array", "[1, 2, 3].lazy.class   # => Enumerator::Lazy"),
    ("methods", "Object", "user-defined instance method names as symbols; builtins omitted", "Object.new.methods   # => [] (builtins omitted)"),
    ("send", "Object", "dispatches named method with remaining args (no visibility check)", "5.send(:+, 3)   # => 8"),
    ("__send__", "Object", "dispatches named method with remaining args (no visibility check)", "(-5).__send__(:abs)   # => 5"),
    ("public_send", "Object", "dispatches named method with remaining args (no visibility check)", "5.public_send(:+, 1)   # => 6"),
    ("method", "Object", "captures a bound Method object for the named method", "5.method(:+).call(3)   # => 8"),
    ("respond_to?", "Object", "true if class/respond_to_missing? defines it; builtins permissive", "\"x\".respond_to?(:upcase)   # => true"),
    ("method_missing", "Object", "wildcard: forwards any undefined method to user method_missing", "class F;def method_missing(n,*a);n;end;end;F.new.foo   # => :foo"),
    // ── Comparable ──
    ("<", "Comparable", "true if <=> is negative; incomparable raises ArgumentError", "(1 < 2)   # => true"),
    ("<=", "Comparable", "true if <=> is <= 0; incomparable raises ArgumentError", "(2 <= 2)   # => true"),
    (">", "Comparable", "true if <=> is positive; incomparable raises ArgumentError", "(3 > 2)   # => true"),
    (">=", "Comparable", "true if <=> is >= 0; incomparable raises ArgumentError", "(2 >= 3)   # => false"),
    ("between?", "Comparable", "true if lo <= self <= hi via <=>", "3.between?(1, 5)   # => true"),
    ("clamp", "Comparable", "confines self to [lo, hi] via <=>", "3.clamp(4, 10)   # => 4"),
    // ── Enumerable ──
    ("map", "Enumerable", "materialize via each; delegates to Array#map", "(1..3).map { |x| x*2 }   # => [2, 4, 6]"),
    ("collect", "Enumerable", "materialize via each; delegates to Array#collect", "(1..3).collect { |x| x*2 }   # => [2, 4, 6]"),
    ("flat_map", "Enumerable", "materialize via each; delegates to Array#flat_map", "(1..2).flat_map { |x| [x, x] }   # => [1, 1, 2, 2]"),
    ("collect_concat", "Enumerable", "materialize via each; delegates to Array#collect_concat", "[1,2,3].collect_concat { |x| [x] }   # => [1, 2, 3]"),
    ("select", "Enumerable", "materialize via each; delegates to Array#select", "(1..5).select(&:even?)   # => [2, 4]"),
    ("filter", "Enumerable", "materialize via each; delegates to Array#filter", "(1..5).filter(&:odd?)   # => [1, 3, 5]"),
    ("filter_map", "Enumerable", "materialize via each; delegates to Array#filter_map", "(1..5).filter_map { |x| x*2 if x.even? }   # => [4, 8]"),
    ("reject", "Enumerable", "materialize via each; delegates to Array#reject", "(1..5).reject(&:even?)   # => [1, 3, 5]"),
    ("reduce", "Enumerable", "materialize via each; delegates to Array#reduce", "(1..4).reduce(:+)   # => 10"),
    ("inject", "Enumerable", "materialize via each; delegates to Array#inject", "(1..4).inject(:+)   # => 10"),
    ("to_a", "Enumerable", "materialize via each; delegates to Array#to_a", "(1..3).to_a   # => [1, 2, 3]"),
    ("entries", "Enumerable", "materialize via each; delegates to Array#entries", "(1..3).entries   # => [1, 2, 3]"),
    ("find", "Enumerable", "materialize via each; delegates to Array#find", "(1..5).find(&:even?)   # => 2"),
    ("detect", "Enumerable", "materialize via each; delegates to Array#detect", "(1..5).detect { |x| x>3 }   # => 4"),
    ("find_index", "Enumerable", "materialize via each; delegates to Array#find_index", "(1..5).find_index(3)   # => 2"),
    ("count", "Enumerable", "materialize via each; delegates to Array#count", "(1..5).count   # => 5"),
    ("min", "Enumerable", "materialize via each; delegates to Array#min", "(1..5).min   # => 1"),
    ("max", "Enumerable", "materialize via each; delegates to Array#max", "(1..5).max   # => 5"),
    ("minmax", "Enumerable", "materialize via each; delegates to Array#minmax", "(1..5).minmax   # => [1, 5]"),
    ("min_by", "Enumerable", "materialize via each; delegates to Array#min_by", "(1..5).min_by { |x| -x }   # => 5"),
    ("max_by", "Enumerable", "materialize via each; delegates to Array#max_by", "(1..5).max_by { |x| -x }   # => 1"),
    ("sort", "Enumerable", "materialize via each; delegates to Array#sort", "(1..3).sort   # => [1, 2, 3]"),
    ("sort_by", "Enumerable", "materialize via each; delegates to Array#sort_by", "(1..3).sort_by { |x| -x }   # => [3, 2, 1]"),
    ("sum", "Enumerable", "materialize via each; delegates to Array#sum", "(1..3).sum   # => 6"),
    ("include?", "Enumerable", "materialize via each; delegates to Array#include?", "(1..5).include?(3)   # => true"),
    ("member?", "Enumerable", "materialize via each; delegates to Array#member?", "(1..5).member?(3)   # => true"),
    ("first", "Enumerable", "materialize via each; delegates to Array#first", "(1..5).first(2)   # => [1, 2]"),
    ("take", "Enumerable", "materialize via each; delegates to Array#take", "(1..5).take(2)   # => [1, 2]"),
    ("drop", "Enumerable", "materialize via each; delegates to Array#drop", "(1..5).drop(2)   # => [3, 4, 5]"),
    ("take_while", "Enumerable", "materialize via each; delegates to Array#take_while", "(1..5).take_while { |x| x<3 }   # => [1, 2]"),
    ("drop_while", "Enumerable", "materialize via each; delegates to Array#drop_while", "(1..5).drop_while { |x| x<3 }   # => [3, 4, 5]"),
    ("each_with_index", "Enumerable", "materialize via each; delegates to Array#each_with_index", "(1..3).each_with_index.to_a   # => [[1, 0], [2, 1], [3, 2]]"),
    ("each_with_object", "Enumerable", "materialize via each; delegates to Array#each_with_object", "(1..3).each_with_object([]) { |x,a| a << x*2 }   # => [2, 4, 6]"),
    ("group_by", "Enumerable", "materialize via each; delegates to Array#group_by", "(1..4).group_by(&:even?)   # => {false => [1, 3], true => [2, 4]}"),
    ("partition", "Enumerable", "materialize via each; delegates to Array#partition", "(1..4).partition(&:even?)   # => [[2, 4], [1, 3]]"),
    ("tally", "Enumerable", "materialize via each; delegates to Array#tally", "(1..3).tally   # => {1 => 1, 2 => 1, 3 => 1}"),
    ("uniq", "Enumerable", "materialize via each; delegates to Array#uniq", "[1,1,2].each.uniq   # => [1, 2]"),
    ("zip", "Enumerable", "materialize via each; delegates to Array#zip", "(1..3).zip([4, 5, 6])   # => [[1, 4], [2, 5], [3, 6]]"),
    ("any?", "Enumerable", "materialize via each; delegates to Array#any?", "(1..5).any?(&:even?)   # => true"),
    ("all?", "Enumerable", "materialize via each; delegates to Array#all?", "(1..5).all?(&:positive?)   # => true"),
    ("none?", "Enumerable", "materialize via each; delegates to Array#none?", "(1..5).none? { |x| x>9 }   # => true"),
    ("one?", "Enumerable", "materialize via each; delegates to Array#one?", "[1,2].each.one? { |x| x>1 }   # => true"),
    ("each_slice", "Enumerable", "materialize via each; delegates to Array#each_slice", "(1..5).each_slice(2)   # => [[1, 2], [3, 4], [5]]"),
    ("each_cons", "Enumerable", "materialize via each; delegates to Array#each_cons", "(1..4).each_cons(2)   # => [[1, 2], [2, 3], [3, 4]]"),
    ("chunk_while", "Enumerable", "materialize via each; delegates to Array#chunk_while", "(1..4).chunk_while { |a,b| b-a==1 }   # => [[1, 2, 3, 4]]"),
    ("to_h", "Enumerable", "materialize via each; delegates to Array#to_h", "(1..3).to_h { |x| [x, x*x] }   # => {1 => 1, 2 => 4, 3 => 9}"),
    // ── Class ──
    ("new", "Class", "Struct.new defines a new struct class; other classes instantiate", "Struct.new(:x).new(1).x   # => 1"),
    ("[]", "Class", "Struct/Set/Hash/Array [...] class constructor forms", "Array[1, 2, 3]   # => [1, 2, 3]"),
    ("members", "Class", "Struct class: member names as symbols", "Struct.new(:a, :b).members   # => [:a, :b]"),
    ("yield", "Class", "Fiber.yield: suspends the running fiber with a value", "Fiber.new { Fiber.yield 1 }.resume   # => 1"),
    ("at", "Class", "Time.at: time from epoch seconds (UTC)", "Time.at(0).year   # => 1970"),
    ("utc", "Class", "Time.utc: build UTC time from broken-down fields", "Time.utc(2020, 1, 1).year   # => 2020"),
    ("gm", "Class", "Time.gm: build UTC time from broken-down fields", "Time.gm(2020).month   # => 1"),
    ("local", "Class", "Time.local: build UTC time from fields (local tz not modeled)", "Time.local(2020, 6).month   # => 6"),
    ("mktime", "Class", "Time.mktime: build UTC time from fields (local tz not modeled)", "Time.mktime(2020).year   # => 2020"),
    ("now", "Class", "Time.now/DateTime.now: current system time", "Time.now.class   # => Time"),
    ("civil", "Class", "Date/DateTime.civil: date(-time) from y,m,d[,h,m,s]", "Date.civil(2020, 1, 1).year   # => 2020"),
    ("today", "Class", "Date.today: today's date from the system clock (UTC)", "Date.today.class   # => Date"),
    ("jd", "Class", "Date/DateTime.jd: from a Julian Day Number", "Date.jd(2451545).class   # => Date"),
    ("parse", "Class", "Date/DateTime.parse: ISO string, else raises ArgumentError", "Date.parse(\"2020-06-15\").month   # => 6"),
    ("PI", "Class", "Math::PI constant", "Math::PI   # => 3.141592653589793"),
    ("E", "Class", "Math::E constant", "Math::E.class   # => Float"),
    ("sqrt", "Class", "Math.sqrt: square root", "Math.sqrt(16)   # => 4.0"),
    ("cbrt", "Class", "Math.cbrt: cube root", "Math.cbrt(27)   # => 3.0"),
    ("sin", "Class", "Math.sin", "Math.sin(0)   # => 0.0"),
    ("cos", "Class", "Math.cos", "Math.cos(0)   # => 1.0"),
    ("tan", "Class", "Math.tan", "Math.tan(0)   # => 0.0"),
    ("asin", "Class", "Math.asin", "Math.asin(0)   # => 0.0"),
    ("acos", "Class", "Math.acos", "Math.acos(1)   # => 0.0"),
    ("atan", "Class", "Math.atan", "Math.atan(0)   # => 0.0"),
    ("atan2", "Class", "Math.atan2(x, y)", "Math.atan2(1, 1)   # => 0.7853981633974483"),
    ("sinh", "Class", "Math.sinh", "Math.sinh(0)   # => 0.0"),
    ("cosh", "Class", "Math.cosh", "Math.cosh(0)   # => 1.0"),
    ("tanh", "Class", "Math.tanh", "Math.tanh(0)   # => 0.0"),
    ("asinh", "Class", "Math.asinh", "Math.asinh(0)   # => 0.0"),
    ("acosh", "Class", "Math.acosh", "Math.acosh(1)   # => 0.0"),
    ("atanh", "Class", "Math.atanh", "Math.atanh(0)   # => 0.0"),
    ("exp", "Class", "Math.exp: e**x", "Math.exp(0)   # => 1.0"),
    ("log", "Class", "Math.log: natural log, or log base y when given 2 args", "Math.log(8, 2)   # => 3.0"),
    ("log2", "Class", "Math.log2", "Math.log2(8)   # => 3.0"),
    ("log10", "Class", "Math.log10", "Math.log10(1000)   # => 3.0"),
    ("hypot", "Class", "Math.hypot(x, y)", "Math.hypot(3, 4)   # => 5.0"),
    ("ldexp", "Class", "Math.ldexp: x * 2**exp", "Math.ldexp(1, 3)   # => 8.0"),
    ("gamma", "Class", "Math.gamma", "Math.gamma(5)   # => 24.0 (approx)"),
    ("erf", "Class", "Math.erf", "Math.erf(1)   # => 0.8427 (approx)"),
    ("erfc", "Class", "Math.erfc: 1 - erf(x)", "Math.erfc(1)   # => 0.1573 (approx)"),
    ("generate", "Class", "JSON.generate/dump: encode value to JSON string", "JSON.generate([1, 2])   # => \"[1,2]\""),
    ("dump", "Class", "JSON.generate/dump: encode value to JSON string", "JSON.dump([1, 2])   # => \"[1,2]\""),
    ("pretty_generate", "Class", "JSON.pretty_generate: indented JSON string", "JSON.pretty_generate([1])   # => \"[\\n  1\\n]\""),
    ("load", "Class", "JSON.parse/load: decode JSON; symbolize_names option", "JSON.load(\"[1, 2]\")   # => [1, 2]"),
    ("name", "Class", "the class name string", "Integer.name   # => \"Integer\""),
    ("superclass", "Class", "direct superclass ref, or nil for BasicObject", "Integer.superclass   # => Numeric"),
    ("ancestors", "Class", "ancestor chain as class refs", "Integer.ancestors   # => [Integer, Numeric, Comparable, Object, Kernel, BasicObject]"),
    ("instance_methods", "Class", "instance method names as symbols; visibility not modeled", "class B; def f; end; end; B.instance_methods   # => [:f]"),
    ("public_instance_methods", "Class", "same as instance_methods; visibility not modeled", "class B; def f; end; end; B.public_instance_methods   # => [:f]"),
    ("method_defined?", "Class", "true if method defined on class or ancestor", "class B; def f; end; end; B.method_defined?(:f)   # => true"),
    ("public_method_defined?", "Class", "same as method_defined?; visibility not modeled", "class B; def f; end; end; B.public_method_defined?(:f)   # => true"),
    ("INFINITY", "Class", "Float::INFINITY", "Float::INFINITY   # => Infinity"),
    ("NAN", "Class", "Float::NAN", "Float::NAN   # => NaN"),
    ("MAX", "Class", "Float::MAX", "Float::MAX.class   # => Float"),
    ("MIN", "Class", "Float::MIN (smallest positive normal)", "Float::MIN.class   # => Float"),
    ("EPSILON", "Class", "Float::EPSILON", "Float::EPSILON.class   # => Float"),
    ("DIG", "Class", "Float::DIG, returns 15", "Float::DIG   # => 15"),
    ("MANT_DIG", "Class", "Float::MANT_DIG, returns 53", "Float::MANT_DIG   # => 53"),
    // ── TrueClass/FalseClass ──
    ("&", "TrueClass/FalseClass", "logical AND by truthiness", "(true & false)   # => false"),
    ("|", "TrueClass/FalseClass", "logical OR by truthiness", "(true | false)   # => true"),
    ("^", "TrueClass/FalseClass", "logical XOR by truthiness", "(true ^ true)   # => false"),
    ("!", "TrueClass/FalseClass", "logical negation of receiver truthiness", "(!true)   # => false"),
    ("to_s", "TrueClass/FalseClass", "host to_s string of true/false/nil", "true.to_s   # => \"true\""),
    ("inspect", "TrueClass/FalseClass", "host to_s string of true/false/nil (same as to_s)", "false.inspect   # => \"false\""),
    ("to_a", "TrueClass/FalseClass", "nil.to_a returns [] (guarded to nil only)", "nil.to_a   # => []"),
    ("to_h", "TrueClass/FalseClass", "nil.to_h returns {} (guarded to nil only)", "nil.to_h   # => {}"),
    // ── Integer ──
    ("to_s", "Integer", "renders bigint in given radix (default 10)", "255.to_s(16)   # => \"ff\""),
    ("inspect", "Integer", "renders bigint in given radix (default 10)", "255.inspect   # => \"255\""),
    ("to_i", "Integer", "returns self unchanged", "5.to_i   # => 5"),
    ("to_int", "Integer", "returns self unchanged", "5.to_int   # => 5"),
    ("floor", "Integer", "returns self unchanged (ignores any ndigits arg)", "5.floor(2)   # => 5"),
    ("ceil", "Integer", "returns self unchanged (ignores any ndigits arg)", "5.ceil(2)   # => 5"),
    ("round", "Integer", "returns self unchanged (ignores any ndigits arg)", "5.round(2)   # => 5"),
    ("truncate", "Integer", "returns self unchanged (ignores any ndigits arg)", "5.truncate(2)   # => 5"),
    ("to_f", "Integer", "converts to Float, INFINITY when out of f64 range", "5.to_f   # => 5.0"),
    ("abs", "Integer", "absolute value", "(-5).abs   # => 5"),
    ("magnitude", "Integer", "absolute value", "(-5).magnitude   # => 5"),
    ("-@", "Integer", "arithmetic negation", "-(5)   # => -5"),
    ("bit_length", "Integer", "number of bits in the value", "255.bit_length   # => 8"),
    ("even?", "Integer", "true if divisible by 2", "4.even?   # => true"),
    ("odd?", "Integer", "true if not divisible by 2", "3.odd?   # => true"),
    ("zero?", "Integer", "true if the value is 0", "0.zero?   # => true"),
    ("positive?", "Integer", "true if greater than 0", "5.positive?   # => true"),
    ("negative?", "Integer", "true if less than 0", "(-5).negative?   # => true"),
    ("integer?", "Integer", "always true", "5.integer?   # => true"),
    ("hash", "Integer", "i64 value, or bit_length as fallback when out of range", "(10**30).hash   # => 100"),
    ("succ", "Integer", "returns self + 1", "5.succ   # => 6"),
    ("next", "Integer", "returns self + 1", "5.next   # => 6"),
    ("pred", "Integer", "returns self - 1", "5.pred   # => 4"),
    ("digits", "Integer", "digits of abs(self) in base (default 10, min 2), low-first", "123.digits   # => [3, 2, 1]"),
    ("/", "Integer", "floored division; ZeroDivisionError on 0, nil if not Integer", "(-7 / 2)   # => -4"),
    ("div", "Integer", "floored division; ZeroDivisionError on 0, nil if not Integer", "(10**20).div(3)   # => 33333333333333333333"),
    ("%", "Integer", "floored modulo; ZeroDivisionError on 0, nil if not Integer", "(-7 % 3)   # => 2"),
    ("modulo", "Integer", "floored modulo; ZeroDivisionError on 0, nil if not Integer", "(-7).modulo(3)   # => 2"),
    ("divmod", "Integer", "[floored quotient, floored modulo]; ZeroDivisionError on 0", "(-7).divmod(3)   # => [-3, 2]"),
    ("<=>", "Integer", "compare returning -1/0/1, nil when arg is not Integer", "(1 <=> 2)   # => -1"),
    ("coerce", "Integer", "returns [arg, self] without numeric conversion", "1.coerce(2)   # => [2, 1]"),
    // ── Numeric ──
    ("times", "Numeric", "yields 0...n; block-less returns an Enumerator", "3.times { |i| print i }   # prints 012"),
    ("**", "Numeric", "exact Integer power (BigInt on overflow), else Float powf", "(2 ** 10)   # => 1024"),
    ("pow", "Numeric", "power; 2-arg form is modular exp, neg exp+mod raises RangeError", "5.pow(3, 7)   # => 6"),
    ("/", "Numeric", "Int/Int floored div, Int/Rational exact, else Float; ZeroDivisionError", "(7 / 2)   # => 3"),
    ("%", "Numeric", "Int floored mod, else float x - floor(x/y)*y; ZeroDivisionError", "(-7 % 3)   # => 2"),
    ("modulo", "Numeric", "Int floored mod, else float x - floor(x/y)*y; ZeroDivisionError", "7.modulo(3)   # => 1"),
    ("step", "Numeric", "iterates by step; all-Int stays Int else Float; block-less Enumerator", "1.step(5, 2) { |i| print i }   # prints 135"),
    ("upto", "Numeric", "iterates self..limit by 1; block-less returns Enumerator", "1.upto(3) { |i| print i }   # prints 123"),
    ("downto", "Numeric", "iterates self down to limit by -1; block-less Enumerator", "3.downto(1) { |i| print i }   # prints 321"),
    ("to_s", "Numeric", "Integer in radix 2..=36, else default string form", "255.to_s(16)   # => \"ff\""),
    ("to_i", "Numeric", "truncates to Integer", "3.7.to_i   # => 3"),
    ("to_int", "Numeric", "truncates to Integer", "3.7.to_int   # => 3"),
    ("to_f", "Numeric", "converts to Float", "3.to_f   # => 3.0"),
    ("to_r", "Numeric", "Int as n/1, Float as exact rational; FloatDomainError on NaN/Inf", "3.to_r   # => (3/1)"),
    ("rationalize", "Numeric", "simplest rational within eps (default round-trips the f64)", "0.3.rationalize   # => (3/10)"),
    ("to_c", "Numeric", "returns Complex (self + 0i)", "5.to_c   # => (5+0i)"),
    ("coerce", "Numeric", "[b, a] as Int when both Int, else both as Float", "1.5.coerce(2)   # => [2.0, 1.5]"),
    ("abs", "Numeric", "absolute value, preserving Int/Float type", "(-5).abs   # => 5"),
    ("magnitude", "Numeric", "absolute value, preserving Int/Float type", "(-2.5).magnitude   # => 2.5"),
    ("abs2", "Numeric", "self * self, preserving Int/Float type", "3.abs2   # => 9"),
    ("even?", "Numeric", "true if as_i(self) % 2 == 0", "4.even?   # => true"),
    ("odd?", "Numeric", "true if as_i(self) % 2 != 0", "3.odd?   # => true"),
    ("zero?", "Numeric", "true if value equals 0.0", "0.0.zero?   # => true"),
    ("nonzero?", "Numeric", "returns self if non-zero, else nil", "5.0.nonzero?   # => 5.0"),
    ("positive?", "Numeric", "true if greater than 0", "5.positive?   # => true"),
    ("negative?", "Numeric", "true if less than 0", "(-5).negative?   # => true"),
    ("integer?", "Numeric", "true for Int, false for Float", "3.5.integer?   # => false"),
    ("succ", "Numeric", "returns as_i(self) + 1", "5.succ   # => 6"),
    ("next", "Numeric", "returns as_i(self) + 1", "5.next   # => 6"),
    ("pred", "Numeric", "returns as_i(self) - 1", "5.pred   # => 4"),
    ("floor", "Numeric", "floor to optional ndigits; Float only when ndigits > 0", "3.14159.floor(2)   # => 3.14"),
    ("ceil", "Numeric", "ceil to optional ndigits; Float only when ndigits > 0", "3.7.ceil   # => 4"),
    ("round", "Numeric", "round to optional ndigits; Float only when ndigits > 0", "3.14159.round(2)   # => 3.14"),
    ("truncate", "Numeric", "truncate to optional ndigits; Float only when ndigits > 0", "3.7.truncate   # => 3"),
    ("nan?", "Numeric", "true if Float is NaN (only dispatched for Float receiver)", "(0.0/0.0).nan?   # => true"),
    ("infinite?", "Numeric", "1/-1 if infinite Float, else nil", "(1.0/0.0).infinite?   # => 1"),
    ("finite?", "Numeric", "true if finite Float, true for Int", "5.0.finite?   # => true"),
    ("divmod", "Numeric", "Int [floor div, floor mod] else [floor q as Int, Float rem]", "7.divmod(2)   # => [3, 1]"),
    ("chr", "Numeric", "1-char string from low byte of self (u8 as char)", "65.chr   # => \"A\""),
    ("gcd", "Numeric", "gcd(self, arg); ArgumentError if no arg", "10.gcd(4)   # => 2"),
    ("lcm", "Numeric", "lcm(self, arg), 0 if either is 0; ArgumentError if no arg", "4.lcm(6)   # => 12"),
    ("gcdlcm", "Numeric", "[gcd, lcm]; ArgumentError if no arg", "6.gcdlcm(4)   # => [2, 12]"),
    ("ceildiv", "Numeric", "ceiling division -floor_div(-a,b); ZeroDivisionError", "10.ceildiv(3)   # => 4"),
    ("[]", "Numeric", "bit i of two's-complement; neg i -> 0, i>=64 -> sign bit", "5[0]   # => 1"),
    ("digits", "Numeric", "digits in base (default 10, min 2); Math::DomainError if negative", "123.digits   # => [3, 2, 1]"),
    ("bit_length", "Numeric", "bit length of self, using ~n for negatives", "255.bit_length   # => 8"),
    ("fdiv", "Numeric", "float division of self by arg", "5.fdiv(2)   # => 2.5"),
    ("clamp", "Numeric", "clamps to lo/hi or range bounds, returning bound's own type", "3.clamp(4, 10)   # => 4"),
    ("<=>", "Numeric", "compare as floats: -1/0/1, nil when unordered (NaN)", "(5.0 <=> 3.0)   # => 1"),
    ("<<", "Numeric", "left shift via BigInt (promotes on overflow); TypeError if non-Int", "(1 << 4)   # => 16"),
    (">>", "Numeric", "right shift via BigInt; TypeError if arg not Integer", "(16 >> 2)   # => 4"),
    ("&", "Numeric", "bitwise AND via BigInt; TypeError if arg not Integer", "(6 & 3)   # => 2"),
    ("|", "Numeric", "bitwise OR via BigInt; TypeError if arg not Integer", "(5 | 2)   # => 7"),
    ("^", "Numeric", "bitwise XOR via BigInt; TypeError if arg not Integer", "(5 ^ 1)   # => 4"),
    ("between?", "Numeric", "true if arg0 <= self <= arg1 (compared as floats)", "3.between?(1, 5)   # => true"),
    ("+", "Numeric", "addition, Int/Int stays Int else Float", "(2 + 3)   # => 5"),
    ("-", "Numeric", "subtraction, Int/Int stays Int else Float", "(5 - 3)   # => 2"),
    ("*", "Numeric", "multiplication, Int/Int stays Int else Float", "(4 * 3)   # => 12"),
    ("<", "Numeric", "less-than comparison", "(1 < 2)   # => true"),
    (">", "Numeric", "greater-than comparison", "(3 > 2)   # => true"),
    ("<=", "Numeric", "less-or-equal comparison", "(2 <= 2)   # => true"),
    (">=", "Numeric", "greater-or-equal comparison", "(3 >= 2)   # => true"),
    // ── Rational ──
    ("numerator", "Rational", "Returns the numerator as a bigint.", "Rational(3, 4).numerator   # => 3"),
    ("denominator", "Rational", "Returns the denominator as a bigint.", "Rational(3, 4).denominator   # => 4"),
    ("to_f", "Rational", "Converts to Float (NAN if unrepresentable).", "Rational(1, 2).to_f   # => 0.5"),
    ("to_i", "Rational", "Truncates toward zero to an integer.", "Rational(7, 2).to_i   # => 3"),
    ("to_int", "Rational", "Truncates toward zero to an integer.", "Rational(7, 2).to_int   # => 3"),
    ("truncate", "Rational", "Truncates toward zero to an integer (ignores digits arg).", "Rational(7, 2).truncate   # => 3"),
    ("to_r", "Rational", "Returns self unchanged.", "Rational(1, 2).to_r   # => (1/2)"),
    ("abs", "Rational", "Absolute value as a Rational.", "Rational(-1, 2).abs   # => (1/2)"),
    ("magnitude", "Rational", "Absolute value as a Rational.", "Rational(-1, 2).magnitude   # => (1/2)"),
    ("-@", "Rational", "Negates the rational.", "Rational(0, 1) - Rational(1, 2)   # => (-1/2)"),
    ("+@", "Rational", "Returns self unchanged.", "+Rational(1, 2)   # => (1/2)"),
    ("zero?", "Rational", "True if the value is zero.", "Rational(0, 1).zero?   # => true"),
    ("positive?", "Rational", "True if the value is positive.", "Rational(1, 2).positive?   # => true"),
    ("negative?", "Rational", "True if the value is negative.", "Rational(-1, 2).negative?   # => true"),
    ("/", "Rational", "Divides; ZeroDivisionError on 0, Float if arg not rational.", "(Rational(1, 2) / Rational(1, 4))   # => (2/1)"),
    ("quo", "Rational", "Divides; ZeroDivisionError on 0, Float if arg not rational.", "Rational(1, 2).quo(Rational(1, 4))   # => (2/1)"),
    ("**", "Rational", "Power; integer exp exact (Rational), else Float via powf.", "(Rational(2, 3) ** 2)   # => (4/9)"),
    ("pow", "Rational", "Power; integer exp exact (Rational), else Float via powf.", "Rational(2, 3).pow(2)   # => (4/9)"),
    ("<=>", "Rational", "Compares; returns -1/0/1 or nil if arg not rational.", "(Rational(1, 2) <=> Rational(1, 3))   # => 1"),
    ("coerce", "Rational", "Returns [other-as-rational, self].", "Rational(1, 2).coerce(2)   # => [(2/1), (1/2)]"),
    ("hash", "Rational", "Hash as the value's i64 (0 on overflow).", "Rational(1, 2).hash   # => 0"),
    ("integer?", "Rational", "True if the denominator is 1.", "Rational(4, 2).integer?   # => true"),
    ("+", "Rational", "Addition via numeric hook.", "(Rational(1, 2) + Rational(1, 3))   # => (5/6)"),
    ("-", "Rational", "Subtraction via numeric hook.", "(Rational(1, 2) - Rational(1, 3))   # => (1/6)"),
    ("*", "Rational", "Multiplication via numeric hook.", "(Rational(2, 3) * Rational(3, 4))   # => (1/2)"),
    ("==", "Rational", "Equality via numeric hook.", "(Rational(1, 2) == Rational(1, 2))   # => true"),
    ("<", "Rational", "Less-than via numeric hook.", "(Rational(1, 3) < Rational(1, 2))   # => true"),
    (">", "Rational", "Greater-than via numeric hook.", "(Rational(1, 2) > Rational(1, 3))   # => true"),
    ("<=", "Rational", "Less-or-equal via numeric hook.", "(Rational(1, 2) <= Rational(1, 2))   # => true"),
    (">=", "Rational", "Greater-or-equal via numeric hook.", "(Rational(1, 2) >= Rational(1, 3))   # => true"),
    // ── Complex ──
    ("real", "Complex", "Returns the real part.", "Complex(3, 4).real   # => 3"),
    ("imaginary", "Complex", "Returns the imaginary part.", "Complex(3, 4).imaginary   # => 4"),
    ("imag", "Complex", "Returns the imaginary part.", "Complex(3, 4).imag   # => 4"),
    ("to_s", "Complex", "String form via host complex_to_s.", "Complex(1, 2).to_s   # => \"1+2i\""),
    ("abs", "Complex", "Magnitude sqrt(re^2+im^2) as a Float.", "Complex(3, 4).abs   # => 5.0"),
    ("magnitude", "Complex", "Magnitude sqrt(re^2+im^2) as a Float.", "Complex(3, 4).magnitude   # => 5.0"),
    ("abs2", "Complex", "Squared magnitude re*re + im*im.", "Complex(3, 4).abs2   # => 25"),
    ("conjugate", "Complex", "Complex conjugate (negates imaginary part).", "Complex(3, 4).conjugate   # => (3-4i)"),
    ("conj", "Complex", "Complex conjugate (negates imaginary part).", "Complex(3, 4).conj   # => (3-4i)"),
    ("rectangular", "Complex", "Returns [real, imaginary] array.", "Complex(3, 4).rectangular   # => [3, 4]"),
    ("rect", "Complex", "Returns [real, imaginary] array.", "Complex(3, 4).rect   # => [3, 4]"),
    ("-@", "Complex", "Negates both real and imaginary parts.", "Complex(0, 0) - Complex(3, 4)   # => (-3-4i)"),
    ("+@", "Complex", "Returns self unchanged.", "+Complex(3, 4)   # => (3+4i)"),
    ("==", "Complex", "Equality via host eq_values.", "(Complex(1, 2) == Complex(1, 2))   # => true"),
    ("+", "Complex", "Addition via numeric hook.", "(Complex(1, 2) + Complex(3, 4))   # => (4+6i)"),
    ("-", "Complex", "Subtraction via numeric hook.", "(Complex(5, 6) - Complex(1, 2))   # => (4+4i)"),
    ("*", "Complex", "Multiplication via numeric hook.", "(Complex(1, 2) * Complex(1, 2))   # => (-3+4i)"),
    ("**", "Complex", "Power by repeated mul; raises on non-integer/negative exp.", "(Complex(1, 2) ** 2)   # => (-3+4i)"),
    ("pow", "Complex", "Power by repeated mul; raises on non-integer/negative exp.", "Complex(1, 2).pow(2)   # => (-3+4i)"),
    // ── String ──
    ("length", "String", "char count of the string (alias size)", "\"héllo\".length   # => 5"),
    ("size", "String", "char count of the string (alias length)", "\"abc\".size   # => 3"),
    ("upcase", "String", "uppercase; :ascii option limits to ASCII letters", "\"abc\".upcase   # => \"ABC\""),
    ("downcase", "String", "lowercase; :ascii option limits to ASCII letters", "\"ABC\".downcase   # => \"abc\""),
    ("swapcase", "String", "invert case of each char, returning a new String", "\"Abc\".swapcase   # => \"aBC\""),
    ("capitalize", "String", "uppercase first char, lowercase the rest", "\"hELLO\".capitalize   # => \"Hello\""),
    ("reverse", "String", "reverse the characters", "\"abc\".reverse   # => \"cba\""),
    ("strip", "String", "trim leading and trailing whitespace", "\"  hi  \".strip   # => \"hi\""),
    ("lstrip", "String", "trim leading whitespace", "\"  hi\".lstrip   # => \"hi\""),
    ("rstrip", "String", "trim trailing whitespace", "\"hi  \".rstrip   # => \"hi\""),
    ("chomp", "String", "remove trailing separator/newline; empty arg strips all trailing \r\n", "\"hi\\n\".chomp   # => \"hi\""),
    ("chop", "String", "remove last char; trailing \r\n counts as one", "\"hi!\".chop   # => \"hi\""),
    ("chars", "String", "array of single-char strings", "\"abc\".chars   # => [\"a\", \"b\", \"c\"]"),
    ("bytes", "String", "array of UTF-8 byte integers", "\"AB\".bytes   # => [65, 66]"),
    ("bytesize", "String", "number of UTF-8 bytes, not chars", "\"é\".bytesize   # => 2"),
    ("each_byte", "String", "with block yield each byte, return self; else Enumerator of bytes", "\"AB\".each_byte { |b| }   # => \"AB\""),
    ("getbyte", "String", "byte at index (negatives ok); nil if out of range", "\"AB\".getbyte(0)   # => 65"),
    ("b", "String", "copy of the string (ASCII-8BIT shim, bytes unchanged)", "\"abc\".b   # => \"abc\""),
    ("ascii_only?", "String", "true when every byte is 7-bit ASCII", "\"abc\".ascii_only?   # => true"),
    ("valid_encoding?", "String", "always true (only valid UTF-8 is carried)", "\"abc\".valid_encoding?   # => true"),
    ("force_encoding", "String", "no-op shim returning self", "\"abc\".force_encoding(\"UTF-8\")   # => \"abc\""),
    ("encode", "String", "no-op shim returning a copy", "\"abc\".encode(\"UTF-8\")   # => \"abc\""),
    ("lines", "String", "array of lines (keeping newlines)", "\"a\\nb\\n\".lines   # => [\"a\\n\", \"b\\n\"]"),
    ("each_line", "String", "with block yield each line, return self; else Enumerator of lines", "\"a\\nb\\n\".each_line { |l| }   # => \"a\\nb\\n\""),
    ("center", "String", "center-pad to width using pad string (default space)", "\"hi\".center(6)   # => \"  hi  \""),
    ("tr", "String", "translate chars from spec to spec, ranges/negation supported", "\"hello\".tr(\"el\",\"ip\")   # => \"hippo\""),
    ("delete", "String", "remove chars matching the spec(s)", "\"hello\".delete(\"l\")   # => \"heo\""),
    ("count", "String", "count chars matching the spec(s)", "\"hello\".count(\"l\")   # => 2"),
    ("squeeze", "String", "collapse adjacent duplicate chars (limited to spec if given)", "\"aaabbb\".squeeze   # => \"ab\""),
    ("empty?", "String", "true if the string has no chars", "\"\".empty?   # => true"),
    ("to_i", "String", "parse leading integer in base (default 10); 0 if none", "\"42abc\".to_i   # => 42"),
    ("hex", "String", "parse leading hex integer; 0 if none", "\"ff\".hex   # => 255"),
    ("oct", "String", "parse leading base-8 int; radix prefix auto-detects base", "\"0x1f\".oct   # => 31"),
    ("to_f", "String", "parse leading float, 0.0 if none", "\"3.14xy\".to_f   # => 3.14"),
    ("to_r", "String", "parse leading rational (\"3/4\", \"3.14\"); else 0/1", "\"3/4\".to_r   # => (3/4)"),
    ("to_s", "String", "returns self (alias to_str)", "\"abc\".to_s   # => \"abc\""),
    ("to_str", "String", "returns self (alias to_s)", "\"abc\".to_str   # => \"abc\""),
    ("to_sym", "String", "the Symbol with this name", "\"abc\".to_sym   # => :abc"),
    ("include?", "String", "true if it contains the substring arg", "\"hello\".include?(\"ell\")   # => true"),
    ("start_with?", "String", "true if any arg matches at the start (Regexp anchored at 0)", "\"hello\".start_with?(\"he\")   # => true"),
    ("end_with?", "String", "true if any string arg is a suffix", "\"hello\".end_with?(\"lo\")   # => true"),
    ("match?", "String", "true if the Regexp/pattern matches; sets no $~", "\"hello\".match?(/l+/)   # => true"),
    ("=~", "String", "match Regexp, set $~/$1..; return char offset or nil", "\"hello\" =~ /l/   # => 2"),
    ("match", "String", "return MatchData for the Regexp, else nil", "\"hello\".match(/l/)   # => #<MatchData \"l\">"),
    ("scan", "String", "with block yield each match return self; else array of matches", "\"a1b2\".scan(/\\d/)   # => [\"1\", \"2\"]"),
    ("split", "String", "split by pattern/string/awk-mode with optional limit", "\"a,b,c\".split(\",\")   # => [\"a\", \"b\", \"c\"]"),
    ("sub", "String", "replace first match of Regexp/string via arg or block", "\"hello\".sub(\"l\",\"L\")   # => \"heLlo\""),
    ("gsub", "String", "replace all matches of Regexp/string via arg or block", "\"hello\".gsub(\"l\",\"L\")   # => \"heLLo\""),
    ("replace", "String", "overwrite receiver contents in place, return self", "\"x\".replace(\"yz\")   # => \"yz\""),
    ("<<", "String", "append arg's to_s in place, return self", "\"ab\" << \"c\"   # => \"abc\""),
    ("+", "String", "return new string concatenating self and arg's to_s", "\"ab\" + \"cd\"   # => \"abcd\""),
    ("concat", "String", "append all args in order in place, return self", "\"a\".concat(\"b\",\"c\")   # => \"abc\""),
    ("<=>", "String", "compare strings -1/0/1; nil if arg not a String", "\"a\" <=> \"b\"   # => -1"),
    ("between?", "String", "true if self is between lo and hi inclusive", "\"b\".between?(\"a\",\"c\")   # => true"),
    ("clamp", "String", "clamp to lo/hi or inclusive range; exclusive range errors", "\"z\".clamp(\"a\",\"m\")   # => \"m\""),
    ("*", "String", "repeat the string n times", "\"ab\" * 3   # => \"ababab\""),
    ("%", "String", "sprintf-format self with array/hash/single operand", "\"%d-%s\" % [1,\"x\"]   # => \"1-x\""),
    ("ljust", "String", "left-justify padded to width (default space)", "\"hi\".ljust(5)   # => \"hi   \""),
    ("rjust", "String", "right-justify padded to width (default space)", "\"hi\".rjust(5)   # => \"   hi\""),
    ("each_char", "String", "with block yield each char, return self; else Enumerator of chars", "\"ab\".each_char { |c| }   # => \"ab\""),
    ("[]", "String", "substring by index/range/regexp (alias slice)", "\"hello\"[1,3]   # => \"ell\""),
    ("slice", "String", "substring by index/range/regexp (alias [])", "\"hello\".slice(0,2)   # => \"he\""),
    ("eql?", "String", "content equality, only another String can be equal", "\"ab\".eql?(\"ab\")   # => true"),
    ("slice!", "String", "remove and return the sliced portion in place; nil if none", "\"hello\".slice!(0,2)   # => \"he\""),
    ("ord", "String", "codepoint of first char; ArgumentError if empty", "\"A\".ord   # => 65"),
    ("chr", "String", "first char as a string, empty if none", "\"abc\".chr   # => \"a\""),
    ("partition", "String", "split on first match of sep into [before, sep, after]", "\"a-b-c\".partition(\"-\")   # => [\"a\", \"-\", \"b-c\"]"),
    ("rpartition", "String", "split on last match of sep into [before, sep, after]", "\"a-b-c\".rpartition(\"-\")   # => [\"a-b\", \"-\", \"c\"]"),
    ("casecmp", "String", "case-insensitive compare returning -1/0/1", "\"ABC\".casecmp(\"abc\")   # => 0"),
    ("casecmp?", "String", "case-insensitive equality boolean", "\"ABC\".casecmp?(\"abc\")   # => true"),
    ("tr_s", "String", "translate like tr then squeeze runs of translated chars", "\"aabbcc\".tr_s(\"ab\",\"x\")   # => \"xcc\""),
    ("succ", "String", "next string in succession (alias next)", "\"az\".succ   # => \"ba\""),
    ("next", "String", "next string in succession (alias succ)", "\"az\".next   # => \"ba\""),
    ("insert", "String", "insert arg before index (negative inserts after) in place", "\"abc\".insert(1,\"X\")   # => \"aXbc\""),
    ("prepend", "String", "prepend all args in place, return self", "\"b\".prepend(\"a\")   # => \"ab\""),
    ("index", "String", "char offset of first substring match from optional pos; nil", "\"hello\".index(\"l\")   # => 2"),
    ("rindex", "String", "char offset of last substring match up to optional pos; nil", "\"hello\".rindex(\"l\")   # => 3"),
    ("[]=", "String", "assign to indexed/range/regexp slice in place", "s=\"hi\"; s[0]=\"H\"; s   # => \"Hi\""),
    // ── Symbol ──
    ("to_s", "Symbol", "the name as a String (alias id2name, name)", ":abc.to_s   # => \"abc\""),
    ("id2name", "Symbol", "the name as a String (alias to_s, name)", ":abc.id2name   # => \"abc\""),
    ("name", "Symbol", "the name as a String (alias to_s, id2name)", ":abc.name   # => \"abc\""),
    ("to_sym", "Symbol", "returns self (alias intern)", ":abc.to_sym   # => :abc"),
    ("intern", "Symbol", "returns self (alias to_sym)", ":abc.intern   # => :abc"),
    ("length", "Symbol", "char count of the name (alias size)", ":abc.length   # => 3"),
    ("size", "Symbol", "char count of the name (alias length)", ":abc.size   # => 3"),
    ("empty?", "Symbol", "true if the name is empty", "\"\".to_sym.empty?   # => true"),
    ("upcase", "Symbol", "uppercased name as a Symbol", ":abc.upcase   # => :ABC"),
    ("downcase", "Symbol", "lowercased name as a Symbol", ":ABC.downcase   # => :abc"),
    ("succ", "Symbol", "next name in succession as a Symbol (alias next)", ":ab.succ   # => :ac"),
    ("next", "Symbol", "next name in succession as a Symbol (alias succ)", ":ab.next   # => :ac"),
    ("swapcase", "Symbol", "case-inverted name as a Symbol", ":Abc.swapcase   # => :aBC"),
    ("capitalize", "Symbol", "capitalized name as a Symbol", ":hello.capitalize   # => :Hello"),
    ("[]", "Symbol", "index the name, returning a String (alias slice)", ":hello[1,3]   # => \"ell\""),
    ("slice", "Symbol", "index the name, returning a String (alias [])", ":hello.slice(0,2)   # => \"he\""),
    ("start_with?", "Symbol", "true if any arg is a prefix of the name", ":hello.start_with?(\"he\")   # => true"),
    ("end_with?", "Symbol", "true if any arg is a suffix of the name", ":hello.end_with?(\"lo\")   # => true"),
    ("match?", "Symbol", "true if the name matches the Regexp/pattern, no $~", ":hello.match?(/l+/)   # => true"),
    ("<=>", "Symbol", "compare names -1/0/1; nil if arg not a Symbol", ":a <=> :b   # => -1"),
    ("to_proc", "Symbol", "proc that sends the named method to its first arg", "[1,2].map(&:to_s)   # => [\"1\", \"2\"]"),
    // ── Regexp ──
    ("source", "Regexp", "the pattern source string", "/ab+/.source   # => \"ab+\""),
    ("match?", "Regexp", "true if the pattern matches the arg, no $~", "/l+/.match?(\"hello\")   # => true"),
    ("=~", "Regexp", "match arg, set $~/$1..; return char offset or nil", "/l/ =~ \"hello\"   # => 2"),
    ("match", "Regexp", "return MatchData for the arg string", "/l/.match(\"hello\")   # => #<MatchData \"l\">"),
    ("scan", "Regexp", "array of all matches in the arg string", "/\\d/.scan(\"a1b2\")   # => [\"1\", \"2\"]"),
    ("to_s", "Regexp", "string form \"(?-mix:source)\"", "/ab/.to_s   # => \"(?-mix:ab)\""),
    ("inspect", "Regexp", "literal form \"/source/\"", "/ab/.inspect   # => \"/ab/\""),
    // ── MatchData ──
    ("[]", "MatchData", "group string at integer index; nil if absent", "\"hello\".match(/(l)(l)/)[1]   # => \"l\""),
    ("pre_match", "MatchData", "text before the match", "\"hello\".match(/ll/).pre_match   # => \"he\""),
    ("post_match", "MatchData", "text after the match", "\"hello\".match(/ll/).post_match   # => \"o\""),
    ("to_a", "MatchData", "array of whole match plus all groups", "\"hello\".match(/(l)(l)/).to_a   # => [\"ll\", \"l\", \"l\"]"),
    ("captures", "MatchData", "array of capture groups excluding the whole match", "\"hello\".match(/(l)(l)/).captures   # => [\"l\", \"l\"]"),
    ("to_s", "MatchData", "the whole matched string", "\"hello\".match(/ll/).to_s   # => \"ll\""),
    ("size", "MatchData", "number of groups including group 0 (alias length)", "\"hello\".match(/(l)(l)/).size   # => 3"),
    ("length", "MatchData", "number of groups including group 0 (alias size)", "\"hello\".match(/(l)(l)/).length   # => 3"),
    // ── Array ──
    ("&", "Array", "set intersection vs arg array (deduped)", "[1,2,3] & [2,3,4]   # => [2, 3]"),
    ("intersection", "Array", "set intersection vs arg array (deduped)", "[1,2].intersection([2,3])   # => [2]"),
    ("|", "Array", "set union vs arg array (deduped)", "[1,2] | [2,3]   # => [1, 2, 3]"),
    ("union", "Array", "set union vs arg array (deduped)", "[1,2].union([2,3])   # => [1, 2, 3]"),
    ("-", "Array", "set difference vs arg array", "[1,2,3] - [2]   # => [1, 3]"),
    ("difference", "Array", "set difference vs arg array", "[1,2,3].difference([2])   # => [1, 3]"),
    ("length", "Array", "element count", "[1,2,3].length   # => 3"),
    ("size", "Array", "element count", "[1,2].size   # => 2"),
    ("push", "Array", "append args to end in place, returns self", "[1].push(2,3)   # => [1, 2, 3]"),
    ("append", "Array", "append args to end in place, returns self", "[1].append(2)   # => [1, 2]"),
    ("<<", "Array", "append args to end in place, returns self", "[1] << 2   # => [1, 2]"),
    ("pop", "Array", "remove and return last element, nil if empty, mutates", "[1,2].pop   # => 2"),
    ("shift", "Array", "remove and return first element, nil if empty, mutates", "[1,2].shift   # => 1"),
    ("unshift", "Array", "insert args at front in place, returns self", "[2].unshift(0,1)   # => [0, 1, 2]"),
    ("prepend", "Array", "insert args at front in place, returns self", "[3].prepend(1,2)   # => [1, 2, 3]"),
    ("first", "Array", "first element, or first n elements as array if arg given", "[1,2,3].first   # => 1"),
    ("last", "Array", "last element, or last n elements as array if arg given", "[1,2,3].last   # => 3"),
    ("each_cons", "Array", "yield each n-wide window; no block returns windows array", "[1,2,3].each_cons(2)   # => [[1, 2], [2, 3]]"),
    ("dig", "Array", "recursively index into nested arrays/hashes", "[[1,[2]]].dig(0,1,0)   # => 2"),
    ("empty?", "Array", "true if array has no elements", "[].empty?   # => true"),
    ("reverse", "Array", "new array with elements reversed", "[1,2,3].reverse   # => [3, 2, 1]"),
    ("to_a", "Array", "return a shallow copy of the array", "[1,2].to_a   # => [1, 2]"),
    ("to_ary", "Array", "return a shallow copy of the array", "[1,2].to_ary   # => [1, 2]"),
    ("dup", "Array", "return a shallow copy of the array", "[1,2].dup   # => [1, 2]"),
    ("clone", "Array", "return a shallow copy of the array", "[1,2].clone   # => [1, 2]"),
    ("deconstruct", "Array", "return a shallow copy of the array", "[1,2].deconstruct   # => [1, 2]"),
    ("include?", "Array", "true if any element == arg", "[1,2].include?(2)   # => true"),
    ("index", "Array", "first index where block truthy or element == arg, else nil", "[1,2,3].index(2)   # => 1"),
    ("find_index", "Array", "first index where block truthy or element == arg, else nil", "[1,2,3].find_index(3)   # => 2"),
    ("rindex", "Array", "last index where block truthy or element == arg, else nil", "[1,2,1].rindex(1)   # => 2"),
    ("bsearch", "Array", "binary search sorted array via block; no block returns self", "[1,2,3,4].bsearch { |x| x >= 3 }   # => 3"),
    ("values_at", "Array", "elements at given indices/ranges, nil for out-of-range", "[10,20,30].values_at(0,2)   # => [10, 30]"),
    ("each_index", "Array", "yield each index; no block returns indices array", "[5,6].each_index { |i| }   # => [5, 6]"),
    ("rotate!", "Array", "rotate elements left by n (default 1) in place, returns self", "[1,2,3].rotate!   # => [2, 3, 1]"),
    ("flatten!", "Array", "flatten nested arrays in place; nil if nothing was nested", "[1,[2,[3]]].flatten!   # => [1, 2, 3]"),
    ("compact!", "Array", "remove nil elements in place; nil if none removed", "[1,nil,2].compact!   # => [1, 2]"),
    ("join", "Array", "concatenate elements to string with optional separator", "[1,2,3].join(\"-\")   # => \"1-2-3\""),
    ("sort", "Array", "sort by <=> or block; returns new sorted array", "[3,1,2].sort   # => [1, 2, 3]"),
    ("sort!", "Array", "sort by <=> or block in place, returns self", "[3,1,2].sort!   # => [1, 2, 3]"),
    ("minmax", "Array", "[min, max] pair by <=> or block; [nil, nil] if empty", "[3,1,2].minmax   # => [1, 3]"),
    ("min", "Array", "min by <=> or block, or n smallest sorted if arg given", "[3,1,2].min   # => 1"),
    ("max", "Array", "max by <=> or block, or n largest sorted if arg given", "[3,1,2].max   # => 3"),
    ("sum", "Array", "sum elements (block-mapped) starting from arg init (default 0)", "[1,2,3].sum   # => 6"),
    ("uniq", "Array", "new array with duplicates removed (block form unsupported)", "[1,1,2,2].uniq   # => [1, 2]"),
    ("compact", "Array", "new array with nil elements removed", "[1,nil,2].compact   # => [1, 2]"),
    ("flatten", "Array", "new fully-flattened array, or up to n levels if arg given", "[1,[2,[3]]].flatten   # => [1, 2, 3]"),
    ("concat", "Array", "append elements of arg arrays in place, returns self", "[1].concat([2,3])   # => [1, 2, 3]"),
    ("each", "Array", "yield each element, returns self; no block returns Enumerator", "[1,2].each { |x| }   # => [1, 2]"),
    ("each_with_index", "Array", "yield element and index; no block returns Enumerator of pairs", "[10,20].each_with_index { |x,i| }   # => [10, 20]"),
    ("map", "Array", "map elements via block; no block returns Enumerator", "[1,2,3].map { |x| x*2 }   # => [2, 4, 6]"),
    ("collect", "Array", "map elements via block; no block returns Enumerator", "[1,2].collect { |x| x+1 }   # => [2, 3]"),
    ("flat_map", "Array", "map via block, flattening array results; no block returns Enumerator", "[[1],[2]].flat_map { |x| x }   # => [1, 2]"),
    ("select", "Array", "keep block-truthy elements; no block returns Enumerator", "[1,2,3,4].select { |x| x.even? }   # => [2, 4]"),
    ("filter", "Array", "keep block-truthy elements; no block returns Enumerator", "[1,2,3].filter { |x| x>1 }   # => [2, 3]"),
    ("find_all", "Array", "keep block-truthy elements; no block returns Enumerator", "[1,2,3].find_all { |x| x>1 }   # => [2, 3]"),
    ("reject", "Array", "drop block-truthy elements; no block returns Enumerator", "[1,2,3].reject { |x| x>1 }   # => [1]"),
    ("filter_map", "Array", "map elements via block, keep only truthy results", "[1,2,3].filter_map { |x| x*2 if x.odd? }   # => [2, 6]"),
    ("transpose", "Array", "transpose array of equal-length rows; IndexError if ragged", "[[1,2],[3,4]].transpose   # => [[1, 3], [2, 4]]"),
    ("find", "Array", "first element where block truthy, else nil", "[1,2,3].find { |x| x>1 }   # => 2"),
    ("detect", "Array", "first element where block truthy, else nil", "[1,2,3].detect { |x| x>2 }   # => 3"),
    ("any?", "Array", "block truthy for any; block-less: any element truthy", "[nil,1].any?   # => true"),
    ("all?", "Array", "block truthy for all elements", "[1,2].all? { |x| x>0 }   # => true"),
    ("none?", "Array", "block truthy for no element", "[1,2].none? { |x| x>5 }   # => true"),
    ("count", "Array", "count block-truthy elements, or == arg, else total length", "[1,2,2,3].count(2)   # => 2"),
    ("reduce", "Array", "fold via block or symbol op with optional initial value", "[1,2,3,4].reduce(:+)   # => 10"),
    ("inject", "Array", "fold via block or symbol op with optional initial value", "[1,2,3].inject(10) { |a,x| a+x }   # => 16"),
    ("min_by", "Array", "min element by block key", "[\"aa\",\"b\"].min_by { |s| s.length }   # => \"b\""),
    ("max_by", "Array", "max element by block key", "[\"aa\",\"b\"].max_by { |s| s.length }   # => \"aa\""),
    ("sort_by", "Array", "sort by block key", "[\"bb\",\"a\"].sort_by { |s| s.length }   # => [\"a\", \"bb\"]"),
    ("minmax_by", "Array", "[min, max] by block key", "[\"aa\",\"b\"].minmax_by { |s| s.length }   # => [\"b\", \"aa\"]"),
    ("[]", "Array", "index or range access", "[1,2,3][1]   # => 2"),
    ("fetch", "Array", "element at index, else arg default, else block, else IndexError", "[1,2,3].fetch(1)   # => 2"),
    ("[]=", "Array", "set element at index, padding with nil, returns assigned value", "a=[1,2,3]; a[1]=9; a   # => [1, 9, 3]"),
    ("take", "Array", "first n elements as new array", "[1,2,3,4].take(2)   # => [1, 2]"),
    ("drop", "Array", "all but first n elements as new array", "[1,2,3,4].drop(2)   # => [3, 4]"),
    ("partition", "Array", "[matching, non-matching] arrays split by block", "[1,2,3,4].partition { |x| x.even? }   # => [[2, 4], [1, 3]]"),
    ("group_by", "Array", "hash grouping elements by block return value", "[1,2,3,4].group_by { |x| x%2 }   # => {1 => [1, 3], 0 => [2, 4]}"),
    ("tally", "Array", "hash counting occurrences of each element", "[\"a\",\"b\",\"a\"].tally   # => {\"a\" => 2, \"b\" => 1}"),
    ("each_with_object", "Array", "yield element and memo object, returns the memo", "[1,2,3].each_with_object([]) { |x,a| a << x*2 }   # => [2, 4, 6]"),
    ("zip", "Array", "merge with arg arrays into rows; block yields rows, returns nil", "[1,2].zip([3,4])   # => [[1, 3], [2, 4]]"),
    ("product", "Array", "cartesian product of self with arg arrays", "[1,2].product([3,4])   # => [[1, 3], [1, 4], [2, 3], [2, 4]]"),
    ("combination", "Array", "n-element combinations; block yields each else returns array", "[1,2,3].combination(2)   # => [[1, 2], [1, 3], [2, 3]]"),
    ("permutation", "Array", "n-element permutations (default all); block yields each else array", "[1,2].permutation   # => [[1, 2], [2, 1]]"),
    ("assoc", "Array", "first sub-array whose first element == arg, else nil", "[[1,\"a\"],[2,\"b\"]].assoc(2)   # => [2, \"b\"]"),
    ("rassoc", "Array", "first sub-array whose second element == arg, else nil", "[[1,\"a\"],[2,\"b\"]].rassoc(\"b\")   # => [2, \"b\"]"),
    ("fill", "Array", "fill with value or block result over optional range, mutates", "[1,2,3].fill(0)   # => [0, 0, 0]"),
    ("insert", "Array", "insert args at index (neg inserts after), padding, returns self", "[1,2,3].insert(1,9)   # => [1, 9, 2, 3]"),
    ("delete_at", "Array", "remove and return element at index, nil if out of range", "[1,2,3].delete_at(1)   # => 2"),
    ("delete_if", "Array", "remove block-truthy elements in place, returns self", "[1,2,3,4].delete_if { |x| x.even? }   # => [1, 3]"),
    ("reject!", "Array", "remove block-truthy elements in place, returns self", "[1,2,3,4].reject! { |x| x.even? }   # => [1, 3]"),
    ("take_while", "Array", "leading elements while block truthy", "[1,2,3,1].take_while { |x| x<3 }   # => [1, 2]"),
    ("drop_while", "Array", "elements after the leading block-truthy run", "[1,2,3,1].drop_while { |x| x<3 }   # => [3, 1]"),
    ("rotate", "Array", "new array rotated left by n (default 1)", "[1,2,3].rotate   # => [2, 3, 1]"),
    ("each_slice", "Array", "yield n-length slices; no block returns slices array", "[1,2,3,4,5].each_slice(2)   # => [[1, 2], [3, 4], [5]]"),
    ("chunk_while", "Array", "split into runs; new run when block falsey for adjacent pair", "[1,2,4,5].chunk_while { |a,b| b-a==1 }   # => [[1, 2], [4, 5]]"),
    ("slice_when", "Array", "split into runs; new run when block truthy for adjacent pair", "[1,2,4,5].slice_when { |a,b| b-a>1 }   # => [[1, 2], [4, 5]]"),
    ("to_h", "Array", "build hash from [k,v] pairs, block maps each element first", "[[1,2],[3,4]].to_h   # => {1 => 2, 3 => 4}"),
    ("cycle", "Array", "repeat elements n times or endlessly to block", "[1,2].cycle(2) { |x| }   # => nil"),
    ("chunk", "Array", "consecutive equal-key runs as [key, elems] pairs", "[1,1,2].chunk { |x| x }   # => [[1, [1, 1]], [2, [2]]]"),
    // ── Hash ──
    ("size", "Hash", "number of key-value pairs", "{a: 1, b: 2}.size   # => 2"),
    ("length", "Hash", "number of key-value pairs", "{a: 1}.length   # => 1"),
    ("count", "Hash", "count block-truthy pairs, or total pairs without block", "{a: 1, b: 2}.count   # => 2"),
    ("empty?", "Hash", "true if hash has no pairs", "{}.empty?   # => true"),
    ("deconstruct_keys", "Hash", "return self for pattern matching (keys arg is only a hint)", "{a: 1}.deconstruct_keys(nil)   # => {a: 1}"),
    ("keys", "Hash", "array of keys", "{a: 1, b: 2}.keys   # => [:a, :b]"),
    ("values", "Hash", "array of values", "{a: 1, b: 2}.values   # => [1, 2]"),
    ("key?", "Hash", "true if hash has the given key", "{a: 1}.key?(:a)   # => true"),
    ("has_key?", "Hash", "true if hash has the given key", "{a: 1}.has_key?(:a)   # => true"),
    ("include?", "Hash", "true if hash has the given key", "{a: 1}.include?(:a)   # => true"),
    ("member?", "Hash", "true if hash has the given key", "{a: 1}.member?(:a)   # => true"),
    ("value?", "Hash", "true if any value == arg", "{a: 1}.value?(1)   # => true"),
    ("has_value?", "Hash", "true if any value == arg", "{a: 1}.has_value?(1)   # => true"),
    ("[]", "Hash", "value for key, else default proc call, else default value (nil)", "{a: 1}[:a]   # => 1"),
    ("fetch", "Hash", "value for key, else block, else arg default, else KeyError", "{a: 1}.fetch(:a)   # => 1"),
    ("[]=", "Hash", "set value for key, returns the value", "h={}; h[:a]=1; h   # => {a: 1}"),
    ("store", "Hash", "set value for key, returns the value", "h={}; h.store(:a,1); h   # => {a: 1}"),
    ("delete", "Hash", "remove key and return its value, nil if absent", "{a: 1, b: 2}.delete(:a)   # => 1"),
    ("merge", "Hash", "new hash merging arg hashes; block resolves key collisions", "{a: 1}.merge({b: 2})   # => {a: 1, b: 2}"),
    ("to_a", "Hash", "array of [key, value] pairs", "{a: 1, b: 2}.to_a   # => [[:a, 1], [:b, 2]]"),
    ("each", "Hash", "yield key and value, returns self", "{a: 1}.each { |k,v| }   # => {a: 1}"),
    ("each_pair", "Hash", "yield key and value, returns self", "{a: 1}.each_pair { |k,v| }   # => {a: 1}"),
    ("map", "Hash", "array of block results, block gets each [k, v] pair", "{a: 1, b: 2}.map { |k,v| v*2 }   # => [2, 4]"),
    ("select", "Hash", "new hash keeping block-truthy pairs", "{a: 1, b: 2}.select { |k,v| v>1 }   # => {b: 2}"),
    ("filter", "Hash", "new hash keeping block-truthy pairs", "{a: 1, b: 2}.filter { |k,v| v>1 }   # => {b: 2}"),
    ("reject", "Hash", "new hash dropping block-truthy pairs", "{a: 1, b: 2}.reject { |k,v| v>1 }   # => {a: 1}"),
    ("transform_values", "Hash", "new hash with each value replaced by block result", "{a: 1, b: 2}.transform_values { |v| v*10 }   # => {a: 10, b: 20}"),
    ("transform_keys", "Hash", "new hash with keys remapped via mapping hash then block", "{a: 1}.transform_keys { |k| k.to_s }   # => {\"a\" => 1}"),
    ("filter_map", "Hash", "array of truthy block results over pairs", "{a: 1, b: 2}.filter_map { |k,v| k if v>1 }   # => [:b]"),
    ("each_with_object", "Hash", "yield [k, v] pair and memo, returns the memo", "{a: 1, b: 2}.each_with_object([]) { |(k,v),a| a << k }   # => [:a, :b]"),
    ("sum", "Hash", "sum of block results over pairs from arg init (default 0)", "{a: 1, b: 2}.sum { |k,v| v }   # => 3"),
    ("any?", "Hash", "block truthy for any pair; without block returns false", "{a: 1}.any?   # => false"),
    ("all?", "Hash", "block truthy for all pairs; without block returns true", "{}.all?   # => true"),
    ("none?", "Hash", "block truthy for no pair; without block returns true", "{}.none?   # => true"),
    ("min_by", "Hash", "min [k, v] pair by block key", "{a: 2, b: 1}.min_by { |k,v| v }   # => [:b, 1]"),
    ("max_by", "Hash", "max [k, v] pair by block key", "{a: 2, b: 1}.max_by { |k,v| v }   # => [:a, 2]"),
    ("sort_by", "Hash", "sort [k, v] pairs by block key", "{a: 2, b: 1}.sort_by { |k,v| v }   # => [[:b, 1], [:a, 2]]"),
    ("reduce", "Hash", "fold over [k, v] pairs", "{a: 1, b: 2}.reduce(0) { |s,(k,v)| s+v }   # => 3"),
    ("inject", "Hash", "fold over [k, v] pairs", "{a: 1, b: 2}.inject(0) { |s,(k,v)| s+v }   # => 3"),
    ("group_by", "Hash", "group [k, v] pairs by block return", "{a: 1, b: 2}.group_by { |k,v| v.odd? }   # => {true => [[:a, 1]], false => [[:b, 2]]}"),
    ("partition", "Hash", "partition [k, v] pairs by block", "{a: 1, b: 2}.partition { |k,v| v>1 }   # => [[[:b, 2]], [[:a, 1]]]"),
    ("find_all", "Hash", "keep block-truthy [k, v] pairs", "{a: 1, b: 2}.find_all { |k,v| v>1 }   # => [[:b, 2]]"),
    ("invert", "Hash", "new hash with keys and values swapped", "{a: 1, b: 2}.invert   # => {1 => :a, 2 => :b}"),
    ("default", "Hash", "the hash's default value for missing keys", "Hash.new(0).default   # => 0"),
    ("default=", "Hash", "set the hash's default value, returns it", "h={}; h.default=5; h[:x]   # => 5"),
    ("to_h", "Hash", "return self", "{a: 1}.to_h   # => {a: 1}"),
    ("except", "Hash", "new hash without the given keys, order preserved", "{a: 1, b: 2, c: 3}.except(:b)   # => {a: 1, c: 3}"),
    ("slice", "Hash", "new hash with only the given keys, in argument order", "{a: 1, b: 2, c: 3}.slice(:a,:c)   # => {a: 1, c: 3}"),
    ("compact", "Hash", "new hash without nil-valued pairs", "{a: 1, b: nil}.compact   # => {a: 1}"),
    ("flat_map", "Hash", "map pairs via block, flattening array results", "{a: 1, b: 2}.flat_map { |k,v| [k,v] }   # => [:a, 1, :b, 2]"),
    ("collect_concat", "Hash", "map pairs via block, flattening array results", "{a: 1}.collect_concat { |k,v| [k,v] }   # => [:a, 1]"),
    ("each_with_index", "Hash", "yield [k,v] pair and index; no block returns pairs array", "{a: 1, b: 2}.each_with_index { |pair,i| }   # => {a: 1, b: 2}"),
    ("find", "Hash", "first [k, v] pair where block truthy, else nil", "{a: 1, b: 2}.find { |k,v| v>1 }   # => [:b, 2]"),
    ("detect", "Hash", "first [k, v] pair where block truthy, else nil", "{a: 1, b: 2}.detect { |k,v| v>1 }   # => [:b, 2]"),
    ("dig", "Hash", "recursively index into nested hashes/arrays", "{a: {b: 1}}.dig(:a,:b)   # => 1"),
    // ── Set ──
    ("add", "Set", "add element to set, returns self", "Set[1].add(2)   # => Set[1, 2]"),
    ("<<", "Set", "add element to set, returns self", "Set[1] << 2   # => Set[1, 2]"),
    ("add?", "Set", "add element; self if newly added, nil if already present", "Set[1].add?(1)   # => nil"),
    ("delete", "Set", "remove element from set, returns self", "Set[1,2].delete(1)   # => Set[2]"),
    ("delete?", "Set", "remove element; self if it was present, else nil", "Set[1].delete?(9)   # => nil"),
    ("include?", "Set", "true if set contains arg", "Set[1,2].include?(1)   # => true"),
    ("member?", "Set", "true if set contains arg", "Set[1].member?(1)   # => true"),
    ("===", "Set", "true if set contains arg", "Set[1,2].include?(1)   # => true"),
    ("contain?", "Set", "true if set contains arg", "Set[1].contain?(1)   # => true"),
    ("size", "Set", "number of elements", "Set[1,2,3].size   # => 3"),
    ("length", "Set", "number of elements", "Set[1,2].length   # => 2"),
    ("count", "Set", "number of elements", "Set[1,2].count   # => 2"),
    ("empty?", "Set", "true if set has no elements", "Set[].empty?   # => true"),
    ("clear", "Set", "remove all elements, returns self", "Set[1,2].clear   # => Set[]"),
    ("to_a", "Set", "elements as a new array", "Set[1,2].to_a   # => [1, 2]"),
    ("to_ary", "Set", "elements as a new array", "Set[1,2].to_ary   # => [1, 2]"),
    ("to_set", "Set", "return a copy as a new set", "Set[1].to_set   # => Set[1]"),
    ("dup", "Set", "return a copy as a new set", "Set[1].dup   # => Set[1]"),
    ("clone", "Set", "return a copy as a new set", "Set[1].clone   # => Set[1]"),
    ("merge", "Set", "add all elements of arg set/array in place, returns self", "Set[1].merge([2,3])   # => Set[1, 2, 3]"),
    ("union", "Set", "new set with elements of self and arg", "Set[1].union(Set[2])   # => Set[1, 2]"),
    ("|", "Set", "new set with elements of self and arg", "Set[1] | Set[2]   # => Set[1, 2]"),
    ("+", "Set", "new set with elements of self and arg", "Set[1] + Set[2]   # => Set[1, 2]"),
    ("merge_new", "Set", "new set with elements of self and arg", "Set[1].merge_new(Set[2])   # => Set[1, 2]"),
    ("intersection", "Set", "new set of elements in both self and arg", "Set[1,2].intersection(Set[2,3])   # => Set[2]"),
    ("&", "Set", "new set of elements in both self and arg", "Set[1,2] & Set[2,3]   # => Set[2]"),
    ("difference", "Set", "new set of elements in self but not arg", "Set[1,2].difference(Set[2])   # => Set[1]"),
    ("-", "Set", "new set of elements in self but not arg", "Set[1,2] - Set[2]   # => Set[1]"),
    ("^", "Set", "new set of symmetric difference (elements in exactly one)", "Set[1,2] ^ Set[2,3]   # => Set[1, 3]"),
    ("subset?", "Set", "true if every element of self is in arg", "Set[1].subset?(Set[1,2])   # => true"),
    ("<=", "Set", "true if every element of self is in arg", "Set[1].subset?(Set[1,2])   # => true"),
    ("superset?", "Set", "true if every element of arg is in self", "Set[1,2].superset?(Set[1])   # => true"),
    (">=", "Set", "true if every element of arg is in self", "Set[1,2].superset?(Set[1])   # => true"),
    ("proper_subset?", "Set", "true if self is subset of arg and strictly smaller", "Set[1].proper_subset?(Set[1,2])   # => true"),
    ("<", "Set", "true if self is subset of arg and strictly smaller", "Set[1].proper_subset?(Set[1,2])   # => true"),
    ("proper_superset?", "Set", "true if self is superset of arg and strictly larger", "Set[1,2].proper_superset?(Set[1])   # => true"),
    (">", "Set", "true if self is superset of arg and strictly larger", "Set[1,2].proper_superset?(Set[1])   # => true"),
    ("disjoint?", "Set", "true if self and arg share no elements", "Set[1].disjoint?(Set[2])   # => true"),
    ("intersect?", "Set", "true if self and arg share any element", "Set[1,2].intersect?(Set[2,3])   # => true"),
    ("each", "Set", "yield each element, returns self (block required)", "Set[1,2].each { |x| }   # => Set[1, 2]"),
    // ── Range ──
    ("to_a", "Range", "materialize integer range into Array", "(1..3).to_a   # => [1, 2, 3]"),
    ("to_ary", "Range", "materialize integer range into Array", "(1..3).to_ary   # => [1, 2, 3]"),
    ("entries", "Range", "materialize integer range into Array", "(1..3).entries   # => [1, 2, 3]"),
    ("first", "Range", "lower bound, or first n elements as Array with count arg", "(1..5).first   # => 1"),
    ("last", "Range", "upper bound, or last n elements as Array with count arg", "(1..5).last   # => 5"),
    ("min", "Range", "lower bound lo", "(1..5).min   # => 1"),
    ("begin", "Range", "lower bound lo", "(1..5).begin   # => 1"),
    ("max", "Range", "upper bound (hi-1 if exclusive)", "(1...5).max   # => 4"),
    ("end", "Range", "raw upper bound hi (not exclusive-adjusted)", "(1...5).end   # => 5"),
    ("size", "Range", "element count (end-lo, min 0)", "(1..5).size   # => 5"),
    ("count", "Range", "element count (end-lo, min 0)", "(1..5).count   # => 5"),
    ("length", "Range", "element count (end-lo, min 0)", "(1..5).length   # => 5"),
    ("sum", "Range", "sum of integer elements", "(1..5).sum   # => 15"),
    ("include?", "Range", "true if arg in [lo, end)", "(1..5).include?(3)   # => true"),
    ("cover?", "Range", "true if arg in [lo, end)", "(1..5).cover?(5)   # => true"),
    ("member?", "Range", "true if arg in [lo, end)", "(1..5).member?(3)   # => true"),
    ("===", "Range", "true if arg in [lo, end)", "(1..5) === 5   # => true"),
    ("step", "Range", "step by Int n or Float; yields to block or returns Array/enum", "(1..10).step(3).to_a   # => [1, 4, 7, 10]"),
    ("each", "Range", "yield each integer to block", "(1..3).each { |x| }   # => 1..3"),
    ("exclude_end?", "Range", "true if range excludes its end", "(1.0...2.0).exclude_end?   # => true"),
    ("%", "Range", "alias of step: float step by arg", "((1.0..3.0) % 0.5).to_a   # => [1.0, 1.5, 2.0, 2.5, 3.0]"),
    // ── Struct ──
    ("to_a", "Struct", "array of member values", "S=Struct.new(:a,:b); S.new(1,2).to_a   # => [1, 2]"),
    ("values", "Struct", "array of member values", "S=Struct.new(:a,:b); S.new(1,2).values   # => [1, 2]"),
    ("deconstruct", "Struct", "array of member values (for array pattern matching)", "S=Struct.new(:a,:b); S.new(1,2).deconstruct   # => [1, 2]"),
    ("members", "Struct", "member names as symbols", "S=Struct.new(:a,:b); S.new(1,2).members   # => [:a, :b]"),
    ("size", "Struct", "number of members", "S=Struct.new(:a,:b); S.new(1,2).size   # => 2"),
    ("length", "Struct", "number of members", "S=Struct.new(:a,:b); S.new(1,2).length   # => 2"),
    ("to_h", "Struct", "hash of member => value", "S=Struct.new(:a,:b); S.new(1,2).to_h   # => {a: 1, b: 2}"),
    ("deconstruct_keys", "Struct", "hash of requested (or all) members for pattern matching", "S=Struct.new(:a); S.new(1).deconstruct_keys(nil)   # => {a: 1}"),
    ("[]", "Struct", "index by int position or by symbol/string member name", "S=Struct.new(:a,:b); S.new(1,2)[:b]   # => 2"),
    ("each", "Struct", "yields each member value, returns receiver (block form only)", "S=Struct.new(:a); S.new(1).each { |v| v }   # => #<struct S a=1>"),
    ("each_pair", "Struct", "yields member symbol and value, returns receiver (block form)", "S=Struct.new(:a); S.new(1).each_pair { |m,v| }   # => #<struct S a=1>"),
    ("==", "Struct", "true if same struct class and all members equal", "S=Struct.new(:a); S.new(1) == S.new(1)   # => true"),
    ("eql?", "Struct", "true if same struct class and all members equal (same as ==)", "S=Struct.new(:a); S.new(1).eql?(S.new(1))   # => true"),
    ("to_s", "Struct", "#<struct Name m=v, ...> representation", "S=Struct.new(:a); S.new(1).to_s   # => \"#<struct S a=1>\""),
    ("inspect", "Struct", "#<struct Name m=v, ...> representation", "S=Struct.new(:a); S.new(1).inspect   # => \"#<struct S a=1>\""),
    // ── Exception ──
    ("message", "Exception", "the message ivar, or the class name if unset", "begin; raise \"boom\"; rescue => e; e.message; end   # => \"boom\""),
    ("to_s", "Exception", "the message ivar, or the class name if unset", "begin; raise \"boom\"; rescue => e; e.to_s; end   # => \"boom\""),
    // ── Enumerator ──
    ("next", "Enumerator", "Returns next buffered value; StopIteration at end.", "e=[1,2,3].each; e.next   # => 1"),
    ("peek", "Enumerator", "Returns next buffered value without advancing; StopIteration at end.", "e=[1,2,3].each; e.peek   # => 1"),
    ("rewind", "Enumerator", "Resets the buffer cursor; returns self.", "e=[1,2,3].each; e.next; e.rewind; e.next   # => 1"),
    ("size", "Enumerator", "Returns buffer length as Int (nil for generator source).", "[1,2,3].each.size   # => 3"),
    ("length", "Enumerator", "Returns buffer length as Int.", "[1,2,3].each.length   # => 3"),
    ("with_index", "Enumerator", "Re-runs source method with index; returns elements array.", "[10,20].each.with_index { |x,i| [x,i] }   # => [10, 20]"),
    ("with_object", "Enumerator", "Threads memo through block per element; returns memo.", "[1,2].each.with_object([]) { |x,a| a<<x }   # => [1, 2]"),
    ("each_with_object", "Enumerator", "Threads memo through block per element; returns memo.", "[1,2].each.each_with_object([]) { |x,a| a<<x }   # => [1, 2]"),
    ("first", "Enumerator", "Drives source for first value, or n as array.", "[1,2,3].each.first(2)   # => [1, 2]"),
    ("take", "Enumerator", "Drives source collecting up to n values as array.", "[1,2,3].each.take(2)   # => [1, 2]"),
    ("to_a", "Enumerator", "Drives source to completion into array.", "[1,2,3].each.to_a   # => [1, 2, 3]"),
    ("force", "Enumerator", "Drives source to completion into array.", "[1,2,3].each.force   # => [1, 2, 3]"),
    ("entries", "Enumerator", "Drives source to completion into array.", "[1,2,3].each.entries   # => [1, 2, 3]"),
    ("each", "Enumerator", "Drives source to completion, calls block per value; returns self.", "s=0; [1,2,3].each.each { |x| s+=x }; s   # => 6"),
    ("lazy", "Enumerator", "Wraps the enumerator as a lazy pipeline source.", "[1,2,3].each.lazy.first(2)   # => [1, 2]"),
    // ── Enumerator::Lazy ──
    ("map", "Enumerator::Lazy", "Appends a lazy Map op; returns new lazy enumerator.", "(1..Float::INFINITY).lazy.map { |x| x*2 }.first(3)   # => [2, 4, 6]"),
    ("collect", "Enumerator::Lazy", "Appends a lazy Map op; returns new lazy enumerator.", "(1..Float::INFINITY).lazy.collect { |x| x+1 }.first(2)   # => [2, 3]"),
    ("select", "Enumerator::Lazy", "Appends a lazy Select op; returns new lazy enumerator.", "(1..Float::INFINITY).lazy.select { |x| x.even? }.first(2)   # => [2, 4]"),
    ("filter", "Enumerator::Lazy", "Appends a lazy Select op; returns new lazy enumerator.", "(1..Float::INFINITY).lazy.filter { |x| x.even? }.first(2)   # => [2, 4]"),
    ("reject", "Enumerator::Lazy", "Appends a lazy Reject op; returns new lazy enumerator.", "[1,2,3,4].lazy.reject { |x| x.even? }.to_a   # => [1, 3]"),
    ("filter_map", "Enumerator::Lazy", "Appends a lazy FilterMap op; returns new lazy enumerator.", "[1,2,3].lazy.filter_map { |x| x*2 if x.odd? }.to_a   # => [2, 6]"),
    ("flat_map", "Enumerator::Lazy", "Appends a lazy FlatMap op; returns new lazy enumerator.", "[1,2,3].lazy.flat_map { |x| [x,x] }.first(4)   # => [1, 1, 2, 2]"),
    ("collect_concat", "Enumerator::Lazy", "Appends a lazy FlatMap op; returns new lazy enumerator.", "[1,2].lazy.collect_concat { |x| [x,-x] }.to_a   # => [1, -1, 2, -2]"),
    ("take_while", "Enumerator::Lazy", "Appends a lazy TakeWhile op; returns new lazy enumerator.", "(1..9).lazy.take_while { |x| x < 4 }.to_a   # => [1, 2, 3]"),
    ("drop_while", "Enumerator::Lazy", "Appends a lazy DropWhile op; returns new lazy enumerator.", "(1..5).lazy.drop_while { |x| x < 3 }.to_a   # => [3, 4, 5]"),
    ("take", "Enumerator::Lazy", "Appends a lazy Take(n) op; returns new lazy enumerator.", "(1..5).lazy.take(2).to_a   # => [1, 2]"),
    ("drop", "Enumerator::Lazy", "Appends a lazy Drop(n) op; returns new lazy enumerator.", "(1..5).lazy.drop(2).to_a   # => [3, 4, 5]"),
    ("zip", "Enumerator::Lazy", "Appends a lazy Zip op over array-coerced args.", "[1,2].lazy.zip([3,4]).to_a   # => [[1, 3], [2, 4]]"),
    ("lazy", "Enumerator::Lazy", "Returns self unchanged.", "[1,2,3].lazy.lazy.first(2)   # => [1, 2]"),
    ("first", "Enumerator::Lazy", "Pulls first element, or first n as array if arg given.", "(1..Float::INFINITY).lazy.first(3)   # => [1, 2, 3]"),
    ("force", "Enumerator::Lazy", "Pulls the whole pipeline into an array.", "[1,2,3].lazy.map { |x| x }.force   # => [1, 2, 3]"),
    ("to_a", "Enumerator::Lazy", "Pulls the whole pipeline into an array.", "[1,2,3].lazy.map { |x| x }.to_a   # => [1, 2, 3]"),
    ("entries", "Enumerator::Lazy", "Pulls the whole pipeline into an array.", "[1,2,3].lazy.map { |x| x }.entries   # => [1, 2, 3]"),
    ("each", "Enumerator::Lazy", "With block: pulls all and calls block per element; returns self.", "s=0; [1,2,3].lazy.each { |x| s+=x }; s   # => 6"),
    // ── Enumerator::Yielder ──
    ("<<", "Enumerator::Yielder", "Pushes value into sink; raises break at limit; returns self.", "Enumerator.new { |y| y << 1 }.to_a   # => [1]"),
    ("yield", "Enumerator::Yielder", "Pushes value into sink; raises break at limit; returns self.", "Enumerator.new { |y| y.yield 2 }.to_a   # => [2]"),
    // ── Fiber ──
    ("resume", "Fiber", "Resumes the fiber with args (nil/one/array).", "f=Fiber.new { Fiber.yield 5 }; f.resume   # => 5"),
    ("alive?", "Fiber", "True if the fiber is still alive.", "f=Fiber.new { Fiber.yield 5 }; f.resume; f.alive?   # => true"),
    ("inspect", "Fiber", "Returns \"#<Fiber (created)>\" with \" (dead)\" if not alive.", "Fiber.new {}.inspect   # => \"#<Fiber (created)>\""),
    ("to_s", "Fiber", "Returns \"#<Fiber (created)>\" with \" (dead)\" if not alive.", "Fiber.new {}.to_s   # => \"#<Fiber (created)>\""),
    // ── Proc ──
    ("call", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc.", "d=->(x){ x*2 }; d.call(3)   # => 6"),
    ("[]", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc.", "d=->(x){ x*2 }; d[3]   # => 6"),
    ("yield", "Proc", "Symbol-proc sends the symbol to args[0]; else invokes the proc.", "d=->(x){ x*2 }; d.yield(3)   # => 6"),
    ("to_proc", "Proc", "Returns self unchanged.", "d=->(x){ x }; d.to_proc.call(5)   # => 5"),
    ("arity", "Proc", "Returns the proc's arity as Int (0 if unknown).", "->(a,b){ a+b }.arity   # => 2"),
    ("lambda?", "Proc", "True if the proc is a lambda.", "->(x){ x }.lambda?   # => true"),
    ("curry", "Proc", "Returns a curried version of the proc.", "c=->(a,b){ a+b }; c.curry[1][2]   # => 3"),
    (">>", "Proc", "Composition: (f >> g).call(x) == g.call(f.call(x)).", "(->(x){ x+1 } >> ->(x){ x*2 }).call(3)   # => 8"),
    ("<<", "Proc", "Composition: (f << g).call(x) == f.call(g.call(x)).", "(->(x){ x+1 } << ->(x){ x*2 }).call(3)   # => 7"),
    // ── Method ──
    ("call", "Method", "Routes back through dispatch on the captured receiver.", "\"hi\".method(:upcase).call   # => \"HI\""),
    ("[]", "Method", "Routes back through dispatch on the captured receiver.", "m=\"hi\".method(:upcase); m[]   # => \"HI\""),
    ("yield", "Method", "Routes back through dispatch on the captured receiver.", "m=\"hi\".method(:upcase); m.yield   # => \"HI\""),
    ("===", "Method", "Routes back through dispatch on the captured receiver.", "m=4.method(:even?); m.call   # => true"),
    ("arity", "Method", "Returns the bound method's arity as Int.", "\"hi\".method(:upcase).arity   # => -1"),
    ("name", "Method", "Returns the method name as a Symbol.", "\"hi\".method(:upcase).name   # => :upcase"),
    ("receiver", "Method", "Returns the captured receiver.", "\"hi\".method(:upcase).receiver   # => \"hi\""),
    ("to_proc", "Method", "Returns self unchanged (Method is callable).", "\"hi\".method(:upcase).to_proc.call   # => \"HI\""),
    // ── Time ──
    ("year", "Time", "year field (UTC breakdown of epoch secs)", "Time.at(0).year   # => 1970"),
    ("month", "Time", "month 1-12", "Time.at(90061).month   # => 1"),
    ("mon", "Time", "month 1-12", "Time.at(90061).mon   # => 1"),
    ("day", "Time", "day of month", "Time.at(90061).day   # => 2"),
    ("mday", "Time", "day of month", "Time.at(90061).mday   # => 2"),
    ("hour", "Time", "hour 0-23", "Time.at(90061).hour   # => 1"),
    ("min", "Time", "minute 0-59", "Time.at(90061).min   # => 1"),
    ("sec", "Time", "second 0-59", "Time.at(90061).sec   # => 1"),
    ("wday", "Time", "weekday, 0=Sunday", "Time.at(0).wday   # => 4"),
    ("yday", "Time", "day of year", "Time.at(90061).yday   # => 2"),
    ("to_i", "Time", "epoch seconds floored to Integer", "Time.at(90061).to_i   # => 90061"),
    ("tv_sec", "Time", "epoch seconds floored to Integer", "Time.at(90061).tv_sec   # => 90061"),
    ("to_f", "Time", "epoch seconds as Float", "Time.at(90061).to_f   # => 90061.0"),
    ("sunday?", "Time", "true if wday == 0", "Time.at(0).sunday?   # => false"),
    ("monday?", "Time", "true if wday == 1", "Time.at(0).monday?   # => false"),
    ("tuesday?", "Time", "true if wday == 2", "Time.at(0).tuesday?   # => false"),
    ("wednesday?", "Time", "true if wday == 3", "Time.at(0).wednesday?   # => false"),
    ("thursday?", "Time", "true if wday == 4", "Time.at(0).thursday?   # => true"),
    ("friday?", "Time", "true if wday == 5", "Time.at(90061).friday?   # => true"),
    ("saturday?", "Time", "true if wday == 6", "Time.at(0).saturday?   # => false"),
    ("utc", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)", "Time.at(0).utc.year   # => 1970"),
    ("getutc", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)", "Time.at(0).getutc.year   # => 1970"),
    ("gmtime", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)", "Time.at(0).gmtime.year   # => 1970"),
    ("localtime", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)", "Time.at(0).localtime.year   # => 1970"),
    ("getlocal", "Time", "no-op: returns equivalent Time (UTC-only, no tz offset)", "Time.at(0).getlocal.year   # => 1970"),
    ("utc?", "Time", "always true (UTC-only model)", "Time.at(0).utc?   # => true"),
    ("gmt?", "Time", "always true (UTC-only model)", "Time.at(0).gmt?   # => true"),
    ("to_s", "Time", "formatted time string", "Time.at(0).to_s   # => \"1970-01-01 00:00:00 UTC\""),
    ("inspect", "Time", "formatted time string (inspect form)", "Time.at(0).inspect   # => \"1970-01-01 00:00:00 UTC\""),
    ("strftime", "Time", "format time by strftime pattern arg", "Time.at(0).strftime(\"%Y\")   # => \"1970\""),
    ("<=>", "Time", "compare epoch secs; nil if arg not a Time", "Time.at(0) <=> Time.at(1)   # => -1"),
    ("==", "Time", "true if arg has equal epoch secs", "Time.at(0) == Time.at(0)   # => true"),
    ("+", "Time", "new Time shifted forward by arg seconds", "(Time.at(0) + 86400).day   # => 2"),
    ("-", "Time", "Float secs diff if arg is Time, else Time minus arg secs", "Time.at(10) - Time.at(0)   # => 10.0"),
    ("hash", "Time", "epoch secs bit pattern as Integer", "Time.at(0).hash   # => 0"),
    // ── Date ──
    ("year", "Date", "year field", "Date.new(2024,3,4).year   # => 2024"),
    ("month", "Date", "month 1-12", "Date.new(2024,3,4).month   # => 3"),
    ("mon", "Date", "month 1-12", "Date.new(2024,3,4).mon   # => 3"),
    ("day", "Date", "day of month", "Date.new(2024,3,4).day   # => 4"),
    ("mday", "Date", "day of month", "Date.new(2024,3,4).mday   # => 4"),
    ("wday", "Date", "weekday, 0=Sunday", "Date.new(2024,3,4).wday   # => 1"),
    ("yday", "Date", "day of year", "Date.new(2024,3,4).yday   # => 64"),
    ("cwday", "Date", "ISO weekday, Monday=1..Sunday=7", "Date.new(2024,3,4).cwday   # => 1"),
    ("jd", "Date", "Julian day number", "Date.new(2024,1,15).jd   # => 2460325"),
    ("leap?", "Date", "true if year is a leap year", "Date.new(2024,1,1).leap?   # => true"),
    ("sunday?", "Date", "true if wday == 0", "Date.new(2024,3,4).sunday?   # => false"),
    ("monday?", "Date", "true if wday == 1", "Date.new(2024,3,4).monday?   # => true"),
    ("tuesday?", "Date", "true if wday == 2", "Date.new(2024,3,4).tuesday?   # => false"),
    ("wednesday?", "Date", "true if wday == 3", "Date.new(2024,3,4).wednesday?   # => false"),
    ("thursday?", "Date", "true if wday == 4", "Date.new(2024,3,4).thursday?   # => false"),
    ("friday?", "Date", "true if wday == 5", "Date.new(2024,3,4).friday?   # => false"),
    ("saturday?", "Date", "true if wday == 6", "Date.new(2024,3,4).saturday?   # => false"),
    ("to_s", "Date", "ISO date string YYYY-MM-DD", "Date.new(2024,1,15).to_s   # => \"2024-01-15\""),
    ("iso8601", "Date", "ISO date string YYYY-MM-DD", "Date.new(2024,1,15).iso8601   # => \"2024-01-15\""),
    ("inspect", "Date", "inspect string form", "Date.new(2024,1,15).inspect   # => \"#<Date: 2024-01-15>\""),
    ("strftime", "Date", "format date by strftime pattern arg", "Date.new(2024,1,15).strftime(\"%Y\")   # => \"2024\""),
    ("next_day", "Date", "date plus arg days (default 1)", "Date.new(2024,1,1).next_day.to_s   # => \"2024-01-02\""),
    ("succ", "Date", "date plus arg days (default 1)", "Date.new(2024,1,1).succ.to_s   # => \"2024-01-02\""),
    ("prev_day", "Date", "date minus arg days (default 1)", "Date.new(2024,1,2).prev_day.to_s   # => \"2024-01-01\""),
    ("next_month", "Date", "add arg months, clamping day to month end (default 1)", "Date.new(2024,1,31).next_month.to_s   # => \"2024-02-29\""),
    ("prev_month", "Date", "subtract arg months, clamping day to month end (default 1)", "Date.new(2024,3,31).prev_month.to_s   # => \"2024-02-29\""),
    (">>", "Date", "add arg months, clamping day to month end", "(Date.new(2024,1,31) >> 1).to_s   # => \"2024-02-29\""),
    ("<<", "Date", "subtract arg months, clamping day to month end", "(Date.new(2024,3,31) << 1).to_s   # => \"2024-02-29\""),
    ("next_year", "Date", "add 12*arg months (default 1 year)", "Date.new(2024,1,1).next_year.to_s   # => \"2025-01-01\""),
    ("prev_year", "Date", "subtract 12*arg months (default 1 year)", "Date.new(2024,1,1).prev_year.to_s   # => \"2023-01-01\""),
    ("+", "Date", "date plus arg days", "(Date.new(2024,1,1) + 1).to_s   # => \"2024-01-02\""),
    ("-", "Date", "Rational day diff if arg is Date, else date minus arg days", "Date.new(2024,1,2) - Date.new(2024,1,1)   # => (1/1)"),
    ("<=>", "Date", "compare day counts; nil if arg not a Date", "Date.new(2024,1,1) <=> Date.new(2024,1,2)   # => -1"),
    ("==", "Date", "true if arg is the same date", "Date.new(2024,1,1) == Date.new(2024,1,1)   # => true"),
    ("hash", "Date", "day count as Integer", "Date.new(1970,1,1).hash   # => 0"),
    // ── DateTime ──
    ("year", "DateTime", "year field", "DateTime.new(2024,3,4,9,8,7).year   # => 2024"),
    ("month", "DateTime", "month 1-12", "DateTime.new(2024,3,4,9,8,7).month   # => 3"),
    ("mon", "DateTime", "month 1-12", "DateTime.new(2024,3,4,9,8,7).mon   # => 3"),
    ("day", "DateTime", "day of month", "DateTime.new(2024,3,4,9,8,7).day   # => 4"),
    ("mday", "DateTime", "day of month", "DateTime.new(2024,3,4,9,8,7).mday   # => 4"),
    ("hour", "DateTime", "hour 0-23", "DateTime.new(2024,3,4,9,8,7).hour   # => 9"),
    ("min", "DateTime", "minute 0-59", "DateTime.new(2024,3,4,9,8,7).min   # => 8"),
    ("sec", "DateTime", "second 0-59", "DateTime.new(2024,3,4,9,8,7).sec   # => 7"),
    ("wday", "DateTime", "weekday, 0=Sunday", "DateTime.new(2024,3,4,9,8,7).wday   # => 1"),
    ("yday", "DateTime", "day of year", "DateTime.new(2024,3,4,9,8,7).yday   # => 64"),
    ("cwday", "DateTime", "ISO weekday, Monday=1..Sunday=7", "DateTime.new(2024,3,4,9,8,7).cwday   # => 1"),
    ("jd", "DateTime", "Julian day number of the day part", "DateTime.new(2024,3,4,9,8,7).jd   # => 2460374"),
    ("leap?", "DateTime", "true if year is a leap year", "DateTime.new(2024,1,1,0,0,0).leap?   # => true"),
    ("sunday?", "DateTime", "true if wday == 0", "DateTime.new(2024,3,4,9,8,7).sunday?   # => false"),
    ("monday?", "DateTime", "true if wday == 1", "DateTime.new(2024,3,4,9,8,7).monday?   # => true"),
    ("tuesday?", "DateTime", "true if wday == 2", "DateTime.new(2024,3,4,9,8,7).tuesday?   # => false"),
    ("wednesday?", "DateTime", "true if wday == 3", "DateTime.new(2024,3,4,9,8,7).wednesday?   # => false"),
    ("thursday?", "DateTime", "true if wday == 4", "DateTime.new(2024,3,4,9,8,7).thursday?   # => false"),
    ("friday?", "DateTime", "true if wday == 5", "DateTime.new(2024,3,4,9,8,7).friday?   # => false"),
    ("saturday?", "DateTime", "true if wday == 6", "DateTime.new(2024,3,4,9,8,7).saturday?   # => false"),
    ("to_s", "DateTime", "ISO8601 datetime string", "DateTime.new(2024,1,15,12,30,0).to_s   # => \"2024-01-15T12:30:00+00:00\""),
    ("iso8601", "DateTime", "ISO8601 datetime string", "DateTime.new(2024,1,15,12,30,0).iso8601   # => \"2024-01-15T12:30:00+00:00\""),
    ("inspect", "DateTime", "inspect string form", "DateTime.new(2024,1,15,0,0,0).inspect   # => \"#<DateTime: 2024-01-15T00:00:00+00:00>\""),
    ("strftime", "DateTime", "format datetime by strftime pattern arg", "DateTime.new(2024,3,4,9,8,7).strftime(\"%Y\")   # => \"2024\""),
    ("to_date", "DateTime", "Date of the day part", "DateTime.new(2024,3,4,9,8,7).to_date.to_s   # => \"2024-03-04\""),
    ("to_time", "DateTime", "Time at the same epoch secs", "DateTime.new(2024,3,4,9,8,7).to_time.to_s   # => \"2024-03-04 09:08:07 UTC\""),
    ("next_day", "DateTime", "datetime plus arg days, keeping time (default 1)", "DateTime.new(2024,1,1,5,0,0).next_day.to_s   # => \"2024-01-02T05:00:00+00:00\""),
    ("succ", "DateTime", "datetime plus arg days, keeping time (default 1)", "DateTime.new(2024,1,1,5,0,0).succ.to_s   # => \"2024-01-02T05:00:00+00:00\""),
    ("prev_day", "DateTime", "datetime minus arg days, keeping time (default 1)", "DateTime.new(2024,1,2,5,0,0).prev_day.to_s   # => \"2024-01-01T05:00:00+00:00\""),
    ("next_month", "DateTime", "add arg months, clamping day, keeping time (default 1)", "DateTime.new(2024,1,31,5,0,0).next_month.to_s   # => \"2024-02-29T05:00:00+00:00\""),
    (">>", "DateTime", "add arg months, clamping day, keeping time of day", "(DateTime.new(2024,1,31,5,0,0) >> 1).to_s   # => \"2024-02-29T05:00:00+00:00\""),
    ("prev_month", "DateTime", "subtract arg months, clamping day, keeping time (default 1)", "DateTime.new(2024,3,31,5,0,0).prev_month.to_s   # => \"2024-02-29T05:00:00+00:00\""),
    ("<<", "DateTime", "subtract arg months, clamping day, keeping time of day", "(DateTime.new(2024,3,31,5,0,0) << 1).to_s   # => \"2024-02-29T05:00:00+00:00\""),
    ("next_year", "DateTime", "add 12*arg months, keeping time (default 1 year)", "DateTime.new(2024,1,1,5,0,0).next_year.to_s   # => \"2025-01-01T05:00:00+00:00\""),
    ("prev_year", "DateTime", "subtract 12*arg months, keeping time (default 1 year)", "DateTime.new(2024,1,1,5,0,0).prev_year.to_s   # => \"2023-01-01T05:00:00+00:00\""),
    ("+", "DateTime", "new DateTime plus arg days", "(DateTime.new(2024,1,1,0,0,0) + 1).day   # => 2"),
    ("-", "DateTime", "Rational day diff if arg is DateTime, else minus arg days", "DateTime.new(2024,1,2,0,0,0) - DateTime.new(2024,1,1,0,0,0)   # => (1/1)"),
    ("<=>", "DateTime", "compare epoch secs; nil if arg not a DateTime", "DateTime.new(2024,1,1,0,0,0) <=> DateTime.new(2024,1,2,0,0,0)   # => -1"),
    ("==", "DateTime", "true if arg has equal epoch secs", "DateTime.new(2024,1,1,0,0,0) == DateTime.new(2024,1,1,0,0,0)   # => true"),
    ("hash", "DateTime", "epoch secs bit pattern as Integer", "DateTime.new(1970,1,1,0,0,0).hash   # => 0"),
];

/// The builtin corpus, exposed for the offline docs generator (`gen-docs`).
pub fn corpus() -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
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
        .map(|(name, _chapter, doc, _example)| CompletionItem {
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
