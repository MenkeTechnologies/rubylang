# Heredocs: multi-line string literals.

# A plain heredoc keeps every line (with a trailing newline).
banner = <<END
== Report ==
generated below
END
puts banner

# A squiggly heredoc (<<~) strips the common leading indentation, so the source
# can stay indented with the surrounding code.
def describe(name, role)
  <<~PROFILE
    Name: #{name}
    Role: #{role}
  PROFILE
end
puts describe("Ada", "engineer")

# A single-quoted delimiter disables interpolation.
template = <<~'TMPL'
  Hello #{user}, your balance is #{amount}.
TMPL
puts template

# Heredocs compose with method calls.
puts <<~LIST.lines.map(&:strip).inspect
  one
  two
  three
LIST
