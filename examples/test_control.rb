# Self-checking examples of rubylang's control flow: blocks, exceptions,
# catch/throw, case, and post-test loops.
#
# Each `check` raises on a mismatch with real Ruby.

$checks = 0

def check(actual, expected)
  $checks += 1
  return if actual == expected
  raise "check ##{$checks} failed: #{actual.inspect} != #{expected.inspect}"
end

# Blocks that capture and mutate the enclosing scope.
total = 0
[1, 2, 3, 4].each { |x| total += x }
check total, 10

# Early return from a block through an enclosing method.
def first_even(arr)
  arr.each { |x| return x if x.even? }
  nil
end
check first_even([1, 3, 4, 7]), 4

# yield.
def wrap
  "[#{yield}]"
end
check wrap { "x" }, "[x]"

# Exceptions: rescue, matching class, and ensure.
def safe_div(a, b)
  a / b
rescue ZeroDivisionError
  :divide_by_zero
end
check safe_div(10, 2), 5
check safe_div(1, 0), :divide_by_zero

trail = []
begin
  raise "boom"
rescue => e
  trail << "rescued #{e.message}"
ensure
  trail << "cleanup"
end
check trail, ["rescued boom", "cleanup"]

# catch / throw for non-local exit out of nested loops.
found = catch(:found) do
  (1..3).each do |i|
    (1..3).each do |j|
      throw :found, [i, j] if i * j == 6
    end
  end
  :none
end
check found, [2, 3]

# case / when with ranges and classes.
def classify(n)
  case n
  when 0 then "zero"
  when 1..9 then "small"
  when Integer then "big"
  else "other"
  end
end
check classify(0), "zero"
check classify(5), "small"
check classify(100), "big"
check classify("x"), "other"

# Post-test loops run the body at least once, then test.
i = 0
begin
  i += 1
end while i < 5
check i, 5

n = 0
begin
  n += 1
end until n >= 3
check n, 3

# A ternary and modifier-if.
check((5 > 3 ? :yes : :no), :yes)
result = []
[1, 2, 3, 4].each { |x| result << x if x.odd? }
check result, [1, 3]

puts "#{$checks} control-flow checks passed"
