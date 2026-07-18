# Regular expressions: literals, matching operators, and MatchData.

# `=~` yields the match offset (nil on no match).
puts("the year 2024" =~ /\d+/)

# match? is a boolean test.
puts "hello world".match?(/\bworld\b/)

# scan collects every match; capture groups produce sub-arrays.
puts "a1 b2 c3".scan(/[a-z]\d/).inspect
puts "a1 b2 c3".scan(/([a-z])(\d)/).inspect

# split on a pattern.
puts "one, two;three four".split(/[,;\s]+/).inspect

# sub/gsub with a Regexp: backrefs in the replacement, or a block.
puts "2024-01-15".sub(/(\d+)-(\d+)-(\d+)/, '\3/\2/\1')
puts "shout quietly".gsub(/\w+/) { |w| w.upcase }

# match returns a MatchData with numbered groups and surrounding text.
m = "user: alice (admin)".match(/(\w+) \((\w+)\)/)
puts m[0]
puts m[1]
puts m[2]
puts m.pre_match.inspect

# A successful =~ sets the match globals $~, $1..$9, $&.
"order #4271 shipped" =~ /#(\d+)/
puts $1
puts $&
puts $~.pre_match

# Regexp objects work as case-equality tests.
["42", "foo", "3.14"].each do |s|
  kind = case s
         when /\A\d+\z/ then "int"
         when /\A\d+\.\d+\z/ then "float"
         else "word"
         end
  puts "#{s} -> #{kind}"
end
