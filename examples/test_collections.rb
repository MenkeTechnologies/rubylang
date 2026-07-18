# Self-checking examples of rubylang's Array and Hash methods.
#
# Each `check` raises on a mismatch with real Ruby. See test_strings.rb for the
# harness contract (stdout + exit status are asserted by tests/examples.rs).

$checks = 0

def check(actual, expected)
  $checks += 1
  return if actual == expected
  raise "check ##{$checks} failed: #{actual.inspect} != #{expected.inspect}"
end

# Building arrays.
check Array.new(3), [nil, nil, nil]
check Array.new(3, 0), [0, 0, 0]
check Array.new(4) { |i| i * i }, [0, 1, 4, 9]

# Transformations.
check [1, 2, 3, 4].select(&:even?), [2, 4]
check [1, 2, 3].map { |n| n * n }, [1, 4, 9]
check [1, [2, [3, 4]]].flatten, [1, 2, 3, 4]
check [1, 2, 2, 3, 3, 3].uniq, [1, 2, 3]
check [3, 1, 2].sort, [1, 2, 3]
check [3, 1, 2].sort_by { |x| -x }, [3, 2, 1]
check [1, 2, 3, 4].partition(&:even?), [[2, 4], [1, 3]]
check ["a", "a", "b", "c", "c"].tally, { "a" => 2, "b" => 1, "c" => 2 }

# Reduction and search.
check [1, 2, 3, 4, 5].reduce(0, :+), 15
check [1, 2, 3, 4].sum(100), 110
check [1, 2, 3, 4, 5].bsearch { |x| x >= 3 }, 3
check [10, 20, 30].values_at(0, 2), [10, 30]
check [1, 2, 3, 4].each_cons(2).to_a, [[1, 2], [2, 3], [3, 4]]

# Combining.
check [1, 2].zip([3, 4]), [[1, 3], [2, 4]]
check [1, 2].product([3, 4]), [[1, 3], [1, 4], [2, 3], [2, 4]]

# Hashes.
h = { a: 1, b: 2, c: 3 }
check h.transform_values { |v| v * 10 }, { a: 10, b: 20, c: 30 }
check h.select { |_, v| v > 1 }, { b: 2, c: 3 }
check h.min_by { |_, v| v }, [:a, 1]
check h.sum { |_, v| v }, 6
check h.except(:b), { a: 1, c: 3 }
check h.slice(:a, :c), { a: 1, c: 3 }
check h.invert, { 1 => :a, 2 => :b, 3 => :c }
check([[:x, 10], [:y, 20]].to_h, { x: 10, y: 20 })

# A default-block hash (auto-vivifying counter / grouping).
counts = Hash.new(0)
"mississippi".each_char { |ch| counts[ch] += 1 }
check counts, { "m" => 1, "i" => 4, "s" => 4, "p" => 2 }

groups = Hash.new { |hsh, k| hsh[k] = [] }
[1, 2, 3, 4, 5, 6].each { |n| groups[n % 3] << n }
check groups, { 1 => [1, 4], 2 => [2, 5], 0 => [3, 6] }

puts "#{$checks} collection checks passed"
