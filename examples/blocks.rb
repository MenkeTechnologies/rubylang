# Blocks, yield, and closures capturing enclosing scope.
def repeat(n)
  i = 0
  while i < n
    yield i
    i += 1
  end
end

total = 0
repeat(5) { |x| total += x }
puts "total: #{total}"   # 0+1+2+3+4 = 10
