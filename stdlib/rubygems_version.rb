# rubygems/version — a pragmatic port of RubyGems' Gem::Version, bundled into
# rubylang and loaded by `require "rubygems/version"`. Parses and compares
# version strings (`"1.2.3"`, `"1.0.0.pre.1"`) with RubyGems' prerelease rules.

module Gem
  class Version
    include Comparable

    # The RubyGems grammar for a version string.
    VERSION_PATTERN = '[0-9]+(?>\.[0-9a-zA-Z]+)*(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?'
    ANCHORED_VERSION_PATTERN = /\A\s*(#{VERSION_PATTERN})?\s*\z/

    attr_reader :version
    alias to_s version

    # True when `version` is a well-formed version string (or nil/empty).
    def self.correct?(version)
      return false if version.nil?
      !!(ANCHORED_VERSION_PATTERN =~ version.to_s)
    end

    # Coerce `input` to a Version (pass Versions and nil through).
    def self.create(input)
      if input.is_a?(Version)
        input
      elsif input.nil?
        nil
      else
        new(input)
      end
    end

    def initialize(version)
      unless self.class.correct?(version)
        raise ArgumentError, "Malformed version number string #{version}"
      end
      # Store a normalized form: a leading `-` marks a prerelease (`1.0-2` =>
      # `1.0.pre.2`), matching RubyGems.
      @version = version.to_s.strip.gsub("-", ".pre.")
    end

    def inspect
      "#<#{self.class.name} #{@version.inspect}>"
    end

    # The version broken into comparable segments: integers stay integers,
    # alphabetic segments (prerelease tags) stay strings.
    def segments
      @segments ||= @version.scan(/[0-9]+|[a-z]+/i).map do |s|
        s =~ /\A\d+\z/ ? s.to_i : s
      end
    end

    # A version is a prerelease if any segment contains a letter.
    def prerelease?
      unless defined?(@prerelease)
        @prerelease = !!(@version =~ /[a-zA-Z]/)
      end
      @prerelease
    end

    # The release version (prerelease segments dropped): `1.2.pre.1` => `1.2`.
    def release
      return self unless prerelease?
      segs = segments.take_while { |s| s.is_a?(Integer) }
      self.class.new(segs.join("."))
    end

    def bump
      segs = segments.take_while { |s| s.is_a?(Integer) }
      segs.pop if segs.size > 1
      segs[-1] = segs[-1] + 1
      self.class.new(segs.join("."))
    end

    def <=>(other)
      other = self.class.create(other) unless other.is_a?(Version)
      return nil unless other.is_a?(Version)
      return 0 if @version == other.version

      lhs = segments
      rhs = other.segments
      limit = [lhs.size, rhs.size].max
      0.upto(limit - 1) do |i|
        l = lhs[i]
        r = rhs[i]
        # A missing segment: numeric release outranks a prerelease tag.
        return 0 if l.nil? && r.nil?
        l = 0 if l.nil?
        r = 0 if r.nil?
        next if l == r
        # Integer segments outrank (are newer than) String prerelease segments.
        if l.is_a?(Integer) && r.is_a?(String)
          return 1
        elsif l.is_a?(String) && r.is_a?(Integer)
          return -1
        else
          cmp = l <=> r
          return cmp unless cmp == 0
        end
      end
      0
    end

    def eql?(other)
      other.is_a?(Version) && segments == other.segments
    end

    def hash
      segments.hash
    end
  end
end
