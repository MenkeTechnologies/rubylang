# Self-checking examples of rubylang's numeric methods.
#
# Each `check` raises on a mismatch with real Ruby. See test_strings.rb for the
# harness contract.

$checks = 0

def check(actual, expected)
  $checks += 1
  return if actual == expected
  raise "check ##{$checks} failed: #{actual.inspect} != #{expected.inspect}"
end

# Integer arithmetic keeps Ruby semantics (integer division floors).
check 10 / 3, 3
check(-7 / 2, -4)
check(-7 % 3, 2)
check 2 ** 10, 1024
check 17.gcd(5), 1
check 12.gcd(8), 4
check 4.lcm(6), 12
check 17.divmod(5), [3, 2]
check 255.to_s(16), "ff"
check "ff".to_i(16), 255
check 255.digits(16), [15, 15]

# Predicates.
check 0.zero?, true
check 4.even?, true
check 7.odd?, true
check (-3).negative?, true
check 5.positive?, true
check 3.14.integer?, false
check 5.integer?, true

# Bit operations.
check (5 & 3), 1
check (5 | 2), 7
check (5 ^ 1), 4
check (1 << 4), 16
check 255.bit_length, 8

# Clamping and comparison.
check 5.clamp(1, 3), 3
check (-2).clamp(0, 10), 0
check 5.clamp(1..3), 3
check 3.between?(1, 5), true

# Floats.
check 3.14159.round(2), 3.14
check 3.14159.floor(1), 3.1
check 10.0 / 4, 2.5
check 2.0 ** 3, 8.0
check (1.0 / 0).infinite?, 1
check (0.0 / 0).nan?, true

# Iteration helpers.
check (1..5).reduce(:+), 15
check (1..100).sum, 5050
check 5.times.to_a, [0, 1, 2, 3, 4]
check 1.upto(4).to_a, [1, 2, 3, 4]

puts "#{$checks} number checks passed"
