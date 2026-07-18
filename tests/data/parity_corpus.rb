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
#==#
puts format("%-10s|%5d|%08.2f", "item", 42, 3.14159)
puts "%x %X %o %b" % [255, 255, 64, 10]
puts "total: %+d" % 7
#==#
[1, "two", :three, 4.5, [6]].each do |v|
  case v
  when Integer then puts "int #{v}"
  when String then puts "str #{v}"
  when Float then puts "float #{v}"
  when Array then puts "arr #{v.inspect}"
  else puts "other #{v.inspect}"
  end
end
#==#
puts 5.is_a?(Integer)
puts 5.is_a?(Numeric)
puts "x".is_a?(Comparable)
class Base; end
class Sub < Base; end
puts Sub.new.is_a?(Base)
#==#
puts (1..10).partition(&:even?).inspect
puts [1, 2, 3, 4, 5, 6].group_by { |n| n % 3 }.inspect
puts "mississippi".chars.tally.inspect
puts [1, 2, 3].zip([4, 5, 6]).inspect
puts({ a: 1, b: 2, c: 3 }.transform_values { |v| v * 10 }.inspect)
#==#
acc = [1, 2, 3, 4].each_with_object([]) { |x, memo| memo << x * x }
puts acc.inspect
stack = []
stack << 1 << 2 << 3
puts stack.inspect
#==#
def add(a, b, c); a + b + c; end
nums = [1, 2, 3]
puts add(*nums)
parts = [2, 3]
puts [1, *parts, 4].inspect
first, *rest = [10, 20, 30, 40]
puts "#{first} / #{rest.inspect}"
a, *mid, z = [1, 2, 3, 4, 5]
puts "#{a} #{mid.inspect} #{z}"
#==#
def greet(name:, greeting: "hi")
  "#{greeting}, #{name}"
end
puts greet(name: "Ann")
puts greet(name: "Bob", greeting: "yo")
#==#
def build(width:, height:, label: "box")
  "#{label} #{width}x#{height}"
end
puts build(height: 3, width: 5)
puts build(width: 2, height: 2, label: "square")
#==#
def config(host, port: 80, secure: false)
  scheme = secure ? "https" : "http"
  "#{scheme}://#{host}:#{port}"
end
puts config("example.com")
puts config("example.com", port: 8080, secure: true)
#==#
fruits = %w[apple banana cherry]
puts fruits.length
puts fruits.map(&:upcase).join(", ")
syms = %i[red green blue]
puts syms.inspect
puts %w(one two three).reverse.inspect
#==#
class Version
  include Comparable
  attr_reader :n
  def initialize(n); @n = n; end
  def <=>(other); @n <=> other.n; end
  def to_s; "v#{@n}"; end
end
puts Version.new(1) < Version.new(2)
puts Version.new(3) >= Version.new(3)
puts [Version.new(3), Version.new(1), Version.new(2)].sort.map(&:to_s).join(", ")
puts [Version.new(5), Version.new(2)].min.to_s
#==#
class Vec
  attr_reader :x, :y
  def initialize(x, y); @x = x; @y = y; end
  def +(o); Vec.new(@x + o.x, @y + o.y); end
  def ==(o); @x == o.x && @y == o.y; end
  def to_s; "(#{@x}, #{@y})"; end
end
puts (Vec.new(1, 2) + Vec.new(3, 4)).to_s
puts Vec.new(1, 1) == Vec.new(1, 1)
#==#
puts [3, 1, 2].sort { |a, b| b <=> a }.inspect
puts ["bb", "a", "ccc"].sort_by(&:length).inspect
puts [5, 3, 8, 1].max { |a, b| a <=> b }
#==#
def describe(name, **attrs)
  "#{name}: #{attrs.map { |k, v| "#{k}=#{v}" }.join(", ")}"
end
puts describe("widget", size: 5, color: "red")
puts describe("empty")
#==#
def connect(host:, port: 80, **opts)
  extra = opts.empty? ? "" : " (#{opts.inspect})"
  "#{host}:#{port}#{extra}"
end
settings = { host: "example.com", port: 8080, timeout: 30 }
puts connect(**settings)
puts connect(host: "localhost")
#==#
def with_logging(&block)
  "before / #{block.call} / after"
end
puts with_logging { "work" }
def run
  block_given? ? yield * 2 : -1
end
puts run { 21 }
puts run
#==#
square = ->(x) { x * x }
puts square.call(6)
puts square.(7)
puts square[8]
adder = ->(a, b) { a + b }
puts [1, 2, 3].map { |n| square.call(n) }.inspect
puts adder.call(10, 20)
#==#
total = 0
1.step(20, 4) { |n| total += n }
puts total
puts [1, 2, 3, 4].each_with_object(0) { |x, _| }.inspect rescue puts "ok"
#==#
def make_counter
  count = 0
  increment = -> { count += 1 }
  get = -> { count }
  [increment, get]
end
inc, get = make_counter
inc.call
inc.call
inc.call
puts get.call
#==#
def multiplier(factor)
  ->(x) { x * factor }
end
double = multiplier(2)
triple = multiplier(3)
puts double.call(10)
puts triple.call(10)
adders = (1..3).map { |n| ->(x) { x + n } }
puts adders.map { |f| f.call(100) }.inspect
#==#
n = 99
[1, 2, 3].each { |n| n * 2 }
puts n
running = 0
[10, 20, 30].each { |v| running += v }
puts running
#==#
puts "ruby".center(10, "-")
puts "a-b-c-d".tr("-", ".")
puts "hello".delete("l")
puts "one\ntwo\nthree".lines.inspect
puts "mississippi".count("s")
#==#
config = { db: { host: "localhost", port: 5432 } }
puts config.dig(:db, :host)
puts config.dig(:db, :missing).inspect
nested = [[1, [2, 3]], [4]]
puts nested.dig(0, 1, 1)
#==#
puts [5, 3, 8, 1, 9, 2].min(3).inspect
puts [5, 3, 8, 1, 9, 2].max(2).inspect
puts [1, 2, 3, 4, 5].first(3).inspect
puts [1, 2, 3, 4, 5].last(2).inspect
puts [1, 2, 3, 4].sum { |x| x * x }
result = []
[1, 2, 3, 4].each_cons(2) { |a, b| result << a + b }
puts result.inspect
#==#
puts 255.to_s(16)
puts 10.to_s(2)
puts "ff".to_i(16)
puts "1010".to_i(2)
puts ?A
puts ?z
#==#
puts "Hello World".scan(/\w+/).inspect
puts "a1b2c3".scan(/([a-z])(\d)/).inspect
puts "a1b2".gsub(/\d/, "#")
puts("foo123" =~ /\d+/)
puts "a,b;c".split(/[,;]/).inspect
puts "hello".match?(/l+/)
puts "hello world".gsub(/o/) { |m| m.upcase }
m = "2024-01-15".match(/(\d+)-(\d+)-(\d+)/)
puts m[1]
puts m[2]
puts m[3]
puts m.pre_match.inspect
puts "cat dog bird".scan(/\w+/).map(&:upcase).inspect
puts(/\d+/.match("id 42").to_s)
#==#
puts Array.new(3).inspect
puts Array.new(3, 0).inspect
puts Array.new(4) { |i| i * i }.inspect
h = Hash.new(0)
"aabbbc".each_char { |c| h[c] += 1 }
puts h.inspect
g = Hash.new { |hh, k| hh[k] = [] }
g[:x] << 1
g[:x] << 2
g[:y] << 9
puts g.inspect
puts Hash[[[:a, 1], [:b, 2]]].inspect
puts "path/to/file".partition("/").inspect
puts "path/to/file".rpartition("/").inspect
puts "Hello".casecmp?("HELLO")
puts "mississippi".tr_s("sp", "*")
#==#
"order #4271 shipped" =~ /#(\d+)/
puts $1
puts $~[0]
puts $~.pre_match
"2024-12-25" =~ /(\d+)-(\d+)-(\d+)/
puts "#{$3}/#{$2}/#{$1}"
puts "the quick brown fox".gsub(/(\w)(\w*)/) { $1.upcase + $2 }
puts "hello world".gsub(/\w+/) { $&.capitalize }
"no match here" =~ /\d+/
puts $1.inspect
#==#
r = []
"one1two2three3".scan(/([a-z]+)(\d)/) { |word, num| r << "#{word}:#{num}" }
puts r.inspect
puts "hello world".gsub(/[aeiou]/, "a" => "4", "e" => "3", "o" => "0")
puts "2024".gsub(/\d/, "0" => "zero", "2" => "two")
#==#
puts [1, 2, 3, 4, 5].inject(&:+)
puts [1, 2, 3, 4].reduce(&:*)
puts [1, 2, 3].reduce(100, &:+)
total = ->(*nums) { nums.sum }
puts total.call(10, 20, 30)
puts [[1, 2], [3, 4]].map { |first, *rest| "#{first}|#{rest.inspect}" }.inspect
#==#
puts "2024-01-15".split("-", 2).inspect
puts "key=value=extra".split("=", 2).inspect
puts "a1b2c3".split(/(\d)/).inspect
puts "one two  three".split(" ").inspect
puts "trailing,,,".split(",").inspect
s = "hello world"
removed = s.slice!(0, 5)
puts removed
puts s
puts "café".eql?("café")
#==#
line = "2024-01-15 event"
puts line[..9]
puts line[11..]
nums = [10, 20, 30, 40, 50]
puts nums[2..].inspect
puts nums[..1].inspect
puts (1..).first(5).inspect
puts (100..).take(3).inspect
result = []
(1..).each { |n| break if n > 6; result << n * n }
puts result.inspect
puts (..10).include?(7)
#==#
puts("%2$s, %1$s!" % ["World", "Hello"])
puts("item %1$d costs $%2$.2f (that's %1$d units)" % [3, 4.5])
puts "STRASSE".downcase(:ascii)
puts "hello world".upcase(:ascii)
puts "10:30:45".tr("0-9", "X")
#==#
puts 2 ** 128
puts (1..30).reduce(1) { |a, b| a * b }
puts (2 ** 100).to_s(16)
puts (2 ** 80) % 1000000
big = 10 ** 40
puts big.bit_length
puts (big / 7).to_s
puts 1.0e20
puts 0.00001
#==#
seen = Set.new
["a", "b", "a", "c", "b"].each { |x| seen << x }
puts seen.to_a.inspect
puts seen.size
evens = Set[2, 4, 6]
odds = Set[1, 3, 5]
puts (evens | odds).to_a.sort.inspect
puts (Set[1, 2, 3, 4] & Set[2, 4, 6]).to_a.inspect
puts Set[1, 2, 3].subset?(Set[1, 2, 3, 4])
puts([1, 2, 3, 4] & [3, 4, 5]).inspect if false
puts ([1, 2, 3] | [3, 4]).inspect
#==#
Point = Struct.new(:x, :y)
origin = Point.new(0, 0)
p1 = Point.new(3, 4)
puts p1.x + p1.y
puts p1.to_a.inspect
puts p1.to_h.inspect
puts (p1 == Point.new(3, 4))
puts p1.inspect
Person = Struct.new(:name, :age, keyword_init: true)
alice = Person.new(name: "Alice", age: 30)
puts "#{alice.name} is #{alice.age}"
puts Point.new(1, 2).members.inspect
#==#
sql = <<~SQL
  SELECT name, age
  FROM users
  WHERE active = true
SQL
puts sql
count = 3
report = <<-REPORT
  Total items: #{count}
  Status: OK
REPORT
puts report
puts(<<~A + <<~B)
  first
A
  second
B
#==#
half = Rational(1, 2)
third = Rational(1, 3)
puts (half + third).inspect
puts (half * 6).inspect
puts Rational(10, 4).inspect
puts Rational(22, 7).to_f.round(4)
total = [Rational(1, 2), Rational(1, 3), Rational(1, 6)].reduce(:+)
puts total.inspect
puts (3/4r).inspect
puts Rational(7, 2).to_i
#==#
a = Complex(2, 3)
b = Complex(1, -1)
puts (a + b).inspect
puts (a * b).inspect
puts a.conjugate.inspect
puts a.abs
puts (3 + 4i).inspect
puts (Complex(0, 1) ** 2).inspect
puts [Complex(1, 0), Complex(0, 1), Complex(1, 1)].reduce(:+).inspect
#==#
def describe(shape)
  case shape
  in {type: "circle", radius:}
    "circle area=#{(3.14 * radius * radius).round(2)}"
  in {type: "rect", w:, h:}
    "rect area=#{w * h}"
  in [x, y]
    "point (#{x}, #{y})"
  in Integer => n
    "number #{n}"
  else
    "unknown"
  end
end
puts describe({type: "circle", radius: 2})
puts describe({type: "rect", w: 3, h: 4})
puts describe([5, 6])
puts describe(42)
puts describe("x")
case [1, 2, 3, 4, 5]
in [first, *middle, last]
  puts "#{first} .. #{last}, middle=#{middle.inspect}"
end
#==#
class DynamicConfig
  def initialize; @data = {}; end
  def method_missing(name, *args)
    key = name.to_s
    if key.end_with?("=")
      @data[key[0..-2]] = args.first
    else
      @data[key]
    end
  end
  def respond_to_missing?(name, include_private = false)
    true
  end
end
c = DynamicConfig.new
c.host = "localhost"
c.port = 8080
puts c.host
puts c.port
puts c.respond_to?(:anything)
nums = [3, 1, 2]
puts nums.send(:sort).inspect
puts [1, 2, 3].send(:map, *[]) { |x| x + 10 }.inspect
#==#
class BankAccount
  @@total_accounts = 0
  @@total_balance = 0

  def initialize(balance)
    @balance = balance
    @@total_accounts += 1
    @@total_balance += balance
  end

  def self.stats
    "#{@@total_accounts} accounts, $#{@@total_balance} total"
  end
end
BankAccount.new(100)
BankAccount.new(250)
BankAccount.new(50)
puts BankAccount.stats

class Registry
  ENTRIES = []
  def self.register(name); ENTRIES << name; end
  def self.list; ENTRIES.join(", "); end
end
Registry.register("alpha")
Registry.register("beta")
puts Registry.list
#==#
class Config
  SETTINGS = [:host, :port, :timeout]
  SETTINGS.each do |key|
    define_method(key) { instance_variable_get("@#{key}") }
    define_method("#{key}=") { |val| instance_variable_set("@#{key}", val) }
  end
end
c = Config.new
c.host = "example.com"
c.port = 443
c.timeout = 30
puts "#{c.host}:#{c.port} (#{c.timeout}s)"

class Calculator
  [[:add, :+], [:sub, :-], [:mul, :*]].each do |name, op|
    define_method(name) { |a, b| a.send(op, b) }
  end
end
calc = Calculator.new
puts calc.add(3, 4)
puts calc.sub(10, 3)
puts calc.mul(6, 7)
#==#
class Stack
  def initialize; @items = []; end
  def push(x); @items.push(x); self; end
  def pop; @items.pop; end
  def size; @items.size; end
  alias_method :<<, :push
  alias length size
  alias count size
end
s = Stack.new
s << 1
s << 2
s << 3
puts s.length
puts s.count
puts s.pop
puts s.size
#==#
first_10_squares = (1..).lazy.map { |n| n * n }.first(10)
puts first_10_squares.inspect
primes = (2..).lazy.select { |n| (2...n).none? { |d| n % d == 0 } }.first(8)
puts primes.inspect
pipeline = (1..).lazy.select { |n| n % 3 == 0 }.map { |n| n * n }.take_while { |sq| sq < 500 }.to_a
puts pipeline.inspect
#==#
total = (1..100)
  .select { |n| n.even? }
  .map { |n| n * n }
  .reduce(0) { |acc, n| acc + n }
puts total
puts Float::INFINITY
puts(-Float::INFINITY < 0)
squares = (1..Float::INFINITY)
  .lazy
  .map { |n| n * n }
  .take_while { |sq| sq < 100 }
  .to_a
puts squares.inspect
#==#
def find_user(id)
  id == 1 ? {name: "Alice", address: {city: "NYC"}} : nil
end
u = find_user(1)
puts u&.fetch(:name)
puts u&.fetch(:address)&.fetch(:city)
missing = find_user(99)
puts missing&.fetch(:name).inspect
puts missing&.fetch(:address)&.fetch(:city).inspect
config = {timeout: 30}
puts config&.fetch(:timeout)
puts config[:retries]&.to_s.inspect
#==#
def greet(name:, greeting: "hi")
  "#{greeting}, #{name}"
end
puts greet name: "Ann"
puts greet greeting: "yo", name: "Bob"
def opts(x, **rest)
  "#{x} #{rest.inspect}"
end
puts opts 5, a: 1, b: 2
defaults = {color: "red", size: 10}
puts opts 9, **defaults
def total(*nums)
  nums.sum
end
puts total *[4, 5, 6]
#==#
e = [10, 20, 30].each
puts e.next
puts e.next
puts e.peek
puts e.next
e.rewind
puts e.next
puts e.size
begin
  e.next
  e.next
  e.next
  e.next
rescue StopIteration => err
  puts err.message
end
letters = %w[a b c].each_with_index
p letters.next
p letters.next
squares = [1, 2, 3, 4].map
puts squares.next
puts squares.each_with_index.map { |x, i| x + i }.inspect
#==#
scores = [85, 92, 78, 90]
labeled = scores.map.with_index(1) { |s, i| "##{i}: #{s}" }
puts labeled.inspect
evens_at_even = [10, 20, 30, 40].select.with_index { |x, i| i.even? }
puts evens_at_even.inspect
kept = [10, 20, 30, 40].reject.with_index { |x, i| x > 25 }
puts kept.inspect
sum = [1, 2, 3, 4].each.with_object({ total: 0 }) { |x, h| h[:total] += x }
puts sum.inspect
[100, 200].each.with_index(10) { |v, i| puts "#{i} -> #{v}" }
#==#
launch = Time.utc(2001, 9, 9, 1, 46, 40)
puts launch.to_i
puts launch.strftime("%Y-%m-%d %H:%M:%S %Z")
puts launch.strftime("%A %B %-d")
deadline = launch + (7 * 24 * 3600)
puts deadline.strftime("%F")
puts (deadline - launch)
epochs = [1_600_000_000, 1_500_000_000, 1_700_000_000]
times = epochs.map { |e| Time.at(e).utc }
puts times.sort.map(&:year).inspect
t = Time.at(0).utc
puts [t.year, t.month, t.day, t.wday, t.yday].inspect
newyear = Time.gm(2024, 1, 1, 0, 0, 0)
puts newyear.strftime("%j %u")
#==#
require "date"
launch = Date.new(2024, 7, 4)
puts launch.to_s
puts launch.strftime("%A, %B %-d, %Y")
puts launch.wday
puts launch.leap?
deadline = launch >> 2
puts deadline.to_s
puts (deadline - launch).to_i
eom = Date.new(2024, 1, 31)
puts eom.next_month.to_s
puts eom.next_year.to_s
dates = [Date.new(2024, 3, 15), Date.new(2024, 1, 5), Date.new(2024, 2, 20)]
puts dates.sort.map(&:iso8601).inspect
puts Date.parse("2000-02-29").leap?
puts Date.new(2024, 12, 31).yday
#==#
def try(&blk)
  blk.call
rescue NoMethodError => e
  e.message
end
puts try { "hello".no_such_method }
puts try { 42.no_such_method }
puts try { [1, 2, 3].no_such_method }
puts try { {a: 1}.no_such_method }
puts try { :sym.no_such_method }
puts try { (1..10).no_such_method }
puts try { nil.no_such_method }
puts try { true.no_such_method }
puts try { Integer.no_such_method }
begin
  nil.upcase
rescue NoMethodError => e
  puts "#{e.class}: #{e.message}"
end
#==#
word = "Ruby"
puts word.each_char.to_a.inspect
puts word.each_char.map { |c| c.ord }.inspect
enum = word.each_char
puts "#{enum.next}#{enum.next}"
text = "one\ntwo\nthree"
puts text.each_line.map(&:chomp).inspect
puts "abc".each_byte.to_a.inspect
squares = 5.times.map { |i| i * i }
puts squares.inspect
puts 1.upto(5).select(&:even?).inspect
puts 10.step(2, -2).to_a.inspect
counter = 3.times
puts [counter.next, counter.next].inspect
puts "hello".each_char.with_index.map { |c, i| "#{i}:#{c}" }.inspect
#==#
pairs = [[1, 2], [3, 4], [5, 6]]
puts pairs.map { |(a, b)| a * b }.inspect
puts pairs.each_with_index.map { |(a, b), i| "#{i}:#{a + b}" }.inspect
nested = [[1, [2, 3]], [4, [5, 6]]]
puts nested.map { |(a, (b, c))| a + b + c }.inspect
prices = {apple: 3, banana: 2}
total = prices.each_with_object([]) { |(name, cost), lines| lines << "#{name}: $#{cost}" }
puts total.inspect
puts prices.map { |(k, v)| "#{k}=#{v}" }.inspect
rows = [[10, 20, 30], [40, 50, 60]]
puts rows.map { |(first, *others)| [first, others.sum] }.inspect
add = ->((x, y)) { x + y }
puts add.call([7, 8])
grouped = [[:a, 1], [:a, 2], [:b, 3]]
puts grouped.each_with_object(Hash.new { |h, k| h[k] = [] }) { |(key, val), acc| acc[key] << val }.inspect
#==#
rows = [[1, 2, 3], [4, 5, 6], [7, 8, 9]]
puts rows.map(&:sum).inspect
puts rows.map(&:max).inspect
puts rows.map(&:min).inspect
words = [["hello", "world"], ["foo", "bar"]]
puts words.map(&:join).inspect
puts (1..12).each_slice(4).map(&:sum).inspect
puts "abcdefgh".each_char.each_slice(2).map(&:join).inspect
puts [1, 2, 3, 4, 5].reduce(&:+)
puts [2, 3, 4].reduce(&:*)
puts [[3, 1], [2, 4], [5, 0]].map(&:min).inspect
puts [[3, 1], [2, 4]].sort_by(&:first).inspect
#==#
puts (1.0..2.0).step(0.5).to_a.inspect
puts (0.0..1.0).step(0.25).to_a.inspect
puts (1..3).step(0.5).to_a.inspect
puts (1.5..4.5).include?(3.2)
puts (1.0...2.0).exclude_end?
puts((1.5..4.5) === 3.0)
puts((1..10) === 5)
puts(Integer === 42)
puts(Float === 3.14)
puts(/\d+/ === "abc123")
temps = [-5.0, 0.0, 15.0, 25.0, 40.0]
temps.each do |t|
  label = case t
          when -100.0...0.0 then "freezing"
          when 0.0..20.0 then "cold"
          when 20.0..30.0 then "mild"
          else "hot"
          end
  puts "#{t}: #{label}"
end
#==#
inventory = {apples: 30, bananas: 12, cherries: 45}
total = inventory.reduce(0) { |sum, (name, count)| sum + count }
puts total
low_stock = inventory.find_all { |name, count| count < 20 }
puts low_stock.inspect
by_parity = inventory.group_by { |name, count| count.even? ? :even : :odd }
puts by_parity.inspect
big, small = inventory.partition { |name, count| count >= 30 }
puts big.inspect
puts small.inspect
counts = Hash.new(0)
"mississippi".each_char { |c| counts[c] += 1 }
puts counts.inspect
puts counts.default
seen = {}
seen.default = "unseen"
puts seen[:missing]
puts inventory.inject(:apples => 0) { |acc, (k, v)| acc }.inspect
#==#
puts 0.5.to_r.inspect
puts 0.1.to_r.inspect
puts 3.to_r.inspect
puts "22/7".to_r.inspect
puts "3.14159".to_r.inspect
puts 3.14159.rationalize(0.001).inspect
puts 0.3.rationalize.inspect
puts 42.to_c.inspect
puts (0.25.to_r + 0.5.to_r).inspect
puts ("1/6".to_r + "1/3".to_r).inspect
puts nil.to_a.inspect
prices = [1.5, 2.25, 0.75]
exact = prices.map(&:to_r)
puts exact.inspect
puts exact.sum.inspect
