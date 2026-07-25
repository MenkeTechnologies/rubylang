# pathname — a pragmatic pure-Ruby subset of Ruby's Pathname, bundled into
# rubylang and loaded by `require "pathname"`. Pathname wraps a filesystem path
# string with path-manipulation and filesystem-query methods. This covers the
# surface Rails and common gems rely on (join, +, parent, basename, dirname,
# expand_path, cleanpath, the exist?/directory?/file? predicates, read/write);
# it is not the full API (no mkpath/rmtree, no find, no fnmatch).

class Pathname
  SEPARATOR = "/"

  def initialize(path)
    @path = path.is_a?(Pathname) ? path.to_s : path.to_s
  end

  def to_s
    @path
  end
  alias to_str to_s
  alias to_path to_s

  def inspect
    "#<Pathname:#{@path}>"
  end

  # Join this path with one or more components (`root.join("config", "x.yml")`).
  def join(*others)
    result = @path
    others.each do |other|
      part = other.to_s
      result = if part.start_with?(SEPARATOR)
        part
      elsif result.empty? || result.end_with?(SEPARATOR)
        "#{result}#{part}"
      else
        "#{result}#{SEPARATOR}#{part}"
      end
    end
    Pathname.new(result)
  end

  def +(other)
    join(other)
  end
  alias_method :/, :+

  def basename(ext = "")
    Pathname.new(File.basename(@path, ext))
  end

  def dirname
    Pathname.new(File.dirname(@path))
  end

  def extname
    File.extname(@path)
  end

  def expand_path(base = nil)
    Pathname.new(base ? File.expand_path(@path, base.to_s) : File.expand_path(@path))
  end

  def parent
    dirname
  end

  # Collapse `.`/`..`/redundant separators without touching the filesystem.
  def cleanpath
    absolute = @path.start_with?(SEPARATOR)
    parts = @path.split(SEPARATOR).reject { |p| p.empty? || p == "." }
    stack = []
    parts.each do |p|
      if p == ".." && !stack.empty? && stack.last != ".."
        stack.pop
      elsif p == ".." && absolute
        # `/..` stays at root
      else
        stack << p
      end
    end
    cleaned = stack.join(SEPARATOR)
    cleaned = "#{SEPARATOR}#{cleaned}" if absolute
    cleaned = "." if cleaned.empty?
    Pathname.new(cleaned)
  end

  def split
    [dirname, basename]
  end

  def each_filename(&block)
    @path.split(SEPARATOR).reject(&:empty?).each(&block)
  end

  def sub(pattern, replacement)
    Pathname.new(@path.sub(pattern, replacement))
  end

  def absolute?
    @path.start_with?(SEPARATOR)
  end

  def relative?
    !absolute?
  end

  def root?
    @path == SEPARATOR
  end

  def exist?
    File.exist?(@path)
  end

  def directory?
    File.directory?(@path)
  end

  def file?
    File.file?(@path)
  end

  def readable?
    File.exist?(@path)
  end

  def read(*args)
    File.read(@path, *args)
  end

  def write(content, *args)
    File.write(@path, content, *args)
  end

  def binread(*args)
    File.read(@path, *args)
  end

  def readlines(*args)
    File.readlines(@path, *args)
  end

  def children(with_directory = true)
    entries = Dir.children(@path)
    entries.map { |e| with_directory ? join(e) : Pathname.new(e) }
  end

  def mkpath
    require "fileutils"
    FileUtils.mkdir_p(@path)
    self
  rescue LoadError, NoMethodError
    self
  end

  def ==(other)
    other.is_a?(Pathname) && @path == other.to_s
  end
  alias eql? ==

  def hash
    @path.hash
  end

  def <=>(other)
    return nil unless other.is_a?(Pathname)
    @path <=> other.to_s
  end
  include Comparable
end

# `Pathname(path)` — the Kernel shorthand constructor.
def Pathname(path)
  path.is_a?(Pathname) ? path : Pathname.new(path)
end
