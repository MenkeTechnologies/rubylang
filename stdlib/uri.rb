# uri — a pragmatic pure-Ruby subset of Ruby's URI library, bundled into
# rubylang and loaded by `require "uri"`. Covers the common surface: parsing a
# URI into its components, reconstructing it, generic/HTTP/HTTPS scheme classes,
# and www-form encode/decode. Not the full RFC 3986 API (no registry of every
# scheme, no `URI.extract`, no IPv6 zone ids).

module URI
  class Error < StandardError; end
  class InvalidURIError < Error; end
  class InvalidComponentError < Error; end

  # RFC 3986 unreserved characters stay literal in www-form / component escaping.
  UNRESERVED = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~"

  class Generic
    attr_accessor :scheme, :userinfo, :host, :port, :path, :query, :fragment

    def initialize(scheme, userinfo, host, port, path, query, fragment)
      @scheme = scheme
      @userinfo = userinfo
      @host = host
      # An authority with no explicit port reports the scheme's default (MRI).
      @port = port || (host ? default_port : nil)
      @path = path
      @query = query
      @fragment = fragment
    end

    # The scheme's well-known port (nil for a generic URI); subclasses override.
    def default_port
      nil
    end

    def hostname
      @host
    end

    def absolute?
      !@scheme.nil?
    end

    def relative?
      @scheme.nil?
    end

    def to_s
      s = +""
      s << "#{@scheme}:" if @scheme
      if @host || @userinfo
        s << "//"
        s << "#{@userinfo}@" if @userinfo
        s << @host.to_s
        s << ":#{@port}" if @port && @port != default_port
      end
      s << @path.to_s
      s << "?#{@query}" if @query
      s << "##{@fragment}" if @fragment
      s
    end

    def inspect
      "#<#{self.class} #{self}>"
    end

    def ==(other)
      other.is_a?(URI::Generic) && to_s == other.to_s
    end
  end

  class HTTP < Generic
    def default_port
      80
    end

    # `request_uri` is the path (defaulting to "/") plus any query.
    def request_uri
      p = @path.nil? || @path.empty? ? "/" : @path
      @query ? "#{p}?#{@query}" : p
    end
  end

  class HTTPS < HTTP
    def default_port
      443
    end
  end

  class FTP < Generic
    def default_port
      21
    end
  end

  SCHEME_CLASSES = {
    "http" => HTTP,
    "https" => HTTPS,
    "ftp" => FTP,
  }

  # Split a URI string into (scheme, userinfo, host, port, path, query, fragment).
  # Mirrors the RFC 3986 appendix-B grouping, then peels the authority apart.
  def self.split(str)
    m = %r{\A(?:([^:/?#]+):)?(//([^/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?\z}.match(str)
    raise InvalidURIError, "bad URI(is not URI?): #{str.inspect}" unless m
    scheme = m[1]
    authority = m[3]
    path = m[4]
    query = m[5]
    fragment = m[6]

    userinfo = nil
    host = nil
    port = nil
    unless authority.nil?
      auth = authority
      if (at = auth.index("@"))
        userinfo = auth[0...at]
        auth = auth[(at + 1)..]
      end
      if (colon = auth.rindex(":"))
        maybe_port = auth[(colon + 1)..]
        if maybe_port =~ /\A\d*\z/
          port = maybe_port.empty? ? nil : maybe_port.to_i
          auth = auth[0...colon]
        end
      end
      host = auth
    end
    [scheme, userinfo, host, port, path, query, fragment]
  end

  # Parse `str` into a URI object — an HTTP/HTTPS/FTP instance for those schemes,
  # else a Generic. The scheme is matched case-insensitively.
  def self.parse(str)
    scheme, userinfo, host, port, path, query, fragment = split(str)
    klass = scheme ? SCHEME_CLASSES[scheme.downcase] || Generic : Generic
    klass.new(scheme, userinfo, host, port, path, query, fragment)
  end

  def self.join(*parts)
    parts = parts.map { |p| p.is_a?(Generic) ? p : parse(p.to_s) }
    base = parts.shift
    parts.each { |rel| base = merge(base, rel) }
    base
  end

  # Resolve `rel` against `base` (enough of RFC 3986 §5.3 for the common cases:
  # absolute rel wins; an absolute path replaces; a relative path is merged).
  def self.merge(base, rel)
    return rel if rel.absolute?
    scheme = base.scheme
    host = base.host
    port = base.port
    userinfo = base.userinfo
    if rel.host
      host = rel.host
      port = rel.port
      userinfo = rel.userinfo
      path = rel.path
    elsif rel.path.nil? || rel.path.empty?
      path = base.path
    elsif rel.path.start_with?("/")
      path = rel.path
    else
      dir = base.path.to_s.sub(%r{[^/]*\z}, "")
      path = dir + rel.path
    end
    klass = scheme ? SCHEME_CLASSES[scheme.downcase] || Generic : Generic
    klass.new(scheme, userinfo, host, port, path, rel.query, rel.fragment)
  end

  # Percent-encode one component value (unreserved chars stay literal).
  def self.encode_component(str)
    out = +""
    str.to_s.each_byte do |b|
      c = b.chr
      if UNRESERVED.include?(c)
        out << c
      else
        out << format("%%%02X", b)
      end
    end
    out
  end

  def self.decode_component(str)
    str.to_s.gsub(/%([0-9a-fA-F]{2})/) { $1.to_i(16).chr }
  end

  # `key=value&key2=value2` form encoding. A Hash or an Array of [k, v] pairs;
  # spaces become `+`, everything else percent-encoded.
  def self.encode_www_form(enum)
    pairs = enum.is_a?(Hash) ? enum.to_a : enum
    pairs.map { |k, v| "#{encode_www_form_component(k)}=#{encode_www_form_component(v)}" }.join("&")
  end

  def self.encode_www_form_component(str)
    out = +""
    str.to_s.each_byte do |b|
      c = b.chr
      if UNRESERVED.include?(c)
        out << c
      elsif c == " "
        out << "+"
      else
        out << format("%%%02X", b)
      end
    end
    out
  end

  def self.decode_www_form(str)
    str.to_s.split("&").map do |pair|
      k, v = pair.split("=", 2)
      [decode_www_form_component(k.to_s), decode_www_form_component(v.to_s)]
    end
  end

  def self.decode_www_form_component(str)
    str.to_s.tr("+", " ").gsub(/%([0-9a-fA-F]{2})/) { $1.to_i(16).chr }
  end

  # The RFC2396 escaping engine. MRI keeps a shared instance for percent-encoding
  # arbitrary strings against a caller-supplied "unsafe" character pattern; sinatra
  # (via mustermann) uses only `escape`/`unescape`.
  class RFC2396_Parser
    # Everything outside the RFC2396 unreserved + reserved set is escaped by default.
    DEFAULT_UNSAFE = /[^\-_.!~*'()a-zA-Z\d;\/?:@&=+$,\[\]]/

    # Percent-encode each character of `str` that matches `unsafe` (a Regexp),
    # leaving the rest literal. Multibyte characters are encoded byte-by-byte.
    def escape(str, unsafe = DEFAULT_UNSAFE)
      str.to_s.gsub(unsafe) do |c|
        c.each_byte.map { |b| format("%%%02X", b) }.join
      end
    end

    # Reverse `escape`: turn every `%XX` sequence back into its byte.
    def unescape(str)
      str.to_s.gsub(/%([0-9a-fA-F]{2})/) { $1.to_i(16).chr }
    end

    # The component-regexp table (`parser.regexp[:UNSAFE]` etc.). mustermann's
    # AST compiler escapes literal characters with `URI_PARSER.regexp[:UNSAFE]`.
    def regexp(component = nil)
      table = {
        UNSAFE: DEFAULT_UNSAFE,
        ESCAPED: /%[a-fA-F0-9]{2}/,
      }
      component ? table[component] : table
    end
  end

  # The shared default parser instance (rack/rails read `URI::DEFAULT_PARSER`).
  DEFAULT_PARSER = RFC2396_Parser.new
end

# `URI("string")` — the shorthand constructor (a private Kernel method), same as
# `URI.parse`.
def URI(str)
  str.is_a?(URI::Generic) ? str : URI.parse(str)
end
