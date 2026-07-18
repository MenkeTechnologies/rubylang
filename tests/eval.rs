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
fn undefined_method_is_an_error() {
    assert!(ev("no_such_method_here(1)").is_err());
}
