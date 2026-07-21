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
