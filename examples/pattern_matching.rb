# Structural pattern matching (case/in).

def route(request)
  case request
  in {method: "GET", path: ["users", id]}
    "show user #{id}"
  in {method: "GET", path: ["users"]}
    "list users"
  in {method: "POST", path: ["users"], body: {name:}}
    "create user #{name}"
  in {method:, path:}
    "#{method} /#{path.join('/')}"
  end
end

puts route({method: "GET", path: ["users", "42"]})
puts route({method: "GET", path: ["users"]})
puts route({method: "POST", path: ["users"], body: {name: "Ada"}})
puts route({method: "DELETE", path: ["sessions", "1"]})

# Array patterns with a splat and a guard.
def summarize(nums)
  case nums
  in []
    "empty"
  in [only]
    "one: #{only}"
  in [first, *rest] if rest.length > 2
    "#{first} and #{rest.length} more"
  in [a, b]
    "pair #{a}, #{b}"
  end
end

puts summarize([])
puts summarize([7])
puts summarize([1, 2])
puts summarize([1, 2, 3, 4, 5])

# Binding the whole match and matching by type.
case [1, 2, 3]
in [Integer, Integer, Integer] => triple
  puts "three ints: #{triple.sum}"
end
