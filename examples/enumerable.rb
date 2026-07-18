# Enumerable pipeline — map / select / reduce over a range.
squares = (1..10).map { |n| n * n }
puts "squares:  #{squares.inspect}"
puts "evens:    #{squares.select { |n| n.even? }.inspect}"
puts "sum:      #{squares.reduce(0) { |a, b| a + b }}"
puts "max:      #{squares.max}"
