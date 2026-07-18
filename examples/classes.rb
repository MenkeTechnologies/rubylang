# Classes, inheritance, super, modules, and exceptions.
module Describable
  def describe; "#{self.class}: #{summary}"; end
end

class Shape
  include Describable
  def initialize(name); @name = name; end
  def area; 0; end
  def summary; "#{@name} (area #{area})"; end
end

class Circle < Shape
  def initialize(r); super("circle"); @r = r; end
  def area; (3.14159 * @r * @r).round; end
end

class Square < Shape
  def initialize(side); super("square"); @side = side; end
  def area; @side * @side; end
end

[Circle.new(2), Square.new(3)].each { |s| puts s.describe }

# Custom exception + a class method factory.
class NegativeError < StandardError; end

class SquareRootArea
  def self.of(side)
    raise NegativeError, "side #{side} is negative" if side < 0
    side * side
  end
end

[4, -1].each do |s|
  begin
    puts "area = #{SquareRootArea.of(s)}"
  rescue NegativeError => e
    puts "error: #{e.message}"
  end
end
