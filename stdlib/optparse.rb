# optparse — a pragmatic pure-Ruby subset of Ruby's OptionParser, bundled into
# rubylang and loaded by `require "optparse"`. Covers the common surface: short
# (`-v`) and long (`--verbose`) flags, options that take a value
# (`--name NAME` / `--name=NAME`), `--[no-]flag` booleans, Integer/Float
# coercion, a banner, `parse!`/`parse`, and `--help`/`to_s`. Not the full API
# (no completion, no required-argument enforcement, no `into:` hash).

class OptionParser
  class ParseError < StandardError; end
  class InvalidOption < ParseError; end
  class MissingArgument < ParseError; end

  attr_accessor :banner

  # One registered option: its short/long switch names, whether it takes an
  # argument, an optional coercion class, the help text, and the handler block.
  Switch = Struct.new(:short, :long, :takes_arg, :negatable, :type, :desc, :block)

  def initialize(banner = nil)
    @banner = banner || "Usage: #{File.basename($0)} [options]"
    @switches = []
    yield self if block_given?
  end

  # Register an option. Arguments are switch specs (`"-v"`, `"--name NAME"`,
  # `"--[no-]color"`), an optional coercion class (Integer/Float), and trailing
  # description strings; the block runs with the parsed value.
  def on(*args, &block)
    short = nil
    long = nil
    takes_arg = false
    negatable = false
    type = nil
    desc = []
    args.each do |a|
      if a.is_a?(Class)
        type = a
        takes_arg = true
      elsif a.is_a?(String) && a.start_with?("--")
        body = a[2..]
        if body.start_with?("[no-]")
          negatable = true
          long = "--" + body[5..].split(/[ =]/, 2).first
        else
          name, arg = body.split(/[ =]/, 2)
          long = "--" + name
          takes_arg = true unless arg.nil? || arg.empty?
        end
      elsif a.is_a?(String) && a.start_with?("-") && a.length >= 2
        short = a[0, 2]
        takes_arg = true if a.length > 2 || a.include?(" ")
      else
        desc << a
      end
    end
    @switches << Switch.new(short, long, takes_arg, negatable, type, desc, block)
    self
  end

  # Parse `argv` in place: remove recognized options (and their values) and
  # return the remaining non-option arguments. Options may appear after
  # positional arguments (MRI's default permutation).
  def parse!(argv)
    rest = []
    i = 0
    while i < argv.length
      tok = argv[i]
      if tok == "--"
        rest.concat(argv[(i + 1)..])
        break
      elsif tok.start_with?("--")
        name, inline = tok.split("=", 2)
        sw = find_long(name)
        raise InvalidOption, "invalid option: #{tok}" unless sw
        if sw.negatable
          run(sw, !name.start_with?("--no-"))
        elsif sw.takes_arg
          value = inline
          if value.nil?
            i += 1
            value = argv[i]
            raise MissingArgument, "missing argument: #{name}" if value.nil?
          end
          run(sw, coerce(sw, value))
        else
          run(sw, true)
        end
      elsif tok.start_with?("-") && tok.length >= 2
        sw = find_short(tok[0, 2])
        raise InvalidOption, "invalid option: #{tok}" unless sw
        if sw.takes_arg
          value = tok.length > 2 ? tok[2..] : nil
          if value.nil?
            i += 1
            value = argv[i]
            raise MissingArgument, "missing argument: #{tok}" if value.nil?
          end
          run(sw, coerce(sw, value))
        else
          run(sw, true)
        end
      else
        rest << tok
      end
      i += 1
    end
    argv.replace(rest)
    argv
  end

  def parse(argv)
    parse!(argv.dup)
  end

  # Render the banner plus one aligned line per option (for `--help`/`puts opts`).
  def to_s
    lines = [@banner]
    @switches.each do |sw|
      flags = []
      flags << sw.short if sw.short
      if sw.long
        flags << (sw.negatable ? sw.long.sub("--", "--[no-]") : sw.long)
      end
      left = "    " + flags.join(", ")
      left += " #{sw.type ? sw.type.to_s.upcase : "VALUE"}" if sw.takes_arg
      desc = sw.desc.first
      lines << (desc ? format("%-32s %s", left, desc) : left)
    end
    lines.join("\n") + "\n"
  end
  alias help to_s

  private

  def find_long(name)
    @switches.find do |sw|
      next false unless sw.long
      sw.long == name || (sw.negatable && name == sw.long.sub("--", "--no-"))
    end
  end

  def find_short(name)
    @switches.find { |sw| sw.short == name }
  end

  def coerce(sw, value)
    case sw.type.to_s
    when "Integer" then Integer(value)
    when "Float" then Float(value)
    else value
    end
  end

  def run(sw, value)
    sw.block.call(value) if sw.block
  end
end

# MRI aliases OptParse to OptionParser.
OptParse = OptionParser
