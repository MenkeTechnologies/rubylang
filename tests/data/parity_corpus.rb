puts 1 + 2 * 3
#==#
puts (1 + 2) * 3
#==#
puts 2 ** 10
#==#
puts 10 / 3
puts -7 / 2
puts -7 % 3
puts 10.0 / 4
#==#
puts "hello".upcase
puts "WORLD".downcase
puts "racecar".reverse
puts "  hi  ".strip
puts "a,b,c".split(",").length
puts "x" * 5
puts "ruby".length
#==#
x = 6
y = 7
puts "#{x} * #{y} = #{x * y}"
#==#
puts [1, 2, 3, 4].select { |n| n.even? }.inspect
puts [1, 2, 3].map { |n| n * n }.inspect
puts [1, 2, 3, 4, 5].reduce(0) { |a, b| a + b }
puts [3, 1, 2].sort.inspect
puts [1, 2, 2, 3, 3, 3].uniq.inspect
puts [1, [2, [3, 4]]].flatten.inspect
puts [1, 2, 3].include?(2)
puts [10, 20, 30].first
puts [10, 20, 30].last
puts [1, 2, 3].map { |x| x * 10 }.join("-")
#==#
h = { a: 1, b: 2 }
h[:c] = 3
puts h.keys.length
puts h[:b]
puts h.values.sum
puts({ x: 10 }.merge({ y: 20 }).values.sum)
#==#
puts (1..5).to_a.inspect
puts (1...5).to_a.inspect
puts (1..100).sum
puts (1..10).select { |n| n % 3 == 0 }.inspect
#==#
def fib(n)
  n < 2 ? n : fib(n - 1) + fib(n - 2)
end
puts (0..10).map { |i| fib(i) }.join(", ")
#==#
sum = 0
[1, 2, 3, 4].each { |x| sum += x }
puts sum
#==#
def first_even(a)
  a.each { |x| return x if x.even? }
  nil
end
puts first_even([1, 3, 4, 7])
#==#
def add
  yield(2) + yield(3)
end
puts add { |n| n * 10 }
#==#
i = 0
while true
  i += 1
  break if i > 5
end
puts i
#==#
n = 7
case n
when 1..5 then puts "low"
when 6..10 then puts "high"
else puts "other"
end
#==#
[[1, 10], [2, 20]].each { |k, v| puts "#{k}=#{v}" }
#==#
(1..15).each do |n|
  if n % 15 == 0
    puts "FizzBuzz"
  elsif n % 3 == 0
    puts "Fizz"
  elsif n % 5 == 0
    puts "Buzz"
  else
    puts n
  end
end
#==#
puts [5, 3, 8, 1].min
puts [5, 3, 8, 1].max
puts [1, 2, 3, 4].sum
puts ["b", "a", "c"].sort.inspect
#==#
puts "hello world".split(" ").map { |w| w.capitalize }.join(" ")
#==#
a = [10, 20, 30]
a[1] = 99
puts a.inspect
puts a[-1]
#==#
class Point
  attr_accessor :x, :y
  def initialize(x, y)
    @x = x
    @y = y
  end
  def to_s
    "(#{@x}, #{@y})"
  end
end
p1 = Point.new(3, 4)
puts p1.x
p1.x = 10
puts p1
#==#
class Animal
  def speak; "..."; end
  def describe; "I say #{speak}"; end
end
class Dog < Animal
  def speak; "woof"; end
end
puts Dog.new.describe
puts Animal.new.describe
#==#
class Counter
  def initialize; @n = 0; end
  def inc; @n += 1; self; end
  def value; @n; end
end
c = Counter.new
c.inc.inc.inc
puts c.value
#==#
begin
  raise "boom"
rescue => e
  puts "caught: #{e.message}"
end
puts "after"
#==#
class MyError < StandardError; end
begin
  raise MyError, "custom"
rescue MyError => e
  puts "got #{e.message}"
ensure
  puts "cleanup"
end
#==#
def safe_div(a, b)
  a / b
rescue ZeroDivisionError
  0
end
puts safe_div(10, 2)
puts(begin; 1 / 0; rescue ZeroDivisionError; -1; end)
#==#
a, b = 1, 2
a, b = b, a
puts "#{a},#{b}"
x, y, z = [10, 20, 30]
puts x + y + z
#==#
def greet(name = "world")
  "hello, #{name}"
end
puts greet
puts greet("ruby")
#==#
def parse_int(s)
  Integer(s)
rescue
  -1
end
puts parse_int("42")
puts parse_int("abc")
nil.foo rescue puts("rescued a bad call")
#==#
class Stack
  def initialize; @items = []; end
  def push(x); @items.push(x); self; end
  def pop; @items.pop; end
  def size; @items.size; end
end
s = Stack.new
s.push(1).push(2).push(3)
puts s.size
puts s.pop
puts s.size
#==#
class Animal
  def initialize(name); @name = name; end
  def greet; "I am #{@name}"; end
end
class Dog < Animal
  def initialize(name); super(name); @legs = 4; end
  def greet; super + " with #{@legs} legs"; end
end
puts Dog.new("Rex").greet
#==#
module Greetable
  def hello; "hello from #{name}"; end
end
class Person
  include Greetable
  def initialize(n); @n = n; end
  def name; @n; end
end
puts Person.new("Ann").hello
#==#
class Widget
  def self.build(n); new(n); end
  def initialize(n); @n = n; end
  def label; "widget #{@n}"; end
end
puts Widget.build(7).label
#==#
def stats(label, *nums)
  "#{label}: count=#{nums.length} sum=#{nums.sum}"
end
puts stats("scores", 10, 20, 30)
puts stats("empty")
#==#
puts [1, 2, 3].map(&:to_s).inspect
puts [1, 2, 3, 4].select(&:even?).inspect
puts (1..5).map(&:to_s).join(",")
