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
#==#
values = [42, 3.14, "hello", :sym, [1, 2], {a: 1}, nil, true]
values.each { |v| puts v.class }
puts values.map { |v| v.class.name }.inspect
puts(5.class == Integer)
puts("x".class == String)
puts(5.class == Float)
class Animal; end
class Dog < Animal; end
d = Dog.new
puts d.class
puts(d.class == Dog)
puts d.class.name
begin
  Integer("not a number")
rescue => e
  puts "#{e.class}: caught"
end
puts values.map(&:class).uniq.length
puts values.count { |v| v.class == Integer }
#==#
puts Integer.ancestors.inspect
puts String.superclass
puts Float.superclass
puts(Integer < Numeric)
puts(Integer < Comparable)
puts(String < Numeric).inspect
class Shape; def area; 0; end; end
class Circle < Shape; end
class Square < Shape; end
puts Circle.superclass
puts Circle.ancestors.inspect
puts(Circle < Shape)
puts(Shape > Circle)
puts(Circle < Square).inspect
module Drawable; end
class Sprite; include Drawable; end
puts Sprite.ancestors.inspect
puts Sprite.ancestors.include?(Drawable)
puts [Integer, Float, Numeric].sort_by { |c| c.ancestors.length }.map(&:name).inspect
#==#
data = [1, 2.5, "hello", :sym, 3, "world", 4.0, :other]
grouped = data.group_by(&:class)
grouped.each { |cls, vals| puts "#{cls}: #{vals.inspect}" }
puts grouped.keys.map(&:name).sort.inspect
tally = Hash.new(0)
data.each { |v| tally[v.class] += 1 }
puts tally.map { |cls, n| "#{cls}=#{n}" }.sort.inspect
type_map = {Integer => :whole, Float => :decimal, String => :text}
puts data.map { |v| type_map[v.class] || :unknown }.inspect
puts({Integer => 1, Float => 2}.fetch(2.0.class))
#==#
grid = {}
grid[[0, 0]] = "origin"
grid[[1, 2]] = "point"
grid[[1, 2]] = "updated"
puts grid[[1, 2]]
puts grid.size
puts grid.keys.inspect
moves = [[0, 1], [1, 0], [0, 1], [-1, 0], [1, 0]]
freq = Hash.new(0)
moves.each { |m| freq[m] += 1 }
puts freq.sort_by { |k, v| [-v, k] }.inspect
buckets = {(0..9) => "low", (10..19) => "mid", (20..29) => "high"}
puts buckets[(10..19)]
puts buckets.keys.inspect
paths = [[1, 2, 3], [1, 2, 3], [4, 5]]
puts paths.group_by(&:itself).transform_values(&:size).inspect
memo = {}
fib = lambda do |n|
  memo[[:fib, n]] ||= n < 2 ? n : fib.call(n - 1) + fib.call(n - 2)
end
puts fib.call(10)
puts memo.size
#==#
matrix = [[1, 2, 3], [4, 5, 6], [7, 8, 9]]
puts matrix.transpose.inspect
puts matrix.map { |row| row.sum }.inspect
words = ["apple", "banana", "", "cherry", nil, "date"]
puts words.filter_map { |w| w.upcase if w && !w.empty? }.inspect
log = "2024-03-15 ERROR failed; 2024-03-16 WARN retry"
puts log.scan(/\d{4}-\d{2}-\d{2}/).inspect
puts log[/ERROR|WARN/]
puts log[/(\d{4})-(\d{2})/, 1]
flags = [0b001, 0b010, 0b100, 0b111]
puts flags.map { |f| f.to_s(2) }.inspect
puts flags.sum
perms = 0o755
puts perms
puts 0xCAFE
mask = 0xFF & 0b1010_1010
puts mask
#==#
name = "Ruby"
puts %q(literal #{name} stays)
puts %Q(interpolated #{name} works)
puts %w[red green blue].map(&:upcase).inspect
tokens = "foo=1; bar=2; baz=3".scan(%r{(\w+)=(\d+)})
puts tokens.inspect
puts %q{path: /usr/local/bin}
labels = %i[alpha beta gamma]
puts labels.inspect
text = "The year 2024 and month 03"
puts text.scan(%r{\d+}).inspect
puts(%r{^\d{4}$}.match?("2024"))
puts %s(symbol).class
#==#
def celsius_to_f(c) = c * 9.0 / 5 + 32
puts celsius_to_f(100)
puts celsius_to_f(0)
def factorial(n) = n <= 1 ? 1 : n * factorial(n - 1)
puts factorial(6)
class Point
  def initialize(x, y)
    @x = x
    @y = y
  end
  def sum = @x + @y
  def to_s = "(#{@x}, #{@y})"
end
pt = Point.new(3, 4)
puts pt.sum
puts pt
def pipeline(x) = [x, x * 2, x * 3]
puts (1..3).flat_map { |n| pipeline(n) }.inspect
class Calc
  def self.pi = 3.14159
  def square(n) = n * n
end
puts Calc.pi
puts Calc.new.square(9)
#==#
def distance(x1, y1, x2, y2) = Math.sqrt((x2 - x1) ** 2 + (y2 - y1) ** 2)
puts distance(0, 0, 3, 4)
puts distance(1, 1, 4, 5)
radius = 10
puts (Math::PI * radius ** 2).round(4)
puts (2 * Math::PI * radius).round(4)
angles = [0, 30, 60, 90]
sines = angles.map { |deg| (Math.sin(deg * Math::PI / 180)).round(4) }
puts sines.inspect
puts Math.log(1000, 10).round(6)
puts Math.hypot(5, 12)
puts [1, 2, 4, 8, 16].map { |n| Math.log2(n).to_i }.inspect
compound = 1000 * Math::E ** (0.05 * 10)
puts compound.round(2)
#==#
puts defined?(puts)
puts defined?(String).inspect
puts defined?(NoSuchThing).inspect
count = 0
puts defined?(count)
puts defined?(@missing).inspect
CONFIG = {debug: true}
puts defined?(CONFIG)
result = defined?(Integer) ? "has Integer" : "no Integer"
puts result
[:puts, :nope, :require].each do |m|
  puts "#{m}: #{defined?(m) ? 'sym' : 'sym'}"
end
def check(x) = defined?(x) ? "defined" : "undefined"
puts check(42)
puts defined?(1 + 2 * 3)
puts defined?({a: 1}).inspect
#==#
puts "tab\tsep\tvalues".inspect
puts "line1\nline2".inspect
puts "esc\e[1mbold".inspect
puts "bell\a and null\x00".inspect
puts "ctrl\x01\x1f\x7f".inspect
puts "literal \#{not_interp}".inspect
puts "unicode: café ☃".inspect
puts ["a\tb", "c\nd", "e\x00f"].inspect
puts({"key\t1" => "val\e2"}.inspect)
data = "field1\x1ffield2\x1efield3"
puts data.inspect
puts data.split("\x1f").inspect
#==#
puts(-7.abs)
puts(-2.abs)
puts(-2**2)
puts(-2.abs**2)
x = 1/0 rescue 99
puts x
puts((1/0 rescue 42))
#==#
puts %(hi #{1 + 1})
puts %{braces}
puts %[brackets]
v = 10
puts v % 3
puts 1
__END__
this is data, ignored
#==#
module Greet
  def hello
    "hi"
  end
end
class C
  extend Greet
end
puts C.hello
module Loud
  def shout
    super.upcase
  end
end
class D
  prepend Loud
  def shout
    "quiet"
  end
end
puts D.new.shout
class E
  class << self
    def build
      "built"
    end
  end
end
puts E.build
#==#
module P
  def who
    "P(" + super + ")"
  end
end
module M
  def who
    "M"
  end
end
class C
  prepend P
  include M
end
puts C.new.who
puts C.ancestors.map(&:to_s).inspect
#==#
case [1, 2, 3, 4, 5]
in [*a, 3, *b]
  puts [a, 3, b].inspect
end
case {a: 1}
in {a:, **nil}
  puts a
end
case {a: 1, b: 2}
in {a:, **nil}
  puts "closed"
else
  puts "open"
end
case 5
in Integer | Float => n
  puts n
end
case 2
in 1 | 2
  puts "alt"
end
#==#
puts 3.pow(4, 5)
puts 2.pow(10, 1000)
puts 15.clamp(..10)
puts 1.clamp(3..)
puts 5.clamp(3..10)
srand(1)
a = rand
srand(1)
b = rand
puts a == b
begin
  3.pow(-1, 7)
rescue RangeError
  puts "range error"
end
#==#
def f(in: 5); :ok; end
p f
def g(**o); o; end
p g(class: 1, if: 2)
p({if: 1, class: 2, end: 3})
#==#
class P
  def deconstruct
    [1, 2, 3, 4, 5]
  end
  def deconstruct_keys(keys)
    {x: 1, y: 2, z: 3}
  end
end
case P.new
in [a, b, *rest]
  p [a, b, rest]
end
case P.new
in [*pre, 3, *post]
  p [pre, post]
end
case P.new
in {x:, **others}
  p [x, others]
end
case 5
in [a, b]
  p :arr
in {a:}
  p :hash
else
  p :fell_through
end
#==#
require "date"
d = DateTime.new(2020, 1, 1, 12, 30, 45)
puts d.to_s
puts d.iso8601
puts d.inspect
puts d.strftime("%Y-%m-%dT%H:%M:%S")
puts [d.year, d.month, d.day, d.hour, d.min, d.sec, d.wday, d.yday, d.jd, d.leap?].inspect
puts (d + 1).to_s
puts (d >> 1).to_s
puts (d << 2).to_s
puts (DateTime.new(2020, 1, 5) - d).to_s
puts d.to_date.to_s
puts DateTime.parse("2019-06-15T08:00:00").to_s
puts [d, DateTime.new(2019, 1, 1), DateTime.new(2020, 1, 5)].sort.map(&:to_s).inspect
puts d.is_a?(Date)
#==#
p Enumerator.new { |y| y << 1; y << 2; y << 3 }.to_a
p Enumerator.new { |y| y.yield(10); y.yield(20) }.to_a
p Enumerator.new { |y| y << 100; y << 200; y << 300 }.first(2)
fib = Enumerator.new { |y| a, b = 0, 1; loop { y << a; a, b = b, a + b } }
p fib.first(10)
g = Enumerator.new { |y| n = 0; loop { y << n; n += 1 } }
p g.lazy.map { |x| x * x }.select { |x| x.even? }.first(4)
p [1, 2, 3].cycle(3).to_a
p Enumerator.new { |y| y << :a; y << :b }.map { |s| s.to_s.upcase }
#==#
def wrap; "[" + yield + "]"; end
p wrap { "core" }
def tight; "<"+yield+">"; end
p tight { "x" }
p [:mm, :bb, :a].sort
p %i[banana apple cherry].sort
S = Struct.new(:x, :y)
pt = S.new(3, 4)
case pt
in [a, b]
  p [a, b]
end
case pt
in {x:, y:}
  p({x: x, y: y})
end
p pt.deconstruct_keys([:y, :x])
class Animal
  def speak; end
  def name; end
end
class Dog < Animal
  def bark; end
end
p Dog.instance_methods(false).sort
p Dog.method_defined?(:speak)
p Dog.method_defined?(:bark)
p Dog.method_defined?(:meow)
#==#
require "json"
puts JSON.generate({"name" => "rubylang", "nums" => [1, 2, 3], "nested" => {"ok" => true, "x" => nil}})
puts({lang: "ruby", version: 4, tags: ["fast", "compiled"]}.to_json)
puts [1, "two", 3.5, true, false, nil].to_json
data = JSON.parse('{"a":1,"b":[2,3],"c":{"d":"e"}}')
p data
p data["b"]
sym = JSON.parse('{"k":[1,2]}', symbolize_names: true)
p sym
puts JSON.parse(JSON.generate({"round" => ["trip", 42]})).inspect
puts JSON.pretty_generate({"a" => 1, "b" => [1, 2]})
begin
  JSON.parse("{bad}")
rescue => e
  puts e.class.name
end
#==#
f = Fiber.new { Fiber.yield(1); Fiber.yield(2); 3 }
p [f.resume, f.resume, f.resume]
gen = Fiber.new do
  n = 1
  loop { Fiber.yield(n * n); n += 1 }
end
p (1..5).map { gen.resume }
producer = Fiber.new do |start|
  acc = start
  3.times { acc = Fiber.yield(acc * 2) }
  acc
end
p producer.resume(5)
p producer.resume(10)
p producer.resume(100)
p producer.resume(7)
fib = Fiber.new { Fiber.yield(:only) }
p fib.alive?
fib.resume
fib.resume
p fib.alive?
begin
  fib.resume
rescue FiberError
  puts "dead"
end
#==#
p [1, 2, 3, 4].map { |x| break x * 100 if x == 3; x }
p [10, 20, 30, 40].inject { |a, b| break :halted if b == 30; a + b }
p(loop { break "done" })
p [1, 2, 3].to_h { |x| [x, x.to_s] }
p({a: 1, b: 2}.merge({b: 10, c: 3}) { |key, old, new| old + new })
p({x: 1, y: 2}.transform_keys({x: :a, y: :b}))
p [1, 2, 3, 4].lazy.zip([:a, :b, :c, :d]).map { |n, s| "#{n}#{s}" }.first(3)
p((1..Float::INFINITY).lazy.zip(["x", "y"]).first(3))
p 42.object_id
p [true.object_id, false.object_id, nil.object_id]

# ── differential-fuzz regression cases (arith/reduce/slice/sort/symbols) ──
#==#
h = { Lang: -1 }; p h[:Lang]
#==#
h = { Lang: -3 }; p h[:Lang]
#==#
h = { Lang: -7 }; p h[:Lang]
#==#
h = { Lang: 0 }; p h[:Lang]
#==#
h = { Lang: 1 }; p h[:Lang]
#==#
h = { Lang: 10 }; p h[:Lang]
#==#
h = { Lang: 100 }; p h[:Lang]
#==#
h = { Lang: 2 }; p h[:Lang]
#==#
h = { Lang: 42 }; p h[:Lang]
#==#
h = { Lang: 5 }; p h[:Lang]
#==#
h = { Lang: 7 }; p h[:Lang]
#==#
h = { Lang: 9 }; p h[:Lang]
#==#
h = { Ruby: -1 }; p h[:Ruby]
#==#
h = { Ruby: -3 }; p h[:Ruby]
#==#
h = { Ruby: -7 }; p h[:Ruby]
#==#
h = { Ruby: 0 }; p h[:Ruby]
#==#
h = { Ruby: 1 }; p h[:Ruby]
#==#
h = { Ruby: 10 }; p h[:Ruby]
#==#
h = { Ruby: 100 }; p h[:Ruby]
#==#
h = { Ruby: 2 }; p h[:Ruby]
#==#
h = { Ruby: 42 }; p h[:Ruby]
#==#
h = { Ruby: 5 }; p h[:Ruby]
#==#
h = { Ruby: 7 }; p h[:Ruby]
#==#
h = { Ruby: 9 }; p h[:Ruby]
#==#
p --1 ** -7
#==#
p --7 ** 42
#==#
p -1 ** -1 % 5
#==#
p -1 ** -3
#==#
p -1 ** -7
#==#
p -10 ** -1
#==#
p -10 ** -3
#==#
p -100 ** -7
#==#
p -100 ** 10
#==#
p -100 ** 100
#==#
p -100 ** 42
#==#
p -2 ** -7
#==#
p -2 ** 100
#==#
p -3 - 1 ** -1
#==#
p -3 ** -7 - 100
#==#
p -3 ** -7 % 100
#==#
p -3 ** -7 % 42
#==#
p -3 / 42 ** 42
#==#
p -3 % -7 ** 100
#==#
p -42 ** -1
#==#
p -42 ** -7
#==#
p -5 ** -3
#==#
p -5 ** -7
#==#
p -7 ** -1
#==#
p -7 ** -1 ** 9
#==#
p -7 ** -7
#==#
p -7 + -7 ** -7
#==#
p -9 ** 100
#==#
p "abc"[-4, 1]
#==#
p "abc"[-4, 2]
#==#
p "abc"[-4, 3]
#==#
p "abc"[-4, 4]
#==#
p "bar"[-4, 0]
#==#
p "bar"[-4, 2]
#==#
p "bar"[-4, 3]
#==#
p "baz"[-4, 0]
#==#
p "baz"[-4, 1]
#==#
p "baz"[-4, 4]
#==#
p "foo"[-4, 0]
#==#
p "foo"[-4, 3]
#==#
p "foo"[-4, 4]
#==#
p "xyz"[-4, 0]
#==#
p "xyz"[-4, 2]
#==#
p "xyz"[-4, 4]
#==#
p (-3 ** -7) % 42
#==#
p (-3 ** 100) ** 0
#==#
p (-3 ** 100) ** 1
#==#
p (-3 ** 100) / -7
#==#
p (-3 ** 42) % 2
#==#
p (-3 + 2) ** -3
#==#
p (-7 ** -1) ** 0
#==#
p (-7 ** -1) / 5
#==#
p (-7 ** 42) + -7
#==#
p (1 - 0) ** -3
#==#
p (1 ** -7) - -1
#==#
p (1 ** 9) ** -7
#==#
p (1..21).reduce(1, :*)
#==#
p (1..22).reduce(1, :*)
#==#
p (1..23).reduce(1, :*)
#==#
p (1..25).reduce(1, :*)
#==#
p (1..26).reduce(1, :*)
#==#
p (1..27).reduce(1, :*)
#==#
p (1..28).reduce(1, :*)
#==#
p (1..29).reduce(1, :*)
#==#
p (1..30).reduce(1, :*)
#==#
p (1..31).reduce(1, :*)
#==#
p (1..32).reduce(1, :*)
#==#
p (1..33).reduce(1, :*)
#==#
p (1..34).reduce(1, :*)
#==#
p (1..35).reduce(1, :*)
#==#
p (1..36).reduce(1, :*)
#==#
p (1..37).reduce(1, :*)
#==#
p (1..39).reduce(1, :*)
#==#
p (1..40).reduce(1, :*)
#==#
p (100 ** -1) % 1
#==#
p (100 ** -3) % -1
#==#
p [-1, 0, 7, 2, -7].max_by { |x| x.abs }
#==#
p [-3, -3, -3, 7, -7].max_by { |x| x.abs }
#==#
p [-3, -7, 7, 1, 5].max_by { |x| x.abs }
#==#
p [-7, 7, -1].max_by { |x| x.abs }
#==#
p [-7, 7, -3].max_by { |x| x.abs }
#==#
p [-7, 7, 0, -3].max_by { |x| x.abs }
#==#
p [-7, 7, 7].max_by { |x| x.abs }
#==#
p [1, 0, 7, 2, -7].max_by { |x| x.abs }
#==#
p [2, -1, 7, -7].max_by { |x| x.abs }
#==#
p [2, -7, 7].max_by { |x| x.abs }
#==#
p [5, -7, -1, 7].max_by { |x| x.abs }
#==#
p [5, -7, 7, 2, -1, -3].max_by { |x| x.abs }
#==#
p 0 - -1 ** -3
#==#
p 1 ** -3 % 2
#==#
p 1 ** -7 + 2
#==#
p 1 ** 7 ** -3
#==#
p 1 % 42 ** 100
#==#
p 1 + -1 ** -3
#==#
p 10 ** 42 ** -7
#==#
p 10 + -7 ** 100
#==#
p 42 ** -3 % 2
#==#
p 42 % 7 ** -7
#==#
p 5 - 1 ** -3
#==#
p 7 * 1 ** -1
#==#
p 7 % 1 ** -1
#==#
p 7 % 42 ** 100
#==#
p 9 ** -1 % 9
#==#
p 9 ** 10 ** 100
#==#
p (-1.5 / 0.0).round(0)
#==#
p (1.0 / 0.0).round(0)
#==#
p (10.0 / 0.0).round(0)
#==#
p 1e15
#==#
# ── intmeth / enumerable / exceptions / struct / rational / pattern-match / kernel-conv ──
p 48.gcd(36)
#==#
p 4.lcm(6)
#==#
p 100.divmod(7)
#==#
p 255.to_s(16)
#==#
p (-42).abs.digits
#==#
p 5.pow(3, 13)
#==#
p 12.bit_length
#==#
p [10.even?, 7.odd?, 0.zero?]
#==#
p [1, 2, 3, 4, 5].each_slice(2).to_a
#==#
p [1, 2, 3, 4].each_cons(2).to_a
#==#
p [1, -2, 3, -4].partition { |x| x > 0 }
#==#
p [1, 2, 3, 4, 5, 6].group_by { |x| x % 3 }
#==#
p [1, 2, 3].flat_map { |x| [x, -x] }
#==#
p [3, 1, 3, 2, 1].tally
#==#
p [1, 2, 2, 3, 3, 3].chunk_while { |a, b| a == b }.to_a
#==#
p (begin; raise "boom"; rescue => e; e.message; end)
#==#
p (begin; 1 / 0; rescue ZeroDivisionError => e; e.message; end)
#==#
p (begin; Integer("nope"); rescue ArgumentError; :caught; end)
#==#
r = []; begin; r << 1; raise "x"; rescue; r << 2; ensure; r << 3; end; p r
#==#
class E1 < StandardError; end; p (begin; raise E1; rescue E1; :custom; end)
#==#
S = Struct.new(:a, :b); p S.new(1, 2).to_a
#==#
S2 = Struct.new(:a, :b); p S2.new(1, 2).to_h
#==#
S3 = Struct.new(:x, :y); p(S3.new(1, 2) == S3.new(1, 2))
#==#
S4 = Struct.new(:a, keyword_init: true); p S4.new(a: 9).a
#==#
p(Rational(1, 3) + Rational(1, 6))
#==#
p(Rational(3, 4) * Rational(2, 9))
#==#
p(Rational(7, 2) % Rational(1, 3))
#==#
p(Rational(2, 3) ** -2)
#==#
p(5 / Rational(2, 3))
#==#
p [Rational(6, 4).numerator, Rational(6, 4).denominator]
#==#
case [1, 2]; in [a, b]; p a + b; end
#==#
case {name: "Ann", age: 30}; in {name: String => s}; p s; end
#==#
case [1, 2, 3, 4]; in [_, _, *rest]; p rest; end
#==#
p Integer("ff", 16)
#==#
p Integer("1010", 2)
#==#
p Float("3.14")
#==#
p Array(nil)
#==#
p Array([1, 2])
#==#
p format("%05.2f", 3.14159)
#==#
# ── regex backreferences + look-around (fancy-regex engine) ──
p "hello".gsub(/([a-z])\1/, "D")
#==#
p "aabbcc".scan(/(.)\1/)
#==#
p "Mississippi".gsub(/(\w)\1/, "-")
#==#
p "committee".scan(/(.)\1/)
#==#
p "foobar".gsub(/o(?=b)/, "0")
#==#
p "banana".gsub(/a(?=n)/, "A")
#==#
p "abcabc".match?(/(abc)\1/)
#==#
p "noon".gsub(/(\w)(\w)\2\1/, "P")
#==#
# ── Ruby 3 argument forwarding `...` (def + call) ──
def fwd_g(a, b:, &blk); r = a + b; blk ? blk.call(r) : r; end
def fwd_h(...); fwd_g(...); end
p fwd_h(1, b: 2)
#==#
def fwd_g2(a, b:, &blk); r = a + b; blk ? blk.call(r) : r; end
def fwd_h2(...); fwd_g2(...); end
p fwd_h2(1, b: 2) { |x| x * 10 }
#==#
def fwd_lead(first, ...); [first, fwd_sum(...)]; end
def fwd_sum(*a, **k); a.sum + k.values.sum; end
p fwd_lead(0, 1, 2, x: 3)
#==#
# ── String#encoding (UTF-8 only) ──
p "café".encoding.name
#==#
p "abc".encoding.to_s
#==#
p "x".encoding.inspect
#==#
# ── respond_to_missing? default via super ──
class RtmD
  def respond_to_missing?(n, priv = false); n.to_s.start_with?("q_") || super; end
end
d = RtmD.new
p [d.respond_to?(:q_x), d.respond_to?(:nope)]
#==#
# ── bound Kernel method (method(:name) over Kernel private methods) ──
method(:puts).call("bound-puts")
#==#
m = method(:format); p m.call("%05.2f", 3.5)
#==#
# ── Enumerable#cycle without a block (endless Enumerator) ──
p [1, 2, 3].cycle.first(7)
#==#
p [1, 2].cycle.take(5)
#==#
c = %w[a b].cycle; p [c.next, c.next, c.next]
#==#
p [1, 2, 3].cycle.lazy.map { |x| x + 10 }.first(4)
#==#
p [].cycle.first(3)
#==#
# ── StringIO reader methods (readlines / each_line / getc) ──
require "stringio"
io = StringIO.new("a\nb\nc\n"); p io.readlines
#==#
require "stringio"
io = StringIO.new("a\nb\nc\n"); p io.each_line.to_a
#==#
require "stringio"
io = StringIO.new("héllo"); p [io.getc, io.getc]
#==#
# ── block passed by value: &blk forwarding keeps block_given? faithful ──
def bpv_outer(&blk); bpv_inner(&blk); end
def bpv_inner; block_given? ? yield(5) : -1; end
p [bpv_outer { |x| x + 1 }, bpv_outer]
#==#
def bpv_map(&b); [1, 2].map(&b); end
p bpv_map { |x| x * 2 }
#==#
def bpv_map2(&b); [1, 2].map(&b); end
p bpv_map2
#==#
def bpv_pairs(&b); { a: 1, b: 2 }.map(&b); end
p bpv_pairs { |k, v| "#{k}=#{v}" }
#==#
p [1, 2, 3].map(&nil)
#==#
sq = ->(x) { x * x }
p [1, 2, 3].map(&sq)
#==#
# ── Array#replace / #clear (in-place) ──
a = [1, 2, 3]; a.replace([9, 8]); p a
#==#
a = [1, 2, 3]; a.clear; p a
#==#
# ── bundled stdlib: uri ──
require "uri"
u = URI.parse("https://user@host.com:9000/a/b?x=1&y=2#frag")
p [u.scheme, u.userinfo, u.host, u.port, u.path, u.query, u.fragment]
#==#
require "uri"
p [URI.parse("http://h.com/a").port, URI.parse("https://h.com/a").port]
#==#
require "uri"
p URI.parse("https://h.com/a/b?c=1").to_s
#==#
require "uri"
p URI.encode_www_form({"a" => "1", "b" => "hello world"})
#==#
require "uri"
p URI.decode_www_form("a=1&b=hello+world")
#==#
require "uri"
p URI("https://x.com").class.to_s
#==#
# ── bundled stdlib: csv ──
require "csv"
p CSV.parse("a,b,c\n1,2,3")
#==#
require "csv"
p CSV.parse(%Q{a,"b,c",d\n1,"x\ny",3})
#==#
require "csv"
p CSV.generate_line(["a", "b,c", "d"])
#==#
require "csv"
s = CSV.generate { |c| c << [1, 2]; c << ["x", "y,z"] }; p s
#==#
# ── bundled stdlib: optparse ──
require "optparse"
o = {}
op = OptionParser.new { |x| x.on("-v", "--verbose") { o[:v] = true }; x.on("--name NAME") { |n| o[:name] = n } }
argv = ["-v", "--name", "bob", "file.txt"]
op.parse!(argv)
p [o, argv]
#==#
require "optparse"
o = {}
OptionParser.new { |x| x.on("--count N", Integer) { |n| o[:c] = n } }.parse!(a = ["--count=5"])
p o
#==#
# ── bundled stdlib: yaml (dump + load round-trip) ──
require "yaml"
p YAML.dump({"a" => 1, "b" => [1, 2]})
#==#
require "yaml"
p YAML.load("---\na: 1\nb:\n- 1\n- 2\n")
#==#
require "yaml"
p YAML.dump([1, "two", :three, true, nil])
#==#
require "yaml"
h = {"name" => "x", "nums" => [1, 2, 3], "nested" => {"k" => "v"}}
p YAML.load(YAML.dump(h)) == h
#==#
require "yaml"
p YAML.load("db:\n  host: localhost\n  port: 5432\n  tags:\n  - a\n  - b")
#==#
# ── subject-less case (multi-way if) ──
x = 5
p(case; when x < 0 then "neg"; when x.zero? then "zero"; else "pos"; end)
#==#
p(case; when false then 1; when 2 == 2, 3 == 4 then "b"; end)
#==#
# ── chained assignment binds into ||/&& (a = b || c = d) ──
c = nil
a = false || c = 5
p [a, c]
#==#
# ── superclass with leading :: (top-level scope) ──
class MyHash < ::Hash; end
p MyHash.new.class.to_s
#==#
# ── Array#to_set / Integer#size ──
require "set"
p [1, 2, 2, 3, 3, 3].to_set.size
#==#
p 255.size
#==#
# ── Regexp class methods ──
p Regexp.escape("a.b*c?")
#==#
p Regexp.union("a", "b.c").source
#==#
p Regexp.new("ab+").class.to_s
#==#
"hello" =~ /l+/
p Regexp.last_match(0)
#==#
# ── extend a sibling nested module by bare name (class + module bodies) ──
module Outer
  module Helper
    def helped; "yes"; end
  end
  extend Helper
end
p Outer.helped
#==#
class Klass
  module Mixin
    def mixed; 42; end
  end
  extend Mixin
end
p Klass.mixed
#==#
# ── super(&block) forwards the block (block_given? stays faithful) ──
class SBase
  def each; yield 1; yield 2; end
end
class SDeriv < SBase
  def each(&b); super(&b); end
end
r = []
SDeriv.new.each { |x| r << x }
p r
#==#
class GBase
  def go; block_given? ? yield : :none; end
end
class GDeriv < GBase
  def go(&b); super(&b); end
end
p GDeriv.new.go
#==#
# ── quoted symbol literals ──
p :"hello world".length
#==#
p :"a\tb".to_s
#==#
# ── modifier if/unless inside parentheses ──
p((5 if true))
#==#
p((5 if false) || 9)
#==#
p [1, 2, 3].map { |n| (n * 2 if n.odd?) }
#==#
# ── block parameters with defaults ──
f = lambda { |a, b = 10| a + b }
p [f.call(1), f.call(1, 2)]
#==#
p [1, 2].map { |x, i = 99| [x, i] }
#==#
# ── `and`/`or` with a leading `not` operand ──
def ang; 1 if true and not (false); end
p ang
#==#
# ── setter alias name ──
class AliasC
  def val=(v); @val = v; end
  def val; @val; end
  alias value= val=
end
o = AliasC.new
o.value = 42
p o.val
#==#
# ── expression superclass: class C < Struct.new(...) ──
class Point < Struct.new(:x, :y)
  def dist; Math.sqrt(x * x + y * y); end
end
pt = Point.new(3, 4)
p [pt.x, pt.y, pt.dist]
#==#
class OneField < Struct.new(:a)
end
p OneField.new(7).a
#==#
# ── leading-:: constant as an expression and as a command argument ──
p ::Kernel.respond_to?(:puts)
#==#
puts ::Math::PI
#==#
def collect(*a, &b); [a, b.call]; end
def forward(&blk); collect ::Kernel, 1, &blk; end
p forward { 9 }
#==#
# ── parenthesized statement sequence ──
p((a = 4; b = 5; a * b))
#==#
# ── splat of a Range / Set expands to elements ──
p [0, *1..3, 4]
#==#
p [*"a".."e"]
#==#
# ── alias with operator + setter method names ──
class Store
  def []=(k, v); (@h ||= {})[k] = v; end
  def [](k); (@h ||= {})[k]; end
  alias set []=
end
s = Store.new
s.set(:a, 1)
p s[:a]
#==#
# ── hash literal larger than the MKHASH argc limit (>127 pairs) ──
big = {}
(1..200).each { |i| big[i] = i * i }
lit = eval("{" + (1..200).map { |i| "#{i} => #{i * i}" }.join(", ") + "}")
p [lit.size, lit[200], lit == big]
#==#
# ── begin/rescue/else (else runs when no exception) ──
p(begin; 1 + 1; rescue; :err; else; :ok; end)
#==#
def raises_then_else
  begin
    raise "boom"
  rescue
    :caught
  else
    :no_raise
  end
end
p raises_then_else
#==#
# ── do...end block with an inline rescue/else (Ruby 2.6+) ──
r = [1, 0, 2].map do |n|
  10 / n
rescue ZeroDivisionError
  -1
end
p r
#==#
# ── UnboundMethod: instance_method / bind / bind_call ──
um = String.instance_method(:upcase)
p um.bind("hi").call
#==#
p Integer.instance_method(:+).bind_call(3, 4)
#==#
# ── rescue with a top-level-scoped exception class ──
p(begin; raise NameError, "boom"; rescue ::NameError => e; e.message; end)
#==#
# ── runtime / conditional include & prepend (with hooks) ──
module Loud
  def self.included(base); puts "included in #{base}"; end
  def speak; "LOUD"; end
end
class Speaker
  include Loud if true
end
p Speaker.new.speak
#==#
module Wrap
  def val; "[" + super + "]"; end
end
class Boxed
  def val; "x"; end
end
Boxed.prepend(Wrap)
p Boxed.new.val
#==#
# ── runtime attr_accessor (class_eval / send / direct) ──
class Dyn1; end
Dyn1.class_eval { attr_accessor :name }
d = Dyn1.new
d.name = "set at runtime"
p d.name
#==#
class Dyn2; end
Dyn2.send(:attr_reader, :a)
Dyn2.send(:attr_writer, :a)
d2 = Dyn2.new
d2.a = 7
p d2.a
#==#
# ── return / break with multiple values yields an Array ──
def multi; return 1, 2, 3; end
p multi
#==#
# ── multi-line index / subscript ──
s = "hello"
p s[
  1, 3
]
#==#
# ── alias to a keyword-named method ──
class Kw
  def foo; 42; end
  alias bar foo
end
p Kw.new.bar
#==#
# ── Encoding constants ──
p Encoding::UTF_8
#==#
p "abc".encoding == Encoding::UTF_8
#==#
# ── and/or are looser than assignment ──
a = (v = 3 or 9)
p [a, v]
#==#
def orassign; l = 5; l or l += 1; l; end
p orassign
#==#
# ── compound assign rebinds into the rightmost &&/|| operand ──
u = nil
r = (true && u ||= 7)
p [r, u]
#==#
# ── punctuation character literals (?c) ──
p ?.
p "x/y"[1] == ?/
p ?A.ord
#==#
# ── namespaced constant assignment (A::B = v) ──
module NsMod; end
NsMod::VALUE = 42
p NsMod::VALUE
#==#
# ── old-style attr (reader) ──
class AttrOld
  attr :label
  def initialize(l); @label = l; end
end
p AttrOld.new("hi").label
#==#
# ── splat on the RHS of parallel assignment + MatchData coercion ──
a, b, c = *[1, 2, 3]
p [a, b, c]
#==#
_, first, second = *"x-y".match(/(.)-(.)/)
p [first, second]
#==#
p Array("a-b".match(/(.)-(.)/))
#==#
# ── /=/ is a regex in match position; /= stays divide-assign after a value ──
p("a=b" =~ /=/)
#==#
n = 10
n /= 2
p n
#==#
# ── Float#round(ndigits) underflows to a POSITIVE zero (MRI float_round_underflow) ──
p (-1.5 / 1e10).round(4)
p (-0.0001).round(2)
p (-0.00001).ceil(2)
p (-0.00001).truncate(2)
p (-0.0).round(2)
#==#
# ── Float#round repairs the `x * 10**n` product's rounding error (round_half_up) ──
p 1.005.round(2)
p 0.145.round(2)
p 2.675.round(2)
p (-2.675).round(2)
#==#
# ── ndigits past DBL_DIG redoes the rounding in exact rationals ──
p 49.989999999999995.round(15)
p (-1.0e-14).ceil(15)
p (-1.0e-14).truncate(17)
p (-1.5e-16).round(16)
#==#
# ── Integer/Float rounding to a power of ten stays in exact integer math ──
p 123456789012345678.0.floor(-3)
p (2**70).round(-3)
p 9999999999999999999999.round(-3)
p (-12345).round(-2)
p (-12345).floor(-2)
p (-12345).ceil(-2)
p 12345.round(-20)
#==#
# ── Integer#fdiv / Rational#to_f cancel the gcd, then convert each side ──
p 49989999999999995.fdiv(10**15)
p Rational(49989999999999995, 10**15).to_f
p 10000000000000001.fdiv(3)
p Rational(1, 3).to_f
#==#
# ── Float#to_s keeps fixed notation while the point falls inside the digits ──
p 3333333333333333.5
p 123456789012345.6
p 1e15
p 1e16
p 1234567890123456.0
p 0.0001
p 9.999e-5
p(-0.0)
p 1.0e-320
#==#
# ── String#to_f honours exponents and digit-separating underscores ──
p "1e15".to_f
p "1_000.5".to_f
p "1.0e-320".to_f
p "1e-400".to_f
p "1e400".to_f
p "1e".to_f
p ".5".to_f
p "0x10".to_f
p "1__0".to_f
p "  3.5  ".to_f
#==#
# ── unpack covers the fixed-width integer and float directives pack emits ──
p [1.5, -2.25].pack("D2").unpack("D2")
p [1.5, -2.25].pack("g2").unpack("g2")
p [-3, 300].pack("i2").unpack("i2")
p [2**64 - 3].pack("Q").unpack1("Q")
p [-1].pack("s").unpack1("s")
p "abcdef".unpack("a2 X1 a2")
p "abcdef".unpack("@3a2")
#==#
# ── `while`/`until` evaluate to their `break` operand, not always nil ──
p(while true do break 7 end)
p(until false do break 8 end)
p(while false do break 9 end)
v = while true
  begin
    break 11
  rescue
    nil
  end
end
p v
#==#
# ── break/next raised from a `begin` body still target the enclosing loop ──
i = 0
until i >= 3
  i += 1
  begin
    next
  rescue
    nil
  end
end
p i
log = []
n = 0
while true
  n += 1
  begin
    break if n == 4
  ensure
    log << n
  end
end
p [n, log]
#==#
# ── a `break` from a block keeps belonging to the method, not the outer loop ──
i = 0
seen = []
while i < 3
  i += 1
  [10, 20].each do |x|
    begin
      break if x == 20
    rescue
      nil
    end
    seen << [i, x]
  end
end
p [i, seen]
#==#
# ── return / raise still unwind straight through a loop's begin ──
def loop_ret
  k = 0
  while true
    k += 1
    begin
      return k if k == 3
    ensure
      nil
    end
  end
end
p loop_ret
def loop_raise
  while true
    begin
      raise ArgumentError, "stop"
    rescue ArgumentError => e
      return e.message
    end
  end
end
p loop_raise
#==#
# ── the builtin exception tree has its real intermediate layers ──
p [ArgumentError.superclass, NoMethodError.superclass, KeyError.superclass]
p [FloatDomainError.superclass, FrozenError.superclass, LoadError.superclass]
p [StandardError.superclass, SystemExit.superclass, NotImplementedError.superclass]
class MyErr < ArgumentError; end
begin
  raise MyErr, "boom"
rescue => e
  p [e.class, e.is_a?(ArgumentError), e.is_a?(StandardError), e.is_a?(Exception)]
end
begin; raise KeyError, "k"; rescue IndexError; puts "KeyError < IndexError"; end
begin; raise FloatDomainError, "f"; rescue RangeError; puts "FloatDomainError < RangeError"; end
#==#
# ── a bare `rescue` catches StandardError only, so ScriptError falls through ──
begin
  begin
    raise NotImplementedError, "ni"
  rescue => e
    puts "wrongly caught #{e.class}"
  end
rescue NotImplementedError => e
  puts "fell through: #{e.class}"
end
#==#
# ── an exception keeps its class across a Fiber boundary ──
f = Fiber.new { raise TypeError, "tboom" }
begin
  f.resume
rescue => e
  p [e.class, e.message]
end
g = Fiber.new { Fiber.yield 1; raise KeyError, "kb" }
p g.resume
begin
  g.resume
rescue KeyError => e
  p [e.class, e.message]
end
#==#
# ── block scoping: a block param is per-iteration, `for` shares one binding ──
procs = []
3.times { |i| procs << -> { i } }
p procs.map(&:call)
cs = []
for k in 0..2
  cs << -> { k }
end
p [cs.map(&:call), k]
1.times { x = 1 }
p defined?(x)
z = 5
[1].each { z = 9 }
p z
#==#
# ── explicit block-locals shadow instead of assigning the outer name ──
tmp = "outer"
[1, 2].each { |i; tmp| tmp = i * 100 }
p tmp
a = 1
[9].each { |a| }
p a
#==#
# ── Proc.new takes the block, like Kernel#proc ──
sq = Proc.new { |n| n * n }
p [sq.call(5), sq.class, sq.lambda?]
p [1, 2, 3].map { |n| Proc.new { n * 2 } }.map(&:call)
#==#
# ── redo re-runs the current block iteration without advancing the iterator ──
n = 0
[1, 2, 3].each do |x|
  n += 1
  redo if x == 2 && n < 5
  p [x, n]
end
p n
c = 0
a = [1, 2].map do |x|
  c += 1
  redo if c == 1
  x * 10
end
p [a, c]
c2 = 0
2.times { |x| c2 += 1; redo if c2 == 1 }
p c2
#==#
# ── redo keeps the block's locals: they are not rebound on the re-run ──
r = []
c = 0
[10].each do |x|
  y = (y || 0) + 1
  r << [x, y]
  c += 1
  redo if c < 3
end
p r
#==#
# ── redo in a while re-runs the body without re-testing the condition ──
i = 0
c = 0
while i < 3
  c += 1
  if c == 2
    i += 1
    redo
  end
  i += 1
end
p [i, c]
j = 0
d = 0
until j >= 2
  d += 1
  redo if d == 1
  j += 1
end
p [j, d]
#==#
# ── redo in a for, and through a begin nested in a loop or a block ──
c = 0
for x in 1..3
  c += 1
  redo if x == 2 && c < 5
end
p c
i = 0
n = 0
while i < 2
  begin
    n += 1
    redo if n == 1
  end
  i += 1
end
p [i, n]
m = 0
out = []
[1, 2].each do |v|
  begin
    m += 1
    redo if m == 1
  end
  out << [v, m]
end
p out
#==#
# ── redo runs a block's ensure once per attempt ──
log = []
c = 0
[1].each do |x|
  begin
    c += 1
    redo if c < 3
  ensure
    log << c
  end
end
p log
#==#
# ── `for` introduces no scope: body locals outlive the loop too ──
for i in 1..3
  sq = i * i
end
p [i, sq, defined?(sq)]
for j in []
  z = 1
end
p [j, z, defined?(z)]
#==#
# ── every closure a `for` body makes shares the one binding ──
ps = []
for i in 0..2
  t = i * 2
  ps << -> { [i, t] }
end
p ps.map(&:call)
p [i, t]
#==#
# ── for body locals leak out of nested control flow and nested loops ──
for i in 1..2
  if i == 2
    m = 9
  end
  while false
    w = 1
  end
  begin
    g = i
  end
  case i
  when 1 then c1 = 1
  end
end
p [m, defined?(w), w, g, c1]
for a in 1..2
  for b in 1..2
    k = a * b
  end
end
p [a, b, k]
#==#
# ── `for k, v in pairs` destructures into enclosing locals ──
for k, v in {a: 1, b: 2}
  s = "#{k}=#{v}"
end
p [k, v, s]
for x, y in [[1, 2], [3, 4]]
  t = x + y
end
p [x, y, t]
for p1, p2, p3 in [[1, 2]]
  q = 1
end
p [p1, p2, p3]
#==#
# ── Hash#each yields the whole [k, v] pair as ONE value (MRI each_pair_i) ──
h = {a: 1, b: 2}
got = []
h.each { |x| got << x }
p got
got2 = []
h.each_pair { |x| got2 << x }
p got2
p h.map { |x| x }
p h.collect { |x| x }
p h.each { |k, v| }.equal?(h)
#==#
# ── so does every Enumerable method Hash derives from `each` ──
h = {a: 1, b: 2}
p h.find { |x| x.is_a?(Array) }
p h.detect { |x| x[1] == 2 }
p h.count { |x| x.is_a?(Array) }
p h.sum(0) { |x| x[1] }
p h.flat_map { |x| x }
p h.filter_map { |x| x[0] }
p [h.any? { |x| x.is_a?(Array) }, h.all? { |x| x.size == 2 }, h.none? { |x| x.nil? }]
p h.take_while { |x| x.is_a?(Array) }
p h.drop_while { |x| x.is_a?(Array) }
p h.find_index { |x| x[1] == 2 }
#==#
# ── Hash's own methods still yield key and value separately ──
h = {a: 1, b: 2}
p h.select { |k, v| v > 1 }
p h.reject { |k, v| v > 1 }
p h.transform_values { |v| v * 10 }
p h.transform_keys { |k| k.to_s }
ks = []
h.each_key { |k| ks << k }
vs = []
h.each_value { |v| vs << v }
p [ks, vs]
p h.to_h { |k, v| [k.to_s, v * 2] }
p h.to_h
#==#
# ── Hash reaches the rest of Enumerable through its pair sequence ──
h = {a: 1, b: 2}
p [h.first, h.min, h.max, h.sort]
p h.take(1)
p h.drop(1)
p h.reverse_each.to_a
p h.zip([9, 8])
p h.tally
p h.uniq
p h.each_entry { |x| }.class
#==#
# ── Struct#each_pair yields one [member, value] pair, like Hash#each ──
SP = Struct.new(:a, :b)
s = SP.new(1, 2)
one = []
s.each_pair { |x| one << x }
p one
two = []
s.each_pair { |k, v| two << [k, v] }
p two
vals = []
s.each { |v| vals << v }
p vals
#==#
# ── Enumerator.new runs its block on a fiber: one `y <<` per `next` ──
log = []
e = Enumerator.new do |y|
  log << :a
  y << 1
  log << :b
  y << 2
  log << :c
end
p log
p e.next
p log
p e.next
p log
begin
  e.next
rescue StopIteration => x
  p [:stop, x.class]
end
p log
#==#
# ── a raise inside a generator surfaces at the `next` that reaches it ──
e = Enumerator.new do |y|
  y << 1
  y << 2
  raise "boom"
end
p e.next
p e.next
begin
  e.next
rescue => ex
  p [ex.class, ex.message]
end
g = Enumerator.new { |y| y << 1; raise ArgumentError, "bad" }
p g.next
begin
  g.next
rescue ArgumentError => ex
  p [ex.class, ex.message, ex.is_a?(StandardError)]
end
#==#
# ── an endless generator answers `next` instead of running forever ──
e = Enumerator.new do |y|
  i = 0
  loop { y << i; i += 1 }
end
p e.next
p e.next
p e.take(3)
p e.first(4)
p e.lazy.map { |x| x * 2 }.first(3)
p e.next
#==#
# ── peek does not advance; rewind restarts the block ──
e = Enumerator.new { |y| y << 1; y << 2 }
p e.peek
p e.next
p e.peek
p e.next
begin
  e.peek
rescue StopIteration
  p :stopped
end
e.rewind
p e.next
p e.to_a
#==#
# ── independent generators keep independent positions ──
a = Enumerator.new { |y| 3.times { |i| y << "a#{i}" } }
b = Enumerator.new { |y| 3.times { |i| y << "b#{i}" } }
p [a.next, b.next, a.next, b.next, a.next, b.next]
begin
  a.next
rescue StopIteration
  p :a_done
end
r = []
c = Enumerator.new { |y| y << 1; y << 2 }
loop { r << c.next }
p r
d = Enumerator.new { |y| y.yield(1, 2); y << 3 }
p [d.next, d.next]
#==#
# ── a wrong positional count raises ArgumentError before the body runs ──
def two(x, y) = [x, y]
begin
  two(1)
rescue ArgumentError => e
  p e.message
end
begin
  two(1, 2, 3)
rescue ArgumentError => e
  p e.message
end
def opt(x, y = 1) = [x, y]
begin
  opt
rescue ArgumentError => e
  p e.message
end
p opt(5)
def sp(x, *r) = [x, r]
begin
  sp
rescue ArgumentError => e
  p e.message
end
p sp(1, 2, 3)
def mid(x, y = 1, z = 2, *r, w) = [x, y, z, r, w]
begin
  mid(1)
rescue ArgumentError => e
  p e.message
end
#==#
# ── unknown and missing keywords raise, and name every offender ──
def kw(x:, y: 1) = [x, y]
begin
  kw(x: 1, z: 2)
rescue ArgumentError => e
  p e.message
end
begin
  kw(x: 1, z: 2, w: 3)
rescue ArgumentError => e
  p e.message
end
begin
  kw
rescue ArgumentError => e
  p e.message
end
def kw2(x:, y:) = [x, y]
begin
  kw2
rescue ArgumentError => e
  p e.message
end
def poskw(a, x:) = [a, x]
begin
  poskw(x: 1)
rescue ArgumentError => e
  p e.message
end
begin
  poskw(1, 2, x: 1)
rescue ArgumentError => e
  p e.message
end
def anykw(**o) = o
p anykw(a: 1, b: 2)
p kw(x: 1)
p poskw(1, x: 2)
#==#
# ── singleton_methods lists what is defined on the object alone ──
o = Object.new
def o.m = 1
def o.n = 2
p o.singleton_methods.sort
o2 = Object.new
o2.define_singleton_method(:k) { 3 }
p [o2.singleton_methods.sort, o2.k]
class SingHost
  def self.cm = 1
  def im = 2
end
p SingHost.singleton_methods.sort
p SingHost.new.singleton_methods
p 1.singleton_methods
#==#
# --- lambda arity: strict, unlike a block ---
begin
  ->(x, y) { [x, y] }.call(1)
rescue ArgumentError => e
  p e.message
end
#==#
begin
  ->(x, y) { [x, y] }.call(1, 2, 3)
rescue ArgumentError => e
  p e.message
end
#==#
begin
  ->(x, y = 5) { [x, y] }.call
rescue ArgumentError => e
  p e.message
end
#==#
p ->(x, y = 5) { [x, y] }.call(1)
#==#
begin
  ->(x, *r) { [x, r] }.call
rescue ArgumentError => e
  p e.message
end
#==#
p ->(x, *r) { [x, r] }.call(1, 2, 3)
#==#
begin
  ->(k:) { k }.call
rescue ArgumentError => e
  p e.message
end
#==#
begin
  ->(k:) { k }.call(k: 1, j: 2)
rescue ArgumentError => e
  p e.message
end
#==#
p ->(k:, j: 7) { [k, j] }.call(k: 1)
#==#
p ->(x, **o) { [x, o] }.call(1, z: 3)
#==#
begin
  lambda { |x, y| [x, y] }.call(1)
rescue ArgumentError => e
  p e.message
end
#==#
# A plain proc stays lenient on both sides.
p [proc { |x, y| [x, y] }.call(1), proc { |x, y| [x, y] }.call(1, 2, 3)]
#==#
# A lambda does NOT auto-splat a single array argument; a block does.
begin
  [[1, 2]].map(&->(x, y) { x + y })
rescue ArgumentError => e
  p e.message
end
#==#
p [[1, 2]].map { |x, y| x + y }
#==#
p [->() {}.arity, ->(x) {}.arity, ->(x, y) {}.arity, ->(x, y = 1) {}.arity]
#==#
p [->(*a) {}.arity, ->(x, *a) {}.arity, ->(*a, z) {}.arity]
#==#
p [->(k:) {}.arity, ->(k:, j:) {}.arity, ->(x, k:) {}.arity, ->(k: 1) {}.arity, ->(x, k: 1) {}.arity]
#==#
p [->(**o) {}.arity, ->(x, **o) {}.arity, ->(&b) {}.arity, ->(x, &b) {}.arity]
#==#
p [proc {}.arity, proc { |x| }.arity, proc { |x, y = 1| }.arity, proc { |*a| }.arity, proc { |k: 1| }.arity]
#==#
p [->(x, y) {}.lambda?, proc { |x, y| }.lambda?, lambda { |x, y| }.lambda?]
#==#
p ->(x, y) { x + y }.curry[2][3]
#==#
begin
  ->(x, y) { x + y }.curry.call(1, 2, 3)
rescue ArgumentError => e
  p e.message
end
#==#
f = ->(x) { x * 2 } >> ->(y) { y + 1 }
p [f.call(3), f.lambda?]
#==#
# A lambda's `&blk` captures the block `call` was given.
p ->(&b) { b.call(4) }.call { |v| v * 2 }
#==#
# `->(x; t)` block-locals: `t` is a fresh nil local, not a parameter.
p ->(x; t) { [x, t] }.call(1)
#==#
p ->(x; t) { }.arity
#==#
# `Proc#===` calls the proc, so a lambda works as a `case` guard.
p [(->(x) { x > 2 } === 3), (case 5 when ->(x) { x > 2 } then :big else :small end)]
#==#
# `define_method` bodies get method (strict) arity, from a block or a lambda.
class DMArity
  define_method(:g) { |x, y| [x, y] }
end
begin
  DMArity.new.g(1)
rescue ArgumentError => e
  p e.message
end
#==#
class DMArity2
  define_method(:g, ->(x, y) { [x, y] })
end
begin
  DMArity2.new.g(1)
rescue ArgumentError => e
  p e.message
end
#==#
class DMArity3
  define_method(:g) { |x, y| [x, y] }
end
p [DMArity3.new.g(1, 2), DMArity3.new.method(:g).arity, DMArity3.instance_method(:g).arity]
#==#
# A `Method` is a lambda: strict arity, `to_proc`, `curry`, and real `arity`.
def marity(x, y) = [x, y]
begin
  method(:marity).call(1)
rescue ArgumentError => e
  p e.message
end
#==#
def marity2(x, y) = [x, y]
p [method(:marity2).arity, method(:marity2).to_proc.lambda?, method(:marity2).to_proc.call(1, 2)]
#==#
def marity3(x, y) = [x, y]
p method(:marity3).curry[1][2]
#==#
def m_shapes0; end
def m_shapes1(x); end
def m_shapes2(x, y = 1); end
def m_shapes3(*a); end
def m_shapes4(x, *a); end
p [method(:m_shapes0).arity, method(:m_shapes1).arity, method(:m_shapes2).arity, method(:m_shapes3).arity, method(:m_shapes4).arity]
#==#
def m_kwshapes1(k:); end
def m_kwshapes2(x, k:); end
def m_kwshapes3(k: 1); end
def m_kwshapes4(**o); end
def m_kwshapes5(&b); end
p [method(:m_kwshapes1).arity, method(:m_kwshapes2).arity, method(:m_kwshapes3).arity, method(:m_kwshapes4).arity, method(:m_kwshapes5).arity]
#==#
class UMArity
  def self.s(x) = x
  def g(a, b) = [a, b]
end
p [UMArity.method(:s).arity, UMArity.instance_method(:g).arity, UMArity.instance_method(:g).bind_call(UMArity.new, 1, 2)]
#==#
# A Hash hands `map`/`find` two values to a fixed-arity-above-one block, and the
# packed pair to anything else — MRI's `rb_block_pair_yield_optimizable`.
p [{ a: 1 }.map(&->(k, v) { [k, v] }), { a: 1 }.map(&->(kv) { kv }), { a: 1 }.map(&:first)]
#==#
p [{ a: 1, b: 2 }.find(&->(k, v) { v == 2 }), { a: 1, b: 2 }.map { |k, v| [k, v] }]
#==#
# --- Enumerator shape: the grouping methods answer an Enumerator ---
p [[1, 2, 3].each_slice(2).class, [1, 2, 3].each_cons(2).class]
#==#
p [[1, 2, 4, 5].chunk_while { |a, b| b == a + 1 }.class, [1, 2, 4, 5].slice_when { |a, b| b > a + 1 }.class, [1, 1, 2].chunk { |x| x }.class]
#==#
p [[1, 2, 4, 5].chunk_while { |a, b| b == a + 1 }.to_a, [1, 2, 4, 5].slice_when { |a, b| b > a + 1 }.to_a]
#==#
p [1, 1, 2, 3, 3].chunk { |x| x.odd? }.to_a
#==#
p({ a: 1, b: 2 }.chunk_while { |x, y| true }.to_a)
#==#
begin
  [1, 2, 3].chunk_while
rescue ArgumentError => e
  p e.message
end
#==#
begin
  [1, 2, 3].slice_when
rescue ArgumentError => e
  p e.message
end
#==#
p [[1, 2, 3].each_slice(2).next, [1, 2, 3].each_cons(2).next, [1, 2, 3].each_slice(2).size]
#==#
p [1, 2, 4].chunk_while { |a, b| b == a + 1 }.map(&:size)
#==#
# --- laziness: a block-less enumerator method over an INFINITE source ---
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p [e.first(3), e.take(2)]
#==#
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p e.each_slice(2).first(2)
#==#
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p e.each_cons(2).first(2)
#==#
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p [e.each_with_index.first(2), e.with_index.first(2)]
#==#
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p [e.map.first(2), e.select.first(2), e.each_entry.first(2)]
#==#
e = Enumerator.new { |y| i = 0; loop { y << i; i += 1 } }
p e.each_with_object([]).first(2)
#==#
c = [1, 2].cycle
p [c.map.first(3), c.each_slice(3).first(2), c.each_with_index.first(3)]
#==#
p [(1..).each_slice(2).first(2), (1..).each_cons(2).first(2)]
#==#
p [(1..).map.first(2), (1..).select.first(2), (1..).each_entry.first(2), (1..).to_enum.first(2)]
#==#
p [(1..).each_with_index.first(2), (1..).each_with_object([]).first(2)]
#==#
p [(1..Float::INFINITY).each_slice(2).first(2), (1..Float::INFINITY).each_slice(2).class]
#==#
# A finite generator still ends where it ends.
g = Enumerator.new { |y| y << 1; y << 2; y << 3 }
p [g.each_slice(2).to_a, g.each_cons(2).to_a, g.each_with_index.to_a]
#==#
# --- Data.define is strict about its members ---
DPoint = Data.define(:x, :y)
begin
  DPoint.new(1)
rescue ArgumentError => e
  p e.class
end
#==#
DPoint2 = Data.define(:x, :y)
begin
  DPoint2.new(1, 2, 3)
rescue ArgumentError => e
  p e.class
end
#==#
DPoint3 = Data.define(:x, :y)
begin
  DPoint3.new(x: 1, z: 2)
rescue ArgumentError => e
  p e.class
end
#==#
DPoint4 = Data.define(:x, :y)
p [DPoint4.new(1, 2).to_h, DPoint4.new(x: 3, y: 4).to_h, DPoint4.new(1, 2).with(y: 9).to_h]
#==#
DPoint5 = Data.define(:x, :y)
p [DPoint5.new(1, 2) == DPoint5.new(1, 2), DPoint5.new(1, 2).hash == DPoint5.new(1, 2).hash]
#==#
# A Struct is a value key too.
SPair = Struct.new(:a, :b)
h = { SPair.new(1, 2) => :hit }
p [h[SPair.new(1, 2)], SPair.new(1, 2).hash == SPair.new(1, 2).hash]
#==#
# --- freeze / clone ---
s = "abc".freeze
p [s.frozen?, s.dup.frozen?, s.clone.frozen?, s.clone(freeze: false).frozen?]
#==#
a = [1, 2].freeze
begin
  a << 3
rescue => e
  p e.class
end
#==#
# --- introspection: the private-by-definition hooks stay out ---
class IntroHost
  def initialize; @x = 1; end
  def m(z) = z
end
p [IntroHost.instance_methods(false).sort, IntroHost.new.public_methods(false).sort]
#==#
class IntroHost2
  def initialize; @x = 1; @y = "s"; end
end
p [IntroHost2.new.instance_variables.sort, IntroHost2.new.instance_variable_get(:@x)]
#==#
# --- Complex: exact division, negation, and MRI's inspect shape ---
p [Complex(1, 2) / Complex(3, 4), Complex(4, 2) / 2, Complex(1, 2) / Complex(1, 0)]
#==#
p [Complex(1.5, 2.5) * Complex(2, 0), Complex(1.5, 2.5) / Complex(2, 0), Complex(1.5, 2.5) + Complex(2, 0)]
#==#
p [-Complex(1, 2), (3 + -2i), Complex(-3, -4) / Complex(4, 4)]
#==#
p [Complex(1, 2).zero?, Complex(0, 0).zero?, Complex(1, 2).nonzero?]
#==#
# --- String codepoints ---
p ["日本語".codepoints, "héllo".codepoints.size, "héllo".each_codepoint.to_a.size]
#==#
# --- break out of a block is the value of the call the LITERAL was written on ---
p [[1, 2, 3].find { break 99 }, [1, 2, 3].sort_by { break 7 }, [1, 2, 3].sum { break 5 }]
#==#
p [[1, 2, 3].each_with_index { |x, i| break [x, i] if i == 1 }, [1, 2, 3, 4].each_slice(2) { break :s }]
#==#
p [{a: 1, b: 2}.each { break :h }, {a: 1, b: 2}.each_key { break :k }, {a: 1}.transform_values { break :t }]
#==#
p ["abc".each_char { break :c }, "a\nb".each_line { break :l }, (1..5).each_with_index { break :r }]
#==#
p [[1, 2, 3].each_with_object([]) { |x, a| break :o }, [1, 2, 3].take_while { break :w }, [1, 2].zip([3, 4]) { break :z }]
#==#
# `break` crossing a user-defined `yield` ends the method and is its call's value.
def brk_yielder; yield 1; :after; end
p [(brk_yielder { break :broke }), (brk_yielder { :body })]
#==#
# A forwarded `&blk` is NOT the break target — the literal's call site is.
def brk_fwd(&b); r = [1, 2].each(&b); $stdout.print "reached "; r; end
p(brk_fwd { break 7 })
#==#
# Nested two deep: the inner call owns the inner break.
def brk_nest; [1, 2].each { |a| [3, 4].each { |b| break 8 } }; end
p [brk_nest, [1, 2].map { |a| [3, 4].each { break 9 } }]
#==#
# `it` is the implicit single block parameter; a real local named `it` wins.
p [[1, 2].map { it * 3 }, [[1, 2]].map { it }, {a: 1}.map { it }]
#==#
it = 5
p [[1, 2].map { it }, [1, 2].map { _1 * 2 }, [1, 2].each_with_index.map { _1 + _2 }]
#==#
# `reject!` answers nil when it removed nothing; `delete_if` always answers self.
p [[1, 2].reject! { false }, [1, 2].reject! { |x| x == 1 }, [1, 2].delete_if { false }]
#==#
# each_with_index yields TWO values: a one-parameter block binds only the first.
p [[10, 20].each_with_index.map { |x| x }, [10, 20].each_with_index.map { |x, i| [x, i] }, [10, 20].each_with_index.to_a]
#==#
# A built-in's arity is what MRI DECLARES for it: the count when every parameter
# is required, and -(required+1) once anything is optional or variadic.
p [3.method(:*).arity, 3.method(:divmod).arity, 3.method(:times).arity, 3.method(:round).arity, [1, 2].method(:push).arity]
#==#
# `#owner` names the module that DEFINES the method, not the receiver's class.
p [3.method(:between?).owner, 3.method(:puts).owner, [1, 2].method(:each_slice).owner, [1, 2].method(:map).owner, {a: 1}.method(:map).owner]
#==#
# A built-in's parameters have no written names, so MRI reports one-element entries.
p [3.method(:+).parameters, 3.method(:round).parameters, [1, 2].method(:each_slice).parameters]
#==#
# A class receiver resolves class methods; the owner is the singleton class.
p [Integer.method(:sqrt).arity, Integer.method(:sqrt).owner.to_s, Math.method(:hypot).arity, Math.method(:hypot).owner.to_s, Integer.method(:name).owner.to_s]
#==#
# An UnboundMethod looks its name up as an INSTANCE method of the class it holds.
p [3.method(:to_s).unbind.owner, Integer.instance_method(:to_s).owner, Integer.method(:to_s).owner, Array.instance_method(:each).arity]
#==#
# A written method reports its own shape, with the module it was defined in.
class ArOwner
  def m(a, b = 1, *c, d:, e: 2, **f, &g); end
end
module ArMod
  def mm(a); end
end
class ArInc
  include ArMod
end
p [ArOwner.new.method(:m).arity, ArOwner.new.method(:m).parameters, ArOwner.new.method(:m).owner, ArInc.new.method(:mm).owner]
#==#
# A subclass of a built-in inherits the built-in's owners.
class ArSub < Array
end
p [ArSub.new.method(:each).owner, ArSub.new.method(:size).arity, ArSub.new.method(:frozen?).owner, ArSub.new.method(:each_slice).owner]
#==#
# A `define_method` body reports the block's shape, checked strictly.
class ArDm
  define_method(:d) { |x, y = 1| }
  define_method(:e) { || }
end
p [ArDm.new.method(:d).arity, ArDm.new.method(:d).parameters, ArDm.new.method(:d).owner, ArDm.new.method(:e).arity]
#==#
# `|| ` is an EMPTY block parameter list, not the or-operator.
p [proc { || 1 }.call, proc { || }.arity, [1, 2].map { || 5 }]
#==#
# `curry` gathers exactly as many arguments as the method's arity.
p [3.method(:+).curry[4], 3.method(:gcd).curry[6], 3.method(:+).to_proc.arity]
#==#
# An included module's methods are owned by the module, whichever way it came in.
class ArCmp
  include Comparable
  def <=>(o) = 0
end
class ArEnum
  include Enumerable
  def each; yield 1; end
end
p [ArCmp.new.method(:between?).owner, ArCmp.new.method(:between?).arity, ArEnum.new.method(:sort_by).owner, ArEnum.new.method(:first).arity]
#==#
# A source that yields TWO values per iteration reshapes the block argument and
# the collected element INDEPENDENTLY: the block binds the first value, the
# element kept is still the packed pair.
e = [10, 20].each_with_index
p [e.take_while { |x| x == 10 }, e.count { |x| x.is_a?(Array) }, e.find_index { |x| x.is_a?(Array) }]
#==#
e = [10, 20].each_with_index
p [e.any? { |x| x.is_a?(Array) }, e.all? { |x| x.is_a?(Array) }, e.none? { |x| x.is_a?(Array) }, e.one? { |x| x == 10 }]
#==#
# The consumers whose block sees the PACKED pair — the other half of the split.
e = [10, 20].each_with_index
p [e.select { |x| x.is_a?(Array) }, e.drop_while { |x| x == 10 }, e.find { |x| x == 20 }, e.sort_by { |x| -x[0] }]
#==#
# Ruby's own binding rules do the reshaping, so a splat collects both values.
e = [10, 20].each_with_index
p [e.map { |*a| a }, e.map { |x| x }, e.map { |x, i| [i, x] }, e.take_while { |*a| a[0] == 10 }]
#==#
# `y.yield a, b` yields two values; `y << [a, b]` yields one that is an array.
two = Enumerator.new { |y| y.yield 1, 2; y.yield 3, 4 }
one = Enumerator.new { |y| y << [1, 2]; y << [3, 4] }
p [two.take_while { |a| a < 3 }, two.map { |a| a }, one.take_while { |a| a[0] < 3 }, one.map { |a| a }]
#==#
# Block-less `each_with_object` is an Enumerator of `[elem, memo]` pairs.
p [[10, 20].each_with_object([]).to_a, [10, 20].each_with_object([]).map { |x| x }]
#==#
# An Enumerator answers the object it iterates, which its buffer cannot
# reconstruct — `each_cons` windows overlap.
p [[1, 2, 3, 4].each_slice(2).each { |x| x }, [1, 2, 3].each_cons(2).each { |x| x }, [10, 20].each_with_index.each { |x| x }]
#==#
p [[1, 2, 3, 4].each_slice(2).inspect, [1, 2, 3].each_cons(2).inspect, [10, 20].each_with_index.inspect, [10, 20].each_with_object([]).inspect]
#==#
# `.lazy` over a source that is not an Array: a Hash, an Enumerator and a
# multi-yield Enumerator all have to feed the pipeline.
h = {a: 1, b: 2}
p [h.lazy.map { |k, v| [k, v] }.to_a, h.lazy.select { |k, v| v < 2 }.to_a, h.lazy.take_while { |pair| pair[1] < 2 }.to_a]
#==#
# The LAZY split is not the eager one: lazy `drop_while` sees the first value,
# eager `drop_while` sees the pair.
e = [10, 20].each_with_index
p [e.lazy.map { |x| x }.to_a, e.lazy.take_while { |x| x == 10 }.to_a, e.lazy.drop_while { |x| x == 10 }.to_a, e.lazy.select { |x| x.is_a?(Array) }.to_a]
#==#
# `each_entry` is the one that always packs, and answers the enumerator.
e = [10, 20].each_with_index
got = []
r = e.each_entry { |x| got << x }
p [got, r.inspect]
#==#
# A `break` in a block a lazy pipeline STORED has no call left to break out of —
# the `.map` it was written on returned the moment it was written.
r = begin
  [1, 2].lazy.map { break 7 }.to_a
rescue LocalJumpError => e
  e.message
end
r2 = begin
  [1, 2].lazy.select { break }.first(1)
rescue LocalJumpError => e
  e.message
end
p [r, r2, [1, 2].map { break 7 }]
#==#
# A lazy pipeline inspects as one wrapper per stage around the object `.lazy`
# was called on.
p [[1, 2].lazy.inspect, [1, 2].lazy.map { }.inspect, [1, 2].lazy.map { }.select { }.inspect]
#==#
p [[1, 2].lazy.take(2).inspect, [1, 2].lazy.drop(1).inspect, [1, 2].lazy.zip([3, 4]).inspect, {a: 1}.lazy.inspect]
#==#
# An Enumerator names the object it iterates, whatever kind of receiver it was.
p [(1..3).each_with_index.inspect, (1..3).each_slice(2).inspect, [1, 2].each.map.inspect, [1, 2].reverse_each.inspect]
#==#
p [(1..3).each_with_index.each { |x| x }, (1..3).each_with_index.lazy.map { |x| x }.to_a, [1, 2].each.map { |x| x * 2 }]
#==#
# `method` on a name nothing defines raises NameError rather than handing back a
# Method object that only fails when called. The message names the class the
# lookup ran against, and `NameError#name` is the missing name.
def miss
  yield
rescue NameError => e
  [e.class, e.message, e.name]
end
p miss { 1.method(:no_such) }
p miss { "s".method(:no_such) }
p miss { Object.new.method(:no_such) }
p miss { String.method(:no_such) }
p miss { String.instance_method(:no_such) }
#==#
# A class receiver's bound `method` names a CLASS method: the instance-side
# surface must not answer it, but a top-level `def` (private on Object) does.
def helper_for_parity_probe; end
class MethodProbe; def inst; end; def self.klass; end; end
def miss2
  yield.class
rescue NameError
  :NameError
end
p [miss2 { MethodProbe.method(:klass) }, miss2 { MethodProbe.method(:inst) },
   miss2 { MethodProbe.method(:helper_for_parity_probe) },
   miss2 { MethodProbe.instance_method(:inst) }, miss2 { String.method(:upcase) },
   miss2 { String.instance_method(:upcase) }]
#==#
# Every way a method can come to exist still answers `method` — a per-object
# singleton, an alias whose target is a built-in, an attr accessor, a Struct
# member, and a name only `respond_to_missing?` claims.
class Aliased < Hash; alias_method :as_hash, :to_h; attr_accessor :tag; end
Pointy = Struct.new(:x)
class Ghosted
  def respond_to_missing?(n, _p = false) = n == :ghost
  def method_missing(n, *a) = n == :ghost ? :spooked : super
end
solo = Object.new
def solo.only_mine; end
solo.define_singleton_method(:also_mine) { }
p [solo.method(:only_mine).class, solo.method(:also_mine).class,
   Aliased.new.method(:as_hash).class, Aliased.new.method(:tag).class,
   Aliased.new.method(:tag=).class, Pointy.new(1).method(:x).class,
   Ghosted.new.method(:ghost).call]
#==#
# A `method_missing` WITHOUT `respond_to_missing?` does not make the name exist.
class OnlyMissing; def method_missing(n, *a) = :anything; end
begin
  OnlyMissing.new.method(:whatever)
rescue NameError => e
  p e.message
end
#==#
# An UnboundMethod is its own class, not a Method: it has `bind`/`bind_call` and
# no `receiver`, and it describes the class it was looked up on.
class Bindable; def takes(a, b = 1) = [a, b]; end
um = Bindable.instance_method(:takes)
recv = begin
  um.receiver
rescue NoMethodError => e
  e.message
end
p [um.class, um.name, um.arity, um.owner, um.parameters, recv,
   um.bind(Bindable.new).call(3), um.bind_call(Bindable.new, 4, 5),
   um.is_a?(UnboundMethod), um.is_a?(Method),
   Bindable.new.method(:takes).unbind.class]
#==#
# A `define_method` body's `**rest` keeps its NAME. The parser desugars the
# collector into a synthetic capture param, so only the recorded arity can say
# what it was written as.
kls = Class.new do
  define_method(:m) { |a, *b, **rest, &blk| }
  define_method(:n) { |a, b = 1, *c, d:, e: 2, **opts, &bl| }
  define_method(:plain) { |x| }
end
p kls.instance_method(:m).parameters
p kls.instance_method(:n).parameters
p kls.instance_method(:plain).parameters
p [kls.instance_method(:m).arity, kls.instance_method(:n).arity]
#==#
# `Hash#each_with_index` without a block is an Enumerator, exactly as the Array
# one is — not the bare Array of pairs.
h = {a: 1, b: 2}
p h.each_with_index.class
p h.each_with_index.to_a
p h.each_with_index.next
p h.each_with_index.inspect
p h.each_with_index.map { |kv, i| [kv, i] }
#==#
# `Enumerator#each` on a generator answers what the generator BODY evaluated to,
# not the enumerator. `y << v` answers the yielder, so a body ending in a push
# reports one.
p (Enumerator.new { |y| y << 1; y << 2 }.each { |x| x }).class
p (Enumerator.new { |y| y << 1; 42 }.each { |x| x })
p (Enumerator.new { |y| y << 1 }.each { |x| x }).equal?(nil)
p [1, 2].each.each { |x| x }
p [1, 2, 3].each_slice(2).each { |x| x }
#==#
# `private def m` / `protected def m` / `module_function def m` define the method
# ON THE CLASS. Left to the runtime class body the `def` landed in the top-level
# method table instead, making it callable as a bare name from anywhere.
class Visible
  def open_one = 1
  private def shut = 2
  protected def guarded = 3
  public def opened = 4
end
module Functional; module_function def helper = 5; end
class Bystander; end
leaked = begin
  Bystander.new.method(:shut)
rescue NameError
  :NameError
end
p [Visible.new.send(:shut), Visible.new.send(:guarded), Visible.new.opened,
   Visible.new.open_one, Functional.helper, leaked,
   Visible.instance_method(:shut).name]
#==#
# A `Struct.new` class keeps `Struct` (and the `Enumerable` it mixes in) in its
# ancestry; a `Data.define` class keeps `Data` and does NOT get Enumerable.
Trio = Struct.new(:a, :b)
Duo = Data.define(:a)
p Trio.ancestors
p Duo.ancestors
p [Trio.new(1, 2).is_a?(Struct), Trio.new(1, 2).is_a?(Enumerable),
   Duo.new(a: 1).is_a?(Data), Duo.new(a: 1).is_a?(Enumerable)]
p [Trio.members, Trio.new(1, 2).map { |v| v }, Duo.members]
#==#
# `yield` starts a paren-less command argument: `p yield` passes the yielded
# value. Excluded from the argument-start set, the argument was dropped and the
# call printed nothing at all.
def shows = p yield
def prints = puts yield
def two = p yield, 1
def forwards(x) = x
def hands_on = forwards yield
shows { 42 }
prints { "hi" }
two { 7 }
p hands_on { 5 }
#==#
# The required argument of `each_with_object` is checked rather than indexed, and
# an argument-less `index`/`rindex` is an Enumerator — both crashed the process.
def arity_err
  yield
rescue ArgumentError => e
  e.message
end
p arity_err { [1, 2].each_with_object }
p arity_err { {a: 1}.each_with_object }
p arity_err { [1, 2].include? }
p [[1, 2, 3].index.class, [1, 2, 3].index.to_a, [1, 2, 3].rindex.class,
   [1, 2, 3].find_index.to_a, [1, 2, 3].index(2), [1, 2, 3].rindex(2)]
