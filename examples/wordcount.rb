# Count word frequencies — exercises hashes, blocks, string methods, sorting.
text = "the quick brown fox the lazy dog the fox"
counts = {}
text.split(" ").each do |word|
  counts[word] = (counts[word] || 0) + 1
end

counts.to_a.sort_by { |pair| -pair[1] }.each do |word, n|
  puts "#{word}: #{n}"
end
