//! End-to-end evaluation tests: parse → compile → run on fusevm, asserting the
//! `inspect` form of the last expression. These exercise the whole pipeline (no
//! mocking), so a regression in the lexer, parser, compiler, host, or fusevm
//! lowering surfaces here.

use rubylang::eval_to_string as ev;

/// Assert that `src` evaluates to `expected` (its inspect form).
fn eq(src: &str, expected: &str) {
    match ev(src) {
        Ok(got) => assert_eq!(got, expected, "for source: {src}"),
        Err(e) => panic!("eval error for `{src}`: {e}"),
    }
}

#[test]
fn arithmetic_and_precedence() {
    eq("1 + 2 * 3", "7");
    eq("(1 + 2) * 3", "9");
    eq("2 ** 10", "1024"); // Integer ** Integer stays Integer, not 1024.0
    eq("10 / 3", "3");
    eq("10.0 / 4", "2.5");
    eq("17 % 5", "2");
    eq("(-7).abs", "7");
    eq("-7 / 2", "-4"); // floored integer division
    eq("-7 % 3", "2"); // modulo takes the divisor's sign
}

#[test]
fn integer_pow_is_integer_but_float_pow_is_float() {
    eq("3 ** 3", "27");
    eq("2 ** 0.5", "1.4142135623730951");
}

#[test]
fn ruby_truthiness_zero_is_true() {
    // Only nil and false are falsy — 0 and "" are truthy (unlike C/shell).
    eq("0 ? :t : :f", ":t");
    eq("\"\" ? :t : :f", ":t");
    eq("nil ? :t : :f", ":f");
    eq("false ? :t : :f", ":f");
}

#[test]
fn string_interpolation_and_methods() {
    eq("\"#{2+3} cats\"", "\"5 cats\"");
    eq("\"Hello\".upcase", "\"HELLO\"");
    eq("\"a,b,c\".split(\",\").length", "3");
    eq("\"racecar\".reverse", "\"racecar\"");
    eq("\"x\" * 3", "\"xxx\"");
}

#[test]
fn arrays_and_blocks() {
    eq("[1,2,3,4].select { |n| n.even? }", "[2, 4]");
    eq("[1,2,3].map { |n| n * n }", "[1, 4, 9]");
    eq("[1,2,3,4,5].reduce(0) { |a,b| a + b }", "15");
    eq("[3,1,2].sort", "[1, 2, 3]");
    eq("[1,2,2,3,3,3].uniq", "[1, 2, 3]");
    eq("[1,[2,[3,4]]].flatten", "[1, 2, 3, 4]");
}

#[test]
fn closures_mutate_enclosing_scope() {
    // Ruby blocks capture and mutate the surrounding locals.
    eq("sum = 0; [1,2,3,4].each { |x| sum += x }; sum", "10");
}

#[test]
fn hashes() {
    eq("h = {a: 1, b: 2}; h[:c] = 3; h.keys.length", "3");
    eq("h = {a: 1, b: 2}; h[:b]", "2");
    eq("{x: 10}.merge({y: 20}).values.sum", "30");
}

#[test]
fn hash_equality_is_order_independent() {
    // Hash#== compares contents, not heap identity or insertion order.
    eq("{a: 1, b: 2} == {a: 1, b: 2}", "true");
    eq("{a: 1, b: 2} == {b: 2, a: 1}", "true");
    eq("{a: 1} == {a: 2}", "false");
    eq("{} == {}", "true");
    eq("{a: [1, 2]} == {a: [1, 2]}", "true");
    eq(
        "[\"a\", \"a\", \"b\"].tally == {\"a\" => 2, \"b\" => 1}",
        "true",
    );
}

#[test]
fn string_scan_block_and_gsub_hash() {
    // scan with a block yields each match and returns the string (self).
    eq(
        "r = []; \"a1b2c3\".scan(/[a-z]\\d/) { |m| r << m }; r",
        "[\"a1\", \"b2\", \"c3\"]",
    );
    eq("\"a1b2\".scan(/[a-z]\\d/) { |m| }", "\"a1b2\"");
    eq(
        "r = []; \"x1y2\".scan(/([a-z])(\\d)/) { |l, d| r << \"#{l}=#{d}\" }; r",
        "[\"x=1\", \"y=2\"]",
    );
    // gsub / sub with a hash maps each match through the hash (missing => empty).
    eq(
        "\"hello\".gsub(/[el]/, \"e\" => \"3\", \"l\" => \"1\")",
        "\"h311o\"",
    );
    eq("\"cat\".sub(/[aeiou]/, \"a\" => \"@\")", "\"c@t\"");
    eq("\"aXbYc\".gsub(/[A-Z]/, \"X\" => \"1\")", "\"a1bc\"");
}

#[test]
fn ranges() {
    eq("(1..5).to_a", "[1, 2, 3, 4, 5]");
    eq("(1...5).to_a", "[1, 2, 3, 4]");
    eq("(1..100).sum", "5050");
    eq("(1..10).select { |n| n % 3 == 0 }", "[3, 6, 9]");
}

#[test]
fn methods_and_recursion() {
    eq(
        "def fib(n); n < 2 ? n : fib(n-1) + fib(n-2); end; (0..10).map { |i| fib(i) }",
        "[0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55]",
    );
}

#[test]
fn early_return_from_block() {
    eq(
        "def first_even(a); a.each { |x| return x if x.even? }; nil; end; first_even([1,3,4,7])",
        "4",
    );
}

#[test]
fn yield_invokes_the_block() {
    eq(
        "def add; yield(2) + yield(3); end; add { |n| n * 10 }",
        "50",
    );
}

#[test]
fn yield_operator_spacing_matches_mri() {
    // `yield` before a tight `+`/`-` is the binary operator, not a yield arg,
    // unless a space precedes and none follows (MRI's `foo +1` command-arg rule).
    eq(r#"def m; "["+yield+"]"; end; m { "X" }"#, r#""[X]""#);
    eq(r#"def m; "[" + yield + "]"; end; m { "X" }"#, r#""[X]""#);
    eq("def m; yield+1; end; m { 5 }", "6");
    eq("def m; yield + 1; end; m { 5 }", "6");
    // Guards — unchanged behavior:
    eq("def m; yield +1; end; m { 5 }", "5"); // space-before/none-after => yield(+1); block ignores arg
    eq("def m; yield 1; end; m { |x| x }", "1");
    eq("def m; yield(1, 2); end; m { |a, b| a + b }", "3");
    eq("def m; yield; end; m { 42 }", "42");
    eq("def m; x = yield; x; end; m { 7 }", "7");
}

#[test]
fn while_with_break() {
    eq("i = 0; while true; i += 1; break if i > 5; end; i", "6");
}

#[test]
fn case_when_with_ranges() {
    eq(
        "n = 7; case n; when 1..5 then :low; when 6..10 then :high; else :other; end",
        ":high",
    );
}

#[test]
fn block_captures_lexical_scope_through_a_method() {
    // The block mutates `total`, which lives in the caller's scope, even though
    // it is invoked (via yield) from inside `repeat` — Ruby captures the frame
    // where the block is written, not where it runs.
    eq(
        "def repeat(n); i = 0; while i < n; yield i; i += 1; end; end; \
         total = 0; repeat(4) { |x| total += x }; total",
        "6",
    );
}

#[test]
fn multi_param_block_destructures_a_single_array() {
    // `|a, b|` over `[a, b]` pairs auto-splats, matching Ruby.
    eq("[[1, 10], [2, 20]].map { |k, v| k + v }", "[11, 22]");
}

#[test]
fn classes_with_initialize_attrs_and_methods() {
    eq(
        "class Point; attr_accessor :x, :y; def initialize(a, b); @x = a; @y = b; end; \
         def to_s; \"(#{@x}, #{@y})\"; end; end; \
         p = Point.new(3, 4); p.x = 10; p.to_s",
        "\"(10, 4)\"",
    );
}

#[test]
fn inheritance_and_implicit_self_dispatch() {
    eq(
        "class Animal; def speak; \"...\"; end; def describe; \"I say #{speak}\"; end; end; \
         class Dog < Animal; def speak; \"woof\"; end; end; Dog.new.describe",
        "\"I say woof\"",
    );
}

#[test]
fn method_chaining_returning_self() {
    eq(
        "class Counter; def initialize; @n = 0; end; def inc; @n += 1; self; end; \
         def value; @n; end; end; Counter.new.inc.inc.inc.value",
        "3",
    );
}

#[test]
fn exceptions_rescue_and_ensure() {
    eq(
        "begin; raise \"boom\"; rescue => e; e.message; end",
        "\"boom\"",
    );
    eq(
        "class MyError < StandardError; end; \
         begin; raise MyError, \"x\"; rescue MyError => e; e.message; end",
        "\"x\"",
    );
    // A rescue only catches matching classes; ensure always runs.
    eq(
        "r = begin; 1 / 0; rescue ZeroDivisionError; :caught; end; r",
        ":caught",
    );
}

#[test]
fn method_body_rescue_without_begin() {
    eq(
        "def safe(a, b); a / b; rescue ZeroDivisionError; -1; end; safe(6, 0)",
        "-1",
    );
}

#[test]
fn default_arguments() {
    eq(
        "def g(name = \"world\"); \"hi #{name}\"; end; g",
        "\"hi world\"",
    );
    eq(
        "def g(name = \"world\"); \"hi #{name}\"; end; g(\"ruby\")",
        "\"hi ruby\"",
    );
}

#[test]
fn parallel_assignment() {
    eq("a, b = 1, 2; a, b = b, a; [a, b]", "[2, 1]");
    eq("x, y, z = [10, 20, 30]; x + y + z", "60");
    eq("a, b = 5; [a, b]", "[5, nil]");
}

#[test]
fn super_forwards_and_extends() {
    eq(
        "class A; def initialize(n); @n = n; end; def val; @n; end; end; \
         class B < A; def initialize(n); super(n); @x = 2; end; def val; super + @x; end; end; \
         B.new(10).val",
        "12",
    );
}

#[test]
fn module_include_mixes_in_methods() {
    eq(
        "module M; def doubled; value * 2; end; end; \
         class C; include M; def initialize(v); @v = v; end; def value; @v; end; end; \
         C.new(21).doubled",
        "42",
    );
}

// Namespaced constants / nested modules. Each expected value was confirmed
// against the reference `ruby` (4.x). See BUGS.md "Namespaces" for the scheme.
#[test]
fn namespaced_constants_and_nested_modules() {
    // A constant defined inside a nested module is reachable by its qualified
    // path; the constant is stored under `A::B::X`.
    eq("module A; module B; X = 1; end; end; A::B::X", "1");
    // Three levels deep.
    eq(
        "module A; module B; module C; X = 9; end; end; end; A::B::C::X",
        "9",
    );
    // The compact form `class A::B::C` registers the class under its qualified
    // name; `Module#name` returns that path.
    eq(
        "module A; module B; end; end; class A::B::C; end; A::B::C.name",
        "\"A::B::C\"",
    );
    // `module A::B` compact-form name is likewise qualified.
    eq("module A; module B; end; end; A::B.name", "\"A::B\"");
    // `include Namespaced::Module` finds the module and mixes its methods in.
    eq(
        "module F; module M; def hi; 1; end; end; end; \
         class C; include F::M; end; C.new.hi",
        "1",
    );
    // A class inheriting a namespaced superclass (`< Foo::Base`).
    eq(
        "module F; class Base; def b; 9; end; end; end; \
         class D < F::Base; end; D.new.b",
        "9",
    );
    // A bare constant read inside a namespace resolves against the lexical
    // nesting (here `M::V`), not just the top level.
    eq("module M; V = 42; def self.read; V; end; end; M.read", "42");
    // A nested class opened inside a class body (`class Outer; class Inner`).
    eq(
        "class Outer; class Inner; def v; 7; end; end; end; Outer::Inner.new.v",
        "7",
    );
    // A namespaced class used as a value / hash key.
    eq(
        "module A; class C; end; end; h = { A::C => \"x\" }; h[A::C]",
        "\"x\"",
    );
    // `const_get` with a qualified string resolves the nested path.
    eq(
        "module A; module B; end; end; Object.const_get(\"A::B\").name",
        "\"A::B\"",
    );
    // `A.const_get(\"B\")` resolves a name relative to the receiver's namespace.
    eq("module A; X = 5; end; A.const_get(\"X\")", "5");
    // A namespaced exception class raised and rescued by its qualified name.
    eq(
        "module E; class Boom < StandardError; end; end; \
         (begin; raise E::Boom, \"x\"; rescue E::Boom => e; e.message; end)",
        "\"x\"",
    );
    // `Class < Module`: a class reference is both a Class and a Module.
    eq("String.is_a?(Module)", "true");
    // `Module.nesting` is best-effort; empty at the top level (matches MRI).
    eq("Module.nesting", "[]");
}

#[test]
fn class_methods_via_def_self() {
    eq(
        "class Factory; def self.make; new; end; def initialize; @ok = true; end; \
         def ok?; @ok; end; end; Factory.make.ok?",
        "true",
    );
}

#[test]
fn splat_parameters() {
    eq("def f(a, *rest); [a, rest]; end; f(1, 2, 3)", "[1, [2, 3]]");
    eq("def f(a, *rest); rest; end; f(1)", "[]");
}

#[test]
fn symbol_to_proc_block_pass() {
    eq("[1, 2, 3].map(&:to_s)", "[\"1\", \"2\", \"3\"]");
    eq("[1, 2, 3, 4].select(&:even?)", "[2, 4]");
}

#[test]
fn sprintf_and_string_percent() {
    eq("format(\"%05d\", 42)", "\"00042\"");
    eq("\"%-6s|\" % \"hi\"", "\"hi    |\"");
    eq("\"%+d\" % 7", "\"+7\"");
    eq("\"%.2f\" % 3.14159", "\"3.14\"");
}

#[test]
fn case_when_class_and_is_a() {
    eq(
        "case 42; when String then :s; when Integer then :i; end",
        ":i",
    );
    eq("5.is_a?(Numeric)", "true");
    eq("\"x\".is_a?(Comparable)", "true");
    eq("class B; end; class S < B; end; S.new.is_a?(B)", "true");
}

#[test]
fn enumerable_breadth() {
    eq("(1..6).partition(&:even?)", "[[2, 4, 6], [1, 3, 5]]");
    eq(
        "[1, 2, 3, 4].group_by { |n| n.even? }",
        "{false => [1, 3], true => [2, 4]}",
    );
    eq("\"aab\".chars.tally", "{\"a\" => 2, \"b\" => 1}");
    eq("[1, 2, 3].zip([4, 5, 6])", "[[1, 4], [2, 5], [3, 6]]");
    eq(
        "[1, 2, 3, 4].each_with_object([]) { |x, m| m << x * x }",
        "[1, 4, 9, 16]",
    );
    eq(
        "{ a: 1, b: 2 }.transform_values { |v| v * 10 }",
        "{a: 10, b: 20}",
    );
}

#[test]
fn shovel_operator_mutates() {
    eq("a = []; a << 1 << 2 << 3; a", "[1, 2, 3]");
    eq("s = \"ab\"; s << \"c\"; s", "\"abc\"");
    eq("1 << 4", "16");
}

#[test]
fn call_site_and_target_splat() {
    eq("def f(a, b, c); a + b + c; end; f(*[1, 2, 3])", "6");
    eq("p = [2, 3]; [1, *p, 4]", "[1, 2, 3, 4]");
    eq(
        "first, *rest = [1, 2, 3, 4]; [first, rest]",
        "[1, [2, 3, 4]]",
    );
    eq(
        "a, *mid, z = [1, 2, 3, 4, 5]; [a, mid, z]",
        "[1, [2, 3, 4], 5]",
    );
}

#[test]
fn keyword_arguments() {
    eq(
        "def g(name:, greeting: \"hi\"); \"#{greeting}, #{name}\"; end; g(name: \"Ann\")",
        "\"hi, Ann\"",
    );
    // Order-independent, with an explicit override.
    eq(
        "def g(name:, greeting: \"hi\"); \"#{greeting}, #{name}\"; end; g(greeting: \"yo\", name: \"Bob\")",
        "\"yo, Bob\"",
    );
    // Mixed positional + keyword.
    eq(
        "def f(a, b, unit: \"px\"); \"#{a + b}#{unit}\"; end; f(1, 2)",
        "\"3px\"",
    );
    eq(
        "def f(a, b, unit: \"px\"); \"#{a + b}#{unit}\"; end; f(3, 4, unit: \"em\")",
        "\"7em\"",
    );
}

#[test]
fn parenless_keyword_args() {
    // Paren-less command call carrying keyword args.
    eq(
        "def g(name:, greeting: \"hi\"); \"#{greeting}, #{name}\"; end; g name: \"Ann\"",
        "\"hi, Ann\"",
    );
    eq(
        "def g(name:, greeting: \"hi\"); \"#{greeting}, #{name}\"; end; g greeting: \"yo\", name: \"Bob\"",
        "\"yo, Bob\"",
    );
    // Mixed positional + keyword, no parens.
    eq("def f(x, k:); \"#{x}-#{k}\"; end; f 10, k: 20", "\"10-20\"");
    // `**hash` splat as a paren-less command arg.
    eq(
        "h = {x: 1, y: 2}; def m(**o); o; end; m **h",
        "{x: 1, y: 2}",
    );
    // Tight `**` is a splat here, but a spaced `**` stays exponentiation.
    eq("x = 2; y = 3; x ** y", "8");
}

#[test]
fn word_and_symbol_arrays() {
    eq(
        "%w[apple banana cherry]",
        "[\"apple\", \"banana\", \"cherry\"]",
    );
    eq("%i[a b c]", "[:a, :b, :c]");
    eq("%w(one two).reverse", "[\"two\", \"one\"]");
    eq("%w[].length", "0");
    eq("%w[a b c].map(&:upcase)", "[\"A\", \"B\", \"C\"]");
}

#[test]
fn operator_methods_and_comparable() {
    // User `+` overloading.
    eq(
        "class V; attr_reader :n; def initialize(n); @n = n; end; def +(o); V.new(@n + o.n); end; end; \
         (V.new(2) + V.new(3)).n",
        "5",
    );
    // Comparable: `<` / `>=` derived from `<=>`.
    eq(
        "class V; include Comparable; attr_reader :n; def initialize(n); @n = n; end; \
         def <=>(o); @n <=> o.n; end; end; [V.new(1) < V.new(2), V.new(3) >= V.new(3)]",
        "[true, true]",
    );
    // Sorting user objects through `<=>`.
    eq(
        "class V; include Comparable; attr_reader :n; def initialize(n); @n = n; end; \
         def <=>(o); @n <=> o.n; end; end; [V.new(3), V.new(1), V.new(2)].sort.map(&:n)",
        "[1, 2, 3]",
    );
}

#[test]
fn sort_min_max_with_block() {
    eq("[3, 1, 2].sort { |a, b| b <=> a }", "[3, 2, 1]");
    eq(
        "[\"bb\", \"a\", \"ccc\"].sort_by(&:length)",
        "[\"a\", \"bb\", \"ccc\"]",
    );
    eq("[5, 3, 8, 1].max { |a, b| a <=> b }", "8");
    eq("(1 <=> 2)", "-1");
    eq("(\"b\" <=> \"a\")", "1");
}

#[test]
fn double_splat_keyword_args() {
    // `**opts` collector.
    eq("def f(**o); o; end; f(a: 1, b: 2)", "{a: 1, b: 2}");
    // `**hash` call-site splat into explicit keyword params.
    eq("def g(a:, b:); a + b; end; h = {a: 4, b: 5}; g(**h)", "9");
    // Explicit keyword param + `**rest` collector.
    eq(
        "def m(a:, **rest); [a, rest]; end; m(a: 1, x: 2, y: 3)",
        "[1, {x: 2, y: 3}]",
    );
    // Positional + `**opts`.
    eq(
        "def d(name, **o); [name, o]; end; d(\"z\", k: 9)",
        "[\"z\", {k: 9}]",
    );
}

#[test]
fn nested_string_interpolation() {
    eq("x = 5; \"outer #{\"inner #{x}\"}\"", "\"outer inner 5\"");
    eq(
        "\"#{[1, 2].map { |n| \"n=#{n}\" }.join(\",\")}\"",
        "\"n=1,n=2\"",
    );
}

#[test]
fn block_param_and_block_given() {
    eq("def f(&b); b.call(20); end; f { |x| x + 1 }", "21");
    eq(
        "def m; block_given? ? yield : :none; end; m { :yes }",
        ":yes",
    );
    eq("def m; block_given? ? yield : :none; end; m", ":none");
    eq("def maybe(&b); b.nil?; end; maybe", "true");
}

#[test]
fn lambdas() {
    eq(
        "sq = ->(x) { x * x }; [sq.call(5), sq.(6), sq[7]]",
        "[25, 36, 49]",
    );
    eq("add = ->(a, b) { a + b }; add.call(3, 4)", "7");
    eq("g = -> { :hi }; g.call", ":hi");
    eq(
        "d = ->(n) { n * 2 }; [1, 2, 3].map { |n| d.call(n) }",
        "[2, 4, 6]",
    );
}

#[test]
fn integer_step() {
    eq(
        "acc = []; 1.step(10, 3) { |n| acc << n }; acc",
        "[1, 4, 7, 10]",
    );
    eq(
        "acc = []; 10.step(1, -3) { |n| acc << n }; acc",
        "[10, 7, 4, 1]",
    );
}

#[test]
fn escaping_closures_keep_their_scope() {
    // A lambda returned from a method keeps the method's locals alive and shared.
    eq(
        "def counter; c = 0; -> { c += 1 }; end; f = counter; f.call; f.call; f.call",
        "3",
    );
    // A lambda capturing an outer lambda's parameter.
    eq(
        "make = ->(n) { ->(x) { x + n } }; make.call(5).call(10)",
        "15",
    );
    // Each block iteration captures its own `n`.
    eq(
        "adders = (1..3).map { |n| ->(x) { x + n } }; adders.map { |f| f.call(10) }",
        "[11, 12, 13]",
    );
    // A block mutates an enclosing local; a block param stays block-local.
    eq("s = 0; [1, 2, 3].each { |x| s += x }; s", "6");
    eq("n = 99; [1, 2, 3].each { |n| n * 2 }; n", "99");
}

#[test]
fn method_breadth_batch() {
    eq("\"ab\".center(6, \"*\")", "\"**ab**\"");
    eq("\"a-b-c\".tr(\"-\", \"_\")", "\"a_b_c\"");
    eq("\"a\\nb\\nc\".lines", "[\"a\\n\", \"b\\n\", \"c\"]");
    eq("{ a: { b: 1 } }.dig(:a, :b)", "1");
    eq("[[1, [2]]].dig(0, 1, 0)", "2");
    eq("[5, 3, 8, 1].min(2)", "[1, 3]");
    eq("[5, 3, 8, 1].max(2)", "[8, 5]");
    eq("[1, 2, 3, 4, 5].first(3)", "[1, 2, 3]");
    eq("[1, 2, 3].sum { |x| x * 2 }", "12");
    eq("255.to_s(16)", "\"ff\"");
    eq("\"1010\".to_i(2)", "10");
    eq("?A", "\"A\"");
}

#[test]
fn regexp_batch() {
    // Literals, matching operators, and String regex methods.
    eq("\"Hello World\".scan(/\\w+/)", "[\"Hello\", \"World\"]");
    eq(
        "\"a1b2c3\".scan(/([a-z])(\\d)/)",
        "[[\"a\", \"1\"], [\"b\", \"2\"], [\"c\", \"3\"]]",
    );
    eq("\"a1b2\".gsub(/\\d/, \"#\")", "\"a#b#\"");
    eq("\"foo123\" =~ /\\d+/", "3");
    eq("\"abc\" =~ /\\d/", "nil");
    eq("\"a,b;c\".split(/[,;]/)", "[\"a\", \"b\", \"c\"]");
    eq("\"hello\".match?(/l+/)", "true");
    eq(
        "\"hello world\".gsub(/o/) { |m| m.upcase }",
        "\"hellO wOrld\"",
    );
    eq(
        "\"cat dog\".scan(/\\w+/).map(&:upcase)",
        "[\"CAT\", \"DOG\"]",
    );
    // Regexp object + case-equality.
    eq("/\\d+/.class", "Regexp");
    eq("/ab/.source", "\"ab\"");
    eq("/AB/i.match?(\"xabx\")", "true");
    eq(
        "case \"word\"; when /\\d/ then 1; when /[a-z]+/ then 2; end",
        "2",
    );
}

#[test]
fn matchdata_batch() {
    eq("\"hello\".match(/l(l)/).class", "MatchData");
    eq("\"hello\".match(/l(l)/)[0]", "\"ll\"");
    eq("\"hello\".match(/l(l)/)[1]", "\"l\"");
    eq("\"hello\".match(/l(l)/).pre_match", "\"he\"");
    eq("\"hello\".match(/l(l)/).post_match", "\"o\"");
    eq("\"hello\".match(/l(l)/).to_a", "[\"ll\", \"l\"]");
    eq("\"hello\".match(/l(l)/).captures", "[\"l\"]");
    eq("\"xyz\".match(/\\d/)", "nil");
    eq("/(\\d+)/.match(\"id 42\")[1]", "\"42\"");
}

#[test]
fn integer_math_batch() {
    eq("12.gcd(8)", "4");
    eq("4.lcm(6)", "12");
    eq("123.digits", "[3, 2, 1]");
    eq("100.digits(16)", "[4, 6]");
    eq("255.bit_length", "8");
    eq("256.bit_length", "9");
    eq("0.bit_length", "0");
    eq("(-1).bit_length", "0");
    eq("17.divmod(5)", "[3, 2]");
    eq("(-17).divmod(5)", "[-4, 3]");
    eq("7.fdiv(2)", "3.5");
    eq("5.pow(3, 7)", "6"); // modular exponentiation
    eq("5.pow(0, 7)", "1");
    eq("(-5).pow(3, 7)", "1");
    eq("5.clamp(1, 3)", "3");
    eq("3.between?(1, 5)", "true");
    eq("5.succ", "6");
    eq("5.pred", "4");
    eq("r = 0; 1.upto(3) { |i| r += i }; r", "6");
    eq("r = 0; 3.downto(1) { |i| r += i }; r", "6");
    eq("r = []; 1.step(10, 3) { |i| r << i }; r", "[1, 4, 7, 10]");
}

#[test]
fn string_case_batch() {
    // Case transforms.
    eq("\"Hello\".swapcase", "\"hELLO\"");
    eq("\"MixEd\".swapcase", "\"mIXeD\"");
    eq("\"hello world\".capitalize", "\"Hello world\"");
    // chomp: no arg removes one trailing separator; arg removes that suffix.
    eq("\"line\\n\".chomp", "\"line\"");
    eq("\"string\\n\\n\".chomp", "\"string\\n\"");
    eq("\"test\\r\\n\".chomp", "\"test\"");
    eq("\"hello.rb\".chomp(\".rb\")", "\"hello\"");
    eq("\"a\\n\\n\\n\".chomp(\"\")", "\"a\"");
    // chop: drop last char, "\r\n" counts as one.
    eq("\"string\".chop", "\"strin\"");
    eq("\"string\\n\".chop", "\"string\"");
    eq("\"x\\r\\n\".chop", "\"x\"");
    eq("\"\".chop", "\"\"");
    // strip family.
    eq("\"  hi  \".strip", "\"hi\"");
    eq("\"  hi  \".lstrip", "\"hi  \"");
    eq("\"  hi  \".rstrip", "\"  hi\"");
    // justify with pad.
    eq("\"5\".rjust(3, \"0\")", "\"005\"");
    eq("\"abc\".ljust(5, \".\")", "\"abc..\"");
    // delete / squeeze / count with selectors (ranges, negation, intersection).
    eq("\"aaabbb\".squeeze", "\"ab\"");
    eq("\"yellow moon\".squeeze", "\"yelow mon\"");
    eq("\"  now   is  the\".squeeze(\" \")", "\" now is the\"");
    eq("\"aaabbbccc\".squeeze(\"a-b\")", "\"abccc\"");
    eq("\"aaabbbccc\".squeeze(\"^a\")", "\"aaabc\"");
    eq("\"hello\".count(\"l\")", "2");
    eq("\"hello world\".count(\"lo\")", "5");
    eq("\"hello world\".count(\"^l\")", "8");
    eq("\"hello world\".count(\"a-y\")", "10");
    eq("\"hello\".count(\"l\", \"lo\")", "2");
    eq("\"hello world\".delete(\"l\", \"lo\")", "\"heo word\"");
    eq("\"hello\".delete(\"a-y\")", "\"\"");
}

#[test]
fn array_transform_batch() {
    // Each expected string is the `.inspect` form, byte-matched against `ruby`.
    eq("[1,[2,[3]]].flatten(1)", "[1, 2, [3]]");
    eq("[1,[2,[3,[4]]]].flatten(2)", "[1, 2, 3, [4]]");
    eq("[1,[2,[3]]].flatten", "[1, 2, 3]");
    eq("[1,nil,2,nil].compact", "[1, 2]");
    eq("[1,2,3,4,5].rotate(2)", "[3, 4, 5, 1, 2]");
    eq("[1,2,3,4].rotate", "[2, 3, 4, 1]");
    eq("[1,2,3,4].each_slice(2).to_a", "[[1, 2], [3, 4]]");
    eq("[1,2,3,4,5,6].partition(&:even?)", "[[2, 4, 6], [1, 3, 5]]");
    eq(
        "[\"a\",\"a\",\"b\",\"c\",\"c\",\"c\"].tally",
        "{\"a\" => 2, \"b\" => 1, \"c\" => 3}",
    );
    eq("[1,2,3,4,5].take(3)", "[1, 2, 3]");
    eq("[1,2,3,4,5].drop(2)", "[3, 4, 5]");
    eq("[1,2,3,4,1].take_while {|x| x < 3}", "[1, 2]");
    eq("[1,2,3,4,1].drop_while {|x| x < 3}", "[3, 4, 1]");
    eq(
        "[1,2,4,9,10,11,12,0].chunk_while {|i,j| i+1==j}.to_a",
        "[[1, 2], [4], [9, 10, 11, 12], [0]]",
    );
    eq("[1,2,3].flat_map {|x| [x,-x]}", "[1, -1, 2, -2, 3, -3]");
}

#[test]
fn array_combine_batch() {
    // zip with multiple others.
    eq("[1, 2].zip([3, 4], [5, 6])", "[[1, 3, 5], [2, 4, 6]]");
    // product: cartesian, first list slowest-varying.
    eq("[1, 2].product([3, 4])", "[[1, 3], [1, 4], [2, 3], [2, 4]]");
    eq(
        "[1, 2].product([3, 4], [5])",
        "[[1, 3, 5], [1, 4, 5], [2, 3, 5], [2, 4, 5]]",
    );
    eq("[1, 2].product", "[[1], [2]]");
    // combination(n).to_a in MRI order.
    eq("[1, 2, 3].combination(2).to_a", "[[1, 2], [1, 3], [2, 3]]");
    eq("[1, 2, 3].combination(0).to_a", "[[]]");
    eq("[1, 2, 3].combination(4).to_a", "[]");
    // permutation(n).to_a in MRI order.
    eq(
        "[1, 2, 3].permutation(2).to_a",
        "[[1, 2], [1, 3], [2, 1], [2, 3], [3, 1], [3, 2]]",
    );
    eq(
        "[1, 2, 3].permutation.to_a",
        "[[1, 2, 3], [1, 3, 2], [2, 1, 3], [2, 3, 1], [3, 1, 2], [3, 2, 1]]",
    );
    // each_with_object.
    eq(
        "[1, 2, 3, 4].each_with_object([]) { |x, a| a << x * 2 }",
        "[2, 4, 6, 8]",
    );
    // find_index value and block forms.
    eq("[10, 20, 30].find_index(20)", "1");
    eq("[10, 20, 30].find_index { |x| x > 15 }", "1");
    // assoc / rassoc.
    eq("[[1, \"a\"], [2, \"b\"]].assoc(2)", "[2, \"b\"]");
    eq("[[1, \"a\"], [2, \"b\"]].assoc(9)", "nil");
    eq("[[1, \"a\"], [2, \"b\"]].rassoc(\"b\")", "[2, \"b\"]");
    // fill: whole, from-start, start+length, block.
    eq("[1, 2, 3].fill(0)", "[0, 0, 0]");
    eq("[1, 2, 3, 4, 5].fill(9, 2)", "[1, 2, 9, 9, 9]");
    eq("[1, 2, 3, 4, 5].fill(9, 1, 2)", "[1, 9, 9, 4, 5]");
    eq("[1, 2, 3].fill { |i| i * i }", "[0, 1, 4]");
    // insert (positive, negative, past-end padding).
    eq("a = [1, 2, 3]; a.insert(1, :x, :y); a", "[1, :x, :y, 2, 3]");
    eq("a = [1, 2, 3]; a.insert(-2, :z); a", "[1, 2, :z, 3]");
    eq(
        "a = [1, 2, 3]; a.insert(5, :x); a",
        "[1, 2, 3, nil, nil, :x]",
    );
    // delete_at returns the removed value.
    eq("a = [1, 2, 3]; a.delete_at(1)", "2");
    eq("a = [1, 2, 3]; a.delete_at(1); a", "[1, 3]");
    eq("a = [1, 2, 3]; a.delete_at(9)", "nil");
    // delete_if mutates and returns self.
    eq("a = [1, 2, 3, 4]; a.delete_if { |x| x.even? }", "[1, 3]");
}

#[test]
fn symbol_methods_batch() {
    eq(":hello.to_s", "\"hello\"");
    eq(":hello.id2name", "\"hello\"");
    eq(":hello.to_sym", ":hello");
    eq(":hello.upcase", ":HELLO");
    eq(":HELLO.downcase", ":hello");
    eq(":hello.capitalize", ":Hello");
    eq(":hello.length", "5");
    eq(":hello.size", "5");
    eq(":hello.empty?", "false");
    eq(":abc[1]", "\"b\"");
    eq(":abc[-1]", "\"c\"");
    eq(":hello[1, 3]", "\"ell\"");
    eq(":hello.succ", ":hellp");
    eq(":az.succ", ":ba");
    eq(":hello.start_with?(\"he\")", "true");
    eq(":hello.start_with?(\"xy\")", "false");
    eq("(:a <=> :b)", "-1");
    eq("(:b <=> :a)", "1");
    eq("(:a <=> :a)", "0");
    eq("(:a <=> 5)", "nil");
    // `Symbol#to_proc` (`&:upcase`) sends the method to its argument.
    eq(":upcase.to_proc.call(\"x\")", "\"X\"");
}

#[test]
fn hash_methods_batch() {
    // Transforms.
    eq(
        "{a: 1, b: 2}.transform_values { |v| v * 10 }",
        "{a: 10, b: 20}",
    );
    eq(
        "{a: 1, b: 2}.transform_keys { |k| k.to_s }",
        "{\"a\" => 1, \"b\" => 2}",
    );
    eq("{a: 1, b: 2}.invert", "{1 => :a, 2 => :b}");
    // Filtering.
    eq("{a: 1, b: 2}.select { |k, v| v > 1 }", "{b: 2}");
    eq("{a: 1, b: 2}.reject { |k, v| v > 1 }", "{a: 1}");
    eq(
        "{a: 1, b: 2, c: 3}.filter_map { |k, v| v * 2 if v > 1 }",
        "[4, 6]",
    );
    // Ordering.
    eq("{a: 1, b: 2}.min_by { |k, v| v }", "[:a, 1]");
    eq("{a: 1, b: 2}.max_by { |k, v| v }", "[:b, 2]");
    eq("{a: 1, b: 2}.sort_by { |k, v| -v }", "[[:b, 2], [:a, 1]]");
    // Aggregation / predicates.
    eq("{a: 1, b: 2}.sum { |k, v| v }", "3");
    eq("{a: 1, b: 2, c: 3}.count { |k, v| v > 1 }", "2");
    eq("{a: 1, b: 2}.any? { |k, v| v > 1 }", "true");
    eq("{a: 1, b: 2}.all? { |k, v| v > 0 }", "true");
    eq("{a: 1, b: 2}.none? { |k, v| v > 5 }", "true");
    // fetch: hit, default, and block-on-miss.
    eq("{a: 1, b: 2}.fetch(:a)", "1");
    eq("{a: 1, b: 2}.fetch(:z, 99)", "99");
    eq("{a: 1, b: 2}.fetch(:z) { |k| \"no #{k}\" }", "\"no z\"");
    // each_with_object yields the [key, value] pair and the memo.
    eq(
        "{a: 1, b: 2}.each_with_object([]) { |kv, acc| acc << kv }",
        "[[:a, 1], [:b, 2]]",
    );
}

#[test]
fn exceptions_batch() {
    // raise Class, msg / rescue Class => e / #message
    eq(
        "begin; raise ArgumentError, \"bad\"; rescue ArgumentError => e; e.message; end",
        "\"bad\"",
    );
    // Kernel Integer() raises ArgumentError on garbage
    eq(
        "begin; Integer(\"x\"); rescue ArgumentError; :caught; end",
        ":caught",
    );
    // Hash#fetch on a missing key raises KeyError (with the Ruby message)
    eq(
        "begin; {}.fetch(:x); rescue KeyError; :nokey; end",
        ":nokey",
    );
    eq(
        "begin; {}.fetch(:x); rescue KeyError => e; e.message; end",
        "\"key not found: :x\"",
    );
    // Hash#fetch default and block forms
    eq("{}.fetch(:x, 99)", "99");
    eq("{}.fetch(:x) { :dflt }", ":dflt");
    // Array#fetch out of bounds raises IndexError; defaults/blocks otherwise
    eq(
        "begin; [1,2].fetch(5); rescue IndexError => e; e.message; end",
        "\"index 5 outside of array bounds: -2...2\"",
    );
    eq("[1,2].fetch(5, :d)", ":d");
    eq("[1,2].fetch(5) { |i| i * 10 }", "50");
    eq("[1,2,3].fetch(-1)", "3");
    // raise "msg" and bare rescue bind to RuntimeError
    eq(
        "begin; raise \"boom\"; rescue => e; e.message; end",
        "\"boom\"",
    );
    // raise Class with no message uses the class name
    eq(
        "begin; raise RuntimeError; rescue RuntimeError => e; e.message; end",
        "\"RuntimeError\"",
    );
    // rescue naming several classes
    eq(
        "begin; raise TypeError, \"t\"; rescue ArgumentError, TypeError => e; e.message; end",
        "\"t\"",
    );
    // StandardError catches every builtin subclass
    eq(
        "begin; raise KeyError, \"kk\"; rescue StandardError => e; e.message; end",
        "\"kk\"",
    );
    // Custom exception subclass carries its message; superclass rescue matches
    eq(
        "class MyErr < StandardError; end; begin; raise MyErr, \"custom\"; rescue MyErr => e; e.message; end",
        "\"custom\"",
    );
    eq(
        "class E1 < StandardError; end; class E2 < E1; end; begin; raise E2, \"deep\"; rescue E1 => e; e.message; end",
        "\"deep\"",
    );
    // Exception.new(msg).message, and raising an instance
    eq("RuntimeError.new(\"hi\").message", "\"hi\"");
    eq("ArgumentError.new(\"z\").message", "\"z\"");
    eq(
        "begin; raise ArgumentError.new(\"na\"); rescue => e; e.message; end",
        "\"na\"",
    );
    // Bounded retry re-runs the begin body
    eq(
        "i = 0; begin; i += 1; raise \"x\" if i < 3; i; rescue; retry if i < 3; end",
        "3",
    );
    eq(
        "n = 0; begin; n += 1; raise \"e\"; rescue; retry if n < 3; n; end",
        "3",
    );
    // ensure runs but a return from the body still wins
    eq("def f; begin; return 1; ensure; nil; end; end; f", "1");
    // first matching rescue clause wins
    eq(
        "begin; raise ArgumentError; rescue TypeError; :t; rescue ArgumentError; :a; end",
        ":a",
    );
}

#[test]
fn enumerable_core_batch() {
    // inject/reduce with an operator symbol.
    eq("[1, 2, 3, 4].inject(:+)", "10");
    eq("[1, 2, 3].reduce(10, :+)", "16");
    eq("[1, 2, 3, 4].inject(:*)", "24");
    eq("[10, 3].inject(:-)", "7");
    eq("[10, 3].inject(:/)", "3");
    eq("[\"a\", \"b\", \"c\"].inject(:+)", "\"abc\"");
    // each_with_index without a block yields `[elem, index]` pairs.
    eq(
        "[1, 2, 3].each_with_index.map { |x, i| x * i }",
        "[0, 2, 6]",
    );
    eq(
        "[1, 2, 3, 4].each_with_index.to_a",
        "[[1, 0], [2, 1], [3, 2], [4, 3]]",
    );
    // minmax, count(&:pred), sum(init).
    eq("[1, 2, 3, 4].minmax", "[1, 4]");
    eq("[5, 3, 8, 1].minmax", "[1, 8]");
    eq("[1, 2, 3, 4].count(&:even?)", "2");
    eq("[1, 2, 3, 4].count(2)", "1");
    eq("[1, 2, 3, 4].sum(100)", "110");
    // find/detect, find_index with a block.
    eq("[1, 2, 3, 4].find { |x| x.even? }", "2");
    eq("[1, 2, 3, 4].detect { |x| x > 10 }", "nil");
    eq("[1, 2, 3, 4].find_index { |x| x > 2 }", "2");
    eq("[10, 20, 30].find_index(20)", "1");
    // cycle(n) { … } runs the block n times and returns nil.
    eq("s = 0; [1, 2, 3].cycle(2) { |x| s += x }; s", "12");
    eq("[1, 2, 3].cycle(2) { |x| }", "nil");
    // chunk groups consecutive runs by the block's value.
    eq(
        "[1, 1, 2, 3, 3].chunk { |x| x }.to_a",
        "[[1, [1, 1]], [2, [2]], [3, [3, 3]]]",
    );
}

#[test]
fn kernel_convert_batch() {
    // Kernel conversion functions: Integer/Float/String/Array with the full
    // Ruby radix-prefix, base-argument, underscore, and sign handling.
    eq("Integer(\"42\")", "42");
    eq("Integer(\"ff\", 16)", "255");
    eq("Integer(\"0xff\", 16)", "255");
    eq("Integer(\"100\", 2)", "4");
    eq("Integer(\"z\", 36)", "35");
    eq("Integer(\"0d99\")", "99");
    eq("Integer(\"-0b101\")", "-5");
    eq("Integer(\"077\")", "63"); // bare leading zero is octal
    eq("Integer(\"077\", 10)", "77"); // explicit base overrides
    eq("Integer(\"1_000\")", "1000");
    eq("Integer(\"  42  \")", "42");
    eq("Integer(3.9)", "3");
    eq("Float(\"3.14\")", "3.14");
    eq("Float(\"1_000.5\")", "1000.5");
    eq("Float(\".5\")", "0.5");
    eq("Float(\"5.\")", "5.0");
    eq("Float(\"0x1.8p3\")", "12.0"); // C99 hex float
    eq("Float(42)", "42.0");
    eq("String(42)", "\"42\"");
    eq("String(nil)", "\"\"");
    eq("Array(nil)", "[]");
    eq("Array([1, 2])", "[1, 2]");
    // Utility methods: tap returns the receiver, then/yield_self return the block.
    eq("5.tap { |x| x }", "5");
    eq("3.then { |x| x * 2 }", "6");
    eq("42.yield_self { |x| x + 1 }", "43");
    eq("x = 0; loop { x += 1; break if x > 3 }; x", "4");
    // Bad input raises the right exception class.
    assert!(ev("Integer(\"3.14\")").is_err());
    assert!(ev("Integer(\"abc\")").is_err());
    assert!(ev("Integer(nil)").is_err());
    assert!(ev("Float(\"inf\")").is_err());
}

#[test]
fn object_model_batch() {
    // instance_variable_get / _set / instance_variables (symbols keep the @ sigil)
    eq(
        "class C; def initialize; @x=5; end; end; C.new.instance_variable_get(:@x)",
        "5",
    );
    eq(
        "class C; def initialize; @x=5; @y=6; end; end; C.new.instance_variables",
        "[:@x, :@y]",
    );
    eq(
        "class C; end; o=C.new; o.instance_variable_set(:@z, 9); o.instance_variable_get(:@z)",
        "9",
    );
    // is_a? / kind_of? / instance_of?
    eq("5.instance_of?(Integer)", "true");
    eq("5.instance_of?(Numeric)", "false");
    eq("5.kind_of?(Numeric)", "true");
    eq("5.is_a?(Integer)", "true");
    // send / public_send
    eq("\"hi\".send(:upcase)", "\"HI\"");
    eq("\"hi\".public_send(:upcase)", "\"HI\"");
    eq("[1,2,3].send(:length)", "3");
    // respond_to?
    eq("\"hi\".respond_to?(:upcase)", "true");
    // dup makes an independent shallow copy (mutating the copy does not leak)
    eq("a=[1,2]; b=a.dup; b<<3; a", "[1, 2]");
    eq("a=[1,2]; b=a.dup; b<<3; b", "[1, 2, 3]");
    // frozen? — immediates and symbols are frozen; mutable containers are not
    eq("5.frozen?", "true");
    eq("nil.frozen?", "true");
    eq(":sym.frozen?", "true");
    eq("\"x\".frozen?", "false");
    eq("[1].frozen?", "false");
    // itself returns the receiver
    eq("[1,2].itself", "[1, 2]");
}

#[test]
fn string_iter_batch() {
    // chars / bytes / each_char
    eq("\"abc\".chars", "[\"a\", \"b\", \"c\"]");
    eq("\"abc\".bytes", "[97, 98, 99]");
    eq(
        "s = \"\"; \"abc\".each_char { |c| s << c.upcase }; s",
        "\"ABC\"",
    );
    // ord / chr
    eq("\"A\".ord", "65");
    eq("\"a\".ord", "97");
    eq("\"hello\".chr", "\"h\"");
    // succ / next
    eq("\"az\".succ", "\"ba\"");
    eq("\"az\".next", "\"ba\"");
    eq("\"Az\".succ", "\"Ba\"");
    eq("\"zz\".succ", "\"aaa\"");
    eq("\"a9\".succ", "\"b0\"");
    eq("\"Zz\".succ", "\"AAa\"");
    eq("\"Az9\".succ", "\"Ba0\"");
    eq("\"\".succ", "\"\"");
    // insert / prepend (mutating)
    eq("\"hello\".insert(2, \"XY\")", "\"heXYllo\"");
    eq("\"hello\".insert(-1, \"!\")", "\"hello!\"");
    eq("\"hello\".insert(-2, \"!\")", "\"hell!o\"");
    eq("\"world\".prepend(\"hello \")", "\"hello world\"");
    // slice (i,len) / slice(range) — same as []
    eq("\"hello\".slice(1, 3)", "\"ell\"");
    eq("\"hello\".slice(1..3)", "\"ell\"");
    eq("\"hello\".slice(1...3)", "\"el\"");
    eq("\"hello\"[1, 3]", "\"ell\"");
    eq("\"hello\"[1..3]", "\"ell\"");
    eq("\"hello\"[1...3]", "\"el\"");
    // []= (int / int,len / range) mutating
    eq("s = \"hello\"; s[1..3] = \"XYZ\"; s", "\"hXYZo\"");
    eq("s = \"hello\"; s[1, 2] = \"Q\"; s", "\"hQlo\"");
    eq("s = \"hello\"; s[0] = \"H\"; s", "\"Hello\"");
    // index / rindex
    eq("\"hello\".index(\"l\")", "2");
    eq("\"hello\".rindex(\"l\")", "3");
    eq("\"hello\".index(\"lo\")", "3");
    eq("\"abcabc\".rindex(\"bc\")", "4");
    eq("\"hello\".index(\"z\")", "nil");
    // start_with? / end_with? multiple args
    eq("\"file.rb\".start_with?(\".\", \"file\")", "true");
    eq("\"file.rb\".end_with?(\".rb\", \".py\")", "true");
    eq("\"file.rb\".end_with?(\".py\", \".c\")", "false");
}

#[test]
fn float_math_batch() {
    // round/ceil/floor/truncate with ndigits keep a Float; without, give an Integer.
    eq("3.14159.round(2)", "3.14");
    eq("3.14159.floor(1)", "3.1");
    eq("3.14159.ceil(2)", "3.15");
    eq("3.99.truncate(1)", "3.9");
    eq("2.5.round", "3");
    eq("(-2.5).round", "-3");
    eq("3.7.to_i", "3");
    eq("3.7.abs", "3.7");
    // nan?/infinite?/finite? classification.
    eq("(1.0/0).infinite?", "1");
    eq("(-1.0/0).infinite?", "-1");
    eq("3.infinite?", "nil");
    eq("(0.0/0).nan?", "true");
    eq("(0.0/0).finite?", "false");
    eq("3.finite?", "true");
    // divmod / modulo / clamp.
    eq("7.5.divmod(2)", "[3, 1.5]");
    eq("5.divmod(3)", "[1, 2]");
    eq("7.5.modulo(2)", "1.5");
    eq("(-2.7).clamp(-1.0, 1.0)", "-1.0");
}

#[test]
fn proc_methods_batch() {
    // call / .() / [] invocation forms.
    eq("add = ->(a, b) { a + b }; add.call(1, 2)", "3");
    eq("sq = ->(x) { x * x }; sq.(3)", "9");
    eq("sq = ->(x) { x * x }; sq[4]", "16");
    // arity: all params are required, so it is the exact count.
    eq("l = ->(a, b) { a }; l.arity", "2");
    eq("g = -> { 42 }; g.arity", "0");
    // lambda?: true for `->`/`lambda`, false for a plain block.
    eq("->(x) { x }.lambda?", "true");
    eq("proc { |a| a }.lambda?", "false");
    eq("lambda { |a| a }.lambda?", "true");
    // curry (best-effort): partial application across successive calls.
    eq("add = ->(a, b) { a + b }; add.curry[1][2]", "3");
    eq("add = ->(a, b) { a + b }; add.curry[1, 2]", "3");
    eq("add = ->(a, b) { a + b }; add.curry.arity", "-1");
    // >> and << composition.
    eq(
        "f = ->(x) { x + 1 }; g = ->(x) { x * 2 }; (f >> g).call(3)",
        "8",
    );
    eq(
        "f = ->(x) { x + 1 }; g = ->(x) { x * 2 }; (f << g).call(3)",
        "7",
    );
    // to_proc on a Proc is the identity (same proc, still callable).
    eq("l = ->(x) { x + 1 }; l.to_proc.call(5)", "6");
}

#[test]
fn sprintf_full_batch() {
    // Base conversions, `#` alternate prefixes, and the `..` two's-complement
    // notation for negative integers.
    eq("format(\"%b\", 10)", "\"1010\"");
    eq("format(\"%x\", 255)", "\"ff\"");
    eq("format(\"%#o\", 8)", "\"010\"");
    eq("format(\"%#b\", 10)", "\"0b1010\"");
    eq("format(\"%#X\", 255)", "\"0XFF\"");
    eq("\"%#x\" % 255", "\"0xff\"");
    eq("format(\"%x\", -255)", "\"..f01\"");
    eq("format(\"%o\", -8)", "\"..70\"");
    eq("format(\"%b\", -1)", "\"..1\"");
    // Float conversions: fixed, scientific (Ruby exponent style), general.
    eq("format(\"%08.3f\", 3.14159)", "\"0003.142\"");
    eq("format(\"%+.2e\", 12345.678)", "\"+1.23e+04\"");
    eq("format(\"%E\", 12345.678)", "\"1.234568E+04\"");
    eq("format(\"%g\", 12345.678)", "\"12345.7\"");
    eq("format(\"%g\", 1000000.0)", "\"1e+06\"");
    eq("format(\"%g\", 0.0001)", "\"0.0001\"");
    // Character conversion (codepoint or first char of a string).
    eq("format(\"%c\", 65)", "\"A\"");
    eq("format(\"%c\", \"hello\")", "\"h\"");
    // Width, precision, flags, and `*` dynamic width/precision.
    eq("format(\"%-8d|\", 42)", "\"42      |\"");
    eq("format(\"% d\", 42)", "\" 42\"");
    eq("format(\"%+d\", 42)", "\"+42\"");
    eq("format(\"%.3d\", 5)", "\"005\"");
    eq("format(\"%*d\", 5, 42)", "\"   42\"");
    eq("format(\"%.*f\", 2, 3.14159)", "\"3.14\"");
    eq("format(\"%#08x\", 255)", "\"0x0000ff\"");
    // Strings, inspect, literal percent, and String#% with an array.
    eq("format(\"%5s\", \"ab\")", "\"   ab\"");
    eq("format(\"%.2s\", \"abcdef\")", "\"ab\"");
    eq("format(\"%p\", \"hi\")", "\"\\\"hi\\\"\"");
    eq("format(\"%d%%\", 50)", "\"50%\"");
    eq("\"%d-%d\" % [1, 2]", "\"1-2\"");
}

#[test]
fn radix_batch() {
    // Integer#to_s(base) and #digits(base) in radix 2..36.
    eq("255.to_s(16)", "\"ff\"");
    eq("255.to_s(2)", "\"11111111\"");
    eq("35.to_s(36)", "\"z\"");
    eq("255.digits(16)", "[15, 15]");
    // String#to_i(base): matching prefixes, underscores, base 0 auto-detect.
    eq("\"ff\".to_i(16)", "255");
    eq("\"-ff\".to_i(16)", "-255");
    eq("\"z\".to_i(36)", "35");
    eq("\"0xff\".to_i(16)", "255");
    eq("\"ff_ff\".to_i(16)", "65535");
    eq("\"0xff\".to_i(0)", "255");
    eq("\"777\".to_i(0)", "777");
    eq("\"1_000\".to_i", "1000");
    // String#hex / #oct.
    eq("\"0xff\".hex", "255");
    eq("\"foo\".hex", "15");
    eq("\"777\".oct", "511");
    eq("\"0b101\".oct", "5");
    eq("\"0o17\".oct", "15");
    eq("\"0x1f\".oct", "31");
    eq("\"9\".oct", "0");
    // Integer#chr and String#ord round-trip.
    eq("65.chr", "\"A\"");
    eq("\"A\".ord", "65");
    // Kernel#Integer(str, base): strict, honours prefixes and signs.
    eq("Integer(\"ff\", 16)", "255");
    eq("Integer(\"0xff\", 16)", "255");
    eq("Integer(\"1010\", 2)", "10");
    eq("Integer(\"777\")", "777");
    eq("Integer(\"010\")", "8");
    eq("Integer(\"-0x10\")", "-16");
    // Integer() is strict: trailing garbage raises.
    assert!(ev("Integer(\"ff\")").is_err());
    assert!(ev("Integer(\"099\")").is_err());
}

#[test]
fn range_methods_batch() {
    // Integer ranges: to_a (inclusive/exclusive), sum, min/max, size.
    eq("(1..10).to_a", "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]");
    eq("(1...10).to_a", "[1, 2, 3, 4, 5, 6, 7, 8, 9]");
    eq("(1..100).sum", "5050");
    eq("(1..10).min", "1");
    eq("(1..10).max", "10");
    eq("(1...10).max", "9");
    eq("(1..10).size", "10");
    eq("(1...10).size", "9");
    // cover? / include? honor exclusivity.
    eq("(1..5).cover?(3)", "true");
    eq("(1..5).cover?(5)", "true");
    eq("(1...5).cover?(5)", "false");
    eq("(1..5).include?(3)", "true");
    eq("(1..5).include?(9)", "false");
    // step: with a block and via .to_a.
    eq("(1..10).step(2).to_a", "[1, 3, 5, 7, 9]");
    eq("(1..10).step(3).to_a", "[1, 4, 7, 10]");
    eq("s = 0; (1..5).step(2) { |x| s += x }; s", "9");
    // first(n) / last(n) return arrays; the no-arg forms a single element.
    eq("(1..10).first(3)", "[1, 2, 3]");
    eq("(1..10).last(2)", "[9, 10]");
    eq("(1..10).first", "1");
    eq("(1..10).last", "10");
    // Character (String) ranges via String#succ succession.
    eq("('a'..'e').to_a", "[\"a\", \"b\", \"c\", \"d\", \"e\"]");
    eq("('a'...'e').to_a", "[\"a\", \"b\", \"c\", \"d\"]");
    eq("('az'..'bb').to_a", "[\"az\", \"ba\", \"bb\"]");
    eq("('a'..'e').include?('c')", "true");
    eq("('a'..'e').include?('z')", "false");
    eq("('a'..'e').cover?('c')", "true");
    eq("('a'..'e').min", "\"a\"");
    eq("('a'..'e').max", "\"e\"");
    eq("('a'..'e').first(2)", "[\"a\", \"b\"]");
    eq("('a'..'e').last(2)", "[\"d\", \"e\"]");
    eq("('a'..'e').count", "5");
    eq(
        "('a'..'e').map { |c| c.upcase }",
        "[\"A\", \"B\", \"C\", \"D\", \"E\"]",
    );
    eq(
        "r = []; ('a'..'c').each { |c| r << c }; r",
        "[\"a\", \"b\", \"c\"]",
    );
}

#[test]
fn comparable_batch() {
    // sort_by / min_by / max_by with a key block.
    eq("[3, 1, 2].sort_by { |x| -x }", "[3, 2, 1]");
    eq("[3, 1, 2].min_by { |x| -x }", "3");
    eq("[3, 1, 2].max_by { |x| -x }", "1");
    eq(r#"["bb", "a", "ccc"].max_by(&:length)"#, r#""ccc""#);
    // minmax / minmax_by return [min, max].
    eq("[5, 3, 8, 1].minmax", "[1, 8]");
    eq("[].minmax", "[nil, nil]");
    eq("[3, 1, 2].minmax_by { |x| -x }", "[3, 1]");
    eq(
        r#"["bb", "a", "ccc"].minmax_by(&:length)"#,
        r#"["a", "ccc"]"#,
    );
    // sort with a two-arg comparator block, and in-place sort!.
    eq("[3, 1, 2].sort { |a, b| b <=> a }", "[3, 2, 1]");
    eq("[3, 1, 2].sort!", "[1, 2, 3]");
    eq("a = [3, 1, 2]; a.sort!; a", "[1, 2, 3]");
    // A user class mixing in Comparable via `<=>` gets the ordering helpers.
    let t = "class T; include Comparable; attr_reader :v; def initialize(v); @v = v; end; def <=>(o); v <=> o.v; end; end; ";
    eq(&format!("{t}T.new(1) < T.new(2)"), "true");
    eq(&format!("{t}T.new(2) > T.new(1)"), "true");
    eq(&format!("{t}T.new(2) >= T.new(2)"), "true");
    eq(&format!("{t}T.new(2) == T.new(2)"), "true");
    eq(&format!("{t}T.new(2).between?(T.new(1), T.new(3))"), "true");
    eq(&format!("{t}T.new(5).clamp(T.new(1), T.new(3)).v"), "3");
    eq(&format!("{t}T.new(0).clamp(T.new(1), T.new(3)).v"), "1");
    eq(
        &format!("{t}[T.new(3), T.new(1), T.new(2)].sort.map(&:v)"),
        "[1, 2, 3]",
    );
    eq(
        &format!("{t}[T.new(3), T.new(1), T.new(2)].minmax.map(&:v)"),
        "[1, 3]",
    );
}

#[test]
fn op_assign_batch() {
    // ||= and &&= (conditional assignment).
    eq("x = nil; x ||= 5; x", "5");
    eq("x = 3; x ||= 5; x", "3");
    eq("y = nil; y &&= 10; y", "nil");
    eq("z = 4; z &&= 10; z", "10");
    // Arithmetic op-assign on a local, chained.
    eq("n = 5; n += 2; n -= 1; n *= 3; n", "18");
    // On an instance variable.
    eq("@c = 0; @c += 5; @c", "5");
    // On array elements, including nested.
    eq("a = [1,2,3]; a[0] += 10; a", "[11, 2, 3]");
    eq("a = [1,2,3]; a[1] *= 2; a", "[1, 4, 3]");
    eq("a = [[1,2],[3,4]]; a[0][1] += 100; a", "[[1, 102], [3, 4]]");
    // On hash elements.
    eq("h = {n: 1}; h[:n] *= 3; h[:n]", "3");
    eq("h = {a: 1, b: 2}; h[:a] += h[:b]; h", "{a: 3, b: 2}");
    // Hash.new(0) default drives the counter idiom.
    eq("h = Hash.new(0); h[:x] += 1; h[:x] += 1; h[:x]", "2");
    eq("h = Hash.new(0); h[\"z\"] == 0", "true");
    eq(
        r#"h = Hash.new(0); "aab".chars.each { |c| h[c] += 1 }; h"#,
        r#"{"a" => 2, "b" => 1}"#,
    );
}

#[test]
fn construction_batch() {
    eq("Array.new(3)", "[nil, nil, nil]");
    eq("Array.new(3, 0)", "[0, 0, 0]");
    eq("Array.new(4) { |i| i * i }", "[0, 1, 4, 9]");
    eq("Array.new(0)", "[]");
    eq("Hash.new(0)[:missing]", "0");
    // Block default: the block runs on each miss and may mutate the hash.
    eq(
        "h = Hash.new { |hh, k| hh[k] = k.to_s }; h[5]; h",
        "{5 => \"5\"}",
    );
    eq(
        "h = Hash.new { |hh, k| hh[k] = [] }; h[:a] << 1; h[:a] << 2; h",
        "{a: [1, 2]}",
    );
    eq("Hash[[[:a, 1], [:b, 2]]]", "{a: 1, b: 2}");
    eq("Hash[:a, 1, :b, 2]", "{a: 1, b: 2}");
    eq("[[:x, 10], [:y, 20]].to_h", "{x: 10, y: 20}");
}

#[test]
fn string_more_batch() {
    eq("\"a-b-c\".partition(\"-\")", "[\"a\", \"-\", \"b-c\"]");
    eq("\"a-b-c\".rpartition(\"-\")", "[\"a-b\", \"-\", \"c\"]");
    eq("\"xyz\".partition(\"-\")", "[\"xyz\", \"\", \"\"]");
    eq("\"Hello\".casecmp(\"hello\")", "0");
    eq("\"aB\".casecmp(\"ac\")", "-1");
    eq("\"Hello\".casecmp?(\"hello\")", "true");
    eq("\"mississippi\".tr_s(\"sp\", \"*\")", "\"mi*i*i*i\"");
    eq(
        "\"a\\nb\\nc\".each_line.to_a",
        "[\"a\\n\", \"b\\n\", \"c\"]",
    );
    eq("\"hi\".center(6, \"*\")", "\"**hi**\"");
}

#[test]
fn regex_match_globals_batch() {
    // `=~` sets `$~`, `$1`..`$9`, `$&`, `` $` ``, `$'`, `$+`.
    eq("\"foo123bar\" =~ /(\\d+)/; $1", "\"123\"");
    eq("\"foo123bar\" =~ /(\\d+)/; $~[0]", "\"123\"");
    eq("\"foo123bar\" =~ /(\\d+)/; $~.pre_match", "\"foo\"");
    eq(
        "\"2024-01-15\" =~ /(\\d+)-(\\d+)-(\\d+)/; [$1, $2, $3]",
        "[\"2024\", \"01\", \"15\"]",
    );
    eq("\"abc\" =~ /b/; $&", "\"b\"");
    eq("\"abc\" =~ /b/; $`", "\"a\"");
    eq("\"abc\" =~ /b/; $'", "\"c\"");
    eq("\"a1b2\" =~ /([a-z])(\\d)/; $+", "\"1\"");
    // A failed match clears the numbered globals to nil.
    eq("\"hello\" =~ /xyz/; $1", "nil");
    // The globals are visible inside a gsub block.
    eq(
        "\"The Quick Brown\".gsub(/(\\w)(\\w+)/) { $1 }",
        "\"T Q B\"",
    );
    eq(
        "\"hello world\".gsub(/(\\w+)/) { $1.capitalize }",
        "\"Hello World\"",
    );
}

#[test]
fn integer_bits_batch() {
    // Integer bit/math extras, byte-matched against ruby 4.0.6.
    eq("2.pow(10, 1000)", "24"); // modular exponentiation
    eq("10.ceildiv(3)", "4");
    eq("(-10).ceildiv(3)", "-3");
    eq("10.ceildiv(-3)", "-3");
    eq("12.ceildiv(4)", "3");
    eq("12.gcdlcm(8)", "[4, 24]");
    eq("0.gcdlcm(5)", "[5, 0]");
    eq("5[0]", "1");
    eq("5[1]", "0");
    eq("5[2]", "1");
    eq("5[100]", "0");
    eq("(-1)[0]", "1");
    eq("(-1)[100]", "1");
    eq("255.bit_length", "8");
}

#[test]
fn string_bytes_batch() {
    eq("\"abc\".bytes", "[97, 98, 99]");
    eq("\"abc\".bytesize", "3");
    eq("\"héllo\".bytesize", "6");
    eq("\"abc\".getbyte(0)", "97");
    eq("\"abc\".getbyte(-1)", "99");
    eq("\"abc\".getbyte(10)", "nil");
    eq("\"\".getbyte(0)", "nil");
    eq("\"abc\".ascii_only?", "true");
    eq("\"héllo\".ascii_only?", "false");
    eq("\"\".ascii_only?", "true");
    eq("\"abc\".valid_encoding?", "true");
    eq("\"abc\".b", "\"abc\"");
    eq("\"abc\".each_byte.to_a", "[97, 98, 99]");
    eq(
        "a = []; \"abc\".each_byte { |b| a << b }; a",
        "[97, 98, 99]",
    );
    eq("\"héllo\".bytes", "[104, 195, 169, 108, 108, 111]");
    eq("\"\".bytes", "[]");
    eq("\"\".bytesize", "0");
    eq("\"abc\".force_encoding(\"UTF-8\")", "\"abc\"");
    eq("\"abc\".encode(\"UTF-8\")", "\"abc\"");
}

#[test]
fn numeric_predicates_batch() {
    eq("0.zero?", "true");
    eq("5.zero?", "false");
    eq("0.0.zero?", "true");
    eq("5.positive?", "true");
    eq("(-3).negative?", "true");
    eq("5.nonzero?", "5");
    eq("0.nonzero?", "nil");
    eq("4.even?", "true");
    eq("5.odd?", "true");
    eq("5.integer?", "true");
    eq("3.14.integer?", "false");
    eq("(-4).abs2", "16");
    eq("3.abs2", "9");
    eq("3.14.abs2", "9.8596");
    eq("(-4).magnitude", "4");
    eq("(-4.0).magnitude", "4.0");
    eq("6.succ", "7");
    eq("6.pred", "5");
}

#[test]
fn times_upto_arrays_batch() {
    // Block-less integer iterators yield an array of their values (approximating
    // an Enumerator whose .to_a/.map work), verified byte-for-byte vs ruby 4.0.6.
    eq("3.times.to_a", "[0, 1, 2]");
    eq("1.upto(4).to_a", "[1, 2, 3, 4]");
    eq("5.downto(2).to_a", "[5, 4, 3, 2]");
    eq("[10,20].each_with_index.to_a", "[[10, 0], [20, 1]]");
    eq("1.step(10,3).to_a", "[1, 4, 7, 10]");
    // Chaining .map onto the block-less enumerator still works.
    eq("3.times.map { |i| i * 2 }", "[0, 2, 4]");
    eq("2.upto(5).map { |i| i }", "[2, 3, 4, 5]");
    // Block forms are unchanged: they run the block and return the receiver.
    eq("3.times { |i| i }", "3");
    eq("1.upto(3) { |i| i }", "1");
    eq("5.downto(3) { |i| i }", "5");
}

#[test]
fn comparable_range_batch() {
    // Integer/Float#clamp(range) alongside the existing clamp(lo, hi).
    eq("5.clamp(1..3)", "3");
    eq("(-2).clamp(0..10)", "0");
    eq("5.5.clamp(1..3)", "3");
    eq("2.5.clamp(1..3)", "2.5");
    eq("5.clamp(1, 3)", "3");
    eq("(-2.7).clamp(-1.0, 1.0)", "-1.0");
    // between? on numbers.
    eq("3.between?(1, 5)", "true");
    eq("7.between?(1, 5)", "false");
    // String#clamp via range and two-arg, plus between?.
    eq("\"m\".clamp(\"a\"..\"z\")", "\"m\"");
    eq("\"A\".clamp(\"a\"..\"z\")", "\"a\"");
    eq("\"a\".clamp(\"m\", \"z\")", "\"m\"");
    eq("\"b\".between?(\"a\", \"c\")", "true");
    eq("\"z\".between?(\"a\", \"c\")", "false");
    // String comparison operators derive from <=>.
    eq("\"a\" < \"b\"", "true");
    eq("\"abc\" <=> \"abd\"", "-1");
    // minmax on arrays.
    eq("[\"b\", \"a\", \"c\"].minmax", "[\"a\", \"c\"]");
    // Exclusive ranges are rejected by clamp.
    eq(
        "begin; 5.clamp(1...3); rescue => e; e.message; end",
        "\"cannot clamp with an exclusive range\"",
    );
}

#[test]
fn catch_throw_batch() {
    // `throw(tag, val)` unwinds to the matching `catch(tag)`, which returns `val`.
    eq(
        "catch(:done) { 10.times { |i| throw(:done, i) if i == 3 }; :never }",
        "3",
    );
    // A bare `throw tag` carries nil.
    eq("catch(:x) { throw :x }", "nil");
    // The block value is returned when no throw fires.
    eq("catch(:z) { 42 }", "42");
    // A throw unwinds past an inner catch to the one whose tag it names.
    eq("catch(:a) { catch(:b) { throw :a, 1 } }", "1");
    // A throw caught by the inner catch lets the outer catch body continue.
    eq(
        "catch(:a) { catch(:b) { throw :b, 7 }; :a_body }",
        ":a_body",
    );
    // `catch { |tag| … }` yields a fresh unique tag to the block.
    eq("catch { |t| throw t, 99 }", "99");
    // Throw unwinds through a method boundary to reach the catch.
    eq(
        "def go(t); throw t, :from_method; end; catch(:m) { go(:m) }",
        ":from_method",
    );
    // A throw from inside `each` returns its value from the enclosing catch.
    eq(
        "catch(:f) { [1, 2, 3, 4].each { |x| throw(:f, x * 10) if x == 2 }; :nope }",
        "20",
    );
}

#[test]
fn hash_more_batch() {
    // except / slice preserve MRI ordering semantics.
    eq("{a: 1, b: 2, c: 3}.except(:b)", "{a: 1, c: 3}");
    eq("{a: 1, b: 2, c: 3}.slice(:a, :c)", "{a: 1, c: 3}");
    eq("{a: 1, b: 2, c: 3}.slice(:c, :a)", "{c: 3, a: 1}");
    eq("{a: 1, b: 2}.slice(:a, :z)", "{a: 1}");
    eq("{a: 1, b: 2}.except(:z)", "{a: 1, b: 2}");
    // compact drops nil values.
    eq("{a: 1, b: nil, c: 3}.compact", "{a: 1, c: 3}");
    // min_by / max_by / sum with a block.
    eq("{a: 1, b: 2}.min_by { |k, v| v }.inspect", "\"[:a, 1]\"");
    eq("{a: 1, b: 2}.max_by { |k, v| v }.inspect", "\"[:b, 2]\"");
    eq("{a: 1, b: 2}.sum { |k, v| v }", "3");
    // flat_map flattens one level; scalar results are collected.
    eq("{a: 1, b: 2}.flat_map { |k, v| [k, v] }", "[:a, 1, :b, 2]");
    eq("{a: 1, b: 2}.flat_map { |k, v| v }", "[1, 2]");
    // each_with_index without a block yields [[k, v], index] pairs.
    eq(
        "{a: 1, b: 2}.each_with_index.to_a",
        "[[[:a, 1], 0], [[:b, 2], 1]]",
    );
    // find / detect return the matching [key, value] pair, else nil.
    eq("{a: 1, b: 2, c: 3}.find { |k, v| v == 2 }", "[:b, 2]");
    eq("{a: 1, b: 2, c: 3}.detect { |k, v| v == 2 }", "[:b, 2]");
    eq("{a: 1}.find { |k, v| v == 9 }", "nil");
    // tally-like counts via each_with_object.
    eq(
        "%w[a b a c b a].each_with_object(Hash.new(0)) { |w, h| h[w] += 1 }",
        "{\"a\" => 3, \"b\" => 2, \"c\" => 1}",
    );
}

#[test]
fn object_dup_freeze_batch() {
    // dup makes a fresh shallow copy; mutating the copy never leaks back.
    eq("[1, 2, 3].dup.push(4)", "[1, 2, 3, 4]");
    eq("a = [1, 2]; b = a.dup; b << 3; a", "[1, 2]");
    eq("h = {1 => 2}; g = h.dup; g[3] = 4; h", "{1 => 2}");
    eq("s = \"x\"; t = s.dup; t << \"y\"; s", "\"x\"");
    // freeze records the frozen flag; frozen? reports it.
    eq("\"x\".frozen?", "false");
    eq("\"a\".freeze.frozen?", "true");
    eq("a = [1]; a.freeze; a.frozen?", "true");
    // immediates and symbols are always frozen.
    eq("5.frozen?", "true");
    eq(":sym.frozen?", "true");
    eq("nil.frozen?", "true");
    eq("({}).frozen?", "false");
    // clone preserves frozen state; dup never does.
    eq("\"abc\".clone.frozen?", "false");
    eq("a = [1].freeze; b = a.clone; b.frozen?", "true");
    eq("a = [1].freeze; a.dup.frozen?", "false");
    // tap yields self and returns self; itself returns self.
    eq("5.tap { |x| x }.itself", "5");
    eq("5.itself", "5");
    // then / yield_self pass self to the block and return its result.
    eq("5.then { |x| x + 1 }", "6");
    eq("5.yield_self { |x| x * 2 }", "10");
    // equal? is object identity; instance_of? tests the exact class.
    eq("5.equal?(5)", "true");
    eq("5.equal?(6)", "false");
    eq("\"a\".equal?(\"a\")", "false");
    eq("a = [1]; a.equal?(a)", "true");
    eq("[1].instance_of?(Array)", "true");
}

#[test]
fn kernel_more_batch() {
    // `pp` behaves like `p`: prints inspect and returns its argument(s).
    eq("pp(1, 2, 3)", "[1, 2, 3]");
    eq("pp([1, 2, 3])", "[1, 2, 3]");
    // Kernel conversion functions and their edge cases.
    eq("Array({a: 1})", "[[:a, 1]]"); // hash → array of [k, v] pairs
    eq("Array({a: 1, b: 2})", "[[:a, 1], [:b, 2]]");
    eq("Array(nil)", "[]");
    eq("Array(\"x\")", "[\"x\"]");
    eq("Array([1, 2])", "[1, 2]");
    eq("String(:sym)", "\"sym\"");
    eq("String(nil)", "\"\"");
    eq("Integer(\"0b101\", 2)", "5"); // radix prefix honoured under explicit base
    eq("Integer(\"101\", 2)", "5");
    eq("Integer(3.9)", "3");
    eq("Float(\"1.5e3\")", "1500.0");
    // `format` is an alias of `sprintf`.
    eq("format(\"%05.2f\", 3.14159)", "\"03.14\"");
    // `__method__` names the enclosing def.
    eq("def foo; __method__; end; foo", ":foo");
    // `$,` (output field separator) parses, stores, and reads back.
    eq("$, = \"-\"; $,", "\"-\"");
    eq("$\\ = \"!\"; $\\", "\"!\"");
}

#[test]
fn string_justify_batch() {
    // Named references in `String#%` and `Kernel#format`.
    eq(
        "\"%<name>s is %<age>d\" % {name: \"Al\", age: 3}",
        "\"Al is 3\"",
    );
    eq("\"%{greet} world\" % {greet: \"hi\"}", "\"hi world\"");
    eq("\"%{a}%{b}\" % {a: 1, b: 2}", "\"12\"");
    eq("\"%<n>05.2f\" % {n: 3.14159}", "\"03.14\"");
    eq("\"%-10<w>s|\" % {w: \"hi\"}", "\"hi        |\"");
    eq("format(\"%<name>s=%<n>d\", name: \"x\", n: 42)", "\"x=42\"");
    eq("sprintf(\"%{a}-%{b}\", a: \"p\", b: \"q\")", "\"p-q\"");
    // prepend(*strs) and concat(*strs) mutate and return the receiver.
    eq("\"b\".prepend(\"a\")", "\"ab\"");
    eq("\"a\".concat(\"b\", \"c\")", "\"abc\"");
    // `*=` desugars to String#* (repeat).
    eq("s = \"ab\"; s *= 3; s", "\"ababab\"");
}

#[test]
fn string_tr_ranges_batch() {
    // tr/tr_s/delete/count/squeeze expand `a-z` ranges and `^` negation.
    eq(r#""hello".tr("a-y", "*")"#, r#""*****""#);
    eq(r#""hello".tr("a-c", "x")"#, r#""hello""#);
    eq(r#""abc".tr("a-c", "x-z")"#, r#""xyz""#);
    eq(r#""hello".tr("^aeiou", "*")"#, r#""*e**o""#);
    eq(r#""hello".tr("el", "")"#, r#""ho""#);
    eq(r#""hello world".tr("a-z", "A-Z")"#, r#""HELLO WORLD""#);
    eq(r#""hello".delete("a-m")"#, r#""o""#);
    eq(r#""hello".delete("a-y")"#, r#""""#);
    eq(r#""hello".delete("^aeiou")"#, r#""eo""#);
    eq(r#""hello world".count("a-z")"#, "10");
    eq(r#""hello".count("a-z")"#, "5");
    eq(r#""hello".count("^l")"#, "3");
    eq(r#""aaabbbccc".squeeze("a-b")"#, r#""abccc""#);
    eq(r#""aabbcc".tr_s("a-c", "x")"#, r#""x""#);
    eq(r#""hello".tr_s("l", "r")"#, r#""hero""#);
}

#[test]
fn float_format_batch() {
    eq("3.14159.ceil(2)", "3.15");
    eq("3.14159.ceil(3)", "3.142");
    eq("3.14159.truncate(1)", "3.1");
    eq("3.14159.floor(2)", "3.14");
    eq("(-3.7).floor", "-4");
    eq("(-3.14159).ceil(2)", "-3.14");
    eq("2.675.round(2)", "2.68");
    eq("3.7.to_i", "3");
    eq("3.7.to_int", "3");
    eq("2.5.coerce(3)", "[3.0, 2.5]");
    eq("2.5.coerce(3).inspect", "\"[3.0, 2.5]\"");
    eq("3.coerce(2)", "[2, 3]");
    eq("3.coerce(2.5)", "[2.5, 3.0]");
    eq("10.0 % 3", "1.0");
    eq("100.0 % 7", "2.0");
    eq("2.0 ** 3", "8.0");
    eq("7.5.divmod(2)", "[3, 1.5]");
    eq("r=[];1.0.step(2.0,0.5){|x|r<<x};r", "[1.0, 1.5, 2.0]");
    eq("r=[];1.0.step(2.0,0.3){|x|r<<x};r", "[1.0, 1.3, 1.6, 1.9]");
    eq("r=[];2.0.step(1.0,-0.5){|x|r<<x};r", "[2.0, 1.5, 1.0]");
    eq("r=[];1.step(5,2){|x|r<<x};r", "[1, 3, 5]");
}

#[test]
fn begin_while_batch() {
    // `begin … end while/until cond` is a post-test loop: the body runs at
    // least once, then the condition is checked.
    eq("i=0; begin; i+=1; end while i<3; i", "3");
    eq("n=0; begin; n+=1; end until n>=5; n", "5");
    eq(
        "r=[]; i=0; begin; r<<i; i+=1; end while i<3; r",
        "[0, 1, 2]",
    );
    // The body runs once even when the condition is immediately false/true.
    eq("x=10; begin; x+=1; end while false; x", "11");
    eq("y=10; begin; y+=1; end until true; y", "11");
    // `next` jumps to the condition check; `break` exits the loop.
    eq(
        "s=0;i=0; begin; i+=1; next if i==2; s+=i; end while i<4; s",
        "8",
    );
    eq("c=0; begin; c+=1; break if c==3; end while true; c", "3");
}

#[test]
fn array_zip_flat_batch() {
    eq("[1,2,3].each_cons(2).to_a", "[[1, 2], [2, 3]]");
    eq("[1,2,4,5].slice_when{|a,b| b-a>1}.to_a", "[[1, 2], [4, 5]]");
    eq("[1,2].product([3,4],[5]).length", "4");
    eq("[[1,2],[3,4]].flat_map{|x| x}", "[1, 2, 3, 4]");
    eq(
        "r=[]; [1,2,3].zip([4,5,6]){|x| r<<x}; r",
        "[[1, 4], [2, 5], [3, 6]]",
    );
    eq("[1,2,3].zip([4,5,6]){|x| x}", "nil");
    eq("[1,2,3].each_slice(2).to_a", "[[1, 2], [3]]");
}

#[test]
fn array_search_batch() {
    // bsearch: find-minimum (block returns bool) and find-any (block returns Integer)
    eq("[1,2,3,4,5].bsearch { |x| x >= 3 }", "3");
    eq("[1,2,3,4,5].bsearch { |x| x > 10 }", "nil");
    eq("[1,4,4,4,5,7,10,12].bsearch { |x| 4 <=> x }", "4");
    // values_at with ints, negatives, and ranges
    eq("[10,20,30].values_at(0,2)", "[10, 30]");
    eq("[1,2,3,4,5].values_at(1,3,4)", "[2, 4, 5]");
    eq("[1,2,3,4,5].values_at(-1,-2)", "[5, 4]");
    eq("[1,2,3].values_at(1..2)", "[2, 3]");
    eq("[].values_at(0,1)", "[nil, nil]");
    // each_index returns the receiver with a block, indices without
    eq("[10,20,30].each_index { |i| }", "[10, 20, 30]");
    eq("[10,20,30].each_index.to_a", "[0, 1, 2]");
    // deep dig
    eq("[[1,[2]]].dig(0,1,0)", "2");
    // rotate! mutates in place and returns self
    eq("[1,2,3].rotate!(1)", "[2, 3, 1]");
    eq("a = [1,2,3]; a.rotate!(-1); a", "[3, 1, 2]");
    // sum with an initial value
    eq("[1,2,3,4].sum(100)", "110");
    // index / rindex with a block
    eq("[1,2,3].index { |x| x > 1 }", "1");
    eq("[1,2,3,2,1].rindex(2)", "3");
    eq("[\"a\",\"bb\",\"ccc\"].rindex { |s| s.length < 3 }", "1");
    // flatten! / compact! return self on change, nil when nothing changed
    eq("[1,[2,[3]]].flatten!", "[1, 2, 3]");
    eq("[1,2,3].flatten!", "nil");
    eq("[1,nil,2,nil].compact!", "[1, 2]");
    eq("[1,2,3].compact!", "nil");
}

#[test]
fn enumerable_module_batch() {
    // A user class that `include Enumerable` and defines `each` derives the whole
    // Enumerable surface from that one method (mirrors real Ruby).
    const L: &str = "class L; include Enumerable; def initialize(*a); @a=a; end; \
                     def each; @a.each { |x| yield x }; end; end\n";
    let with_l = |expr: &str, expected: &str| eq(&format!("{L}{expr}"), expected);

    with_l("L.new(3, 1, 2).sort", "[1, 2, 3]");
    with_l("L.new(1, 2, 3).map { |x| x * 2 }", "[2, 4, 6]");
    with_l("L.new(1, 2, 3).select(&:odd?)", "[1, 3]");
    with_l("L.new(1, 2, 3).reduce(:+)", "6");
    with_l("L.new(1, 2, 3).reject(&:odd?)", "[2]");
    with_l("L.new(1, 2, 3).to_a", "[1, 2, 3]");
    with_l("L.new(1, 2, 3).find { |x| x > 1 }", "2");
    with_l("L.new(1, 2, 3).count", "3");
    with_l("L.new(3, 1, 2).min", "1");
    with_l("L.new(3, 1, 2).max", "3");
    with_l("L.new(1, 2, 3).include?(2)", "true");
    with_l("L.new(1, 2, 3).include?(9)", "false");
    with_l("L.new(1, 2, 3).first", "1");
    with_l("L.new(1, 2, 3).first(2)", "[1, 2]");
    // reduce with an initial value and a block; sum; sort_by / min_by key blocks.
    with_l("L.new(1, 2, 3).reduce(10) { |s, x| s + x }", "16");
    with_l("L.new(1, 2, 3).sum", "6");
    with_l("L.new(1, 2, 3).min_by { |x| -x }", "3");
    with_l("L.new(1, 2, 3).sort_by { |x| -x }", "[3, 2, 1]");
    with_l("L.new(1, 2, 3).partition(&:odd?)", "[[1, 3], [2]]");
    with_l("L.new(1, 2, 3, 3).tally", "{1 => 1, 2 => 1, 3 => 2}");
    with_l("L.new(1, 2, 3).any? { |x| x > 2 }", "true");
    with_l("L.new(1, 2, 3).all? { |x| x > 0 }", "true");
    with_l(
        "L.new(1, 2, 3).each_with_index.to_a",
        "[[1, 0], [2, 1], [3, 2]]",
    );
}

#[test]
fn symbol_more_batch() {
    // `swapcase` returns a Symbol, inverting case per-character.
    eq(":hello.swapcase", ":HELLO");
    eq(":HELLO.swapcase", ":hello");
    eq(":HeLLo.swapcase", ":hEllO");
    // `end_with?` mirrors `start_with?`.
    eq(":hello.end_with?(\"lo\")", "true");
    eq(":hello.end_with?(\"xy\")", "false");
    // `match?` tests the name against a Regexp without touching `$~`.
    eq(":abc.match?(/b/)", "true");
    eq(":abc.match?(/z/)", "false");
    // Range/index and succ on the name.
    eq(":hello[1..3]", "\"ell\"");
    eq(":abc.succ", ":abd");
    // `Symbol#to_proc` forwards multiple args: arg[0] is the receiver, the rest
    // become method arguments.
    eq(":concat.to_proc.call(\"a\", \"b\")", "\"ab\"");
    eq(":end_with?.to_proc.call(\"hello\", \"lo\")", "true");
}

#[test]
fn method_objects_batch() {
    // `obj.method(:name)` captures a bound, callable Method.
    eq("m = \"hello\".method(:upcase); m.call", "\"HELLO\"");
    eq("[1,2,3].method(:size).call", "3");
    eq("m = 5.method(:+); m.call(3)", "8");
    eq("\"x\".method(:+).call(\"y\")", "\"xy\"");
    // `&method` block-pass: the Method feeds `map`/`each`/… as a proc.
    eq("[\"a\",\"b\"].map(&\"x\".method(:+))", "[\"xa\", \"xb\"]");
    eq("[1,2,3].map(&5.method(:+))", "[6, 7, 8]");
    // `#name`, `#to_proc`, `#is_a?`.
    eq("\"hello\".method(:upcase).name", ":upcase");
    eq("[1,2,3].method(:size).name", ":size");
    eq("\"hello\".method(:upcase).to_proc.call", "\"HELLO\"");
    eq("5.method(:+).to_proc.call(3)", "8");
    eq("\"hello\".method(:upcase).is_a?(Method)", "true");
    // `#arity` and `#call` on a user-defined method.
    eq(
        "class Foo\n  def bar(a,b); a+b; end\n  def baz; 42; end\n  def qux(*a); a; end\nend\nFoo.new.method(:bar).call(3,4)",
        "7",
    );
    eq(
        "class Foo\n  def bar(a,b); a+b; end\nend\nFoo.new.method(:bar).arity",
        "2",
    );
    eq(
        "class Foo\n  def baz; 42; end\nend\nFoo.new.method(:baz).arity",
        "0",
    );
    eq(
        "class Foo\n  def qux(*a); a; end\nend\nFoo.new.method(:qux).arity",
        "-1",
    );
    eq(
        "class Foo\n  def qux(*a); a; end\nend\nFoo.new.method(:qux).call(1,2,3)",
        "[1, 2, 3]",
    );
}

#[test]
fn block_and_lambda_splat_params() {
    // Symbol#to_proc forwards surplus args, so it works as a reduce operator.
    eq("[1, 2, 3, 4].inject(&:+)", "10");
    eq("[1, 2, 3, 4].reduce(&:*)", "24");
    eq("[1, 2, 3].reduce(10, &:+)", "16");
    // A `*rest` block param collects the surplus positional args into an array.
    eq(
        "r = []; [1, 2, 3].each { |*x| r << x }; r",
        "[[1], [2], [3]]",
    );
    eq("[[1, 2], [3, 4]].map { |*a| a }", "[[[1, 2]], [[3, 4]]]");
    eq(
        "[1, 2, 3].map { |x, *rest| [x, rest] }",
        "[[1, []], [2, []], [3, []]]",
    );
    // Lambda splat params.
    eq("sq = ->(*a) { a.sum }; sq.call(1, 2, 3)", "6");
    eq("f = ->(a, *b) { [a, b] }; f.call(1, 2, 3)", "[1, [2, 3]]");
    // Auto-splat of a two-param block over pair elements still works.
    eq("[[1, 10], [2, 20]].map { |k, v| k + v }", "[11, 22]");
}

#[test]
fn string_split_semantics() {
    // Limit keeps at most N fields; the last holds the remainder.
    eq("\"a,b,c,d\".split(\",\", 2)", "[\"a\", \"b,c,d\"]");
    // A capture group in the pattern is interleaved into the result.
    eq(
        "\"a1b2c\".split(/(\\d)/)",
        "[\"a\", \"1\", \"b\", \"2\", \"c\"]",
    );
    // Trailing empty fields are dropped at the default limit, kept when negative.
    eq("\"a,b,c,,\".split(\",\")", "[\"a\", \"b\", \"c\"]");
    eq("\"a,b,,\".split(\",\", -1)", "[\"a\", \"b\", \"\", \"\"]");
    // Leading empty fields are always kept.
    eq("\",a,b\".split(\",\")", "[\"\", \"a\", \"b\"]");
    // Empty separator splits into characters; awk mode on a single space.
    eq(
        "\"hello\".split(\"\")",
        "[\"h\", \"e\", \"l\", \"l\", \"o\"]",
    );
    eq("\"a b  c\".split(\" \")", "[\"a\", \"b\", \"c\"]");
    eq("\"\".split(\",\")", "[]");
}

#[test]
fn string_slice_bang_and_eql() {
    eq(
        "s = \"hello\"; x = s.slice!(1, 2); [x, s]",
        "[\"el\", \"hlo\"]",
    );
    eq("s = \"hello\"; s.slice!(0); s", "\"ello\"");
    eq("s = \"hello\"; s.slice!(\"ell\"); s", "\"ho\"");
    eq("\"abc\".eql?(\"abc\")", "true");
    eq("\"abc\".eql?(:abc)", "false");
}

#[test]
fn endless_and_beginless_ranges() {
    // Endless / beginless ranges as slice indices for strings and arrays.
    eq("\"hello\"[2..]", "\"llo\"");
    eq("\"hello\"[..2]", "\"hel\"");
    eq("\"hello\"[..-2]", "\"hell\"");
    eq("[1, 2, 3, 4, 5][2..]", "[3, 4, 5]");
    eq("[1, 2, 3, 4, 5][..2]", "[1, 2, 3]");
    eq("[1, 2, 3, 4, 5][1...3]", "[2, 3]");
    eq("s = \"hello\"; s.slice!(2..); s", "\"he\"");
    // Endless-range methods that don't need an upper bound.
    eq("(1..).first(3)", "[1, 2, 3]");
    eq("(1..).take(5)", "[1, 2, 3, 4, 5]");
    eq("(10..).min", "10");
    eq("(5..).include?(100)", "true");
    eq("(5..).cover?(2)", "false");
    eq(
        "r = []; (1..).each { |i| break if i > 4; r << i }; r",
        "[1, 2, 3, 4]",
    );
    // Beginless-range membership.
    eq("(..5).include?(3)", "true");
    eq("(..5).cover?(10)", "false");
    // Materializing an endless range raises rather than hanging.
    assert!(ev("(1..).to_a").is_err());
}

#[test]
fn format_positional_and_case_and_tr() {
    // `%N$` positional arguments; reuse and out-of-order both work.
    eq("\"%2$s %1$s\" % [\"a\", \"b\"]", "\"b a\"");
    eq("\"%1$s %1$s\" % [\"x\"]", "\"x x\"");
    eq("\"%2$s=%1$d\" % [7, \"k\"]", "\"k=7\"");
    eq("\"%1$05d\" % [42]", "\"00042\"");
    // `:ascii` case option leaves non-ASCII untouched; default is full Unicode.
    eq("\"groß\".upcase(:ascii)", "\"GROß\"");
    eq("\"ÜBER\".downcase(:ascii)", "\"Über\"");
    eq("\"groß\".upcase", "\"GROSS\"");
    // A descending tr range raises ArgumentError.
    assert!(ev("\"12345\".tr(\"0-9\", \"9-0\")").is_err());
    eq("\"hello\".tr(\"a-y\", \"*\")", "\"*****\"");
}

#[test]
fn bignum_arithmetic() {
    // Integers auto-promote past i64 instead of overflowing.
    eq("9223372036854775807 + 1", "9223372036854775808");
    eq("2 ** 64", "18446744073709551616");
    eq("2 ** 100", "1267650600228229401496703205376");
    eq("1 << 64", "18446744073709551616");
    eq("(10 ** 20) & 255", "0");
    eq("(2 ** 64) + (2 ** 64)", "36893488147419103232");
    eq("(2 ** 64) / 3", "6148914691236517205");
    eq("(2 ** 64) % 7", "2");
    eq("(2 ** 64) > (2 ** 63)", "true");
    eq("(2 ** 64) == (2 ** 64)", "true");
    eq("(2 ** 64).to_s(16)", "\"10000000000000000\"");
    eq("(2 ** 64).bit_length", "65");
    eq("(2 ** 64).class", "Integer");
    // Factorials stay exact.
    eq(
        "def fact(n); (1..n).reduce(1) { |a, b| a * b }; end; fact(30)",
        "265252859812191058636308480000000",
    );
}

#[test]
fn float_to_s_matches_ruby() {
    eq("1e20", "1.0e+20");
    eq("0.00001", "1.0e-05");
    eq("0.0001", "0.0001");
    eq("1000000.0", "1000000.0");
    eq("(2 ** 64).to_f", "1.8446744073709552e+19");
    eq("1.0 / 0", "Infinity");
}

#[test]
fn set_type() {
    eq("Set.new([1, 2, 3, 2, 1]).to_a", "[1, 2, 3]");
    eq("Set[1, 2, 3]", "Set[1, 2, 3]");
    eq(
        "s = Set.new([1, 2]); s.add(3); s << 4; s.to_a",
        "[1, 2, 3, 4]",
    );
    eq("Set[1, 2, 3].include?(2)", "true");
    eq("Set[1, 2, 3] | Set[3, 4, 5]", "Set[1, 2, 3, 4, 5]");
    eq("Set[1, 2, 3] & Set[2, 3, 4]", "Set[2, 3]");
    eq("Set[1, 2, 3] - Set[2]", "Set[1, 3]");
    eq("Set[1, 2, 3] ^ Set[2, 3, 4]", "Set[1, 4]");
    eq("Set[1, 2].subset?(Set[1, 2, 3])", "true");
    eq("Set[1, 2, 3].superset?(Set[1, 2])", "true");
    eq("Set[1, 2, 3] == Set[3, 2, 1]", "true");
    eq("Set[1, 2, 3].size", "3");
    eq("Set[1, 2, 3].map { |x| x * 2 }", "[2, 4, 6]");
    eq("Set[1, 2].disjoint?(Set[3, 4])", "true");
}

#[test]
fn bitwise_operators_dispatch_by_type() {
    // `&`/`|`/`^` are methods: Integer bit ops, Array/Set algebra, boolean logic.
    eq("5 & 3", "1");
    eq("5 | 2", "7");
    eq("[1, 2, 3] & [2, 3, 4]", "[2, 3]");
    eq("[1, 2] | [2, 3]", "[1, 2, 3]");
    eq("[1, 2, 3, 2] - [2]", "[1, 3]");
    eq("true & false", "false");
    eq("true | false", "true");
    eq("false ^ true", "true");
}

#[test]
fn struct_type() {
    eq("Point = Struct.new(:x, :y); Point.new(1, 2).x", "1");
    eq(
        "Point = Struct.new(:x, :y); p = Point.new(1, 2); p.y = 5; p.y",
        "5",
    );
    eq("Point = Struct.new(:x, :y); Point.new(1, 2).to_a", "[1, 2]");
    eq(
        "Point = Struct.new(:x, :y); Point.new(1, 2).to_h",
        "{x: 1, y: 2}",
    );
    eq(
        "Point = Struct.new(:x, :y); Point.new(1, 2).members",
        "[:x, :y]",
    );
    eq(
        "Point = Struct.new(:x, :y); Point.new(1, 2) == Point.new(1, 2)",
        "true",
    );
    eq(
        "Point = Struct.new(:x, :y); Point.new(1, 2) == Point.new(1, 3)",
        "false",
    );
    eq("Point = Struct.new(:x, :y); Point.new(1, 2)[0]", "1");
    eq("Point = Struct.new(:x, :y); Point.new(1, 2)[:y]", "2");
    eq("Config = Struct.new(:host, :port, keyword_init: true); Config.new(host: \"h\", port: 80).port", "80");
    // The anonymous struct is named after the constant it is bound to.
    eq(
        "Point = Struct.new(:x, :y); Point.new(3, 4).inspect",
        "\"#<struct Point x=3, y=4>\"",
    );
}

#[test]
fn heredocs() {
    // Plain heredoc interpolates; the body keeps its own newlines.
    eq("x = <<END\nhello\nworld\nEND\nx", "\"hello\\nworld\\n\"");
    // Squiggly `<<~` strips the common leading indentation.
    eq("x = <<~TXT\n  a\n  b\nTXT\nx", "\"a\\nb\\n\"");
    // Interpolation inside a heredoc.
    eq("n = \"Bob\"\n<<~MSG\n  Hi #{n}\nMSG", "\"Hi Bob\\n\"");
    // Single-quoted delimiter is a literal (no interpolation).
    eq("<<'RAW'\nno #{x} here\nRAW", "\"no \\#{x} here\\n\"");
    // Squiggly keeps relative indentation.
    eq("<<~A\n  x\n    y\n  z\nA", "\"x\\n  y\\nz\\n\"");
    // `<<` is still the shift operator in value context.
    eq("a = []; a << 1 << 2; a", "[1, 2]");
}

#[test]
fn rational_numbers() {
    eq("Rational(1, 2)", "(1/2)");
    eq("Rational(6, 4)", "(3/2)"); // reduced to lowest terms
    eq("Rational(3, 4) + Rational(1, 4)", "(1/1)");
    eq("Rational(1, 2) + Rational(1, 3)", "(5/6)");
    eq("Rational(1, 2) * 4", "(2/1)");
    eq("Rational(1, 2) ** 3", "(1/8)");
    eq("Rational(1, 2) < Rational(2, 3)", "true");
    eq("Rational(2, 4) == Rational(1, 2)", "true");
    eq("Rational(1, 2).to_f", "0.5");
    eq("Rational(1, 2).numerator", "1");
    eq("Rational(1, 2).denominator", "2");
    eq("Rational(3, 2).to_i", "1");
    eq("Rational(-3, 4).abs", "(3/4)");
    // Literal `Nr` suffix; a Float operand demotes the result to Float.
    eq("1r + 2r", "(3/1)");
    eq("3 / 4r", "(3/4)");
    eq("Rational(1, 2) + 0.5", "1.0");
}

#[test]
fn complex_numbers() {
    eq("Complex(3, 4)", "(3+4i)");
    eq("Complex(3, -4)", "(3-4i)");
    eq("Complex(1, 2) + Complex(3, 4)", "(4+6i)");
    eq("Complex(1, 2) - Complex(2, 3)", "(-1-1i)");
    eq("Complex(1, 2) * Complex(3, 4)", "(-5+10i)");
    eq("Complex(3, 4).real", "3");
    eq("Complex(3, 4).imaginary", "4");
    eq("Complex(3, 4).abs", "5.0");
    eq("Complex(3, 4).conjugate", "(3-4i)");
    eq("Complex(3, 4) == Complex(3, 4)", "true");
    // Imaginary literal and real-number promotion.
    eq("1i", "(0+1i)");
    eq("2 + 3i", "(2+3i)");
    eq("[Complex(1, 1), Complex(2, 2)].reduce(:+)", "(3+3i)");
}

#[test]
fn pattern_matching_case_in() {
    // Array patterns bind elements; splat collects the middle/rest.
    eq("case [1, 2]; in [a, b]; a + b; end", "3");
    eq(
        "case [1, 2, 3, 4]; in [first, *rest]; rest; end",
        "[2, 3, 4]",
    );
    eq("case [1, 2, 3]; in [a, *, c]; [a, c]; end", "[1, 3]");
    // Hash patterns bind by key (shorthand) or match a subpattern.
    eq(
        "case {name: \"Al\", age: 30}; in {name:, age:}; \"#{name}:#{age}\"; end",
        "\"Al:30\"",
    );
    eq(
        "case {t: \"c\", r: 5}; in {t: \"c\", r: Integer => n}; n; end",
        "5",
    );
    // Type patterns with binding, ordered by clause.
    eq("case 5; in String; :s; in Integer => n; n * 2; end", "10");
    // Ranges, alternatives, and pins.
    eq("case 7; in 1..5; :lo; in 6..10; :hi; end", ":hi");
    eq("case 3; in 1 | 2 | 3; :small; else; :big; end", ":small");
    eq("x = 5; case 5; in ^x; :pinned; end", ":pinned");
    // Guards, nesting, and the whole-match `=> name` binding.
    eq(
        "case [1, 2]; in [a, b] if a < b; :ok; else; :no; end",
        ":ok",
    );
    eq("case [1, [2, 3]]; in [a, [b, c]]; a + b + c; end", "6");
    eq(
        "case [1, 2]; in [Integer, Integer] => pair; pair; end",
        "[1, 2]",
    );
    // No matching clause and no else raises.
    assert!(ev("case 99; in 1; :x; end").is_err());
}

#[test]
fn method_missing_and_respond_to_missing() {
    // method_missing handles otherwise-undefined calls, with args and a block.
    eq(
        "class P; def method_missing(n, *a); \"#{n}:#{a.inspect}\"; end; end; P.new.foo(1, 2)",
        "\"foo:[1, 2]\"",
    );
    eq(
        "class P; def method_missing(n, *a, &b); b.call(a.first); end; end; P.new.x(5) { |v| v * 2 }",
        "10",
    );
    // respond_to? consults respond_to_missing?.
    eq(
        "class P; def method_missing(n,*a); n; end; def respond_to_missing?(n, _=false); n == :ok; end; end; \
         p = P.new; [p.respond_to?(:ok), p.respond_to?(:no)]",
        "[true, false]",
    );
    // A class with no method_missing still raises.
    assert!(ev("class Q; end; Q.new.nope").is_err());
    // A proxy forwards calls (incl. a block through a splat) via method_missing.
    eq(
        "class Px; def initialize(t); @t = t; end; def method_missing(n, *a, &b); @t.send(n, *a, &b); end; end; \
         Px.new([1, 2, 3]).map { |x| x * 2 }",
        "[2, 4, 6]",
    );
}

#[test]
fn class_variables() {
    // A class variable is shared across all instances of the class.
    eq(
        "class C; @@n = 0; def initialize; @@n += 1; end; def self.count; @@n; end; end; \
         C.new; C.new; C.new; C.count",
        "3",
    );
    // Shared through the superclass chain.
    eq(
        "class Base; @@v = \"a\"; def self.get; @@v; end; end; \
         class Sub < Base; def self.set(x); @@v = x; end; end; Sub.set(\"b\"); Base.get",
        "\"b\"",
    );
    // Mutating a shared collection is visible everywhere.
    eq(
        "class L; @@items = []; def add(x); @@items << x; end; def all; @@items; end; end; \
         a = L.new; a.add(1); L.new.add(2); a.all",
        "[1, 2]",
    );
    // Class-body statements (constant assignment) now run at definition time.
    eq(
        "class K; MSG = \"hi\"; def m; MSG; end; end; K.new.m",
        "\"hi\"",
    );
}

#[test]
fn define_method_metaprogramming() {
    // A block becomes an instance method taking arguments.
    eq(
        "class C; define_method(:double) { |x| x * 2 }; end; C.new.double(21)",
        "42",
    );
    // The method body runs with self = the receiver (accesses its ivars).
    eq(
        "class C; def initialize; @x = 10; end; define_method(:plus) { |n| @x + n }; end; \
         C.new.plus(5)",
        "15",
    );
    // Defined in a loop, each block captures its own binding.
    eq(
        "class C; [:a, :b].each { |s| define_method(s) { s.to_s } }; end; \
         [C.new.a, C.new.b]",
        "[\"a\", \"b\"]",
    );
    // define_method also works with a dynamically built name.
    eq("class C; define_method(\"m2\") { 99 }; end; C.new.m2", "99");
}

#[test]
fn method_aliases() {
    // alias_method makes a second name for a method.
    eq(
        "class C; def greet; \"hi\"; end; alias_method :hi, :greet; end; C.new.hi",
        "\"hi\"",
    );
    // The `alias` keyword form.
    eq(
        "class C; def size; 42; end; alias length size; end; C.new.length",
        "42",
    );
    // Aliases carry through arguments and access the receiver's ivars.
    eq(
        "class C; def initialize; @b = 100; end; def calc(x); x + @b; end; \
         alias_method :compute, :calc; end; C.new.compute(5)",
        "105",
    );
}

#[test]
fn lazy_enumerators() {
    // Infinite ranges are safe: only as many elements as needed are pulled.
    eq("(1..).lazy.map { |x| x * 2 }.first(5)", "[2, 4, 6, 8, 10]");
    eq("(1..).lazy.select { |x| x.even? }.first(3)", "[2, 4, 6]");
    eq(
        "(1..).lazy.select(&:odd?).map { |x| x * 10 }.first(3)",
        "[10, 30, 50]",
    );
    eq("(1..).lazy.map { |x| x * x }.take(4).to_a", "[1, 4, 9, 16]");
    eq(
        "(1..).lazy.filter_map { |x| x * 2 if x.odd? }.first(3)",
        "[2, 6, 10]",
    );
    eq("(1..).lazy.drop(3).first(2)", "[4, 5]");
    // Finite sources materialize fully with to_a/force.
    eq(
        "[1, 2, 3, 4, 5].lazy.map { |x| x * x }.select { |x| x > 5 }.to_a",
        "[9, 16, 25]",
    );
    eq("(1..20).lazy.take_while { |x| x < 5 }.to_a", "[1, 2, 3, 4]");
    eq(
        "[1, 2, 3].lazy.flat_map { |x| [x, -x] }.force",
        "[1, -1, 2, -2, 3, -3]",
    );
}

#[test]
fn enumerator_external_iteration() {
    // Block-less `each` yields an Enumerator advanced by `next`.
    eq("e = [1, 2, 3].each; [e.next, e.next, e.next]", "[1, 2, 3]");
    // `peek` reads the pending element without advancing the cursor.
    eq(
        "e = [10, 20].each; [e.peek, e.next, e.peek]",
        "[10, 10, 20]",
    );
    // `rewind` resets the cursor to the start.
    eq("e = [1, 2, 3].each; e.next; e.rewind; e.next", "1");
    // `size` reports the buffered length.
    eq("[5, 6, 7].each.size", "3");
    // Running off the end raises StopIteration.
    eq(
        "e = [1].each; e.next; begin; e.next; rescue StopIteration => x; x.message; end",
        "\"iteration reached an end\"",
    );
    // Block-less `map` yields the original elements for external iteration.
    eq("e = [1, 2, 3].map; [e.next, e.next]", "[1, 2]");
    // Block-less `each_with_index` yields `[elem, index]` pairs.
    eq(
        "e = %w[a b].each_with_index; [e.next, e.next]",
        "[[\"a\", 0], [\"b\", 1]]",
    );
    // Enumerable methods still delegate to the buffer.
    eq("[1, 2, 3].each.to_a", "[1, 2, 3]");
    eq(
        "[1, 2, 3].each_with_index.map { |x, i| x * i }",
        "[0, 2, 6]",
    );
}

#[test]
fn enumerator_with_index_and_object() {
    // `map.with_index` collects the block's results.
    eq(
        "[10, 20, 30].map.with_index { |x, i| [x, i] }",
        "[[10, 0], [20, 1], [30, 2]]",
    );
    // An explicit start offset.
    eq(
        "[10, 20, 30].map.with_index(1) { |x, i| \"#{i}:#{x}\" }",
        "[\"1:10\", \"2:20\", \"3:30\"]",
    );
    // `select.with_index` filters by the block's truthiness.
    eq(
        "[10, 20, 30].select.with_index { |x, i| i.even? }",
        "[10, 30]",
    );
    // `reject.with_index` keeps the falsy ones.
    eq("[10, 20, 30].reject.with_index { |x, i| i.even? }", "[20]");
    // `each.with_index` runs for side effects and returns the elements.
    eq("[10, 20, 30].each.with_index { |x, i| x }", "[10, 20, 30]");
    // `with_index` block-less yields `[elem, offset+index]` pairs.
    eq("[10, 20].map.with_index.to_a", "[[10, 0], [20, 1]]");
    // `with_object` threads a memo through the block and returns it.
    eq(
        "[1, 2, 3].map.with_object([]) { |x, memo| memo << x * 2 }",
        "[2, 4, 6]",
    );
}

#[test]
fn time_utc() {
    // Broken-down UTC fields from an epoch.
    eq(
        "t = Time.at(1500000000).utc; [t.year, t.month, t.day, t.hour, t.min, t.sec]",
        "[2017, 7, 14, 2, 40, 0]",
    );
    eq("Time.at(0).utc.year", "1970");
    eq("Time.at(0).utc.wday", "4"); // 1970-01-01 was a Thursday
    eq("Time.at(86400).utc.yday", "2");
    // Building from fields.
    eq("Time.utc(2020, 1, 1).to_i", "1577836800");
    eq("Time.utc(2020, 2, 29, 12, 30, 15).to_i", "1582979415"); // leap day
    eq("Time.gm(1999, 12, 31, 23, 59, 59).to_i", "946684799");
    // to_s (no subsecond) and inspect (with subsecond).
    eq(
        "Time.at(1000000000).utc.to_s",
        "\"2001-09-09 01:46:40 UTC\"",
    );
    eq("Time.at(1.5).utc.inspect", "\"1970-01-01 00:00:01.5 UTC\"");
    // strftime.
    eq(
        "Time.at(1000000000).utc.strftime(\"%Y-%m-%d %H:%M:%S\")",
        "\"2001-09-09 01:46:40\"",
    );
    eq(
        "Time.at(1000000000).utc.strftime(\"%A, %B %d, %Y\")",
        "\"Sunday, September 09, 2001\"",
    );
    eq(
        "Time.at(1000000000).utc.strftime(\"%a %b %e %T %Z\")",
        "\"Sun Sep  9 01:46:40 UTC\"",
    );
    // Arithmetic and comparison.
    eq("Time.at(100) - Time.at(40)", "60.0");
    eq("(Time.at(100) + 3600).to_i", "3700");
    eq("Time.at(100) < Time.at(200)", "true");
    eq("Time.at(100) <=> Time.at(200)", "-1");
    eq("Time.at(100) == Time.at(100)", "true");
    eq(
        "[Time.at(30), Time.at(10), Time.at(20)].sort.map(&:to_i)",
        "[10, 20, 30]",
    );
    // Negative epoch (before 1970) via floor-division calendar math.
    eq("Time.at(-1).utc.year", "1969");
    eq(
        "t = Time.at(-1).utc; [t.month, t.day, t.hour, t.min, t.sec]",
        "[12, 31, 23, 59, 59]",
    );
}

#[test]
fn string_inspect_escaping() {
    // Named control escapes round-trip through inspect.
    eq("\"\\e[0m\"", "\"\\e[0m\""); // ESC -> \e
    eq("\"\\a\\b\\f\\v\\r\"", "\"\\a\\b\\f\\v\\r\"");
    eq("\"tab\\there\"", "\"tab\\there\"");
    eq("\"a\\nb\"", "\"a\\nb\"");
    // Control chars without a named escape use \uXXXX (uppercase, UTF-8).
    eq("\"\\x00\"", "\"\\u0000\"");
    eq("\"\\x01\\x1f\\x7f\"", "\"\\u0001\\u001F\\u007F\"");
    // `#` is escaped only before `{`/`@`/`$` (so the literal is unambiguous).
    eq("\"\\#{x}\"", "\"\\#{x}\"");
    eq("\"a # b\"", "\"a # b\"");
    // Quotes and backslashes.
    eq("\"say \\\"hi\\\"\"", "\"say \\\"hi\\\"\"");
    eq("\"a\\\\b\"", "\"a\\\\b\"");
    // Multibyte UTF-8 is verbatim.
    eq("\"café\"", "\"café\"");
    // Nested in a collection.
    eq("[\"a\\tb\", \"c\\x01d\"]", "[\"a\\tb\", \"c\\u0001d\"]");
}

#[test]
fn defined_operator() {
    // Undefined names return nil; defined ones return a description string.
    eq("defined?(nonexistent_xyz)", "nil");
    eq("x = 5; defined?(x)", "\"local-variable\"");
    eq("defined?(also_undefined)", "nil");
    // Constants (user, builtin class, module).
    eq("defined?(String)", "\"constant\"");
    eq("defined?(Math)", "\"constant\"");
    eq("CONST = 1; defined?(CONST)", "\"constant\"");
    eq("defined?(NoSuchConst)", "nil");
    // Instance / global variables.
    eq("defined?(@unset)", "nil");
    eq("@ivar = 1; defined?(@ivar)", "\"instance-variable\"");
    // Kernel methods and keyword literals.
    eq("defined?(puts)", "\"method\"");
    eq("defined?(nil)", "\"nil\"");
    eq("defined?(true)", "\"true\"");
    eq("defined?(self)", "\"self\"");
    // Assignment / method call / expression classifications (no evaluation).
    eq("x = 1; defined?(x = 10)", "\"assignment\"");
    eq("defined?(String.new)", "\"method\"");
    eq("defined?(1 + 1)", "\"method\"");
    eq("defined?([1, 2, 3])", "\"expression\"");
    // The bare (paren-less) form.
    eq("y = 1; defined? y", "\"local-variable\"");
    // Common guard patterns.
    eq("defined?(unknown_thing) ? :y : :n", ":n");
    eq("defined?(Integer) && :ok", ":ok");
}

#[test]
fn math_module() {
    // Functions.
    eq("Math.sqrt(25)", "5.0");
    eq("Math.sqrt(2)", "1.4142135623730951");
    eq("Math.cbrt(27)", "3.0");
    eq("Math.sin(0)", "0.0");
    eq("Math.cos(0)", "1.0");
    eq("Math.exp(0)", "1.0");
    eq("Math.log(Math::E)", "1.0");
    eq("Math.log(100, 10)", "2.0"); // change-of-base
    eq("Math.log2(8)", "3.0");
    eq("Math.log10(1000)", "3.0");
    eq("Math.hypot(3, 4)", "5.0");
    eq("Math.atan2(1, 1)", "0.7853981633974483");
    // Constants.
    eq("Math::PI", "3.141592653589793");
    eq("Math::E", "2.718281828459045");
    eq("Math::PI.round(5)", "3.14159");
    // Composed usage.
    eq("Math.sqrt(3 ** 2 + 4 ** 2)", "5.0");
    eq(
        "[1, 4, 9, 16].map { |n| Math.sqrt(n) }",
        "[1.0, 2.0, 3.0, 4.0]",
    );
    eq("r = 5; (Math::PI * r ** 2).round(2)", "78.54");
    eq("Math.sin(Math::PI / 2)", "1.0");
}

#[test]
fn endless_method_definitions() {
    // `def name(params) = expression` (Ruby 3+): single-expression body, no end.
    eq("def square(x) = x * x; square(5)", "25");
    eq("def greeting = \"hi\"; greeting", "\"hi\"");
    eq("def add(a, b) = a + b; add(3, 4)", "7");
    eq(
        "def double(x) = x * 2; [1, 2, 3].map { |n| double(n) }",
        "[2, 4, 6]",
    );
    // A ternary body and self-recursion.
    eq(
        "def fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2); fib(10)",
        "55",
    );
    // Inside a class, and a singleton (`def self.`) endless def.
    eq(
        "class C; def area(w, h) = w * h; end; C.new.area(3, 4)",
        "12",
    );
    eq(
        "class C; def self.build(n) = new; end; C.build(1).class == C",
        "true",
    );
    // Default arguments in an endless def.
    eq("def compute(x, y = 10) = x + y; compute(5)", "15");
    // Mixed with a normal def in the same class.
    eq(
        "class T; def initialize(c) = @c = c; def f = @c * 2; end; T.new(21).f",
        "42",
    );
}

#[test]
fn percent_literals() {
    // %q — single-quoted (no interpolation), various delimiters.
    eq("%q(hello world)", "\"hello world\"");
    eq("%q{braces}", "\"braces\"");
    eq("%q[brackets]", "\"brackets\"");
    eq("%q<angle>", "\"angle\"");
    eq("%q!bang!", "\"bang\"");
    eq("%q(nested (parens) work)", "\"nested (parens) work\"");
    // %Q — double-quoted (interpolation).
    eq("%Q(sum=#{2 * 3})", "\"sum=6\"");
    eq("x = 5; %Q(val is #{x})", "\"val is 5\"");
    // %r — Regexp, including flags and a paren-less command position.
    eq("%r{ab+c} =~ \"xabbbc\"", "1");
    eq("%r{[0-9]+}.match?(\"abc123\")", "true");
    eq("%r{ABC}i =~ \"xabc\"", "1");
    eq("\"Hello World\".scan(%r{\\w+})", "[\"Hello\", \"World\"]");
    // %s — Symbol.
    eq("%s(hello)", ":hello");
    // A plain regex literal as a paren-less command arg now parses.
    eq("/\\d+/.match(\"abc123\")[0]", "\"123\"");
    // Modulo is unaffected.
    eq("10 % 3", "1");
    eq("\"%05.2f\" % 3.14", "\"03.14\"");
}

#[test]
fn filter_map_transpose_string_index_radix() {
    // Array#filter_map maps and keeps truthy results.
    eq("[1, 2, 3, 4].filter_map { |x| x * 2 if x.even? }", "[4, 8]");
    eq("(1..10).filter_map { |x| x if x.odd? }", "[1, 3, 5, 7, 9]");
    // Array#transpose.
    eq("[[1, 2], [3, 4]].transpose", "[[1, 3], [2, 4]]");
    eq(
        "[[1, 2, 3], [4, 5, 6]].transpose",
        "[[1, 4], [2, 5], [3, 6]]",
    );
    eq("[].transpose", "[]");
    // String#[] with a Regexp or substring.
    eq("\"hello\"[/l+/]", "\"ll\"");
    eq("\"hello world\"[/\\w+/]", "\"hello\"");
    eq("\"hello\"[/xyz/]", "nil");
    eq("\"2024-01-15\"[/(\\d+)-(\\d+)/, 2]", "\"01\"");
    eq("\"hello\"[\"ll\"]", "\"ll\"");
    eq("\"hello\"[\"xyz\"]", "nil");
    // Radix integer literals.
    eq("0b1010", "10");
    eq("0o17", "15");
    eq("0xff", "255");
    eq("0b1111_0000", "240");
    eq("017", "15"); // leading-zero octal
    eq("0xDEAD", "57005");
    eq("0d99", "99");
    eq("0xff + 1", "256");
    eq("[0b1, 0b10, 0b100]", "[1, 2, 4]");
}

#[test]
fn composite_hash_keys() {
    // Arrays as Hash keys — structural equality, round-trip, nesting.
    eq("{[1, 2] => \"a\", [3, 4] => \"b\"}[[3, 4]]", "\"b\"");
    eq("{[1, [2, 3]] => \"nested\"}[[1, [2, 3]]]", "\"nested\"");
    eq("{[1, 2] => \"x\"}.keys", "[[1, 2]]");
    eq("{[1, 2] => \"x\"}.inspect", "\"{[1, 2] => \\\"x\\\"}\"");
    eq("{[:a, :b] => 1}[[:a, :b]]", "1");
    // A default-0 hash counted by array key.
    eq("h = {}; h[[1, 2]] = 1; h[[1, 2]] += 1; h", "{[1, 2] => 2}");
    // group_by(&:itself) over arrays dedups equal arrays.
    eq(
        "[[1, 2], [1, 2], [3, 4]].group_by(&:itself)",
        "{[1, 2] => [[1, 2], [1, 2]], [3, 4] => [[3, 4]]}",
    );
    // Ranges as Hash keys.
    eq("{(1..3) => \"r\"}[(1..3)]", "\"r\"");
    eq("{(1..3) => \"x\"}.keys", "[1..3]");
    eq("{(1...5) => \"e\"}.inspect", "\"{1...5 => \\\"e\\\"}\"");
    eq("{(\"a\"..\"c\") => 1}[(\"a\"..\"c\")]", "1");
    // Duplicate literal keys keep the last value.
    eq("{[1, 2] => 1, [1, 2] => 2}", "{[1, 2] => 2}");
    // Memoization keyed by an argument array.
    eq(
        "cache = {}; f = ->(a, b) { cache[[a, b]] ||= a + b }; [f.call(1, 2), f.call(1, 2), cache.size]",
        "[3, 3, 1]",
    );
}

#[test]
fn class_objects_as_hash_keys() {
    // A Class object works as a Hash key (compares by class, round-trips).
    eq("{Integer => 1, String => 2}[Integer]", "1");
    eq("{Integer => 1}[5.class]", "1");
    eq("{Integer => \"int\"}.keys.first", "Integer");
    eq("{Integer => 1}.inspect", "\"{Integer => 1}\"");
    // group_by(&:class) — the common idiom — keys by the actual class.
    eq(
        "[1, \"a\", :b, 2].group_by(&:class)",
        "{Integer => [1, 2], String => [\"a\"], Symbol => [:b]}",
    );
    eq(
        "[1, \"a\", :b, 2].group_by(&:class).keys",
        "[Integer, String, Symbol]",
    );
    // Counting by class through a default-0 hash.
    eq(
        "h = Hash.new(0); [1, 2.0, \"x\", 3].each { |v| h[v.class] += 1 }; h",
        "{Integer => 2, Float => 1, String => 1}",
    );
    eq(
        "[1, 2, 3, \"a\", \"b\"].group_by(&:class).transform_values(&:size)",
        "{Integer => 3, String => 2}",
    );
}

#[test]
fn class_reflection() {
    // superclass walks the class chain (skipping modules).
    eq("Integer.superclass", "Numeric");
    eq("String.superclass", "Object");
    eq("Numeric.superclass", "Object");
    eq("Object.superclass", "BasicObject");
    eq("BasicObject.superclass", "nil");
    eq("class A; end; A.superclass", "Object");
    eq("class A; end; class B < A; end; B.superclass", "A");
    // ancestors includes modules.
    eq(
        "Integer.ancestors",
        "[Integer, Numeric, Comparable, Object, Kernel, BasicObject]",
    );
    eq(
        "String.ancestors",
        "[String, Comparable, Object, Kernel, BasicObject]",
    );
    eq(
        "Array.ancestors",
        "[Array, Enumerable, Object, Kernel, BasicObject]",
    );
    eq(
        "class A; end; class B < A; end; B.ancestors",
        "[B, A, Object, Kernel, BasicObject]",
    );
    eq(
        "module M; end; class C; include M; end; C.ancestors",
        "[C, M, Object, Kernel, BasicObject]",
    );
    eq("Integer.ancestors.include?(Numeric)", "true");
    eq("5.class.ancestors.first", "Integer");
    // The class comparison operators: subclass (<), self-or-subclass (<=),
    // unrelated (nil).
    eq("Integer < Numeric", "true");
    eq("Integer <= Integer", "true");
    eq("Integer < Integer", "false");
    eq("Numeric > Integer", "true");
    eq("String < Numeric", "nil");
    eq("class A; end; class B < A; end; B < A", "true");
    eq("Object > Integer", "true");
}

#[test]
fn object_class_returns_class_object() {
    // `.class` is a Class object (prints the bare name), not its String name.
    eq("5.class", "Integer");
    eq("\"x\".class", "String");
    eq("[1].class", "Array");
    eq(":s.class", "Symbol");
    eq("nil.class", "NilClass");
    eq("3.14.class", "Float");
    eq("(2 ** 64).class", "Integer");
    eq("{}.class", "Hash");
    // Class-object identity: `obj.class == SomeClass`.
    eq("5.class == Integer", "true");
    eq("\"x\".class == String", "true");
    eq("5.class == String", "false");
    eq("1.0.class == Float", "true");
    eq("Integer == Integer", "true");
    // `.class.name` / `.class.to_s` give the name String.
    eq("5.class.name", "\"Integer\"");
    eq("[].class.to_s", "\"Array\"");
    // A user class instance.
    eq("class Widget; end; Widget.new.class == Widget", "true");
    eq("class Widget; end; Widget.new.class.name", "\"Widget\"");
    // Rescued exceptions report their Class.
    eq(
        "begin; raise ArgumentError, \"y\"; rescue => e; e.class == ArgumentError; end",
        "true",
    );
    // Interpolation and mapping.
    eq("\"#{5.class}\"", "\"Integer\"");
    eq(
        "[1, \"a\", :b].map { |x| x.class }",
        "[Integer, String, Symbol]",
    );
}

#[test]
fn numeric_rational_conversions() {
    // Integer#to_r is n/1; Float#to_r is the exact rational the f64 represents.
    eq("3.to_r", "(3/1)");
    eq("(-4).to_r", "(-4/1)");
    eq("0.5.to_r", "(1/2)");
    eq("0.75.to_r", "(3/4)");
    eq("0.1.to_r", "(3602879701896397/36028797018963968)");
    eq("(-1.5).to_r", "(-3/2)");
    // String#to_r parses a leading rational/decimal.
    eq("\"3/4\".to_r", "(3/4)");
    eq("\"3.14\".to_r", "(157/50)");
    eq("\"-5/10\".to_r", "(-1/2)");
    eq("\"abc\".to_r", "(0/1)");
    // rationalize finds the simplest rational within a tolerance.
    eq("3.14.rationalize(0.01)", "(22/7)");
    eq("0.3.rationalize", "(3/10)");
    eq("0.1.rationalize", "(1/10)");
    // to_c wraps a real number as a Complex.
    eq("10.to_c", "(10+0i)");
    eq("3.5.to_c", "(3.5+0i)");
    // nil conversions.
    eq("nil.to_a", "[]");
    eq("nil.to_h", "{}");
    // Round-trips through arithmetic.
    eq("0.5.to_r + 0.25.to_r", "(3/4)");
    eq("\"1/3\".to_r * 3", "(1/1)");
}

#[test]
fn hash_enumerable_and_default() {
    // Hash#reduce/inject iterate the `[k, v]` pairs.
    eq("{a: 1, b: 2, c: 3}.reduce(0) { |s, (k, v)| s + v }", "6");
    eq("{a: 1, b: 2}.inject(0) { |s, (k, v)| s + v }", "3");
    eq(
        "{a: 1, b: 2}.reduce([]) { |acc, (k, v)| acc << k }",
        "[:a, :b]",
    );
    // group_by / partition / find_all over pairs.
    eq(
        "{a: 1, b: 2}.group_by { |k, v| v.even? }",
        "{false => [[:a, 1]], true => [[:b, 2]]}",
    );
    eq(
        "{a: 1, b: 2}.partition { |k, v| v > 1 }",
        "[[[:b, 2]], [[:a, 1]]]",
    );
    eq("{a: 1, b: 2}.find_all { |k, v| v > 1 }", "[[:b, 2]]");
    eq("[1, 2, 3, 4].find_all { |x| x > 2 }", "[3, 4]");
    // Hash#default / #default= for missing keys.
    eq("h = {}; h.default = 5; [h[:x], h[:y]]", "[5, 5]");
    eq("h = Hash.new; h.default = 0; h[:a] += 1; h", "{a: 1}");
    eq("{a: 1}.default", "nil");
    eq("Hash.new(9).default", "9");
    // default= does not affect present keys.
    eq("h = {a: 1}; h.default = 0; h[:a]", "1");
}

#[test]
fn float_ranges() {
    // Float ranges support step (with Ruby's drift-free count).
    eq("(1.0..2.0).step(0.5).to_a", "[1.0, 1.5, 2.0]");
    eq("(0.0..1.0).step(0.25).to_a", "[0.0, 0.25, 0.5, 0.75, 1.0]");
    eq("(1.0...2.0).step(0.5).to_a", "[1.0, 1.5]");
    // An Integer range with a Float step steps in Float space.
    eq("(1..3).step(0.5).to_a", "[1.0, 1.5, 2.0, 2.5, 3.0]");
    // Endpoints and containment.
    eq("(1.0..2.0).min", "1.0");
    eq("(1.0..2.0).max", "2.0");
    eq("(1.0..2.0).begin", "1.0");
    eq("(1.5..4.5).include?(3.2)", "true");
    eq("(1.0...5.0).include?(5.0)", "false");
    eq("(1.0..5.0).include?(5.0)", "true");
    eq("(1.0...2.0).exclude_end?", "true");
    eq("(1.0..2.0).to_s", "\"1.0..2.0\"");
    eq("(1.0..2.0).inspect", "\"1.0..2.0\"");
}

#[test]
fn case_equality_operator() {
    // `===` is case-equality, NOT `==`: a Range covers, a Class matches
    // instances, a Regexp matches a string.
    eq("(1..5) === 3", "true");
    eq("(1..5) === 9", "false");
    eq("(1.5..4.5) === 3.0", "true");
    eq("Integer === 5", "true");
    eq("Integer === \"x\"", "false");
    eq("Numeric === 5", "true");
    eq("/ab/ === \"xabc\"", "true");
    // `==` stays structural equality (unaffected by the `===` fix).
    eq("(1..5) == (1..5)", "true");
    eq("(1..5) == 3", "false");
    // case/when uses `===`.
    eq("case 3.0; when 1.5..4.5 then :in; else :out; end", ":in");
    eq(
        "case 42; when Integer then :int; when String then :str; end",
        ":int",
    );
}

#[test]
fn symproc_over_array_elements() {
    // `&:sym` must send the method to each element as a whole — an array
    // element is the receiver, never auto-splatted into (recv, *args).
    eq(
        "[[\"a\", \"b\"], [\"c\", \"d\"]].map(&:join)",
        "[\"ab\", \"cd\"]",
    );
    eq("[[1, 2], [3, 4]].map(&:sum)", "[3, 7]");
    eq("[[1, 2, 3], [4, 5]].map(&:max)", "[3, 5]");
    // Chained through Enumerators (each_slice yields sub-arrays).
    eq("(1..10).each_slice(3).map(&:sum)", "[6, 15, 24, 10]");
    eq(
        "\"abcdef\".each_char.each_slice(2).map(&:join)",
        "[\"ab\", \"cd\", \"ef\"]",
    );
    // Surplus-argument forwarding still works: reduce yields (acc, x).
    eq("[1, 2, 3, 4].reduce(&:+)", "10");
    eq("[1, 2, 3, 4].reduce(&:*)", "24");
    // Scalar elements unchanged.
    eq("[1, -2, 3].map(&:abs)", "[1, 2, 3]");
    eq("[\"x\", \"y\"].map(&:upcase)", "[\"X\", \"Y\"]");
    // A pair element: `&:first` takes the whole pair as receiver.
    eq("[[1, 2], [3, 4]].map(&:first)", "[1, 3]");
}

#[test]
fn block_parameter_destructuring() {
    // A `(a, b)` block parameter unpacks the corresponding array argument.
    eq("[[1, 2], [3, 4]].map { |(a, b)| a + b }", "[3, 7]");
    // Nested destructuring.
    eq("[[1, [2, 3]]].map { |(a, (b, c))| a + b + c }", "[6]");
    // Splat inside a destructuring group.
    eq("[[1, 2, 3]].map { |(a, *rest)| rest }", "[[2, 3]]");
    // Destructure the first param, keep a plain second param.
    eq(
        "[[1, 2], [3, 4]].each_with_index.map { |(a, b), i| [a, b, i] }",
        "[[1, 2, 0], [3, 4, 1]]",
    );
    // The classic each_with_object over hash pairs.
    eq(
        "{a: 1, b: 2}.each_with_object([]) { |(k, v), acc| acc << \"#{k}=#{v}\" }",
        "[\"a=1\", \"b=2\"]",
    );
    // A single destructuring param over a hash pair.
    eq("{x: 1}.map { |(k, v)| \"#{k}=#{v}\" }", "[\"x=1\"]");
    // Lambda and proc destructuring.
    eq("f = ->((a, b)) { a + b }; f.call([1, 2])", "3");
    eq("pr = proc { |(a, b)| a * b }; pr.call([3, 4])", "12");
    // Auto-splat of a hash pair to a 2-param block still works.
    eq("{a: 1, b: 2}.map { |k, v| v * 10 }", "[10, 20]");
    // A single plain param receives the whole pair.
    eq("{a: 1}.map { |pair| pair }", "[[:a, 1]]");
}

#[test]
fn blockless_iterators_return_enumerators() {
    // String#each_char without a block yields an Enumerator, not the string.
    eq(
        "\"Hello\".each_char.to_a",
        "[\"H\", \"e\", \"l\", \"l\", \"o\"]",
    );
    eq("e = \"ab\".each_char; [e.next, e.next]", "[\"a\", \"b\"]");
    eq("\"hi\".each_char.map { |c| c.upcase }", "[\"H\", \"I\"]");
    eq(
        "\"ab\".each_char.with_index.to_a",
        "[[\"a\", 0], [\"b\", 1]]",
    );
    // each_line / each_byte likewise.
    eq("\"a\\nb\".each_line.to_a", "[\"a\\n\", \"b\"]");
    eq("\"AB\".each_byte.to_a", "[65, 66]");
    eq("e = \"AB\".each_byte; [e.next, e.next]", "[65, 66]");
    // Integer#times / upto / downto / step yield Enumerators.
    eq("e = 3.times; [e.next, e.next, e.next]", "[0, 1, 2]");
    eq("3.times.map { |i| i * i }", "[0, 1, 4]");
    eq("5.upto(8).to_a", "[5, 6, 7, 8]");
    eq("e = 5.upto(8); e.next", "5");
    eq("8.downto(5).to_a", "[8, 7, 6, 5]");
    eq("1.step(10, 3).to_a", "[1, 4, 7, 10]");
    eq("e = 1.step(10, 3); [e.next, e.next]", "[1, 4]");
    // With a block, these still run eagerly and return the receiver.
    eq("s = \"ab\"; s.each_char { |c| c }; s", "\"ab\"");
    eq("3.times { |i| i }", "3");
}

#[test]
fn no_method_error_messages() {
    // Ruby 4.0 message form: instances say "for an instance of <Class>".
    eq(
        "begin; \"x\".nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for an instance of String\"",
    );
    eq(
        "begin; 5.nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for an instance of Integer\"",
    );
    eq(
        "begin; [].nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for an instance of Array\"",
    );
    // Range/Set/Enumerator name their own class, not the delegated Array.
    eq(
        "begin; (1..2).nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for an instance of Range\"",
    );
    eq(
        "begin; [1].each.nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for an instance of Enumerator\"",
    );
    // nil/true/false use the bare-value form (not "an instance of").
    eq(
        "begin; nil.nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for nil\"",
    );
    eq(
        "begin; true.nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for true\"",
    );
    // A class/module reference says "for class <Name>".
    eq(
        "begin; Integer.nope; rescue => e; e.message; end",
        "\"undefined method 'nope' for class Integer\"",
    );
    // The exception class is NoMethodError (rescuable specifically).
    eq(
        "begin; 1.nope; rescue NoMethodError; :caught; end",
        ":caught",
    );
}

#[test]
fn date_calendar() {
    // Construction and field readers.
    eq(
        "d = Date.new(2024, 2, 29); [d.year, d.month, d.day, d.wday, d.yday]",
        "[2024, 2, 29, 4, 60]",
    );
    eq("Date.new(2024, 1, 1).leap?", "true");
    eq("Date.new(2023, 1, 1).leap?", "false");
    eq("Date.new(2024, 3, 1).jd", "2460371");
    eq("Date.new(2024, 3, 1).cwday", "5"); // Friday
                                           // Arithmetic: Date - Date is a Rational day count; Date ± Integer shifts days.
    eq("Date.new(2024, 3, 1) - Date.new(2024, 2, 1)", "(29/1)");
    eq("(Date.new(2024, 1, 1) + 40).to_s", "\"2024-02-10\"");
    eq("Date.new(2024, 2, 29).next_day.to_s", "\"2024-03-01\"");
    // Month arithmetic clamps the day to the target month's length.
    eq("Date.new(2024, 1, 31).next_month.to_s", "\"2024-02-29\"");
    eq("(Date.new(2024, 1, 31) >> 1).to_s", "\"2024-02-29\"");
    eq("(Date.new(2024, 3, 31) << 1).to_s", "\"2024-02-29\"");
    eq("Date.new(2024, 2, 29).next_year.to_s", "\"2025-02-28\"");
    // Formatting and parsing.
    eq(
        "Date.new(2024, 7, 4).strftime(\"%A %B %-d, %Y\")",
        "\"Thursday July 4, 2024\"",
    );
    eq("Date.new(2024, 7, 4).to_s", "\"2024-07-04\"");
    eq("Date.parse(\"2024-07-04\").month", "7");
    eq("Date.parse(\"2024/12/25\").day", "25");
    // inspect uses the Julian Day Number form.
    eq(
        "Date.new(2024, 7, 4).inspect",
        "\"#<Date: 2024-07-04 ((2460496j,0s,0n),+0s,2299161j)>\"",
    );
    // Comparison and sort.
    eq("Date.new(2024, 1, 1) < Date.new(2024, 2, 1)", "true");
    eq("Date.new(2024, 1, 1) <=> Date.new(2024, 2, 1)", "-1");
    eq(
        "[Date.new(2024, 3, 1), Date.new(2024, 1, 1), Date.new(2024, 2, 1)].sort.map(&:to_s)",
        "[\"2024-01-01\", \"2024-02-01\", \"2024-03-01\"]",
    );
}

#[test]
fn float_constants_and_leading_dot_chains() {
    eq("Float::INFINITY", "Infinity");
    eq("5 < Float::INFINITY", "true");
    eq("Float::NAN.nan?", "true");
    // An infinite Float range is an endless range.
    eq(
        "(1..Float::INFINITY).lazy.map { |x| x * x }.first(4)",
        "[1, 4, 9, 16]",
    );
    // A newline before `.method` continues the chain.
    eq(
        "[1, 2, 3, 4, 5]\n  .map { |x| x * 2 }\n  .select { |x| x > 4 }\n  .reduce(0) { |a, b| a + b }",
        "24",
    );
    eq(
        "(1..)\n  .lazy\n  .select(&:even?)\n  .first(3)",
        "[2, 4, 6]",
    );
    // A range `..` on a new line is NOT a leading-dot chain.
    eq("x = 1\n2\n[x, 2]", "[1, 2]");
}

#[test]
fn safe_navigation() {
    // `&.` short-circuits to nil when the receiver is nil.
    eq("a = nil; a&.upcase", "nil");
    eq("b = \"hi\"; b&.upcase", "\"HI\"");
    // Chains stop at the first nil.
    eq("a = nil; a&.foo&.bar", "nil");
    eq("s = \"Hello\"; s&.downcase&.reverse", "\"olleh\"");
    // Works with arguments, blocks, and after an index.
    eq("x = [1, 2, 3]; x&.map { |n| n * 2 }", "[2, 4, 6]");
    eq("h = {x: {y: 5}}; h[:x]&.fetch(:y)", "5");
    // The receiver is evaluated once; the call is skipped entirely on nil.
    eq("n = 0; a = nil; a&.tap { n += 1 }; n", "0");
}

#[test]
fn no_panic_on_edge_inputs() {
    // These all used to panic (abort the process); they must degrade gracefully.
    // Multibyte string content near operators (was a lexer char-boundary panic).
    eq("\"café\".count(\"é\")", "1");
    eq("\"café\".index(\"é\")", "3");
    // Negative justify width returns the string unchanged (was a capacity overflow).
    eq("\"hi\".ljust(-3, \"x\")", "\"hi\"");
    eq("\"hi\".rjust(-3)", "\"hi\"");
    // merge with no args returns a copy; multiple hash args merge left-to-right.
    eq("{a: 1, b: 2}.merge", "{a: 1, b: 2}");
    eq("{a: 1}.merge({b: 2}, {c: 3})", "{a: 1, b: 2, c: 3}");
    // A required argument omitted raises ArgumentError instead of panicking.
    assert!(ev("10.gcd").is_err());
    assert!(ev("10.ceildiv").is_err());
    eq(
        "begin; 10.gcd; rescue ArgumentError; :caught; end",
        ":caught",
    );
}

#[test]
fn undefined_method_is_an_error() {
    assert!(ev("no_such_method_here(1)").is_err());
}

// --- Fanout completion: numeric / Math -------------------------------------

#[test]
fn srand_makes_rand_reproducible() {
    // Reseeding with the same value replays the sequence; srand returns the
    // previous seed (MRI semantics), and rand(n) stays in range.
    eq("srand(1); a = rand; srand(1); b = rand; a == b", "true");
    eq("srand(5); srand(10)", "5");
    eq("srand(42); r = rand(10); r >= 0 && r < 10", "true");
}

#[test]
fn integer_pow_with_modulus() {
    eq("3.pow(4, 5)", "1"); // 81 % 5
    eq("2.pow(10, 1000)", "24"); // 1024 % 1000
                                 // MRI raises RangeError for a negative exponent with a modulus (no inverse).
    eq(
        "begin; 3.pow(-1, 7); rescue RangeError; :raised; end",
        ":raised",
    );
    eq(
        "begin; 2.pow(-1, 4); rescue RangeError; :raised; end",
        ":raised",
    );
}

#[test]
fn clamp_with_open_ranges() {
    eq("15.clamp(..10)", "10");
    eq("1.clamp(3..)", "3");
    eq("5.clamp(3..10)", "5");
    eq("5.clamp(..10)", "5");
    eq("5.clamp(3..)", "5");
}

#[test]
fn math_gamma_and_erf_approximate() {
    // Approximations (Lanczos / Abramowitz-Stegun) don't match MRI's libm to
    // the last bit, so assert within tolerance rather than exact inspect form.
    eq("(Math.gamma(5) - 24.0).abs < 1e-6", "true");
    eq("(Math.gamma(10) - 362880.0).abs < 1e-3", "true");
    eq("(Math.erf(1.0) - 0.8427007929497148).abs < 1e-6", "true");
    eq("(Math.erfc(1.0) - 0.15729920705028516).abs < 1e-6", "true");
}

// --- Fanout completion: lexer ----------------------------------------------

#[test]
fn bare_percent_string_literal() {
    eq("%(hi)", "\"hi\"");
    eq("%{a}", "\"a\"");
    eq("%[a]", "\"a\"");
    eq("%<a>", "\"a\"");
    eq("x = %(a#{1 + 1}b); x", "\"a2b\"");
    // After a value, `%` stays the modulo operator.
    eq("10 %(3)", "1");
    eq("v = 10; v % 3", "1");
}

#[test]
fn end_marker_stops_the_program() {
    // Everything after a line of `__END__` is the DATA section and is not lexed.
    eq("1\n__END__\nboom(", "1");
}

// --- Fanout completion: parser (numeric-literal binding, modifier rescue) ---

#[test]
fn negative_literal_method_binding() {
    eq("-7.abs", "7"); // (-7).abs, not -(7.abs)
    eq("-2.abs", "2");
    eq("-7.5.abs", "7.5");
    eq("-3.5e2.abs", "350.0");
    eq("-0xff.abs", "255");
    eq("-2**2", "-4"); // ** binds tighter than the sign
    eq("-2.abs**2", "4"); // fuse, method, then power
    eq("-2**2.abs", "-4");
    eq("(-7).abs", "7");
    eq("1 - 2.abs", "-1"); // binary minus untouched
    eq("-7 / 2", "-4");
    eq("x = 5\n-x.abs", "-5"); // a variable stays -(x.abs)
}

#[test]
fn modifier_rescue_binds_inside_assignment() {
    eq("x = 1/0 rescue 99", "99"); // x = (1/0 rescue 99)
    eq("x = 1/0 rescue 99\nx", "99");
    eq("y = (1/0 rescue 5)\ny", "5"); // grouping-paren rescue parses
    eq("1/0 rescue 42", "42"); // statement-level still works
    eq("foo = 1 rescue 2\nfoo", "1"); // non-raising RHS unaffected
}

// --- Fanout completion: mixins (extend / prepend / class << self) ----------

#[test]
fn extend_adds_module_methods_as_class_methods() {
    eq(
        "module Greet; def hello; \"hi\"; end; end; \
         class C; extend Greet; end; C.hello",
        "\"hi\"",
    );
}

#[test]
fn prepend_overrides_and_super_reaches_class() {
    eq(
        "module Loud; def name; super.upcase; end; end; \
         class C; prepend Loud; def name; \"bob\"; end; end; C.new.name",
        "\"BOB\"",
    );
}

#[test]
fn prepend_and_include_super_chain() {
    eq(
        "module P; def who; \"P(\" + super + \")\"; end; end; \
         module M; def who; \"M\"; end; end; \
         class C; prepend P; include M; end; C.new.who",
        "\"P(M)\"",
    );
    eq(
        "module P; end; class C; prepend P; end; C.ancestors.map(&:to_s)",
        "[\"P\", \"C\", \"Object\", \"Kernel\", \"BasicObject\"]",
    );
}

#[test]
fn prepend_super_reaches_superclass_method() {
    eq(
        "module P; def val; \"P>\" + super; end; end; \
         class B; def val; \"B\"; end; end; \
         class C < B; prepend P; end; C.new.val",
        "\"P>B\"",
    );
}

#[test]
fn singleton_class_defs_are_class_methods() {
    eq(
        "class C; class << self; def build; \"b\"; end; def kind; \"k\"; end; end; end; \
         C.build + C.kind",
        "\"bk\"",
    );
    eq(
        "class C; def self.a; \"A\"; end; class << self; def b; \"B\"; end; end; end; \
         C.a + C.b",
        "\"AB\"",
    );
}

// --- Fanout completion: pattern matching (find pattern, **nil, alternation) -

#[test]
fn find_pattern_two_sided() {
    eq("case [1, 2, 3, 4]; in [*, x, *]; x; end", "1");
    eq(
        "case [1, 2, 3, 4, 5]; in [*a, 3, *b]; [a, 3, b]; end",
        "[[1, 2], 3, [4, 5]]",
    );
    eq("case [1, 2, 3, 4]; in [*, x, y, *]; [x, y]; end", "[1, 2]");
    eq("case [1, 2, 2, 3]; in [*, 2 => x, *]; x; end", "2");
    // Empty / too-short array does not match a find pattern.
    assert!(ev("case []; in [*, x, *]; x; end").is_err());
}

#[test]
fn hash_pattern_exact_key_enforcement() {
    eq("case {a: 1}; in {a:, **nil}; a; end", "1");
    eq(
        "case {a: 1, b: 2}; in {a:, **nil}; :m; else; :no; end",
        ":no",
    );
    eq("case {}; in {**nil}; :m; else; :no; end", ":m");
    eq("case {a: 1}; in {}; :m; else; :no; end", ":no");
    eq("case {}; in {}; :m; end", ":m");
}

#[test]
fn alternation_binding_and_capture_rejection() {
    // `=>` binds the whole alternation (looser than `|`).
    eq("case 5; in Integer | Float => n; n; end", "5");
    eq("case 2.5; in Integer | Float => n; n; end", "2.5");
    eq("case 5; in 1 | Integer => n; n; end", "5");
    // A bare `|` in a value pattern is alternation, not bitwise-or.
    eq("case 2; in 1 | 2; :m; end", ":m");
    // Chained `=>` binds the subject repeatedly.
    eq("case 5; in Integer => a => b; [a, b]; end", "[5, 5]");
    // Variable capture inside an alternation branch is rejected (MRI SyntaxError).
    assert!(ev("case 5; in a | Integer; :x; end").is_err());
}

// --- Fanout round 2: reserved-word keyword labels --------------------------

#[test]
fn reserved_word_keyword_labels() {
    // reserved-word keyword parameter with a default
    eq("def f(in: 5); :ok; end; f", ":ok");
    // reserved-word required keyword parameter, called with reserved-word kwarg
    eq("def opts(in:); :ok; end; opts(in: 9)", ":ok");
    // reserved-word kwarg value flows through **opts (value observable)
    eq(
        "def g(**o); o; end; g(class: 1, if: 2)",
        "{class: 1, if: 2}",
    );
    // reserved-word symbol hash keys
    eq("{if: 1, class: 2, end: 3}", "{if: 1, class: 2, end: 3}");
    // multiple reserved kwparams
    eq("def f(if: 1, unless: 2); :ok; end; f", ":ok");
    // no space before colon
    eq(r#"def f(class:"x"); :ok; end; f"#, ":ok");
    // GUARD: ternary is untouched (the `:` is not adjacent to a bare keyword).
    eq("x = 5; x > 3 ? :big : :small", ":big");
}

// --- Fanout round 2: AOP method-intercept weave ----------------------------

#[test]
fn aop_intercept_weave() {
    // before appends, after appends, around transforms the result; all fire.
    eq(
        r#"
        $log = []
        def audit(*a); $log << :before; end
        def note(r);   $log << :after;  end
        def double(*a); r = yield; r * 2; end
        def compute(x); x + 1; end
        intercept("compute", :before, :audit)
        intercept("compute", :after,  :note)
        intercept("compute", :around, :double)
        r = compute(10)
        [r, $log]
        "#,
        "[22, [:before, :after]]",
    );

    // No advice registered on an unmatched name: body runs untouched.
    eq(
        r#"
        def audit(*a); end
        def plain(x); x + 1; end
        intercept("compute", :before, :audit)
        plain(41)
        "#,
        "42",
    );
}

#[test]
fn aop_true_around_advice() {
    // around handler that yields: before/after side effects fire and the
    // original result passes through unchanged.
    eq(
        r#"
        $log = []
        def wrap(*a)
          $log << :in
          r = yield
          $log << :out
          r
        end
        def compute(x); x + 1; end
        intercept("compute", :around, :wrap)
        r = compute(10)
        [r, $log]
        "#,
        "[11, [:in, :out]]",
    );

    // around handler transforms the yielded result.
    eq(
        r#"
        def wrap(*a); r = yield; r * 3; end
        def compute(x); x + 1; end
        intercept("compute", :around, :wrap)
        compute(4)
        "#,
        "15",
    );

    // around handler receives the ORIGINAL call args.
    eq(
        r#"
        def wrap(x); r = yield; r + x; end
        def compute(y); y * 2; end
        intercept("compute", :around, :wrap)
        compute(5)
        "#,
        "15",
    );

    // around handler that does NOT yield: the original body never runs
    // (side-effect flag stays false) and the handler's own value is the result.
    eq(
        r#"
        $ran = false
        def wrap(*a); 99; end
        def compute(x); $ran = true; x + 1; end
        intercept("compute", :around, :wrap)
        r = compute(10)
        [r, $ran]
        "#,
        "[99, false]",
    );

    // before/after advice unchanged when no around is present: body runs, both fire.
    eq(
        r#"
        $log = []
        def audit(*a); $log << :before; end
        def note(r);   $log << :after;  end
        def compute(x); x + 1; end
        intercept("compute", :before, :audit)
        intercept("compute", :after,  :note)
        r = compute(10)
        [r, $log]
        "#,
        "[11, [:before, :after]]",
    );
}

// --- Fanout round 2: DateTime ----------------------------------------------

#[test]
fn datetime_calendar() {
    // Construction and field readers.
    eq(
        "d = DateTime.new(2020, 1, 1, 12, 30, 45); \
         [d.year, d.month, d.day, d.hour, d.min, d.sec, d.wday, d.yday, d.cwday, d.jd, d.leap?]",
        "[2020, 1, 1, 12, 30, 45, 3, 1, 3, 2458850, true]",
    );
    eq("DateTime.new(2020, 1, 1).class.to_s", "\"DateTime\"");
    eq("DateTime.new(2020, 1, 1).is_a?(Date)", "true");
    eq("DateTime.new(2020, 1, 1).is_a?(Comparable)", "true");
    // to_s / iso8601 / inspect (ISO8601, UTC offset +00:00).
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).to_s",
        "\"2020-01-01T12:30:45+00:00\"",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).iso8601",
        "\"2020-01-01T12:30:45+00:00\"",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).inspect",
        "\"#<DateTime: 2020-01-01T12:30:45+00:00 ((2458850j,45045s,0n),+0s,2299161j)>\"",
    );
    // strftime reuses the Date/Time directive engine.
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).strftime(\"%Y-%m-%dT%H:%M:%S\")",
        "\"2020-01-01T12:30:45\"",
    );
    // Constructors: default year, jd, ISO8601 parse.
    eq("DateTime.new.to_s", "\"-4712-01-01T00:00:00+00:00\"");
    eq("DateTime.jd(2458850).to_s", "\"2020-01-01T00:00:00+00:00\"");
    eq(
        "DateTime.parse(\"2020-01-01T12:30:00\").to_s",
        "\"2020-01-01T12:30:00+00:00\"",
    );
    // Arithmetic: ± day, month shift keep the time of day.
    eq(
        "(DateTime.new(2020, 1, 1, 12, 30, 45) + 1).to_s",
        "\"2020-01-02T12:30:45+00:00\"",
    );
    eq(
        "(DateTime.new(2020, 1, 1, 12, 30, 45) - 1).to_s",
        "\"2019-12-31T12:30:45+00:00\"",
    );
    eq(
        "(DateTime.new(2020, 1, 1, 12, 30, 45) >> 1).to_s",
        "\"2020-02-01T12:30:45+00:00\"",
    );
    eq(
        "(DateTime.new(2020, 1, 1, 12, 30, 45) << 2).to_s",
        "\"2019-11-01T12:30:45+00:00\"",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).next_day.to_s",
        "\"2020-01-02T12:30:45+00:00\"",
    );
    // DateTime - DateTime is a Rational day count (with sub-day precision).
    eq(
        "DateTime.new(2020, 1, 5, 0, 0, 0) - DateTime.new(2020, 1, 1, 12, 30, 45)",
        "(6679/1920)",
    );
    // Conversions.
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).to_date.to_s",
        "\"2020-01-01\"",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).to_date.class.to_s",
        "\"Date\"",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).to_time.to_i",
        "1577881845",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45).to_time.class.to_s",
        "\"Time\"",
    );
    // Comparison, equality, and sort.
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45) < DateTime.new(2020, 1, 5)",
        "true",
    );
    eq(
        "DateTime.new(2020, 1, 1, 12, 30, 45) <=> DateTime.new(2020, 1, 5)",
        "-1",
    );
    eq(
        "DateTime.new(2020, 1, 1) == DateTime.new(2020, 1, 1)",
        "true",
    );
    eq(
        "d = DateTime.new(2020, 1, 1, 12, 30, 45); \
         e = DateTime.new(2020, 1, 5, 0, 0, 0); \
         [d, e, DateTime.new(2019, 1, 1)].sort.map(&:to_s)",
        "[\"2019-01-01T00:00:00+00:00\", \"2020-01-01T12:30:45+00:00\", \"2020-01-05T00:00:00+00:00\"]",
    );
}

// --- Fanout round 2: case/in deconstruct protocol --------------------------

#[test]
fn case_in_deconstruct_protocol() {
    // Array pattern fires user `deconstruct` (exact arity).
    eq(
        "class P; def deconstruct; [1,2]; end; end; case P.new; in [a,b]; [a,b]; end",
        "[1, 2]",
    );
    // Arity mismatch (too few / too many, no splat) falls through to else.
    eq(
        "class P; def deconstruct; [1]; end; end; case P.new; in [a,b]; 1; else; :no; end",
        ":no",
    );
    eq(
        "class P; def deconstruct; [1,2,3]; end; end; case P.new; in [a,b]; 1; else; :no; end",
        ":no",
    );
    // Splat binds the tail slice.
    eq(
        "class P; def deconstruct; [1,2,3,4]; end; end; case P.new; in [a,*b]; b; end",
        "[2, 3, 4]",
    );
    // Nested: inner user object deconstructs inside an outer array.
    eq(
        "class Pt; def deconstruct; [1,2]; end; end; case [Pt.new, 9]; in [[a,b], c]; [a,b,c]; end",
        "[1, 2, 9]",
    );
    // Find pattern over a user object.
    eq(
        "class P; def deconstruct; [1,2,3,4,5]; end; end; case P.new; in [*pre, 3, *post]; [pre,post]; end",
        "[[1, 2], [4, 5]]",
    );
    // Hash pattern fires user `deconstruct_keys`.
    eq(
        "class P; def deconstruct_keys(k); {x:1,y:2}; end; end; case P.new; in {x:}; x; end",
        "1",
    );
    // Hash value subpattern against a deconstructed key.
    eq(
        "class P; def deconstruct_keys(k); {x:5}; end; end; case P.new; in {x: Integer => n}; n; else; :no; end",
        "5",
    );
    // `**rest` binds remaining keys from the deconstructed hash.
    eq(
        "class P; def deconstruct_keys(k); {x:1,y:2,z:3}; end; end; case P.new; in {x:, **rest}; [x, rest]; end",
        "[1, {y: 2, z: 3}]",
    );
    // GUARD: plain arrays/hashes still match; non-array/hash receivers fall through.
    eq("case [1,2]; in [a,b]; [a,b]; end", "[1, 2]");
    eq("case({a:1}); in {a:}; a; end", "1");
    eq("case 5; in [a,b]; 1; else; :no; end", ":no");
    eq("case 5; in {a:}; 1; else; :no; end", ":no");
}

// --- Fanout round 3: Enumerator.new generators + cycle ---------------------

#[test]
fn enumerator_new_block_generators() {
    // Finite generator: to_a collects every yielded value.
    eq(
        "Enumerator.new { |y| y << 1; y << 2; y << 3 }.to_a",
        "[1, 2, 3]",
    );
    // `Yielder#yield` is an alias of `<<` (paren-less and parenthesized forms
    // both parse — see `parenless_command_call_on_dot_and_const_receiver`).
    eq(
        "Enumerator.new { |y| y.yield(1); y.yield(2) }.to_a",
        "[1, 2]",
    );
    // first(n) with an explicit count returns an array.
    eq(
        "Enumerator.new { |y| y << 10; y << 20; y << 30 }.first(2)",
        "[10, 20]",
    );
    // Bare next on a finite generator (materialize + cursor).
    eq("Enumerator.new { |y| y << 1 }.next", "1");
    // Infinite fib generator bounded by first(n) via the loop{} early-stop.
    eq(
        "fib = Enumerator.new { |y| a,b=0,1; loop { y << a; a,b = b,a+b } }; fib.first(8)",
        "[0, 1, 1, 2, 3, 5, 8, 13]",
    );
    // Non-terminal Enumerable method over a finite generator delegates to Array.
    eq(
        "Enumerator.new { |y| y << 1; y << 2 }.map { |x| x * 10 }",
        "[10, 20]",
    );
}

#[test]
fn enumerator_new_infinite_lazy() {
    // Infinite generator + lazy pipeline, bounded by first(n).
    eq(
        "g = Enumerator.new { |y| a=0; loop { y << a; a+=1 } }; g.lazy.map { |x| x*2 }.first(3)",
        "[0, 2, 4]",
    );
}

#[test]
fn array_cycle_count() {
    eq("[1, 2].cycle(3).to_a", "[1, 2, 1, 2, 1, 2]");
    eq("[1, 2].cycle(0).to_a", "[]");
    eq("[].cycle(3).to_a", "[]");
    // Block form (already worked) — regression guard.
    eq("s = 0; [1, 2].cycle(2) { |x| s += x }; s", "6");
}

#[test]
fn enumerator_block_less_still_works() {
    // Guard: block-less each/lazy on real collections is unaffected.
    eq("[1, 2, 3].each.to_a", "[1, 2, 3]");
    eq(
        "(1..Float::INFINITY).lazy.map { |x| x * x }.first(3)",
        "[1, 4, 9]",
    );
    eq("[10, 20, 30].each.next", "10");
}

// --- Fanout round 4: Struct patterns, reflection, symbol sort --------------

#[test]
fn struct_deconstruct_patterns() {
    // Struct instances honour the deconstruction protocol in patterns.
    eq(
        "S = Struct.new(:a, :b); case S.new(1, 2); in [x, y]; [x, y]; end",
        "[1, 2]",
    );
    eq(
        "S = Struct.new(:a, :b); case S.new(1, 2); in {a:, b:}; [a, b]; end",
        "[1, 2]",
    );
    // A hash-pattern subset requests only some keys.
    eq(
        "S = Struct.new(:a, :b); case S.new(1, 2); in {a:}; a; end",
        "1",
    );
    // The class/find pattern deconstructs positionally.
    eq(
        "S = Struct.new(:a, :b); case S.new(1, 2); in S[x, y]; [x, y]; end",
        "[1, 2]",
    );
    // deconstruct_keys filters to the requested keys (in request order); nil = all.
    eq(
        "S = Struct.new(:a, :b); S.new(1, 2).deconstruct_keys([:a])",
        "{a: 1}",
    );
    eq(
        "S = Struct.new(:a, :b); S.new(1, 2).deconstruct_keys([:b, :a])",
        "{b: 2, a: 1}",
    );
    eq(
        "S = Struct.new(:a, :b); S.new(1, 2).deconstruct_keys(nil)",
        "{a: 1, b: 2}",
    );
    // Guard: plain Array/Hash and user-object deconstruct patterns still work.
    eq("case [1, 2]; in [x, y]; [x, y]; end", "[1, 2]");
    eq(
        "class C; def deconstruct; [7, 8]; end; end; case C.new; in [a, b]; [a, b]; end",
        "[7, 8]",
    );
    eq("case 5; in [a, b]; :bad; else; :ok; end", ":ok");
}

#[test]
fn instance_methods_reflection() {
    eq(
        "class Foo; def a; end; def b; end; end; Foo.instance_methods(false).sort",
        "[:a, :b]",
    );
    // inherited=true walks superclass + included module (user-defined portion;
    // rubylang does not enumerate builtin Kernel methods).
    eq(
        "module M; def mm; end; end; \
         class Base; def bb; end; end; \
         class Foo < Base; include M; def a; end; def b; end; end; \
         Foo.instance_methods(true).sort",
        "[:a, :b, :bb, :mm]",
    );
    // no-arg defaults to inherited=true
    eq(
        "module M; def mm; end; end; \
         class Base; def bb; end; end; \
         class Foo < Base; include M; def a; end; def b; end; end; \
         Foo.instance_methods.sort",
        "[:a, :b, :bb, :mm]",
    );
    // attr_accessor generated methods appear as own methods
    eq(
        "class Foo; attr_accessor :x; end; Foo.instance_methods(false).sort",
        "[:x, :x=]",
    );
    // instance-side Object#methods
    eq(
        "module M; def mm; end; end; \
         class Base; def bb; end; end; \
         class Foo < Base; include M; def a; end; def b; end; end; \
         Foo.new.methods.sort",
        "[:a, :b, :bb, :mm]",
    );
    // synthetic __class_body__ never leaks into instance_methods
    eq(
        "class C; X = 1; def a; end; end; C.instance_methods(false)",
        "[:a]",
    );
}

#[test]
fn method_defined_predicate() {
    eq(
        "module M; def mm; end; end; \
         class Base; def bb; end; end; \
         class Foo < Base; include M; def a; end; end; \
         [Foo.method_defined?(:a), Foo.method_defined?(:bb), \
          Foo.method_defined?(:mm), Foo.method_defined?(:nope)]",
        "[true, true, true, false]",
    );
    eq(
        "class Foo; def a; end; end; Foo.method_defined?(\"a\")",
        "true",
    );
}

#[test]
fn symbols_sort_by_name() {
    eq("[:mm, :bb, :a].sort", "[:a, :bb, :mm]");
    eq("%i[x y a].sort", "[:a, :x, :y]");
    eq("[:c, :a, :b].min", ":a");
    eq("[:c, :a, :b].max", ":c");
}

// --- Fanout round 4: JSON module -------------------------------------------

#[test]
fn json_module() {
    // --- generate (compact) ---
    eq(
        "require \"json\"; JSON.generate({\"a\" => 1, \"b\" => [1,2]})",
        "\"{\\\"a\\\":1,\\\"b\\\":[1,2]}\"",
    );
    eq(
        "require \"json\"; JSON.generate([1,\"x\",true,nil])",
        "\"[1,\\\"x\\\",true,null]\"",
    );
    eq("require \"json\"; JSON.generate(1.5)", "\"1.5\"");
    eq("require \"json\"; JSON.generate(2)", "\"2\"");
    eq(
        "require \"json\"; JSON.generate({a: 1, b: {c: [1,2]}})",
        "\"{\\\"a\\\":1,\\\"b\\\":{\\\"c\\\":[1,2]}}\"",
    );
    // string escaping: " \ \n \t
    eq(
        "require \"json\"; JSON.generate(\"he\\\"llo\\n\\tx\\\\y\")",
        "\"\\\"he\\\\\\\"llo\\\\n\\\\tx\\\\\\\\y\\\"\"",
    );
    eq("require \"json\"; JSON.generate(nil)", "\"null\"");
    eq("require \"json\"; JSON.generate(true)", "\"true\"");
    eq("require \"json\"; JSON.generate({})", "\"{}\"");
    eq("require \"json\"; JSON.generate([])", "\"[]\"");

    // --- to_json (generic receiver + symbol-key hash) ---
    eq(
        "require \"json\"; {a: 1, b: 2}.to_json",
        "\"{\\\"a\\\":1,\\\"b\\\":2}\"",
    );
    eq(
        "require \"json\"; [1,\"x\",true,nil].to_json",
        "\"[1,\\\"x\\\",true,null]\"",
    );
    eq("require \"json\"; 1.5.to_json", "\"1.5\"");
    eq("require \"json\"; 2.to_json", "\"2\"");
    eq("require \"json\"; :sym.to_json", "\"\\\"sym\\\"\"");
    eq("require \"json\"; nil.to_json", "\"null\"");
    eq("require \"json\"; true.to_json", "\"true\"");
    eq("require \"json\"; false.to_json", "\"false\"");

    // --- parse (string keys, scalars, nested) ---
    eq(
        "require \"json\"; JSON.parse('{\"a\":1,\"b\":[2,3]}')",
        "{\"a\" => 1, \"b\" => [2, 3]}",
    );
    eq(
        "require \"json\"; JSON.parse('{\"a\":1}', symbolize_names: true)",
        "{a: 1}",
    );
    eq("require \"json\"; JSON.parse('[1,2,3]')", "[1, 2, 3]");
    eq("require \"json\"; JSON.parse('42')", "42");
    eq("require \"json\"; JSON.parse('-3.14e2')", "-314.0");
    eq("require \"json\"; JSON.parse('\"hi\\n\"')", "\"hi\\n\"");
    eq("require \"json\"; JSON.parse('true')", "true");
    eq("require \"json\"; JSON.parse('null')", "nil");
    eq(
        "require \"json\"; JSON.parse('{\"a\":{\"b\":[1,{\"c\":2}]}}')",
        "{\"a\" => {\"b\" => [1, {\"c\" => 2}]}}",
    );
    eq(
        "require \"json\"; JSON.parse('{\"a\":{\"b\":2}}', symbolize_names: true)",
        "{a: {b: 2}}",
    );

    // --- round-trip ---
    eq(
        "require \"json\"; JSON.parse(JSON.generate({\"x\" => [1, {\"y\" => 2}]}))",
        "{\"x\" => [1, {\"y\" => 2}]}",
    );

    // --- malformed input raises JSON::ParserError ---
    eq(
        "require \"json\"; begin; JSON.parse('[1,2'); rescue => e; e.class.name; end",
        "\"JSON::ParserError\"",
    );
}

// --- Fanout round 5: Fiber (stackful coroutines) ---------------------------

#[test]
fn fiber_resume_and_yield() {
    // Finite resume sequence: two yields then a return.
    eq(
        "f = Fiber.new { Fiber.yield(1); Fiber.yield(2); 3 }; [f.resume, f.resume, f.resume]",
        "[1, 2, 3]",
    );
    // resume(v) supplies Fiber.yield's return; the first resume is the block param.
    eq(
        "f = Fiber.new { |a| b = Fiber.yield(a + 1); Fiber.yield(b + 1); :done }; \
         [f.resume(10), f.resume(20), f.resume(30)]",
        "[11, 21, :done]",
    );
    // alive? transitions to false once the block returns.
    eq(
        "f = Fiber.new { Fiber.yield(nil) }\n\
         before = f.alive?\n\
         f.resume\n\
         f.resume\n\
         [before, f.alive?]",
        "[true, false]",
    );
    // A Fibonacci generator over an infinite loop, driven by resume.
    eq(
        "fib = Fiber.new { a, b = 0, 1; loop { Fiber.yield(a); a, b = b, a + b } }; \
         8.times.map { fib.resume }",
        "[0, 1, 1, 2, 3, 5, 8, 13]",
    );
    // A fiber captures its defining scope.
    eq(
        "x = 100; f = Fiber.new { Fiber.yield(x + 1); x + 2 }; [f.resume, f.resume]",
        "[101, 102]",
    );
    // Nested fibers resume each other; each keeps its own execution state.
    eq(
        "inner = Fiber.new { Fiber.yield(:i1); :i_done }; \
         outer = Fiber.new { Fiber.yield(inner.resume); Fiber.yield(inner.resume); :o_done }; \
         [outer.resume, outer.resume, outer.resume]",
        "[:i1, :i_done, :o_done]",
    );
}

#[test]
fn fiber_error_conditions() {
    // Resuming a dead fiber raises FiberError.
    eq(
        "f = Fiber.new { 42 }; f.resume; begin; f.resume; rescue FiberError; :raised; end",
        ":raised",
    );
    // Fiber.yield at the root (no running fiber) raises FiberError.
    eq(
        "begin; Fiber.yield(1); rescue FiberError; :raised; end",
        ":raised",
    );
}

// --- Fanout round 5: surface-completeness fixes ----------------------------

#[test]
fn break_value_short_circuits_iterators() {
    // `break value` inside a block becomes the iterator's result.
    eq("[1, 2, 3, 4].map { |x| break x if x == 3; x }", "3");
    eq(
        "[1, 2, 3, 4].select { |x| break :stop if x == 3; x.odd? }",
        ":stop",
    );
    eq(
        "[1, 2, 3, 4].filter_map { |x| break 99 if x == 3; x if x.even? }",
        "99",
    );
    eq(
        "[1, 2, 3, 4].inject { |a, b| break b if b == 3; a + b }",
        "3",
    );
    eq("loop { break 42 }", "42");
    // Bare `break` before a closing delimiter now parses.
    eq("[1, 2].each { break }", "nil");
    eq("loop { break }", "nil");
    // `Kernel#loop` silently rescues StopIteration (external iterator exhausted).
    eq("e = [1, 2].each; s = 0; loop { s += e.next }; s", "3");
}

#[test]
fn object_id_identity() {
    eq("5.object_id", "11");
    eq("nil.object_id", "4");
    eq("true.object_id", "20");
    eq("s = \"x\"; s.object_id == s.object_id", "true");
    eq("\"a\".object_id == \"a\".object_id", "false");
    eq("\"x\".object_id.class", "Integer");
}

#[test]
fn hash_merge_and_transform_keys_variants() {
    // merge with a conflict-resolution block.
    eq(
        "{a: 1, b: 2}.merge({b: 3, c: 4}) { |k, o, n| o + n }",
        "{a: 1, b: 5, c: 4}",
    );
    // transform_keys with a mapping hash, and hash + block.
    eq("{a: 1, b: 2}.transform_keys({a: :x})", "{x: 1, b: 2}");
    eq(
        "{a: 1, b: 2, c: 3}.transform_keys({a: :x}) { |k| k.upcase }",
        "{x: 1, B: 2, C: 3}",
    );
}

#[test]
fn array_to_h_block_and_lazy_zip() {
    eq(
        "[1, 2, 3].to_h { |x| [x, x * x] }",
        "{1 => 1, 2 => 4, 3 => 9}",
    );
    eq(
        "[1, 2, 3].lazy.zip([4, 5, 6]).to_a",
        "[[1, 4], [2, 5], [3, 6]]",
    );
    eq(
        "[1, 2, 3].lazy.zip([4, 5], [7, 8, 9]).to_a",
        "[[1, 4, 7], [2, 5, 8], [3, nil, 9]]",
    );
    eq(
        "(1..Float::INFINITY).lazy.zip([9, 8, 7]).first(2)",
        "[[1, 9], [2, 8]]",
    );
}

#[test]
fn parenless_command_call_on_dot_and_const_receiver() {
    // Paren-less args on a dot receiver: single, multiple, kwargs, splat.
    eq("3.between? 1, 5", "true");
    eq("[1, 2, 3].include? 2", "true");
    eq(r#""hi".sub "i", "o""#, r#""ho""#);
    eq(r#""abc".slice 1, 2"#, r#""bc""#);
    eq("[1, 2].push 3", "[1, 2, 3]");
    eq("10.gcd 4", "2");
    // Operator method name with a paren-less arg.
    eq("2.+ 3", "5");
    // Trailing keyword args collect into an implicit hash.
    eq("{}.merge a: 1", "{a: 1}");
    // `*splat` as a paren-less command arg.
    eq("[0].concat *[[1, 2]]", "[0, 1, 2]");
    // Chained: the arg is itself a dot chain (`a.b c.d`).
    eq(r#""a".concat "b".upcase"#, r#""aB""#);
    eq("[1, 2].push [3].first", "[1, 2, 3]");
    // A method defined on a class, invoked paren-less on an instance.
    eq("class C; def go(x); x * 2; end; end; C.new.go 21", "42");
    // Const receiver + paren-less command (`Const.meth arg`).
    eq("Math.sqrt 16", "4.0");
    // Safe navigation carries paren-less args too.
    eq(r#""hi"&.sub "h", "b""#, r#""bi""#);
    // Enumerator::Yielder#yield and Fiber.yield, paren-less (previously
    // required parentheses — see BUGS.md).
    eq("Enumerator.new { |y| y.yield 1; y.yield 2 }.to_a", "[1, 2]");
    eq("Fiber.new { |x| Fiber.yield x + 1 }.resume 5", "6");
}

#[test]
fn parenless_dot_call_guards_no_regression() {
    // No args, then a terminator — a plain method call, no slurped argument.
    eq("[9, 8].first", "9");
    // `[` with no leading space is indexing, not `first([0])`.
    eq("[[1], [2]].first[0]", "1");
    // A tight/spaced binary `-` after a dot call stays binary.
    eq("1 - 2.abs", "-1");
    eq("5.abs - 3", "2");
    // Parenthesized calls still parse (no space before `(`).
    eq("{}.merge(a: 1)", "{a: 1}");
    eq("5.between?(1, 9)", "true");
    eq("[1].push(2)", "[1, 2]");
    // `= x` after a dot call is a setter, not a command arg.
    eq(
        "class C; attr_accessor :x; end; c = C.new; c.x = 5; c.x",
        "5",
    );
    // Blocks bind to the dot call, not consumed as args.
    eq("3.tap { |n| }", "3");
    eq("[1, 2].map { |x| x * 2 }.first", "2");
    eq("[3, 1, 2].sort { |a, b| a <=> b }.last", "3");
    // Method chains with no args.
    eq(r#""abc".upcase.reverse"#, r#""CBA""#);
    // A dot call followed by a low-precedence operator, not an argument.
    eq("5.between?(1, 9) && true", "true");
}

// ---- metaprogramming: singleton methods, hooks, reflection, eval ----------

#[test]
fn singleton_method_on_object() {
    // `def obj.foo` defines a per-object singleton method.
    eq("o = Object.new; def o.foo; 7; end; o.foo", "7");
    // The singleton is bound to that object only; a sibling does not get it.
    eq(
        "o = Object.new; def o.foo; 7; end; p2 = Object.new; p2.respond_to?(:foo)",
        "false",
    );
    // `@ivar` inside a singleton method reads the receiver's own state.
    eq(
        "o = Object.new; o.instance_variable_set(:@v, 5); def o.d; @v * 2; end; o.d",
        "10",
    );
}

#[test]
fn singleton_method_on_class_is_a_class_method() {
    // `def Klass.bar` registers a class method.
    eq("class Klass; end; def Klass.bar; 8; end; Klass.bar", "8");
}

#[test]
fn singleton_class_body_on_object() {
    // `class << obj; def m; end; end` defines singleton methods on the object.
    eq(
        "o = Object.new; class << o; def baz; 9; end; end; o.baz",
        "9",
    );
}

#[test]
fn const_missing_is_called_for_unresolved_scoped_const() {
    eq(
        r#"class C; def self.const_missing(name); "missing:#{name}"; end; end; C::Nope"#,
        r#""missing:Nope""#,
    );
}

#[test]
fn inherited_hook_fires_with_the_subclass() {
    eq(
        "$log = nil; class Base; def self.inherited(sub); $log = sub.name; end; end; class Sub < Base; end; $log",
        r#""Sub""#,
    );
}

#[test]
fn included_hook_fires_with_the_including_class() {
    eq(
        "$log = nil; module M; def self.included(base); $log = base.name; end; end; class Host; include M; end; $log",
        r#""Host""#,
    );
}

#[test]
fn extended_and_prepended_hooks_fire() {
    eq(
        "$log = nil; module E; def self.extended(base); $log = base.name; end; end; class HE; extend E; end; $log",
        r#""HE""#,
    );
    eq(
        "$log = nil; module P; def self.prepended(base); $log = base.name; end; end; class HP; prepend P; end; $log",
        r#""HP""#,
    );
}

#[test]
fn constant_reflection() {
    eq("Object.const_get(:String)", "String");
    eq("X = 5; Object.const_get(:X)", "5");
    eq("Object.const_set(:Y, 42); Y", "42");
    eq("Object.const_defined?(:String)", "true");
    eq("Object.const_defined?(:NoSuchConst)", "false");
    // A nested path resolves against the flat constant store by its last segment.
    eq("module A; B = 99; end; A.const_get(\"A::B\")", "99");
    eq(
        "Object.const_set(:Zeta, 1); Object.constants.include?(:Zeta)",
        "true",
    );
}

#[test]
fn top_level_self_is_an_object_named_main() {
    // MRI's top-level `self` is `main`, an Object — not nil.
    eq("self.class.name", r#""Object""#);
    eq("self.is_a?(Object)", "true");
    eq("self.nil?", "false");
}

#[test]
fn eval_runs_a_string_on_the_current_host() {
    eq("eval(\"1 + 2\")", "3");
    // A method defined inside eval persists and is callable afterward.
    eq("eval(\"def em; 111; end\"); em", "111");
    // A constant defined inside eval persists.
    eq("eval(\"K = 21\"); K * 2", "42");
}

#[test]
fn class_eval_block_defines_instance_methods() {
    eq(
        "class CE; end; CE.class_eval { def hi; 42; end }; CE.new.hi",
        "42",
    );
}

#[test]
fn instance_eval_block_sees_receiver_state_and_defines_singletons() {
    // `@ivar` in an instance_eval block hits the receiver.
    eq(
        "o = Object.new; o.instance_variable_set(:@x, 3); o.instance_eval { @x }",
        "3",
    );
    // A bare `def` in an instance_eval block defines a singleton on the receiver.
    eq(
        "o = Object.new; o.instance_eval { def s; 9; end }; o.s",
        "9",
    );
}

#[test]
fn instance_exec_passes_arguments() {
    eq(
        "o = Object.new; o.instance_variable_set(:@x, 4); o.instance_exec(3) { |n| n * @x }",
        "12",
    );
}

#[test]
fn instance_exec_rebinds_self_for_dispatch_ivars_and_args() {
    // A block run under `instance_exec` rebinds `self` to the receiver: bare-name
    // calls dispatch on it, `@ivar` reads its state, `self` is it, and explicit
    // args still bind. Verified against MRI (ruby 4.0.6): 99 / 7 / Foo / 12.
    let cls = "class Foo; def initialize; @n = 7; end; def bar; 99; end; end; f = Foo.new; ";
    eq(&format!("{cls}f.instance_exec {{ bar }}"), "99");
    eq(&format!("{cls}f.instance_exec {{ @n }}"), "7");
    eq(&format!("{cls}f.instance_exec {{ self }}.class"), "Foo");
    eq(&format!("{cls}f.instance_exec(5) {{ |x| x + @n }}"), "12");
}

#[test]
fn instance_eval_block_dispatches_to_receiver_singleton() {
    // Under `instance_eval`, a bare-name call resolves the receiver's singleton
    // method (not the lexical self), and `self` is the receiver. MRI: 42 / D.
    eq(
        "class D; end; o = D.new; def o.m; 42; end; o.instance_eval { m }",
        "42",
    );
    eq(
        "class D2; end; o = D2.new; o.instance_eval { self }.class",
        "D2",
    );
    // An ivar write under instance_eval still lands on the receiver (regression
    // guard for the previously-working write path).
    eq(
        "o = Object.new; o.instance_eval { @v = 3 }; o.instance_variable_get(:@v)",
        "3",
    );
}

#[test]
fn define_method_on_class_receiver_rebinds_self() {
    // `Klass.define_method(:m) { ... }` (explicit class receiver) registers an
    // instance method whose block rebinds `self` to the invoking instance: it
    // reads that instance's `@ivar` and dispatches its other methods. MRI:
    // "hi" / 1.
    eq(
        "class C; def helper; 1; end; end; C.define_method(:r) { @name }; \
         c = C.new; c.instance_variable_set(:@name, \"hi\"); c.r",
        "\"hi\"",
    );
    eq(
        "class C2; def helper; 1; end; end; C2.define_method(:g) { helper }; C2.new.g",
        "1",
    );
}

#[test]
fn ordinary_iteration_block_keeps_lexical_self_unchanged() {
    // Regression: a plain `each` block is NOT under instance_exec/eval/
    // define_method, so its `self` stays the lexically enclosing method's self —
    // `@ivar` and bare calls resolve against that, and the accumulation works.
    // MRI: 6 / [11, 12] / 30.
    eq(
        "class E; def initialize; @base = 0; end; def go; s = @base; \
         [1,2,3].each { |x| s += x }; s; end; end; E.new.go",
        "6",
    );
    eq(
        "class F; def bump(x); x + 10; end; def run; a = []; \
         [1,2].each { |x| a << bump(x) }; a; end; end; F.new.run",
        "[11, 12]",
    );
    eq("t = 0; [10, 20].each { |x| t += x }; t", "30");
}

// ---- stdlib modules: Digest / Base64 / SecureRandom / OpenStruct ----------

#[test]
fn digest_hexdigest_matches_reference() {
    // Deterministic vectors verified byte-for-byte against MRI `ruby`.
    eq(
        "Digest::MD5.hexdigest(\"abc\")",
        "\"900150983cd24fb0d6963f7d28e17f72\"",
    );
    eq(
        "Digest::MD5.hexdigest(\"\")",
        "\"d41d8cd98f00b204e9800998ecf8427e\"",
    );
    eq(
        "Digest::SHA1.hexdigest(\"abc\")",
        "\"a9993e364706816aba3e25717850c26c9cd0d89d\"",
    );
    eq(
        "Digest::SHA1.hexdigest(\"\")",
        "\"da39a3ee5e6b4b0d3255bfef95601890afd80709\"",
    );
    eq(
        "Digest::SHA256.hexdigest(\"abc\")",
        "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"",
    );
    eq(
        "Digest::SHA256.hexdigest(\"The quick brown fox jumps over the lazy dog\")",
        "\"d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592\"",
    );
    eq(
        "Digest::SHA256.base64digest(\"abc\")",
        "\"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=\"",
    );
}

#[test]
fn base64_encode_decode_matches_reference() {
    eq("Base64.encode64(\"hello\")", "\"aGVsbG8=\\n\"");
    eq("Base64.encode64(\"\")", "\"\"");
    eq(
        "Base64.strict_encode64(\"hello world\")",
        "\"aGVsbG8gd29ybGQ=\"",
    );
    // Line-wrap at 60 output chars with a trailing newline.
    eq(
        "Base64.encode64(\"a\" * 50)",
        "\"YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFh\\nYWFhYWE=\\n\"",
    );
    eq(
        "Base64.urlsafe_encode64(\"subjects?_d\")",
        "\"c3ViamVjdHM_X2Q=\"",
    );
    eq(
        "Base64.urlsafe_encode64(\"subjects?_d\", padding: false)",
        "\"c3ViamVjdHM_X2Q\"",
    );
    eq("Base64.decode64(\"aGVsbG8=\\n\")", "\"hello\"");
    eq("Base64.strict_decode64(\"aGVsbG8=\")", "\"hello\"");
    eq(
        "Base64.urlsafe_decode64(\"c3ViamVjdHM_X2Q=\")",
        "\"subjects?_d\"",
    );
    // Round-trip.
    eq(
        "Base64.strict_decode64(Base64.strict_encode64(\"round trip!\"))",
        "\"round trip!\"",
    );
}

#[test]
fn securerandom_shapes() {
    // Random output can't match MRI byte-for-byte; assert length/format instead.
    eq("SecureRandom.hex(8).length == 16", "true");
    eq("SecureRandom.hex.length == 32", "true");
    eq(
        "SecureRandom.hex(8) =~ /\\A[0-9a-f]{16}\\z/ ? true : false",
        "true",
    );
    eq(
        "SecureRandom.uuid =~ /\\A[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\\z/ ? true : false",
        "true",
    );
    eq("SecureRandom.alphanumeric(20).length == 20", "true");
    eq(
        "SecureRandom.alphanumeric(20) =~ /\\A[0-9A-Za-z]{20}\\z/ ? true : false",
        "true",
    );
    eq("SecureRandom.base64(6).length == 8", "true");
    eq("SecureRandom.random_number(100).between?(0, 99)", "true");
    eq("SecureRandom.random_number.class == Float", "true");
    eq(
        "n = SecureRandom.random_number(5.0); n >= 0.0 && n < 5.0",
        "true",
    );
}

#[test]
fn openstruct_dynamic_attributes() {
    eq("OpenStruct.new(a: 1, b: 2).a", "1");
    eq("os = OpenStruct.new(a: 1); os.a = 9; os.a", "9");
    // A new attribute is created on assignment; unknown readers return nil.
    eq("os = OpenStruct.new; os.x = 5; os.x", "5");
    eq("OpenStruct.new(a: 1).z", "nil");
    eq("OpenStruct.new(a: 1, b: 2).to_h", "{a: 1, b: 2}");
    eq("os = OpenStruct.new(a: 1, b: 2); os[:b]", "2");
    eq("os = OpenStruct.new; os[:k] = 7; os.k", "7");
    eq("OpenStruct.new(a: 1, b: 2).members", "[:a, :b]");
    eq("OpenStruct.new(a: 1).respond_to?(:a)", "true");
    eq("OpenStruct.new(a: 1).respond_to?(:z)", "false");
    // A writer responds only for an already-set attribute (MRI semantics).
    eq("OpenStruct.new(a: 1).respond_to?(\"a=\")", "true");
    eq("OpenStruct.new(a: 1).respond_to?(\"x=\")", "false");
    eq(
        "OpenStruct.new(a: 1, b: 2).inspect",
        "\"#<OpenStruct a=1, b=2>\"",
    );
    eq("OpenStruct.new.inspect", "\"#<OpenStruct>\"");
    // Attribute-wise equality, order-independent, including inside collections.
    eq(
        "OpenStruct.new(a: 1, b: 2) == OpenStruct.new(a: 1, b: 2)",
        "true",
    );
    eq("OpenStruct.new(a: 1) == OpenStruct.new(a: 2)", "false");
    eq("[OpenStruct.new(a: 1)] == [OpenStruct.new(a: 1)]", "true");
    // Nested dig.
    eq("OpenStruct.new(a: OpenStruct.new(b: 5)).dig(:a, :b)", "5");
}

/// ERB templating: `ERB.new(...).result` / `#result_with_hash`, tag syntax
/// (`<%= %>`, `<% %>`, `<%# %>`), `<%%` escaping, and the `"-"` trim mode. Every
/// expected value was confirmed byte-for-byte against MRI (`ruby -rerb`).
#[test]
fn erb_templating() {
    // `<%= expr %>` evaluates and inserts; `result` returns the buffer String.
    eq(r#"require "erb"; ERB.new("<%= 1+1 %>").result"#, "\"2\"");
    // Literal text passthrough around an interpolation tag.
    eq(
        r#"require "erb"; ERB.new("Hello, <%= 2*3 %>!").result"#,
        "\"Hello, 6!\"",
    );
    // `<% code %>` drives a loop without inserting; `<%= %>` inserts each pass.
    eq(
        r#"require "erb"; ERB.new("<% 3.times do |i| %>row<%= i %> <% end %>").result"#,
        "\"row0 row1 row2 \"",
    );
    // `<%# comment %>` is dropped entirely.
    eq(
        r#"require "erb"; ERB.new("a<%# note %>b").result"#,
        "\"ab\"",
    );
    // `<%%` is an escaped literal `<%` (the trailing `%>` stays literal text).
    eq(
        r#"require "erb"; ERB.new("<%% literal %>").result"#,
        "\"<% literal %>\"",
    );
    // `result_with_hash` binds hash keys as template locals.
    eq(
        r#"require "erb"; ERB.new("Hi <%= name %>").result_with_hash(name: "Ann")"#,
        "\"Hi Ann\"",
    );
    eq(
        r#"require "erb"; ERB.new("<%= a %>-<%= b %>").result_with_hash(a: 10, b: 20)"#,
        "\"10-20\"",
    );
    // `result` sees the caller's instance variables (evaluated in current scope).
    eq(
        r#"require "erb"; @x = 42; ERB.new("x=<%= @x %>").result"#,
        "\"x=42\"",
    );
    // `result` sees caller-defined methods too.
    eq(
        r#"require "erb"; def greet(n); "Hi #{n}"; end; ERB.new("<%= greet('Bo') %>").result"#,
        "\"Hi Bo\"",
    );
    // Template text keeps MRI's `#{...}` interpolation (embedded in a Ruby string).
    eq(
        r#"require "erb"; ERB.new("a #{1+1} b").result"#,
        "\"a 2 b\"",
    );
    // `"-"` trim mode: `-%>` chomps the trailing newline, so only the `L<%= i %>`
    // lines survive — output is "L1\nL2\n".
    eq(
        r#"require "erb"; ERB.new("<% [1,2].each do |i| -%>\nL<%= i %>\n<% end -%>\n", trim_mode: "-").result"#,
        "\"L1\\nL2\\n\"",
    );
    // Without trim mode, the newline after `%>` is preserved.
    eq(
        r#"require "erb"; ERB.new("<% x=1 %>\nB").result"#,
        "\"\\nB\"",
    );
    // A full HTML fragment: a loop over an array rendered into list items.
    eq(
        r#"require "erb"; ERB.new("<ul>\n<% %w[a b].each do |it| -%>\n<li><%= it %></li>\n<% end -%>\n</ul>", trim_mode: "-").result"#,
        "\"<ul>\\n<li>a</li>\\n<li>b</li>\\n</ul>\"",
    );
    // `#src` exposes the generated buffer-building Ruby source.
    eq(
        r#"require "erb"; ERB.new("hi").src.include?("_erbout")"#,
        "true",
    );
}

#[test]
fn splat_only_params_parse_and_collect() {
    // Anonymous and named splat-only params on lambdas, blocks, and methods.
    eq("->(*a) { a }.call(1, 2)", "[1, 2]");
    eq("->(*) { 42 }.call(1, 2)", "42");
    eq("proc { |*x| x }.call(1, 2)", "[1, 2]");
    eq("proc { |*| :ok }.call(1, 2)", ":ok");
    eq("def m1(*); 9; end; m1(1, 2)", "9");
    eq("def m2(**); :kw; end; m2(a: 1)", ":kw");
    eq("def m3(*, **); :both; end; m3(1, a: 2)", ":both");
    // Arity of a splat block is -(required + 1), negative like MRI.
    eq("proc { |a, *b| }.arity", "-2");
    eq("proc { |*| }.arity", "-1");
    eq("->(a, *b) {}.arity", "-2");
}

#[test]
fn reopening_a_class_merges_methods() {
    // Both methods survive a reopening (MRI keeps `a` and `b`).
    eq(
        "class A1; def a; 1; end; end; class A1; def b; 2; end; end; [A1.new.a, A1.new.b]",
        "[1, 2]",
    );
    // Reopening a module and mixing it in exposes methods from both openings.
    eq(
        "module M1; def m1; :one; end; end; module M1; def m2; :two; end; end; \
         class C1; include M1; end; [C1.new.m1, C1.new.m2]",
        "[:one, :two]",
    );
    // Constants accumulate across reopenings.
    eq(
        "class E1; X = 1; end; class E1; Y = 2; end; [E1::X, E1::Y]",
        "[1, 2]",
    );
    // Each opening's class body runs (separate `__class_body__` entries).
    eq(
        "class D1; @@log = []; def self.log; @@log; end; @@log << :first; end; \
         class D1; @@log << :second; end; D1.log",
        "[:first, :second]",
    );
    // Class methods merge too.
    eq(
        "class F1; def self.a; 1; end; end; class F1; def self.b; 2; end; end; [F1.a, F1.b]",
        "[1, 2]",
    );
}

#[test]
fn shift_operator_versus_heredoc_disambiguation() {
    // `s<<"x"` glued to a value is String#<< append, not a heredoc start.
    eq("s = \"a\"; s << \"b\"", "\"ab\"");
    eq("s = \"a\"; s<<\"b\"; s", "\"ab\"");
    eq("a = []; a<<1; a", "[1]");
    // A quoted heredoc still works in value position.
    eq("x = <<\"E\"\nhi\nE\nx", "\"hi\\n\"");
}

#[test]
fn label_colon_glued_to_keyword_or_constant_value() {
    // `key:value` with no space, where the value is a keyword/constant, is a
    // label (was mis-lexed as a symbol like `:true`).
    eq("def f(x:); x; end; f(x:true)", "true");
    eq("def f(x:); x; end; f(x:nil)", "nil");
    eq("{a:true}", "{a: true}");
    eq("{k:String}", "{k: String}");
    eq("S = Struct.new(:a, keyword_init:true); S.new(a: 5).a", "5");
    // Bare symbols in value position are unaffected.
    eq("[:a, :b]", "[:a, :b]");
    eq("{a: :b}", "{a: :b}");
}

#[test]
fn parallel_assignment_with_leading_splat() {
    eq("*x, y = 1, 2, 3; [x, y]", "[[1, 2], 3]");
    eq("*x = 1, 2, 3; x", "[1, 2, 3]");
    eq("a, *b, c = 1, 2, 3, 4, 5; [a, b, c]", "[1, [2, 3, 4], 5]");
    eq("a, *b = 1, 2, 3; [a, b]", "[1, [2, 3]]");
}

#[test]
fn lambda_literal_as_command_argument() {
    // `p ->(x){ }` — a lambda literal begins a command argument (was rejected
    // with "unexpected '->'"). `p` returns its argument, so the last-expression
    // value is the lambda-call result.
    eq("p ->(x) { x + 1 }.call(5)", "6");
    eq("p ->(*a) { a }.call(1, 2)", "[1, 2]");
    eq("->(a, b) {}.arity", "2");
}

#[test]
fn array_uniq_with_block_uses_key() {
    eq("[1, 2, 3, 4].uniq { |x| x % 2 }", "[1, 2]");
    eq("%w[a bb cc ddd].uniq(&:length)", "[\"a\", \"bb\", \"ddd\"]");
    eq("[1, 1, 2, 3, 3].uniq", "[1, 2, 3]");
}

/// Framework-shaped runtime gaps a real web library (Rack / ActiveRecord-style)
/// trips on. Every expected value confirmed against ruby 4.0.6.
#[test]
fn setter_method_definitions() {
    // Instance setter def `def x=(v)` — dispatched by `obj.x = v`.
    eq(
        "class C; def x=(v); @x = v; end; def x; @x; end; end; c = C.new; c.x = 7; c.x",
        "7",
    );
    // Singleton/class-method setter `def self.x=(v)`.
    eq(
        "class C; def self.x=(v); @x = v; end; def self.x; @x; end; end; C.x = 5; C.x",
        "5",
    );
    // `class << self; attr_accessor` — class-level attributes.
    eq(
        "class C; class << self; attr_accessor :cfg; end; end; C.cfg = 1; C.cfg",
        "1",
    );
    // A setter reached through `send`.
    eq(
        "class C; def x=(v); @x = v; end; def x; @x; end; end; c = C.new; c.send(:x=, 9); c.x",
        "9",
    );
}

#[test]
fn multiline_parenthesized_call_arguments() {
    // A newline after `(` and before `)` inside a CALL arg list must parse (the
    // #1 blocker both framework libraries hit).
    eq("def f(a, b); a + b; end; f(\n  1,\n  2\n)", "3");
    // Trailing comma + closing paren on its own line.
    eq("[1, 2, 3].map { |x|\n  x * 2\n}", "[2, 4, 6]");
    // Nested multi-line call.
    eq(
        "def g(x); x; end; def f(a, b); a + b; end; g(\n  f(\n    10,\n    20\n  )\n)",
        "30",
    );
}

#[test]
fn framework_metaprogramming_idioms() {
    // alias_method.
    eq(
        "class C; def a; 1; end; alias_method :b, :a; end; C.new.b",
        "1",
    );
    // prepend + super.
    eq(
        "module M; def g; \"M\" + super; end; end; class C; def g; \"C\"; end; prepend M; end; C.new.g",
        "\"MC\"",
    );
    // define_singleton_method.
    eq(
        "o = Object.new; o.define_singleton_method(:h) { 42 }; o.h",
        "42",
    );
    // Anonymous class instantiation (Class.new { ... }.new).
    eq("Class.new { def z; 9; end }.new.z", "9");
    // superclass.
    eq("class B; end; class D < B; end; D.superclass", "B");
    // instance_eval — literal block rebinds self.
    eq(
        "o = Object.new; o.instance_eval { @v = 3 }; o.instance_variable_get(:@v)",
        "3",
    );
    // instance_eval — a forwarded &block also rebinds self.
    eq("def d(&b); Object.new.instance_eval(&b); end; d { 5 }", "5");
}

#[test]
fn dir_and_env_builtins() {
    // ENV pseudo-hash over the process environment.
    eq("ENV[\"PATH\"].nil?", "false");
    eq("ENV.fetch(\"RUBYLANG_NOPE_XYZ\", \"d\")", "\"d\"");
    eq("ENV.key?(\"RUBYLANG_NOPE_XYZ\")", "false");
    // Dir.tmpdir (needs `require \"tmpdir\"`, a no-op) returns a String path.
    eq("require \"tmpdir\"; Dir.tmpdir.is_a?(String)", "true");
    eq("require \"tmpdir\"; Dir.respond_to?(:mktmpdir)", "true");
}

#[test]
fn string_new_is_a_real_mutable_string() {
    // `String.new` / `String.new("x")` must produce a real mutable String
    // (backed by the same representation as a literal), not an opaque object.
    eq("String.new", "\"\"");
    eq("String.new(\"x\")", "\"x\"");
    eq("String.new.length", "0");
    eq("String.new(\"x\").length", "1");
    eq("a = String.new; a << \"hi\"; a << \"!\"; a", "\"hi!\"");
    eq("String.new(\"ab\") + \"cd\"", "\"abcd\"");
    eq("String.new(\"hi\").upcase", "\"HI\"");
}

#[test]
fn array_reverse_each() {
    // Block form yields elements in reverse, returns the receiver.
    eq(
        "r = []; [1,2,3].reverse_each { |x| r << x }; r",
        "[3, 2, 1]",
    );
    // Block-less form is a reverse Enumerator (chainable).
    eq("[1,2,3].reverse_each.to_a", "[3, 2, 1]");
    eq("[1,2,3].reverse_each.map { |x| x * 2 }", "[6, 4, 2]");
    // Enumerable derives it for a user class with its own `each`.
    eq(
        "class C1; include Enumerable; def initialize(*a); @a=a; end; \
         def each(&b); @a.each(&b); end; end; C1.new(1,2,3).reverse_each.to_a",
        "[3, 2, 1]",
    );
}

#[test]
fn kernel_warn_returns_nil() {
    // `warn` writes to $stderr and returns nil; no error, multiple args allowed.
    eq("warn(\"a\", \"b\")", "nil");
    eq("warn", "nil");
    eq("x = warn(\"msg\"); x.nil?", "true");
}

#[test]
fn array_reverse_bang_and_uniq_bang() {
    // In-place reverse returns the receiver, mutated.
    eq("[3,1,2].reverse!", "[2, 1, 3]");
    eq("x = [3,1,2]; x.reverse!; x", "[2, 1, 3]");
    // In-place uniq: receiver when a dup was removed, nil when unchanged.
    eq("[1,1,2,3,3,3].uniq!", "[1, 2, 3]");
    eq("[1,2,3].uniq!", "nil");
    eq("x = [1,1,2]; x.uniq!; x", "[1, 2]");
}

#[test]
fn array_of_arrays_sorts_element_wise() {
    // `Array#<=>` compares element-wise, so sorting `[key, value]` pairs
    // (the `hash.to_a.sort` idiom) orders by the first element.
    eq("[[:b, 2], [:a, 1]].sort", "[[:a, 1], [:b, 2]]");
    eq("{ b: 2, a: 1 }.to_a.sort", "[[:a, 1], [:b, 2]]");
    eq("[[1, 2], [1, 1], [0, 9]].sort", "[[0, 9], [1, 1], [1, 2]]");
    eq("[[1], [1, 0], [1, 0, 0]].sort", "[[1], [1, 0], [1, 0, 0]]");
}

#[test]
fn kernel_printf_writes_and_returns_nil() {
    // `printf` returns nil (the formatting itself is covered by `format`).
    eq("printf(\"%d\", 1)", "nil");
    eq("format(\"%08.3f\", 3.14159)", "\"0003.142\"");
    eq("format(\"%-5d|\", 42)", "\"42   |\"");
}

#[test]
fn enumerable_chunk_and_slice_when_via_user_each() {
    let prelude = "class C2; include Enumerable; def initialize(*a); @a=a; end; \
                   def each(&b); @a.each(&b); end; end; ";
    eq(
        &format!("{prelude}C2.new(1,1,2,3,3,3).chunk {{ |x| x }}.to_a"),
        "[[1, [1, 1]], [2, [2]], [3, [3, 3, 3]]]",
    );
    eq(
        &format!("{prelude}C2.new(1,2,4,5).slice_when {{ |a, b| b > a + 1 }}.to_a"),
        "[[1, 2], [4, 5]]",
    );
}

#[test]
fn respond_to_consults_user_method_table() {
    // Own instance method, included-module method, define_method'd method,
    // alias, and per-object singleton all report true; an undefined name false.
    let prelude = "module M; def mixed; end; end; \
                   class C; def foo; end; include M; \
                   define_method(:dyn) { 42 }; alias_method :dyn2, :dyn; end; ";
    eq(&format!("{prelude}C.new.respond_to?(:foo)"), "true");
    eq(&format!("{prelude}C.new.respond_to?(:mixed)"), "true");
    eq(&format!("{prelude}C.new.respond_to?(:dyn)"), "true");
    eq(&format!("{prelude}C.new.respond_to?(:dyn2)"), "true");
    eq(&format!("{prelude}C.new.respond_to?(:nope)"), "false");
    eq(
        "o = Object.new; def o.sing; end; o.respond_to?(:sing)",
        "true",
    );
    // Inherited user method (through the superclass chain) also responds.
    eq(
        "class A; def a; end; end; class B < A; end; B.new.respond_to?(:a)",
        "true",
    );
}

#[test]
fn super_in_exception_initialize_sets_message() {
    let prelude = "class E < StandardError; def initialize(x); super(\"boom: #{x}\"); end; end; ";
    eq(&format!("{prelude}E.new(5).message"), "\"boom: 5\"");
    eq(
        &format!("{prelude}begin; raise E.new(7); rescue => e; e.message; end"),
        "\"boom: 7\"",
    );
    // `raise E, 9` routes through the user initialize (message becomes "boom: 9").
    eq(
        &format!("{prelude}begin; raise E, 9; rescue => e; e.message; end"),
        "\"boom: 9\"",
    );
    // A plain builtin raise still carries its message.
    eq(
        "begin; raise ArgumentError, \"bad\"; rescue => e; e.message; end",
        "\"bad\"",
    );
}

#[test]
fn hash_merge_bang_and_update() {
    eq(
        "h = {a: 1, b: 2}; h.merge!({b: 3, c: 4}); h",
        "{a: 1, b: 3, c: 4}",
    );
    // Returns the receiver (same object).
    eq("h = {a: 1}; h.merge!({b: 2}).equal?(h)", "true");
    // Block resolves collisions; `update` is the alias.
    eq(
        "h = {a: 1, b: 2}; h.update({b: 10}) { |k, old, new| old + new }; h",
        "{a: 1, b: 12}",
    );
    // Multiple hash arguments, left-to-right.
    eq(
        "h = {a: 1}; h.merge!({b: 2}, {c: 3}); h",
        "{a: 1, b: 2, c: 3}",
    );
    // Non-bang merge with a block is unaffected.
    eq(
        "{x: 1}.merge({x: 2, y: 3}) { |k, o, n| o + n }",
        "{x: 3, y: 3}",
    );
}

#[test]
fn stringio_read_write_buffer() {
    let req = "require \"stringio\"; ";
    eq(
        &format!("{req}io = StringIO.new; io.write(\"a\"); io << \"b\"; io.puts(\"c\"); io.string"),
        "\"abc\\n\"",
    );
    eq(
        &format!("{req}r = StringIO.new(\"x\\ny\\n\"); r.gets"),
        "\"x\\n\"",
    );
    eq(
        &format!("{req}r = StringIO.new(\"abcdef\"); r.read(3)"),
        "\"abc\"",
    );
    // read advances the cursor; pos and rewind track it.
    eq(
        &format!("{req}r = StringIO.new(\"abc\"); r.read(2); r.pos"),
        "2",
    );
    eq(
        &format!("{req}r = StringIO.new(\"abc\"); r.read; r.rewind; r.read"),
        "\"abc\"",
    );
    // write returns the byte count.
    eq(&format!("{req}StringIO.new.write(\"12345\")"), "5");
}

#[test]
fn array_pack_and_string_unpack_round_trip() {
    eq("[72, 105].pack(\"C*\")", "\"Hi\"");
    eq("\"ABC\".unpack(\"C*\")", "[65, 66, 67]");
    eq("\"Hi\".unpack1(\"a*\")", "\"Hi\"");
    // Hex directive (used by digests): decode then re-encode.
    eq(
        "[\"deadbeef\"].pack(\"H*\").unpack1(\"H*\")",
        "\"deadbeef\"",
    );
    // Big/little-endian fixed-width integers.
    eq("[1].pack(\"N\").unpack1(\"N\")", "1");
    eq("[65535].pack(\"n\").unpack1(\"n\")", "65535");
    eq("[1].pack(\"V\").unpack1(\"V\")", "1");
    eq("[513].pack(\"v\").unpack1(\"v\")", "513");
    // Every byte value round-trips (the crypto/web property).
    eq(
        "(0..255).to_a.pack(\"C*\").unpack(\"C*\") == (0..255).to_a",
        "true",
    );
    // Integer#chr for a high byte round-trips through unpack.
    eq("200.chr.unpack(\"C*\")", "[200]");
    eq("[\"hi\"].pack(\"A5\")", "\"hi   \"");
}

#[test]
fn script_file_constant_reports_running_file() {
    // Under `-e`-style evaluation MRI reports `__FILE__` as "-e".
    eq("__FILE__", "\"-e\"");
    eq("File.dirname(__FILE__)", "\".\"");
}

/// Three rubylang↔MRI parity fixes: Integer ** negative → Rational, negative-range
/// slice underflow → empty, and Array#<=>. All confirmed against ruby 4.0.6.
#[test]
fn integer_pow_negative_is_rational() {
    eq("5 ** -7", "(1/78125)");
    eq("2 ** -1", "(1/2)");
    eq("(-5) ** -3", "(-1/125)");
    // Non-negative integer power stays an exact Integer.
    eq("10 ** 3", "1000");
    eq("2 ** 70", "1180591620717411303424");
    // Float base keeps Float semantics.
    eq("2.0 ** -1", "0.5");
}

#[test]
fn negative_range_slice_underflow_is_empty() {
    // A negative endpoint that underflows past index 0 yields an empty slice.
    eq("\"foo\"[1...-4]", "\"\"");
    eq("[1, 2, 3][1...-4]", "[]");
    // In-range negative endpoints still work.
    eq("\"foo\"[1..-1]", "\"oo\"");
    eq("\"hello\"[1...-1]", "\"ell\"");
}

#[test]
fn array_spaceship_operator() {
    eq("[1, 2] <=> [1, 2]", "0");
    eq("[1, 2] <=> [1, 3]", "-1");
    eq("[1] <=> [1, 2]", "-1");
    eq("[1, 2, 3] <=> [1, 2]", "1");
    // Non-array operand → nil.
    eq("([1, 2] <=> 5).inspect", "\"nil\"");
}

/// Fuzzer-found parity fixes (differential MRI fuzz across arith/reduce/slice/
/// sorting/symbols). Every expected value confirmed against ruby 4.0.6.
#[test]
fn reduce_symbol_promotes_and_never_panics() {
    // reduce(:*) factorial promotes to BigInt instead of panicking on overflow.
    eq("(1..25).reduce(:*)", "15511210043330985984000000");
    eq("(1..25).reduce(1, :*)", "15511210043330985984000000");
    eq("[100, 2, 5].reduce(:/)", "10");
    eq("[17, 5].reduce(:%)", "2");
    eq("[2, 3, 2].reduce(:**)", "64");
    eq("[1.5, 2.0].reduce(:*)", "3.0");
}

#[test]
fn negation_of_heap_numbers() {
    // Negating a BigInt or Rational (the VM forwards Negate for heap numbers).
    eq("-(2 ** 70)", "-1180591620717411303424");
    eq("-2 ** -7", "(-1/128)");
    eq("-9 ** 2", "-81");
}

#[test]
fn integer_pow_edge_cases() {
    // |base| == 1 short-circuits at any exponent; base 0 has 0**0==1, 0**+ ==0.
    eq("1 ** -7", "1");
    eq("(-1) ** -7", "-1");
    eq("(-1) ** -8", "1");
    eq("1 ** -3 ** -1", "(1/1)"); // Rational exponent -> Rational (1/1)
    eq("0 ** 42 ** 42", "0");
    eq("0 ** 0", "1");
    // Rational contagion through % and mixed integer division.
    eq("10 ** -3 % 5", "(1/1000)");
    eq("10 % 9 ** -3", "(0/1)");
    eq("7 / 42 ** 42", "0");
}

#[test]
fn max_by_returns_first_on_tie() {
    // Ruby max_by returns the FIRST element on a key tie (Rust's returns last).
    eq("[5, -7, 7, 2].max_by { |x| x.abs }", "-7");
    eq("[-7, 7].max_by { |x| x.abs }", "-7");
    eq("[-7, 7].min_by { |x| x.abs }", "-7");
}

#[test]
fn capitalized_symbol_hash_keys() {
    eq("h = { Ruby: 5 }; h[:Ruby]", "5");
    eq("{ Lang: 10, Ruby: 2 }", "{Lang: 10, Ruby: 2}");
}

/// Float formatting / rounding parity fixes from the differential fuzzer.
/// Confirmed against ruby 4.0.6.
#[test]
fn float_scientific_notation_threshold() {
    // Ruby switches to scientific notation at magnitude >= 1e15 (and < 1e-4).
    eq("1e15", "1.0e+15");
    eq("9.9e14", "990000000000000.0");
    eq("1.5e15", "1.5e+15");
    eq("1e14", "100000000000000.0");
    eq("1e-4", "0.0001");
    eq("9e-5", "9.0e-05");
}

#[test]
fn float_round_domain_and_promotion() {
    // Non-finite floats raise FloatDomainError on Integer-producing conversions;
    // round(positive) passes them through.
    eq("(1.0 / 0.0).round(2)", "Infinity");
    // A round/to_i past the i64 range promotes to BigInt (not i64 saturation).
    eq("(1e10 * 1e10).round", "100000000000000000000");
    eq("(2.0 ** 70).round", "1180591620717411303424");
    eq("1e20.to_i", "100000000000000000000");
    // Natural IEEE rounding preserves sign: a normal-magnitude negative that
    // rounds to zero is -0.0 (like MRI); a ±0.0 input keeps its sign.
    eq("(-0.01).round(1)", "-0.0");
    eq("(-0.0).round(2)", "-0.0");
    // Ordinary rounds unaffected.
    eq("3.14159.round(2)", "3.14");
}

/// String#-@ (frozen copy) and String#+@ (mutable copy) — Ruby's frozen-string
/// operators, found missing by running a real MVC app. vs ruby 4.0.6.
#[test]
fn string_unary_freeze_operators() {
    eq("(-\"foo\").frozen?", "true");
    eq("-\"foo\"", "\"foo\"");
    eq("(+\"bar\").frozen?", "false");
    eq("+\"bar\"", "\"bar\"");
    // -@ on an already-frozen string returns a frozen string.
    eq("s = \"x\".freeze; (-s).frozen?", "true");
}

/// Enumerable#grep/grep_v and Module visibility directives with arguments —
/// found missing while loading real gems onto the load path. vs ruby 4.0.6.
#[test]
fn grep_and_visibility_directives() {
    eq("[1, 2, \"a\", \"b\", 3].grep(Integer)", "[1, 2, 3]");
    eq("[\"x1\", \"y\", \"x2\"].grep(/x/)", "[\"x1\", \"x2\"]");
    eq("(1..10).to_a.grep(3..6)", "[3, 4, 5, 6]");
    eq(
        "[1, 2, 3, 4].grep(Integer) { |x| x * 10 }",
        "[10, 20, 30, 40]",
    );
    eq("[1, \"a\", 2].grep_v(Integer)", "[\"a\"]");
    // Visibility directives with args are accepted (rubylang doesn't enforce
    // visibility); private_constant/private return the name, module_function nil.
    eq(
        "module M1; X = 5; private_constant :X; def self.g; X; end; end; M1.g",
        "5",
    );
    eq(
        "class C1; def a; 1; end; private :a; end; C1.new.send(:a)",
        "1",
    );
}

/// Operator symbols (`:-@`, `:+@`, `:!~`) and operator method-name defs (`def =~`)
/// — found parsing real gem source (tzinfo). vs ruby 4.0.6.
#[test]
fn operator_symbols_and_operator_method_defs() {
    eq(":-@", ":-@");
    eq(":+@", ":+@");
    eq("\"x\".respond_to?(:-@)", "true");
    // Existing operator symbols still lex correctly after the reorder.
    eq("[1, 2, 3].reduce(:-)", "-4");
    eq("[:+, :-, :*, :<=>].length", "4");
    // `def =~` (and other operator method names) parse.
    eq("class C9; def =~(o); 42; end; end; (C9.new =~ 5)", "42");
}

/// `super { block }` — passing a new block to super (not forwarding the current
/// one). Found in real gem code (tzinfo). vs ruby 4.0.6.
#[test]
fn super_with_a_block() {
    // super { blk } passes a fresh block to the superclass method.
    eq(
        "class A; def each; yield 1; yield 2; end; end; \
         class B < A; def each; r = []; super { |x| r << x * 10 }; r; end; end; \
         B.new.each",
        "[10, 20]",
    );
    // super(args) { blk } — explicit args + new block.
    eq(
        "class A; def go(n); n.times { |i| yield i }; end; end; \
         class B < A; def go(n); r = []; super(n) { |i| r << i + 100 }; r; end; end; \
         B.new.go(3)",
        "[100, 101, 102]",
    );
    // Plain super still forwards args and block.
    eq(
        "class A; def m; yield 5; end; end; class B < A; def m; super; end; end; \
         v = nil; B.new.m { |x| v = x }; v",
        "5",
    );
    eq(
        "class A; def f(x); x + 1; end; end; class B < A; def f(x); super; end; end; B.new.f(10)",
        "11",
    );
}

/// Multi-line `while/until/for … do` — the optional `do` keyword binds to the
/// loop, not to a call in the condition. Found in real gem code (tzinfo). vs MRI.
#[test]
fn loop_with_optional_do_keyword() {
    eq("i = 0; while i < 3 do\n  i += 1\nend\ni", "3");
    eq("i = 5; until i <= 0 do\n  i -= 2\nend\ni", "-1");
    eq("s = 0; for x in [1, 2, 3] do\n  s += x\nend\ns", "6");
    // A brace/do block in the condition still attaches to its call.
    eq("r = []; [1, 2].each do |x| r << x end; r", "[1, 2]");
    eq("while [1].any? { |x| x > 5 } do\n  break\nend\n:ok", ":ok");
}

// --- Fanout round 3: reserved-word keyword labels, quoted/op-symbol branches ---

/// The quoted (`class:"x"`) and op-symbol (`if:+1`) colon branches must treat a
/// glued keyword as a label, not a symbol — the same guard the alphabetic-label
/// branch uses. Regression for the lexer emitting `:x`/`:+` symbols after a
/// keyword. vs MRI 4.0.6.
#[test]
fn reserved_word_labels_quoted_and_glued() {
    // quoted label value, no space before the colon (the fixed branch)
    eq(r#"{class:"x"}"#, r#"{class: "x"}"#);
    eq(r#"{def:"y", if:"z"}"#, r#"{def: "y", if: "z"}"#);
    // glued alphabetic keyword label (`class:foo` — foo is a call → nil here)
    eq("{class:nil, unless:nil}", "{class: nil, unless: nil}");
    // keyword kwarg with a quoted default and no space; value observed via **o
    eq(r#"def f(class:"x"); :ok; end; f"#, ":ok");
    eq(r#"def g(**o); o; end; g(class: "x")"#, r#"{class: "x"}"#);
    // `return:` glued is a label, matching MRI (`{return: 5}`)
    eq("{return:5}", "{return: 5}");
    // GUARD: a real symbol at expression start is untouched
    eq("[:class, :if, :+]", "[:class, :if, :+]");
    // GUARD: spaced command-arg symbol after a value is still a symbol
    eq("[1].push :end", "[1, :end]");
    // GUARD: ternary colon (spaced, not glued to a keyword) is untouched
    eq("x = 5; x > 3 ? :big : :small", ":big");
}

// --- Fanout round 3: additive Enumerable/Array + String method arms ---------

/// `collect_concat` (flat_map alias), `each_entry` (each-yielding), and `chain`
/// (concatenating Enumerator). vs MRI 4.0.6.
#[test]
fn enumerable_collect_concat_each_entry_chain() {
    // collect_concat splices one array level, like flat_map
    eq(
        "[1, 2, 3].collect_concat { |x| [x, x] }",
        "[1, 1, 2, 2, 3, 3]",
    );
    eq("[1, 2].collect_concat { |x| x }", "[1, 2]");
    // each_entry yields each element and returns the receiver
    eq(
        "r = []; [1, 2, 3].each_entry { |x| r << x }; r",
        "[1, 2, 3]",
    );
    eq("[1, 2, 3].each_entry { |x| }", "[1, 2, 3]");
    // block-less each_entry is an Enumerator whose to_a is the elements
    eq("[1, 2].each_entry.to_a", "[1, 2]");
    // chain concatenates the receiver with each argument's elements
    eq("[1, 2, 3].chain([4, 5]).to_a", "[1, 2, 3, 4, 5]");
    eq("[1, 2, 3].chain([4, 5], [6]).to_a", "[1, 2, 3, 4, 5, 6]");
    eq("[1, 2, 3].chain.to_a", "[1, 2, 3]");
}

/// `String#delete_prefix` / `#delete_suffix` strip one exact leading/trailing
/// match; a non-match returns an unchanged copy. vs MRI 4.0.6.
#[test]
fn string_delete_prefix_suffix() {
    eq(r#""hello".delete_prefix("hel")"#, r#""lo""#);
    eq(r#""hello".delete_prefix("xyz")"#, r#""hello""#);
    eq(r#""hello".delete_suffix("llo")"#, r#""he""#);
    eq(r#""hello".delete_suffix("xyz")"#, r#""hello""#);
    // only one occurrence is stripped
    eq(r#""aa".delete_prefix("a")"#, r#""a""#);
    eq(r#""aaa".delete_suffix("a")"#, r#""aa""#);
    // empty receiver / empty argument edge cases
    eq(r#""".delete_prefix("a")"#, r#""""#);
    eq(r#""abc".delete_prefix("")"#, r#""abc""#);
}

/// `super` inside a singleton/class method (`def self.m`) resolves through the
/// superclass's singleton (the class-method chain), not the instance chain.
/// Every value byte-verified against MRI 4.0.6.
#[test]
fn super_from_class_method() {
    // Basic: B.f's super reaches A.f.
    eq(
        "class A; def self.f; 10; end; end; \
         class B < A; def self.f; super + 1; end; end; \
         B.f",
        "11",
    );
    // Forwarded args: bare `super` passes B.f's argument up to A.f.
    eq(
        "class A; def self.f(x); x * 2; end; end; \
         class B < A; def self.f(x); super + 1; end; end; \
         B.f(5)",
        "11",
    );
    // Explicit args: `super(x)`.
    eq(
        "class A; def self.build(x); x * 2; end; end; \
         class B < A; def self.build(x); super(x) + 1; end; end; \
         B.build(5)",
        "11",
    );
    // Three-level chain: C inherits B.f; the super in B.f still reaches A.f once
    // (def_class is the defining class B, not the lookup-origin C).
    eq(
        "class A; def self.f; 10; end; end; \
         class B < A; def self.f; super + 1; end; end; \
         class C < B; end; \
         C.f",
        "11",
    );
    // A `super { … }` block override reaches the superclass singleton method that
    // yields.
    eq(
        "class A; def self.f; yield 5; end; end; \
         class B < A; def self.f; super { |x| x + 100 }; end; end; \
         B.f",
        "105",
    );
    // `super` from a class method that extends a module reaches the module method.
    eq(
        "module M; def greet; \"hi\"; end; end; \
         class A; extend M; end; \
         class B < A; extend M; def self.greet; super + \"!\"; end; end; \
         B.greet",
        "\"hi!\"",
    );
}

/// One-line pattern matching at the statement / expression level: rightward
/// assignment (`expr => pattern`, raises on no match, binds) and the boolean
/// match (`expr in pattern`, yields true/false, binds). vs MRI 4.0.6.
#[test]
fn one_line_pattern_matching() {
    // `expr in pattern` yields a boolean.
    eq("5 in Integer", "true");
    eq("5 in String", "false");
    // Bindings from `in` persist into the surrounding scope.
    eq("{a: 1} in {a: Integer => v}; v", "1");
    // `and`/`or` are looser than the one-line pattern, so each side is its own
    // match.
    eq("(1 in Integer) && (2 in Integer)", "true");
    // Rightward assignment binds and yields nil.
    eq(
        "{name: \"Alice\", age: 30} => {name:, age:}; [name, age]",
        "[\"Alice\", 30]",
    );
    // Array destructuring via `=>`.
    eq("[1, 2, 3] => [a, b, c]; [a, b, c]", "[1, 2, 3]");
    // Nested destructuring.
    eq(
        "config = {db: {host: \"x\", port: 5}}; \
         config => {db: {host:, port:}}; [host, port]",
        "[\"x\", 5]",
    );
    // A non-matching `=>` raises NoMatchingPatternError.
    assert!(ev("1 => String").is_err());
    // The `=>` inside a hash literal stays a pair separator (no regression).
    eq("{1 => 2, 3 => 4}", "{1 => 2, 3 => 4}");
    eq("({:a => 1, :b => 2})", "{a: 1, b: 2}");
    // The `=>` inside an argument list stays a keyword/hash arg (no regression).
    eq("[{one: 1}].map { |h| h }", "[{one: 1}]");
}

/// `String#each_grapheme_cluster` / `#grapheme_clusters` segment by UAX #29
/// extended grapheme clusters (base + combining marks count as one). vs MRI.
/// Structural assertions (cluster count / codepoint length) avoid depending on
/// how a combining mark renders in `inspect`.
#[test]
fn string_grapheme_clusters() {
    // "é" as base 'e' + U+0301 combining acute is ONE cluster of two codepoints,
    // so "éllo" is 4 clusters and the first has length 2.
    eq("\"e\\u0301llo\".each_grapheme_cluster.to_a.length", "4");
    eq(
        "\"e\\u0301llo\".each_grapheme_cluster.to_a.first.length",
        "2",
    );
    eq("\"e\\u0301llo\".grapheme_clusters.length", "4");
    // Per-cluster codepoint lengths for a mixed string.
    eq(
        "\"cafe\\u0301\".grapheme_clusters.map(&:length)",
        "[1, 1, 1, 2]",
    );
    // ASCII degenerates to per-character.
    eq(
        "\"abc\".each_grapheme_cluster.to_a",
        "[\"a\", \"b\", \"c\"]",
    );
    // With a block, returns self.
    eq("\"ab\".each_grapheme_cluster { |g| g }", "\"ab\"");
}

/// `String#unicode_normalize` to NFC (default), NFD, NFKC, NFKD. Verified via the
/// resulting byte sequences against MRI 4.0.6.
#[test]
fn string_unicode_normalize() {
    // NFC (default) composes 'e' + combining acute into precomposed "é" (2 bytes).
    eq("\"e\\u0301\".unicode_normalize.bytes", "[195, 169]");
    eq("\"e\\u0301\".unicode_normalize(:nfc).bytes", "[195, 169]");
    // NFD decomposes precomposed "é" into 'e' + combining acute (3 bytes).
    eq(
        "\"\\u00e9\".unicode_normalize(:nfd).bytes",
        "[101, 204, 129]",
    );
    eq("\"\\u00e9\".unicode_normalize(:nfkc).bytes", "[195, 169]");
    eq(
        "\"\\u00e9\".unicode_normalize(:nfkd).bytes",
        "[101, 204, 129]",
    );
    // `unicode_normalized?` reports whether the string already equals its form.
    eq("\"abc\".unicode_normalized?", "true");
    // An unknown form raises ArgumentError.
    assert!(ev("\"x\".unicode_normalize(:bogus)").is_err());
}

/// `String#b` returns an ASCII-8BIT (BINARY) copy; `#encoding` tracks the tag and
/// `force_encoding` sets/clears it. vs MRI 4.0.6.
#[test]
fn string_binary_encoding() {
    // A default String is UTF-8.
    eq("\"abc\".encoding.inspect", "\"#<Encoding:UTF-8>\"");
    // `#b` yields a BINARY-tagged copy whose Encoding inspects with the alias.
    eq(
        "\"abc\".b.encoding.inspect",
        "\"#<Encoding:BINARY (ASCII-8BIT)>\"",
    );
    eq("\"abc\".b.encoding.name", "\"ASCII-8BIT\"");
    // The copy is still a usable String.
    eq("\"abc\".b.length", "3");
    eq("\"abc\".b", "\"abc\"");
    // `force_encoding` sets the BINARY tag (name string or alias).
    eq(
        "\"x\".force_encoding(\"ASCII-8BIT\").encoding.name",
        "\"ASCII-8BIT\"",
    );
    eq(
        "\"x\".force_encoding(\"BINARY\").encoding.name",
        "\"ASCII-8BIT\"",
    );
    // Passing an Encoding object works too.
    eq(
        "\"x\".force_encoding(Encoding::ASCII_8BIT).encoding.name",
        "\"ASCII-8BIT\"",
    );
    // Forcing back to UTF-8 clears the tag.
    eq(
        "\"x\".b.force_encoding(\"UTF-8\").encoding.name",
        "\"UTF-8\"",
    );
}
