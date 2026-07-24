# logger — a pragmatic port of Ruby's stdlib Logger, bundled into rubylang and
# loaded by `require "logger"`. Writes severity-tagged lines to an IO (or any
# object with `#write`/`#<<`), with the standard severity levels and predicates.

class Logger
  # Severity levels (also available as `Logger::Severity::*`).
  DEBUG = 0
  INFO = 1
  WARN = 2
  ERROR = 3
  FATAL = 4
  UNKNOWN = 5

  module Severity
    DEBUG = 0
    INFO = 1
    WARN = 2
    ERROR = 3
    FATAL = 4
    UNKNOWN = 5

    LABELS = %w[DEBUG INFO WARN ERROR FATAL ANY].freeze
  end

  LABELS = %w[DEBUG INFO WARN ERROR FATAL ANY].freeze

  class LogDevice
    attr_reader :dev
    def initialize(dev)
      @dev = dev
    end

    def write(message)
      @dev.write(message) if @dev.respond_to?(:write)
    end

    def close
      @dev.close if @dev.respond_to?(:close)
    end
  end

  attr_accessor :level, :progname, :formatter

  def initialize(logdev = $stderr, _shift_age = 0, _shift_size = 1_048_576, level: DEBUG, progname: nil, formatter: nil, **_opts)
    @level = level
    @progname = progname
    @formatter = formatter
    @logdev = logdev
  end

  def level=(sev)
    @level = sev.is_a?(Integer) ? sev : LABELS.index(sev.to_s.upcase) || DEBUG
  end

  # Emit a message at `severity` if it clears the current level. The message can
  # be the second argument, the block's return value, or the progname.
  def add(severity, message = nil, progname = nil, &block)
    severity ||= UNKNOWN
    return true if severity < @level
    progname ||= @progname
    if message.nil?
      if block
        message = block.call
      else
        message = progname
        progname = @progname
      end
    end
    write_log(format_message(LABELS[severity] || "ANY", message, progname))
    true
  end
  alias log add

  def debug(progname = nil, &block); add(DEBUG, nil, progname, &block); end
  def info(progname = nil, &block); add(INFO, nil, progname, &block); end
  def warn(progname = nil, &block); add(WARN, nil, progname, &block); end
  def error(progname = nil, &block); add(ERROR, nil, progname, &block); end
  def fatal(progname = nil, &block); add(FATAL, nil, progname, &block); end
  def unknown(progname = nil, &block); add(UNKNOWN, nil, progname, &block); end

  def debug?; @level <= DEBUG; end
  def info?; @level <= INFO; end
  def warn?; @level <= WARN; end
  def error?; @level <= ERROR; end
  def fatal?; @level <= FATAL; end

  def <<(msg)
    write_log(msg.to_s)
    msg.to_s.length
  end

  def close
    @logdev.close if @logdev.respond_to?(:close)
  end

  private

  def format_message(label, message, progname)
    if @formatter
      @formatter.call(label, Time.now, progname, message)
    else
      pn = progname ? " -- #{progname}:" : " --"
      "#{label[0]}, [#{Time.now.strftime('%Y-%m-%dT%H:%M:%S')}]#{pn} #{message}\n"
    end
  end

  def write_log(str)
    if @logdev.respond_to?(:write)
      @logdev.write(str)
    elsif @logdev.respond_to?(:<<)
      @logdev << str
    end
  end
end
