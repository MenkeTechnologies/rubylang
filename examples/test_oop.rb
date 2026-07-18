# Self-checking examples of rubylang's object model: classes, inheritance,
# modules, Comparable, Enumerable, and Method objects.
#
# Each `check` raises on a mismatch with real Ruby.

$checks = 0

def check(actual, expected)
  $checks += 1
  return if actual == expected
  raise "check ##{$checks} failed: #{actual.inspect} != #{expected.inspect}"
end

# Classes with state, accessors, and chaining.
class Counter
  def initialize
    @n = 0
  end

  def inc
    @n += 1
    self
  end

  def value
    @n
  end
end

check Counter.new.inc.inc.inc.value, 3

# Inheritance with super.
class Animal
  def initialize(name)
    @name = name
  end

  def describe
    "a #{kind} named #{@name}"
  end

  def kind
    "animal"
  end
end

class Dog < Animal
  def kind
    "dog"
  end
end

check Dog.new("Rex").describe, "a dog named Rex"

# Comparable via <=>.
class Version
  include Comparable
  attr_reader :n

  def initialize(n)
    @n = n
  end

  def <=>(other)
    n <=> other.n
  end
end

check Version.new(1) < Version.new(2), true
check Version.new(3) >= Version.new(3), true
check [Version.new(3), Version.new(1), Version.new(2)].min.n, 1
check Version.new(5).clamp(Version.new(1), Version.new(3)).n, 3

# Enumerable via each.
class NumberList
  include Enumerable

  def initialize(*nums)
    @nums = nums
  end

  def each
    @nums.each { |n| yield n }
  end
end

list = NumberList.new(3, 1, 2)
check list.sort, [1, 2, 3]
check list.map { |x| x * 2 }, [6, 2, 4]
check list.select(&:odd?), [3, 1]
check list.reduce(:+), 6
check list.include?(2), true

# Operator overloading.
class Vec
  attr_reader :x, :y

  def initialize(x, y)
    @x = x
    @y = y
  end

  def +(other)
    Vec.new(@x + other.x, @y + other.y)
  end

  def ==(other)
    @x == other.x && @y == other.y
  end
end

check((Vec.new(1, 2) + Vec.new(3, 4)) == Vec.new(4, 6), true)

# Method objects.
upcaser = "hello".method(:upcase)
check upcaser.call, "HELLO"
adder = 5.method(:+)
check adder.call(3), 8

puts "#{$checks} oop checks passed"
