# yaml — a pragmatic pure-Ruby subset of Ruby's YAML (Psych) library, bundled
# into rubylang and loaded by `require "yaml"`. Covers block-style emit/parse for
# the everyday Ruby types: Hash, Array, String, Symbol, Integer, Float,
# true/false/nil. Handles block mappings, block sequences, nested indentation,
# and inline flow collections (`[1, 2]`, `{a: 1}`) on load. Not the full YAML
# spec (no anchors/aliases, tags, multi-document streams, block scalars, or
# custom object (de)serialization). `dump` output round-trips through `load`.

module YAML
  # Registries mapping YAML tags <-> Ruby class names (Psych uses these for
  # custom object (de)serialization). rubylang does not (de)serialize tagged
  # objects, but Rails/activesupport register tags at load time, so provide the
  # persistent hashes so `YAML.load_tags[tag] = class_name` works.
  def self.load_tags
    @load_tags ||= {}
  end

  def self.dump_tags
    @dump_tags ||= {}
  end

  # Serialize `obj` to a YAML document string (`---` header + block body).
  def self.dump(obj, io = nil)
    body = Emitter.new.emit(obj)
    doc = "---" + body
    if io
      io.write(doc)
      io
    else
      doc
    end
  end

  # Parse the first YAML document in `str` into a Ruby object.
  def self.load(str, **_opts)
    Parser.new(str).parse
  end

  # `safe_load` here is `load` (this subset never deserializes arbitrary objects).
  def self.safe_load(str, **_opts)
    load(str)
  end

  def self.parse(str)
    load(str)
  end

  def self.load_file(path)
    load(File.read(path))
  end

  def self.dump_file(path, obj)
    File.write(path, dump(obj))
  end

  # --- emitter ------------------------------------------------------------
  class Emitter
    # Emit `obj`. Top-level scalars follow the header inline (`--- 5`);
    # collections start on the next line. Returns the text after `---`.
    def emit(obj)
      case obj
      when Hash
        obj.empty? ? " {}\n" : "\n" + emit_map(obj, 0)
      when Array
        obj.empty? ? " []\n" : "\n" + emit_seq(obj, 0)
      else
        " " + scalar(obj) + "\n"
      end
    end

    def emit_map(hash, indent)
      pad = " " * indent
      out = +""
      hash.each do |k, v|
        key = pad + scalar_key(k) + ":"
        out << emit_value(key, v, indent)
      end
      out
    end

    def emit_seq(arr, indent)
      pad = " " * indent
      out = +""
      arr.each do |v|
        prefix = pad + "-"
        out << emit_value(prefix, v, indent, seq: true)
      end
      out
    end

    # Emit `value` after `prefix` (`key:` or `-`): a scalar goes inline; a nested
    # collection breaks to the next line. Psych keeps sequences at the parent
    # indent under a mapping key, and indents nested mappings by two.
    def emit_value(prefix, value, indent, seq: false)
      case value
      when Hash
        if value.empty?
          "#{prefix} {}\n"
        else
          child = seq ? indent + 2 : indent + 2
          # A mapping right after a `-` starts on the same line as the dash.
          if seq
            first, *rest = map_lines(value, indent + 2)
            "#{prefix} #{first.lstrip}\n" + rest.map { |l| l + "\n" }.join
          else
            "#{prefix}\n" + emit_map(value, child)
          end
        end
      when Array
        if value.empty?
          "#{prefix} []\n"
        else
          # Sequences stay at the parent indent (Psych style).
          "#{prefix}\n" + emit_seq(value, indent)
        end
      else
        s = scalar(value)
        # A nil (empty) scalar has no trailing space: `key:` / `-`, not `key: `.
        s.empty? ? "#{prefix}\n" : "#{prefix} #{s}\n"
      end
    end

    def map_lines(hash, indent)
      emit_map(hash, indent).split("\n")
    end

    def scalar_key(k)
      scalar(k)
    end

    # Render a scalar, quoting strings only when bare form would be ambiguous.
    def scalar(v)
      case v
      when nil then ""
      when true then "true"
      when false then "false"
      when Integer then v.to_s
      when Float then v.to_s
      when Symbol then ":" + v.to_s
      when String then string_scalar(v)
      else string_scalar(v.to_s)
      end
    end

    def string_scalar(s)
      if s.empty?
        '""'
      elsif s =~ /[\n\t]/ || s.include?("\\")
        # Escapes needed → double-quoted with backslash escapes.
        '"' + s.gsub("\\", "\\\\\\\\").gsub('"', '\\"') + '"'
      elsif needs_quote?(s)
        # Psych prefers single quotes for plain quoting (doubling any `'`).
        "'" + s.gsub("'", "''") + "'"
      else
        s
      end
    end

    # A string needs quoting when it would otherwise parse as another type or
    # break the block syntax (leading indicators, colons, look-alike literals).
    def needs_quote?(s)
      return true if s =~ /\A[\s]|[\s]\z/
      return true if s =~ /\A[-?:,\[\]{}#&*!|>'"%@`]/
      return true if s.include?(": ") || s.include?(" #") || s.include?("\n")
      return true if s =~ /\A(true|false|null|~|-?\d+(\.\d+)?)\z/i
      return true if s.start_with?(":")
      false
    end
  end

  # --- parser -------------------------------------------------------------
  class Parser
    def initialize(str)
      # Drop the document header and comment/blank lines; keep indentation.
      @lines = str.to_s.split("\n").reject do |l|
        st = l.strip
        st.empty? || st == "---" || st == "..." || st.start_with?("#")
      end
      @pos = 0
    end

    def parse
      return nil if @lines.empty?
      # A lone top-level scalar (`--- 5`) leaves one bare line.
      if @lines.length == 1 && !structural?(@lines[0])
        return scalar(@lines[0].strip)
      end
      parse_node(0)
    end

    private

    def structural?(line)
      s = line.strip
      s.start_with?("- ") || s == "-" || s.include?(": ") || s.end_with?(":")
    end

    def indent_of(line)
      line.length - line.lstrip.length
    end

    # Parse the block whose members are at column `min_indent`.
    def parse_node(min_indent)
      line = @lines[@pos]
      return nil if line.nil?
      ind = indent_of(line)
      body = line.strip
      if body.start_with?("- ") || body == "-"
        parse_seq(ind)
      else
        parse_map(ind)
      end
    end

    def parse_seq(indent)
      arr = []
      while @pos < @lines.length
        line = @lines[@pos]
        ind = indent_of(line)
        break if ind < indent
        body = line.strip
        break unless body.start_with?("- ") || body == "-"
        rest = body == "-" ? "" : body[2..]
        if rest.empty?
          @pos += 1
          arr << parse_child(indent + 1, indent + 1)
        elsif inline_map?(rest)
          # `- key: value` — a mapping whose first pair shares the dash line.
          @lines[@pos] = (" " * (ind + 2)) + rest
          arr << parse_map(ind + 2)
        else
          @pos += 1
          arr << scalar(rest)
        end
      end
      arr
    end

    def parse_map(indent)
      map = {}
      while @pos < @lines.length
        line = @lines[@pos]
        ind = indent_of(line)
        break if ind < indent
        break if ind > indent # handled by recursion
        body = line.strip
        break if body.start_with?("- ")
        key_str, val_str = split_kv(body)
        key = scalar_key(key_str)
        @pos += 1
        if val_str.nil? || val_str.empty?
          # A sequence value sits at the key's own indent (Psych); a nested
          # mapping is indented deeper.
          map[key] = parse_child(indent + 1, indent)
        else
          map[key] = scalar(val_str)
        end
      end
      map
    end

    # Parse the nested block that belongs to the just-consumed key or dash. A
    # sequence child is accepted at column >= `seq_min` (Psych puts a mapping's
    # sequence value at the key's own indent); a mapping child at >= `map_min`.
    def parse_child(map_min, seq_min)
      line = @lines[@pos]
      return nil if line.nil?
      ind = indent_of(line)
      body = line.strip
      if body.start_with?("- ") || body == "-"
        ind >= seq_min ? parse_seq(ind) : nil
      else
        ind >= map_min ? parse_map(ind) : nil
      end
    end

    def inline_map?(str)
      !str.start_with?("[") && !str.start_with?("{") && split_kv(str)[1] != nil ||
        (split_kv(str)[0] != str)
    end

    # Split `key: value` at the first `": "` (or a trailing `:`), respecting that
    # a colon inside a quoted key does not count.
    def split_kv(str)
      if str.end_with?(":")
        [str[0...-1], nil]
      elsif (idx = str.index(": "))
        [str[0...idx], str[(idx + 2)..]]
      else
        [str, nil]
      end
    end

    def scalar_key(str)
      scalar(str)
    end

    # Interpret a scalar token: flow collections, quoted strings, symbols, the
    # YAML literals, numbers, else a bare string.
    def scalar(token)
      s = token.strip
      return nil if s.empty? || s == "~" || s == "null" || s == "Null" || s == "NULL"
      return true if s == "true" || s == "True" || s == "TRUE"
      return false if s == "false" || s == "False" || s == "FALSE"
      if s.start_with?("[") && s.end_with?("]")
        return parse_flow_seq(s)
      end
      if s.start_with?("{") && s.end_with?("}")
        return parse_flow_map(s)
      end
      if s.start_with?('"') && s.end_with?('"') && s.length >= 2
        return unescape(s[1...-1])
      end
      if s.start_with?("'") && s.end_with?("'") && s.length >= 2
        return s[1...-1].gsub("''", "'")
      end
      if s.start_with?(":") && s.length > 1
        return s[1..].to_sym
      end
      if s =~ /\A-?\d+\z/
        return s.to_i
      end
      if s =~ /\A-?\d+\.\d+\z/
        return s.to_f
      end
      s
    end

    def parse_flow_seq(s)
      inner = s[1...-1].strip
      return [] if inner.empty?
      split_flow(inner).map { |e| scalar(e) }
    end

    def parse_flow_map(s)
      inner = s[1...-1].strip
      return {} if inner.empty?
      map = {}
      split_flow(inner).each do |pair|
        k, v = pair.split(":", 2)
        map[scalar(k.to_s.strip)] = scalar(v.to_s.strip)
      end
      map
    end

    # Split a flow collection body on top-level commas (ignoring commas nested in
    # brackets or quotes).
    def split_flow(str)
      parts = []
      depth = 0
      cur = +""
      in_q = nil
      str.each_char do |c|
        if in_q
          cur << c
          in_q = nil if c == in_q
        elsif c == '"' || c == "'"
          in_q = c
          cur << c
        elsif c == "[" || c == "{"
          depth += 1
          cur << c
        elsif c == "]" || c == "}"
          depth -= 1
          cur << c
        elsif c == "," && depth == 0
          parts << cur
          cur = +""
        else
          cur << c
        end
      end
      parts << cur unless cur.strip.empty?
      parts
    end

    def unescape(s)
      s.gsub(/\\(.)/) do
        case $1
        when "n" then "\n"
        when "t" then "\t"
        when '"' then '"'
        when "\\" then "\\"
        else $1
        end
      end
    end
  end
end

# `obj.to_yaml` shorthand for `YAML.dump(obj)`.
class Object
  def to_yaml
    YAML.dump(self)
  end
end
