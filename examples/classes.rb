# Classes, inheritance, and exceptions.
class Shape
  def area; 0; end
  def describe; "#{self.class} with area #{area}"; end
end

class Circle < Shape
  def initialize(r); @r = r; end
  def area; (3.14159 * @r * @r).round; end
end

class Square < Shape
  def initialize(side); @side = side; end
  def area; @side * @side; end
end

[Circle.new(2), Square.new(3)].each do |shape|
  puts shape.describe
end

# Exception handling with a custom class.
class NegativeError < StandardError; end

def sqrt_area(side)
  raise NegativeError, "side #{side} is negative" if side < 0
  side * side
end

[4, -1].each do |s|
  begin
    puts "area = #{sqrt_area(s)}"
  rescue NegativeError => e
    puts "error: #{e.message}"
  end
end
