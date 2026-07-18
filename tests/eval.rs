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
    eq("a = [1, 2, 3]; a.insert(5, :x); a", "[1, 2, 3, nil, nil, :x]");
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
    eq("{a: 1, b: 2}.transform_values { |v| v * 10 }", "{a: 10, b: 20}");
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
fn undefined_method_is_an_error() {
    assert!(ev("no_such_method_here(1)").is_err());
}
