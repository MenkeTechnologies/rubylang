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
    eq("/\\d+/.class", "\"Regexp\"");
    eq("/ab/.source", "\"ab\"");
    eq("/AB/i.match?(\"xabx\")", "true");
    eq(
        "case \"word\"; when /\\d/ then 1; when /[a-z]+/ then 2; end",
        "2",
    );
}

#[test]
fn matchdata_batch() {
    eq("\"hello\".match(/l(l)/).class", "\"MatchData\"");
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
    eq("(2 ** 64).class", "\"Integer\"");
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
    eq("<<'RAW'\nno #{x} here\nRAW", "\"no #{x} here\\n\"");
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
