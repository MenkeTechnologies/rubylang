# strscan — a pure-Ruby StringScanner, bundled into rubylang and loaded by
# `require "strscan"`. A cursor over a string that matches anchored patterns at
# the current position (`scan`) or searches ahead (`scan_until`). rack, sinatra
# (via mustermann), and rails all lean on it for tokenizing.

class StringScanner
  attr_reader :pos

  def initialize(string, _dup = false)
    @string = string.to_s
    @pos = 0
    @match = nil
    @prev = 0
  end

  def string
    @string
  end

  def string=(str)
    @string = str.to_s
    reset
    str
  end

  # Advance the cursor to `n` (clamped to the string length).
  def pos=(n)
    n += @string.length if n < 0
    @pos = n.clamp(0, @string.length)
  end
  alias pointer pos
  alias pointer= pos=

  def reset
    @pos = 0
    @match = nil
    self
  end

  def terminate
    @pos = @string.length
    @match = nil
    self
  end
  alias clear terminate

  def eos?
    @pos >= @string.length
  end

  def rest
    @string[@pos..] || ""
  end

  def rest_size
    rest.length
  end
  alias rest_length rest_size

  # The unscanned remainder as of the last match (for pre/post match).
  def matched
    @match && @match[0]
  end

  def matched?
    !@match.nil?
  end

  def matched_size
    @match ? @match[0].length : nil
  end

  def pre_match
    @match ? @string[0, @prev] : nil
  end

  def post_match
    @match ? @string[@pos..] : nil
  end

  # `scanner[n]` — capture group `n` from the last match.
  def [](n)
    @match && @match[n]
  end

  # Match `pattern` anchored at the current position; on success advance past it
  # and return the matched text, else return nil and leave the cursor put.
  def scan(pattern)
    do_scan(pattern, true, true)
  end

  # Like `scan` but return the number of characters consumed (or nil).
  def skip(pattern)
    m = do_scan(pattern, true, true)
    m&.length
  end

  # Match at the current position without advancing; return the matched text.
  def check(pattern)
    do_scan(pattern, false, true)
  end

  # Match at the current position without advancing; return the match length.
  def match?(pattern)
    m = do_scan(pattern, false, true)
    m&.length
  end

  # Search forward for `pattern`; consume everything up to and including the
  # match and return that span (or nil).
  def scan_until(pattern)
    do_scan(pattern, true, false)
  end

  # Search forward without advancing; return the span up to and including a match.
  def check_until(pattern)
    do_scan(pattern, false, false)
  end

  # Consume and return the next character.
  def getch
    return nil if eos?
    ch = @string[@pos]
    @prev = @pos
    @pos += 1
    @match = [ch]
    ch
  end

  # Look ahead `len` characters without advancing.
  def peek(len)
    @string[@pos, len] || ""
  end
  alias peep peek

  # Un-scan the last match, restoring the previous position.
  def unscan
    raise "unscan failed: previous match record not exist" if @match.nil?
    @pos = @prev
    @match = nil
    self
  end

  def beginning_of_line?
    @pos == 0 || @string[@pos - 1] == "\n"
  end
  alias bol? beginning_of_line?

  def inspect
    "#<StringScanner #{eos? ? "fin" : "#{@pos}/#{@string.length}"}>"
  end

  private

  # `anchored` true matches only at the cursor (`scan`/`check`); false searches
  # ahead (`scan_until`). `advance` moves the cursor past the match on success.
  def do_scan(pattern, advance, anchored)
    pattern = Regexp.new(Regexp.escape(pattern)) if pattern.is_a?(String)
    target = @string[@pos..] || ""
    m = pattern.match(target)
    return nil if m.nil?
    pre = m.pre_match
    # An anchored scan (`scan`/`check`) only accepts a match at the cursor.
    return nil if anchored && !pre.empty?
    consumed_to = pre.length + m[0].length
    result = anchored ? m[0] : target[0, consumed_to]
    @prev = @pos
    @match = m
    @pos += consumed_to if advance
    result
  end
end

# MRI exposes the same class under `::StringScanner`; some code also references
# `StringScanner::Error`.
class StringScanner
  class Error < StandardError; end
end
