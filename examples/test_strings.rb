# Self-checking examples of rubylang's String methods.
#
# Each `check` is a tiny unit test: it raises (aborting the script with a
# non-zero exit) if the result is not what real Ruby produces. Run it with
# `ruby examples/test_strings.rb`; the Rust harness in `tests/examples.rs`
# runs it too and asserts the output matches the reference interpreter.

$checks = 0

def check(actual, expected)
  $checks += 1
  return if actual == expected
  raise "check ##{$checks} failed: #{actual.inspect} != #{expected.inspect}"
end

# Case and trimming.
check "hello".upcase, "HELLO"
check "HELLO".downcase, "hello"
check "hello world".capitalize, "Hello world"
check "MixedCase".swapcase, "mIXEDcASE"
check "  padded  ".strip, "padded"
check "xxhelloxx".delete("x"), "hello"
check "aaabbbccc".squeeze, "abc"

# Search, split, and transform.
check "hello world".split.map(&:capitalize).join(" "), "Hello World"
check "a,b,c".split(",").length, 3
check "path/to/file".partition("/"), ["path", "/", "to/file"]
check "path/to/file".rpartition("/"), ["path/to", "/", "file"]
check "banana".count("a"), 3
check "hello".index("l"), 2

# Character-set translation with ranges (rubylang expands `a-z`).
check "hello".tr("a-y", "*"), "*****"
check "Hello".tr("A-Z", "a-z"), "hello"
check "mississippi".tr_s("sp", "*"), "mi*i*i*i"

# Padding and justification.
check "5".rjust(3, "0"), "005"
check "hi".center(6, "*"), "**hi**"
check "abc".ljust(5), "abc  "

# Regexp: match, scan, substitution, and the match globals.
check "The year 2024".scan(/\d+/), ["2024"]
check "a1b2c3".scan(/([a-z])(\d)/), [["a", "1"], ["b", "2"], ["c", "3"]]
check "hello world".gsub(/\w+/) { |w| w.capitalize }, "Hello World"
check("2024-01-15".sub(/(\d+)-(\d+)-(\d+)/, '\3/\2/\1'), "15/01/2024")
"order #4271" =~ /#(\d+)/
check $1, "4271"

# Codepoints.
check "A".ord, 65
check 97.chr, "a"

puts "#{$checks} string checks passed"
