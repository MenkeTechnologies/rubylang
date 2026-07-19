# rack.rb — a pure-Ruby implementation of the Rack interface for rubylang.
#
# Rack is the contract every Ruby web framework (Rails, Sinatra, Roda) speaks:
# an "app" is any object responding to `call(env)` and returning a triple
#
#     [status_integer, headers_hash, body_enumerable]
#
# where `body_enumerable` responds to `each` and yields String chunks. This file
# implements that contract plus the pieces that sit on top of it: a Request/
# Response pair, a middleware Builder, a raw-TCP HTTP handler, a routing DSL with
# `:param`/`*wildcard` path matching, cookie + nested-param decoding, a handful
# of middlewares (Static, CommonLogger, Session::Cookie), and an in-process
# MockRequest test driver.
#
# It is self-contained — it requires only rubylang's own socket/erb/json/digest/
# base64, not the Rack gem — and it is written to run on `target/debug/ruby`.
# Where a common MRI idiom is not yet available on rubylang the code uses an
# equivalent that is; the accompanying report enumerates each such gap.

require "socket"
require "digest"
require "base64"

module Rack
  # Rack::Utils — the small pile of encoding/decoding helpers Rack ships. Kept as
  # module functions so both Request and the middlewares can call them.
  module Utils
    # Percent-decode a query component: '+' -> space, %XX -> byte.
    def self.unescape(str)
      return "" if str.nil?
      out = str.gsub("+", " ")
      result = ""
      i = 0
      len = out.length
      while i < len
        ch = out[i]
        if ch == "%" && i + 2 <= len - 1
          hex = out[(i + 1)..(i + 2)]
          if hex.length == 2 && hex =~ /\A[0-9a-fA-F]{2}\z/
            result << hex.to_i(16).chr
            i += 3
            next
          end
        end
        result << ch
        i += 1
      end
      result
    end

    # Percent-encode a string for use in a cookie/query value. Encodes anything
    # outside the unreserved set (RFC 3986: A-Z a-z 0-9 - _ . ~).
    def self.escape(str)
      out = ""
      str.to_s.each_char do |ch|
        if ch =~ /[A-Za-z0-9\-_.~]/
          out << ch
        elsif ch == " "
          out << "+"
        else
          ch.bytes.each { |b| out << format("%%%02X", b) }
        end
      end
      out
    end

    # Parse a flat "a=1&b=2&flag" query string into a String=>String hash.
    # A bare key with no "=" maps to "". Later duplicate keys win.
    def self.parse_query(qs)
      params = {}
      return params if qs.nil? || qs.empty?
      qs.split("&").each do |pair|
        next if pair.empty?
        key, value = pair.split("=", 2)
        params[unescape(key)] = value.nil? ? "" : unescape(value)
      end
      params
    end

    # Parse a query string with Rack's nested bracket syntax into a deep hash:
    #   "a[b][c]=1&a[b][d]=2&xs[]=x&xs[]=y"
    #     => {"a"=>{"b"=>{"c"=>"1","d"=>"2"}}, "xs"=>["x","y"]}
    def self.parse_nested_query(qs)
      params = {}
      return params if qs.nil? || qs.empty?
      qs.split("&").each do |pair|
        next if pair.empty?
        key, value = pair.split("=", 2)
        normalize_params(params, unescape(key), value.nil? ? "" : unescape(value))
      end
      params
    end

    # Recursively insert `value` into `params` under the bracketed `name`.
    # "a[b][c]" nests hashes; "a[]" appends to an array. Faithful port of the
    # shape of Rack::Utils.normalize_params for the common cases.
    def self.normalize_params(params, name, value)
      return unless name =~ /\A([^\[\]]+)(.*)\z/
      key = $1
      after = $2
      if after == ""
        params[key] = value
      elsif after == "[]"
        (params[key] ||= []) << value
      elsif after =~ /\A\[([^\[\]]+)\](.*)\z/
        params[key] = {} unless params[key].is_a?(Hash)
        normalize_params(params[key], $1 + $2, value)
      else
        params[key] = value
      end
    end

    # Parse a Cookie header ("a=1; b=2") into a String=>String hash.
    def self.parse_cookies(header)
      cookies = {}
      return cookies if header.nil? || header.empty?
      header.split(/[;,]\s*/).each do |pair|
        next if pair.empty?
        key, value = pair.split("=", 2)
        next if key.nil?
        cookies[unescape(key.strip)] = value.nil? ? "" : unescape(value.strip)
      end
      cookies
    end

    # Build a single "Set-Cookie" header value from a name and an options hash
    # ({ value:, path:, domain:, expires:, max_age:, secure:, http_only: }).
    def self.set_cookie_header(key, options)
      value = options[:value].nil? ? "" : options[:value].to_s
      parts = ["#{escape(key)}=#{escape(value)}"]
      parts << "path=#{options[:path]}"        if options[:path]
      parts << "domain=#{options[:domain]}"    if options[:domain]
      parts << "expires=#{options[:expires]}"  if options[:expires]
      parts << "max-age=#{options[:max_age]}"  unless options[:max_age].nil?
      parts << "secure"                        if options[:secure]
      parts << "HttpOnly"                      if options[:http_only]
      parts.join("; ")
    end

    # Guess a Content-Type from a file extension. Small deterministic table.
    MIME_TYPES = {
      ".html" => "text/html",
      ".htm"  => "text/html",
      ".txt"  => "text/plain",
      ".css"  => "text/css",
      ".js"   => "application/javascript",
      ".json" => "application/json",
      ".xml"  => "application/xml",
      ".png"  => "image/png",
      ".jpg"  => "image/jpeg",
      ".jpeg" => "image/jpeg",
      ".gif"  => "image/gif",
      ".svg"  => "image/svg+xml",
      ".ico"  => "image/x-icon",
      ".pdf"  => "application/pdf",
    }

    def self.mime_type(ext, default = "application/octet-stream")
      MIME_TYPES[ext.to_s.downcase] || default
    end
  end

  # Rack::Request — a read-only view over a Rack `env` hash.
  #
  # `env` is the CGI-style string-keyed hash the spec mandates: REQUEST_METHOD,
  # PATH_INFO, QUERY_STRING, plus HTTP_* header mirrors and a `rack.input` body.
  class Request
    attr_reader :env

    def initialize(env)
      @env = env
    end

    def request_method
      @env["REQUEST_METHOD"]
    end

    def path
      @env["PATH_INFO"] || "/"
    end
    alias_method :path_info, :path

    def script_name
      @env["SCRIPT_NAME"] || ""
    end

    def query_string
      @env["QUERY_STRING"] || ""
    end

    def content_type
      ct = @env["CONTENT_TYPE"]
      (ct.nil? || ct.empty?) ? nil : ct
    end

    def content_length
      @env["CONTENT_LENGTH"]
    end

    def media_type
      ct = content_type
      ct.nil? ? nil : ct.split(";").first.strip.downcase
    end

    # The URL scheme ("http"/"https"), honoring rack.url_scheme or HTTPS env.
    def scheme
      return @env["rack.url_scheme"] if @env["rack.url_scheme"]
      return "https" if @env["HTTPS"] == "on"
      "http"
    end

    def ssl?
      scheme == "https"
    end

    # The request host without a port, from the Host header or SERVER_NAME.
    def host
      if (h = @env["HTTP_HOST"])
        h.split(":").first
      else
        @env["SERVER_NAME"] || "localhost"
      end
    end

    # The request port, from the Host header, SERVER_PORT, or the scheme default.
    def port
      if (h = @env["HTTP_HOST"]) && h.include?(":")
        h.split(":", 2)[1].to_i
      elsif @env["SERVER_PORT"]
        @env["SERVER_PORT"].to_i
      else
        scheme == "https" ? 443 : 80
      end
    end

    # host:port, omitting the port when it is the scheme default.
    def host_with_port
      p = port
      default = scheme == "https" ? 443 : 80
      p == default ? host : "#{host}:#{p}"
    end

    # The fully-reconstructed request URL: scheme://host[:port]/path[?query].
    def url
      out = "#{scheme}://#{host_with_port}#{script_name}#{path}"
      qs = query_string
      out += "?#{qs}" unless qs.empty?
      out
    end

    # The request body as a String (drained from `rack.input`, or "" if absent).
    def body
      input = @env["rack.input"]
      return "" if input.nil?
      return input if input.is_a?(String)
      input.respond_to?(:read) ? (input.read || "") : input.to_s
    end

    # Cookies sent by the client, parsed from the Cookie header.
    def cookies
      Utils.parse_cookies(@env["HTTP_COOKIE"])
    end

    # The Rack session hash, if a session middleware populated it.
    def session
      @env["rack.session"] || {}
    end

    # An AJAX request advertises itself via X-Requested-With.
    def xhr?
      @env["HTTP_X_REQUESTED_WITH"].to_s.downcase == "xmlhttprequest"
    end

    # Nested-decoded query-string params only.
    def query_params
      Utils.parse_nested_query(query_string)
    end
    alias_method :GET, :query_params

    # Nested-decoded form-body params only (urlencoded POST/PUT).
    def form_params
      ct = @env["CONTENT_TYPE"] || ""
      if !get? && ct.start_with?("application/x-www-form-urlencoded")
        Utils.parse_nested_query(body)
      else
        {}
      end
    end
    alias_method :POST, :form_params

    # Merged params: query overlaid with form body, then any router path params
    # (`env["rack.path_params"]`, e.g. the `:id` captured from "/posts/:id").
    def params
      merged = query_params
      form_params.each { |k, v| merged[k] = v }
      if (pp = @env["rack.path_params"])
        pp.each { |k, v| merged[k.to_s] = v }
      end
      merged
    end

    # HTTP request headers reconstructed from the HTTP_* env mirror.
    # HTTP_USER_AGENT -> "User-Agent".
    def headers
      result = {}
      @env.each do |key, value|
        next unless key.start_with?("HTTP_")
        name = key[5..-1].split("_").map { |w| w.capitalize }.join("-")
        result[name] = value
      end
      result
    end

    def get?;    request_method == "GET";    end
    def post?;   request_method == "POST";   end
    def put?;    request_method == "PUT";    end
    def delete?; request_method == "DELETE"; end
    def head?;   request_method == "HEAD";   end
    def patch?;  request_method == "PATCH";  end

    # Back-compat class methods (older callers used Request.parse_query/unescape).
    def self.parse_query(qs); Utils.parse_query(qs); end
    def self.unescape(str);   Utils.unescape(str);   end
  end

  # Rack::Response — a mutable builder for the `[status, headers, body]` triple.
  #
  #     res = Rack::Response.new
  #     res["Content-Type"] = "text/plain"
  #     res.write("hello")
  #     res.set_cookie("sid", "abc")
  #     res.finish   # => [200, {...}, ["hello"]]
  class Response
    attr_accessor :status
    attr_reader :headers

    def initialize(body = [], status = 200, headers = {})
      @status = status
      @headers = {}
      headers.each { |k, v| @headers[k] = v }
      @body = []
      @length = 0
      if body.is_a?(String)
        write(body) unless body.empty?
      else
        body.each { |chunk| write(chunk) }
      end
    end

    # Append a chunk to the body, tracking total byte length. Returns the chunk.
    def write(chunk)
      str = chunk.to_s
      @body << str
      @length += str.bytesize
      str
    end

    def [](key);        @headers[key];         end
    def []=(key, value); @headers[key] = value; end
    def set_header(key, value); @headers[key] = value; end
    def get_header(key);        @headers[key];         end

    def content_type;        @headers["Content-Type"];         end
    def content_type=(value); @headers["Content-Type"] = value; end

    def redirect?; @status >= 300 && @status < 400; end
    def ok?;       @status == 200;                  end
    def not_found?; @status == 404;                 end

    # Turn the response into a redirect: set Location and status, clear the body.
    def redirect(target, status = 302)
      @status = status
      @headers["Location"] = target
      self
    end

    # Add a Set-Cookie header. `value` may be a String (the cookie value) or an
    # options hash ({ value:, path:, expires:, http_only:, ... }). Multiple
    # cookies accumulate as newline-joined values (Rack's multi-cookie convention).
    def set_cookie(key, value)
      options = value.is_a?(Hash) ? value : { value: value }
      header = Utils.set_cookie_header(key, options)
      existing = @headers["Set-Cookie"]
      @headers["Set-Cookie"] = existing.nil? || existing.empty? ? header : "#{existing}\n#{header}"
      header
    end

    # Expire a cookie now by setting it empty with a past expiry.
    def delete_cookie(key, options = {})
      set_cookie(key, {
        value:   "",
        path:    options[:path],
        domain:  options[:domain],
        expires: "Thu, 01 Jan 1970 00:00:00 -0000",
        max_age: "0",
      })
    end

    # The finished Rack triple, with an auto-computed Content-Length when the
    # status permits a body and none was set explicitly.
    def finish
      unless @headers.key?("Content-Length") || no_body_status?
        @headers["Content-Length"] = @length.to_s
      end
      [@status, @headers, @body]
    end
    alias_method :to_a, :finish

    def body; @body; end
    def length; @length; end

    private

    def no_body_status?
      @status == 204 || @status == 304 || (@status >= 100 && @status < 200)
    end
  end

  # Rack::Builder — assembles a middleware stack around a terminal app.
  #
  #     app = Rack::Builder.new do |b|
  #       b.use Rack::Logger
  #       b.run my_app
  #     end.to_app
  #
  # `use` records a middleware class (instantiated as `Middleware.new(next_app,
  # *args)`); `run` sets the innermost app; `to_app` wraps them inside-out so the
  # first `use`d middleware is the outermost caller.
  class Builder
    def initialize
      @middlewares = []
      @app = nil
      # rubylang has no `instance_eval`, so the block receives the builder as an
      # argument rather than being evaluated in the builder's context. On MRI
      # both `Rack::Builder.new { use X }` and `{ |b| b.use X }` work; here only
      # the explicit-argument form is available.
      yield self if block_given?
    end

    def use(middleware, *args)
      @middlewares << [middleware, args]
    end

    def run(app)
      @app = app
    end

    def to_app
      raise "missing run: no app given to Rack::Builder" if @app.nil?
      app = @app
      # Wrap inside-out: reverse so the first `use` ends up outermost.
      @middlewares.reverse.each do |middleware, args|
        app = middleware.new(app, *args)
      end
      app
    end

    # Convenience: build and return the app in one call.
    def self.app(&block)
      new(&block).to_app
    end
  end

  # Rack::Logger — a terse logging middleware. Logs one line per request to an IO
  # (stderr by default) so stdout stays the app's output.
  class Logger
    def initialize(app, io = nil)
      @app = app
      @io = io || $stderr
    end

    def call(env)
      method = env["REQUEST_METHOD"]
      path = env["PATH_INFO"]
      status, headers, body = @app.call(env)
      @io.puts("[Rack::Logger] #{method} #{path} -> #{status}")
      [status, headers, body]
    end
  end

  # Rack::CommonLogger — logs each request in the Apache Common Log Format:
  #     %h %l %u %t "%r" %>s %b
  # to any object responding to `write` or `<<` (a real IO, or a test sink).
  class CommonLogger
    def initialize(app, logger = nil)
      @app = app
      @logger = logger || $stderr
    end

    def call(env)
      status, headers, body = @app.call(env)
      log(env, status, headers, body)
      [status, headers, body]
    end

    private

    def log(env, status, headers, body)
      length = extract_length(headers, body)
      line = format(
        "%s - %s [%s] \"%s %s %s\" %d %s\n",
        env["REMOTE_ADDR"] || "-",
        env["REMOTE_USER"] || "-",
        env["rack.request.time"] || "-",   # injectable clock keeps output testable
        env["REQUEST_METHOD"],
        env["PATH_INFO"],
        env["SERVER_PROTOCOL"] || "HTTP/1.1",
        status,
        length.nil? ? "-" : length.to_s
      )
      write(line)
    end

    def write(line)
      if @logger.respond_to?(:write)
        @logger.write(line)
      else
        @logger << line
      end
    end

    def extract_length(headers, body)
      cl = headers["Content-Length"]
      return cl.to_i if cl
      total = 0
      body.each { |c| total += c.to_s.bytesize }
      total
    end
  end

  # Rack::Static — serve a static file out of a root directory by PATH_INFO.
  #
  #     Rack::Static.new(app, root: "public")
  #
  # If PATH_INFO maps to an existing regular file under `root`, it is served with
  # a Content-Type derived from its extension. Otherwise the request falls through
  # to the wrapped app, or 404s when no app was given. Path traversal ("..") is
  # rejected.
  class Static
    def initialize(app = nil, options = {})
      @app = app
      @root = options[:root] || "."
      @urls = options[:urls]  # optional array of path prefixes to intercept
    end

    def call(env)
      path = env["PATH_INFO"] || "/"
      if match?(path) && (file = resolve(path))
        serve(file)
      elsif @app
        @app.call(env)
      else
        not_found
      end
    end

    private

    # When :urls is given, only intercept paths under one of the prefixes.
    def match?(path)
      return true if @urls.nil?
      @urls.any? { |prefix| path.start_with?(prefix) }
    end

    # Resolve PATH_INFO to a safe file path under root, or nil.
    def resolve(path)
      return nil if path.include?("..")
      rel = path.sub(/\A\/+/, "")
      full = rel.empty? ? @root : File.join(@root, rel)
      File.exist?(full) && File.file?(full) ? full : nil
    end

    def serve(file)
      data = File.read(file)
      ctype = Utils.mime_type(File.extname(file))
      [200,
       { "Content-Type" => ctype, "Content-Length" => data.bytesize.to_s },
       [data]]
    end

    def not_found
      body = "File not found"
      [404,
       { "Content-Type" => "text/plain", "Content-Length" => body.bytesize.to_s },
       [body]]
    end
  end

  module Session
    # Rack::Session::Cookie (lite) — store a small session hash in a signed cookie.
    #
    #     Rack::Session::Cookie.new(app, secret: "s3cret")
    #
    # On the way in it reads the "rack.session" cookie, verifies its HMAC-ish
    # signature, JSON-decodes the payload, and exposes it as `env["rack.session"]`.
    # On the way out it re-encodes whatever the app left in `env["rack.session"]`
    # and emits a Set-Cookie. The signature is `Digest::SHA1.hexdigest(secret +
    # payload)` — enough to detect tampering for a demo, not a real MAC.
    class Cookie
      require "json"

      def initialize(app, options = {})
        @app = app
        @secret = options[:secret] || "change_me"
        @key = options[:key] || "rack.session"
        @path = options[:path] || "/"
      end

      def call(env)
        env["rack.session"] = load_session(env)
        status, headers, body = @app.call(env)
        commit(env, headers)
        [status, headers, body]
      end

      # Public so tests can construct/verify a cookie value directly.
      def encode(session)
        payload = Base64.strict_encode64(JSON.generate(session))
        "#{payload}--#{sign(payload)}"
      end

      def decode(cookie)
        return {} if cookie.nil? || cookie.empty?
        payload, digest = cookie.split("--", 2)
        return {} if payload.nil? || digest.nil?
        return {} unless secure_equal(sign(payload), digest)
        JSON.parse(Base64.strict_decode64(payload))
      rescue
        {}
      end

      private

      def load_session(env)
        cookie = Utils.parse_cookies(env["HTTP_COOKIE"])[@key]
        decode(cookie)
      end

      def commit(env, headers)
        session = env["rack.session"] || {}
        cookie = "#{@key}=#{Utils.escape(encode(session))}; path=#{@path}; HttpOnly"
        existing = headers["Set-Cookie"]
        headers["Set-Cookie"] =
          existing.nil? || existing.empty? ? cookie : "#{existing}\n#{cookie}"
      end

      def sign(payload)
        Digest::SHA1.hexdigest(@secret + payload)
      end

      # Length-checked constant-ish comparison (no timing guarantees on rubylang).
      def secure_equal(a, b)
        return false unless a.length == b.length
        mismatch = 0
        i = 0
        while i < a.length
          mismatch |= (a.bytes[i] ^ b.bytes[i])
          i += 1
        end
        mismatch == 0
      end
    end
  end

  # Rack::Router — a routing DSL with `:param` and `*wildcard` path matching.
  #
  #     router = Rack::Router.new do |r|
  #       r.get("/")               { "<h1>home</h1>" }         # String -> 200 html
  #       r.get("/posts/:id")      { |env, p| "post #{p['id']}" }
  #       r.get("/files/*")        { |env, p| "path #{p['splat']}" }
  #       r.post("/x")             { |env| ... }               # explicit triple
  #     end
  #
  # `#call(env)` dispatches on REQUEST_METHOD + PATH_INFO. Among matching routes
  # the most specific wins: a fully-literal match beats one with params, which
  # beats a wildcard. Captured `:params` are stored in `env["rack.path_params"]`
  # (so `Rack::Request#params` sees them) and passed as the block's 2nd argument.
  # A block may return a full Rack triple or a String (wrapped as 200 text/html).
  # No match yields 404.
  class Router
    # One compiled route: its method, the segmented pattern, and the handler.
    class Route
      attr_reader :method, :segments, :block, :wildcard

      def initialize(method, path, block)
        @method = method
        @block = block
        @wildcard = false
        @segments = path.split("/").map do |seg|
          if seg == "*"
            @wildcard = true
            [:splat, "splat"]
          elsif seg.start_with?(":")
            [:param, seg[1..-1]]
          else
            [:literal, seg]
          end
        end
      end

      # Try to match a request path; return a captured-params hash or nil.
      def match(path)
        parts = path.split("/")
        params = {}
        i = 0
        while i < @segments.length
          kind, name = @segments[i]
          if kind == :splat
            # A trailing wildcard swallows the rest of the path.
            params["splat"] = parts[i..-1].join("/")
            return params
          end
          return nil if i >= parts.length
          case kind
          when :literal
            return nil unless parts[i] == name
          when :param
            params[name] = parts[i]
          end
          i += 1
        end
        parts.length == @segments.length ? params : nil
      end

      # Specificity key for "most specific wins": non-wildcard beats wildcard,
      # then more literals, then more params. Compared as an array, higher wins.
      def specificity
        literals = @segments.count { |k, _| k == :literal }
        params   = @segments.count { |k, _| k == :param }
        [@wildcard ? 0 : 1, literals, params]
      end
    end

    def initialize
      @routes = { "GET" => [], "POST" => [], "PUT" => [], "DELETE" => [],
                  "PATCH" => [], "HEAD" => [] }
      yield self if block_given?
    end

    def get(path, &block);    add("GET", path, block);    end
    def post(path, &block);   add("POST", path, block);   end
    def put(path, &block);    add("PUT", path, block);    end
    def delete(path, &block); add("DELETE", path, block); end
    def patch(path, &block);  add("PATCH", path, block);  end

    def call(env)
      method = env["REQUEST_METHOD"]
      path = env["PATH_INFO"] || "/"
      list = @routes[method] || []

      best = nil
      best_params = nil
      list.each do |route|
        params = route.match(path)
        next if params.nil?
        if best.nil? || compare_specificity(route, best) > 0
          best = route
          best_params = params
        end
      end

      return not_found if best.nil?

      env["rack.path_params"] = best_params
      result = best.block.call(env, best_params)
      coerce(result)
    end

    private

    def add(method, path, block)
      (@routes[method] ||= []) << Route.new(method, path, block)
    end

    # >0 when `a` is strictly more specific than `b`.
    def compare_specificity(a, b)
      sa = a.specificity
      sb = b.specificity
      i = 0
      while i < sa.length
        return sa[i] <=> sb[i] unless sa[i] == sb[i]
        i += 1
      end
      0
    end

    def coerce(result)
      if result.is_a?(String)
        [200, { "Content-Type" => "text/html" }, [result]]
      elsif result.is_a?(Array) && result.length == 3
        result
      else
        [200, { "Content-Type" => "text/html" }, [result.to_s]]
      end
    end

    def not_found
      [404, { "Content-Type" => "text/plain" }, ["Not Found"]]
    end
  end

  # Rack::MockResponse — the result object a MockRequest returns: a Rack triple
  # with convenience accessors, so apps can be asserted against in-process.
  class MockResponse
    attr_reader :status, :headers, :body

    def initialize(status, headers, body_enum)
      @status = status
      @headers = headers
      chunks = []
      body_enum.each { |c| chunks << c.to_s }
      @body = chunks.join
    end

    def [](key); @headers[key]; end
    def ok?;        @status == 200; end
    def redirect?;  @status >= 300 && @status < 400; end
    def not_found?; @status == 404; end
    def location;   @headers["Location"]; end
    def content_type; @headers["Content-Type"]; end

    # Cookies the response set, parsed from any Set-Cookie header(s).
    def cookies
      raw = @headers["Set-Cookie"]
      return {} if raw.nil? || raw.empty?
      jar = {}
      raw.split("\n").each do |line|
        pair = line.split(";").first
        next if pair.nil?
        key, value = pair.split("=", 2)
        jar[Utils.unescape(key.strip)] = value.nil? ? "" : Utils.unescape(value.strip)
      end
      jar
    end
  end

  # Rack::MockRequest — drive a Rack app in-process, no socket required.
  #
  #     mock = Rack::MockRequest.new(app)
  #     res  = mock.get("/posts/42", "HTTP_COOKIE" => "sid=abc")
  #     res.status   # => 200
  #     res.body     # => "..."
  #
  # The second argument is an opts hash merged into env; use CGI keys directly
  # ("HTTP_*", "CONTENT_TYPE") or `:input` for a request body.
  class MockRequest
    def initialize(app)
      @app = app
    end

    def get(path, opts = {});    request("GET", path, opts);    end
    def post(path, opts = {});   request("POST", path, opts);   end
    def put(path, opts = {});    request("PUT", path, opts);    end
    def delete(path, opts = {}); request("DELETE", path, opts); end
    def patch(path, opts = {});  request("PATCH", path, opts);  end
    def head(path, opts = {});   request("HEAD", path, opts);   end

    def request(method, path, opts = {})
      env = self.class.env_for(method, path, opts)
      status, headers, body = @app.call(env)
      MockResponse.new(status, headers, body)
    end

    # Build a Rack env for a method/path plus opts, exactly what a real handler
    # would assemble from an HTTP request off a socket.
    def self.env_for(method, path, opts = {})
      uri, query = path.split("?", 2)
      env = {
        "REQUEST_METHOD"  => method,
        "PATH_INFO"       => uri,
        "QUERY_STRING"    => (query || ""),
        "SERVER_NAME"     => "example.org",
        "SERVER_PORT"     => "80",
        "SERVER_PROTOCOL" => "HTTP/1.1",
        "rack.url_scheme" => "http",
        "rack.input"      => "",
      }
      opts.each do |k, v|
        key = k.to_s
        if key == "input" || key == "rack.input"
          env["rack.input"] = v
          env["CONTENT_LENGTH"] = v.to_s.bytesize.to_s
        else
          env[key] = v
        end
      end
      env
    end
  end

  module Handler
    # Rack::Handler::Simple — a blocking single-connection HTTP/1.1 server that
    # serves any Rack app over rubylang's TCPServer.
    #
    #     Rack::Handler::Simple.run(app, 9292)
    #
    # Per connection it parses the request line + headers (+ a Content-Length
    # body) into a Rack `env`, calls the app, and writes back a well-formed
    # HTTP/1.1 response with a computed Content-Length. Handles GET and POST.
    class Simple
      def self.run(app, port = 9292, host = "127.0.0.1")
        server = TCPServer.new(host, port)
        loop do
          conn = server.accept
          begin
            env = parse_request(conn)
            if env.nil?
              conn.close
              next
            end
            status, headers, body = app.call(env)
            write_response(conn, status, headers, body)
          rescue => e
            $stderr.puts("[Rack::Handler::Simple] error: #{e}")
          end
          conn.close
        end
      ensure
        server.close if server
      end

      # Parse a raw HTTP request off the socket into a Rack env hash.
      def self.parse_request(conn)
        request_line = conn.gets
        return nil if request_line.nil?
        method, target, _version = request_line.strip.split(" ")
        return nil if method.nil? || target.nil?
        path, query = target.split("?", 2)

        env = {
          "REQUEST_METHOD"  => method,
          "PATH_INFO"       => path,
          "QUERY_STRING"    => (query || ""),
          "SERVER_PROTOCOL" => "HTTP/1.1",
          "rack.url_scheme" => "http",
        }

        # Read headers up to the blank line, mirroring each into HTTP_* keys.
        content_length = 0
        while (line = conn.gets) && line != "\r\n" && line != "\n"
          name, value = line.strip.split(": ", 2)
          next if name.nil?
          value = "" if value.nil?
          up = name.upcase.gsub("-", "_")
          if up == "CONTENT_LENGTH"
            env["CONTENT_LENGTH"] = value
            content_length = value.to_i
          elsif up == "CONTENT_TYPE"
            env["CONTENT_TYPE"] = value
          else
            env["HTTP_#{up}"] = value
          end
        end

        # Read a fixed-length body if one was announced.
        env["rack.input"] = content_length > 0 ? (conn.read(content_length) || "") : ""
        env
      end

      # Serialize a Rack triple as an HTTP/1.1 response and write it to the socket.
      def self.write_response(conn, status, headers, body)
        chunks = []
        body.each { |chunk| chunks << chunk.to_s }
        payload = chunks.join
        reason = REASONS[status] || "OK"

        out = "HTTP/1.1 #{status} #{reason}\r\n"
        wrote_length = false
        headers.each do |key, value|
          # A multi-value header (e.g. Set-Cookie joined by "\n") becomes one
          # header line per value, as HTTP requires.
          value.to_s.split("\n").each do |v|
            wrote_length = true if key.downcase == "content-length"
            out << "#{key}: #{v}\r\n"
          end
        end
        out << "Content-Length: #{payload.bytesize}\r\n" unless wrote_length
        out << "Connection: close\r\n"
        out << "\r\n"
        out << payload
        conn.write(out)
      end

      REASONS = {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
      }
    end
  end
end
