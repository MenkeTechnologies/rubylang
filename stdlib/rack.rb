# rack.rb — a pure-Ruby implementation of the Rack interface for rubylang.
#
# Rack is the contract every Ruby web framework (Rails, Sinatra, Roda) speaks:
# an "app" is any object responding to `call(env)` and returning a triple
#
#     [status_integer, headers_hash, body_enumerable]
#
# where `body_enumerable` responds to `each` and yields String chunks. This file
# implements that contract plus the pieces that sit on top of it: a Request/
# Response pair, a middleware Builder, a raw-TCP HTTP handler, and a routing DSL.
#
# It is self-contained — it requires only rubylang's own socket/erb/json, not the
# Rack gem — and it is written to run on `target/debug/ruby`. Where a common MRI
# idiom is not yet available on rubylang the code uses an equivalent that is; the
# accompanying report enumerates each such gap.

require "socket"

module Rack
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

    def query_string
      @env["QUERY_STRING"] || ""
    end

    # The request body as a String (drained from `rack.input`, or "" if absent).
    def body
      input = @env["rack.input"]
      return "" if input.nil?
      return input if input.is_a?(String)
      # A rack.input IO-like object responds to #read.
      input.respond_to?(:read) ? (input.read || "") : input.to_s
    end

    # Merged params: query-string params overlaid with form-encoded body params
    # for POST/PUT with an application/x-www-form-urlencoded content type.
    def params
      merged = Request.parse_query(query_string)
      ctype = @env["CONTENT_TYPE"] || ""
      if !get? && ctype.start_with?("application/x-www-form-urlencoded")
        Request.parse_query(body).each { |k, v| merged[k] = v }
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

    def get?
      request_method == "GET"
    end

    def post?
      request_method == "POST"
    end

    def put?
      request_method == "PUT"
    end

    def delete?
      request_method == "DELETE"
    end

    # Parse an "a=1&b=2&flag" query string into a String=>String hash.
    # A bare key with no "=" maps to "". Later duplicate keys win (MRI keeps
    # the last for the scalar accessor form used here).
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

    # Percent-decode a query component: '+' -> space, %XX -> byte.
    def self.unescape(str)
      return str if str.nil?
      out = str.gsub("+", " ")
      result = ""
      i = 0
      len = out.length
      while i < len
        ch = out[i]
        if ch == "%" && i + 2 < len + 1 && i + 2 <= len
          hex = out[(i + 1)..(i + 2)]
          if hex.length == 2
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
  end

  # Rack::Response — a mutable builder for the `[status, headers, body]` triple.
  #
  #     res = Rack::Response.new
  #     res["Content-Type"] = "text/plain"
  #     res.write("hello")
  #     res.finish   # => [200, {"Content-Type"=>"text/plain"}, ["hello"]]
  class Response
    attr_accessor :status
    attr_reader :headers

    def initialize(body = [], status = 200, headers = {})
      @status = status
      @headers = {}
      headers.each { |k, v| @headers[k] = v }
      @body = []
      if body.is_a?(String)
        @body << body unless body.empty?
      else
        body.each { |chunk| @body << chunk }
      end
    end

    # Append a chunk to the body. Returns the chunk (MRI returns the string).
    def write(chunk)
      str = chunk.to_s
      @body << str
      str
    end

    # Read a response header.
    def [](key)
      @headers[key]
    end

    # Set a response header.
    def []=(key, value)
      @headers[key] = value
    end

    def set_header(key, value)
      @headers[key] = value
    end

    def get_header(key)
      @headers[key]
    end

    # The finished Rack triple.
    def finish
      [@status, @headers, @body]
    end
    alias_method :to_a, :finish

    def body
      @body
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

  # Rack::Logger — an example logging middleware conforming to the middleware
  # contract: `initialize(app)` then `call(env)` delegating to `@app.call(env)`.
  #
  # Written as a named class because rubylang cannot yet instantiate the
  # anonymous classes produced by `Class.new { ... }`. Logs one line per request
  # to stderr (method, path, resulting status) so stdout stays the app's output.
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

  # Rack::Router — a minimal routing DSL over the Rack contract.
  #
  #     router = Rack::Router.new do |r|
  #       r.get("/")     { "<h1>home</h1>" }             # String -> 200 text/html
  #       r.get("/api")  { [200, {...}, ["{}"]] }         # explicit triple
  #       r.post("/x")   { |env| ... }
  #     end
  #
  # `#call(env)` dispatches on [REQUEST_METHOD, PATH_INFO] to the matching block.
  # A block may return a full Rack triple, or a String (wrapped as a 200 text/html
  # response). No match yields 404.
  class Router
    def initialize
      @routes = {}
      yield self if block_given?
    end

    def get(path, &block)
      @routes[["GET", path]] = block
    end

    def post(path, &block)
      @routes[["POST", path]] = block
    end

    def put(path, &block)
      @routes[["PUT", path]] = block
    end

    def delete(path, &block)
      @routes[["DELETE", path]] = block
    end

    def call(env)
      method = env["REQUEST_METHOD"]
      path = env["PATH_INFO"]
      block = @routes[[method, path]]
      return not_found unless block

      result = block.call(env)
      coerce(result)
    end

    private

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
          "REQUEST_METHOD" => method,
          "PATH_INFO"      => path,
          "QUERY_STRING"   => (query || ""),
          "SERVER_PROTOCOL" => "HTTP/1.1",
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
          wrote_length = true if key.downcase == "content-length"
          out << "#{key}: #{value}\r\n"
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
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
      }
    end
  end
end
