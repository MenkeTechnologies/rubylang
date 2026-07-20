# csv — a pragmatic pure-Ruby subset of Ruby's CSV library, bundled into
# rubylang and loaded by `require "csv"`. Covers RFC 4180 parsing/generation:
# comma separation, `"`-quoting with doubled-quote escaping, embedded newlines
# inside quotes, and the common class methods (`parse`, `parse_line`,
# `generate`, `generate_line`, `foreach`, `read`). Not the full API (no
# converters, no `:headers` Row objects, no custom quote chars).

class CSV
  attr_accessor :col_sep, :row_sep, :quote_char

  def initialize(io_or_str, col_sep: ",", row_sep: "\n", quote_char: '"')
    @data = io_or_str.respond_to?(:read) ? io_or_str.read : io_or_str.to_s
    @col_sep = col_sep
    @row_sep = row_sep
    @quote_char = quote_char
  end

  # Parse the whole input into an Array of row Arrays.
  def read
    self.class.parse(@data, col_sep: @col_sep, quote_char: @quote_char)
  end
  alias to_a read

  def each
    return enum_for(:each) unless block_given?
    read.each { |row| yield row }
    self
  end

  # Parse a CSV string into an Array of rows (each an Array of String/nil
  # fields). With a block, yields each row instead and returns nil.
  def self.parse(str, col_sep: ",", quote_char: '"', &block)
    rows = []
    row = []
    field = +""
    in_quotes = false
    quoted_field = false
    field_started = false
    chars = str.chars
    i = 0
    push_field = lambda do
      row << (field_started || quoted_field ? field : nil)
      field = +""
      field_started = false
      quoted_field = false
    end
    push_row = lambda do
      push_field.call
      rows << row
      row = []
    end
    while i < chars.length
      c = chars[i]
      if in_quotes
        if c == quote_char
          if chars[i + 1] == quote_char
            field << quote_char
            i += 1
          else
            in_quotes = false
          end
        else
          field << c
        end
      elsif c == quote_char
        in_quotes = true
        quoted_field = true
        field_started = true
      elsif c == col_sep
        push_field.call
      elsif c == "\r"
        # Swallow CR; a following LF is the row break, else CR alone ends the row.
        if chars[i + 1] == "\n"
          i += 1
        end
        push_row.call
      elsif c == "\n"
        push_row.call
      else
        field << c
        field_started = true
      end
      i += 1
    end
    # A trailing row with no final newline still counts (unless the input was empty).
    unless field.empty? && row.empty? && !field_started && !quoted_field
      push_row.call
    end
    if block
      rows.each(&block)
      nil
    else
      rows
    end
  end

  # Parse a single CSV line into an Array of fields.
  def self.parse_line(str, col_sep: ",", quote_char: '"')
    rows = parse(str, col_sep: col_sep, quote_char: quote_char)
    rows.first
  end

  def self.read(path, **opts)
    parse(File.read(path), **opts)
  end

  def self.foreach(path, **opts, &block)
    parse(File.read(path), **opts, &block)
  end

  # Build CSV text by yielding a sink that rows are appended to with `<<`.
  def self.generate(str = +"", col_sep: ",", row_sep: "\n", quote_char: '"')
    io = Generator.new(str, col_sep: col_sep, row_sep: row_sep, quote_char: quote_char)
    yield io
    io.string
  end

  # Render one row as a CSV line (including the row separator).
  def self.generate_line(row, col_sep: ",", row_sep: "\n", quote_char: '"')
    row.map { |f| quote(f, col_sep, row_sep, quote_char) }.join(col_sep) + row_sep
  end

  # Quote a field only when it contains the separator, a quote, or a newline;
  # embedded quotes are doubled.
  def self.quote(field, col_sep, row_sep, quote_char)
    return "" if field.nil?
    s = field.to_s
    if s.include?(col_sep) || s.include?(quote_char) || s.include?("\n") || s.include?("\r")
      quote_char + s.gsub(quote_char, quote_char * 2) + quote_char
    else
      s
    end
  end

  class Generator
    attr_reader :string

    def initialize(str, col_sep:, row_sep:, quote_char:)
      @string = str
      @col_sep = col_sep
      @row_sep = row_sep
      @quote_char = quote_char
    end

    def <<(row)
      @string << CSV.generate_line(row, col_sep: @col_sep, row_sep: @row_sep, quote_char: @quote_char)
      self
    end
  end
end
