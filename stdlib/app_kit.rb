# app_kit.rb — a pure-Ruby, Sinatra-style web-app DSL for rubylang.
#
# This is the ergonomic layer that sits directly on top of rack.rb: instead of
# hand-writing `[status, headers, body]` triples you subclass AppKit::Base and
# declare routes with a block DSL:
#
#     class App < AppKit::Base
#       get "/hello/:name" do
#         "Hello, #{params[:name]}!"
#       end
#     end
#
# The class itself is a Rack app — `App.call(env)` returns a Rack triple, so it
# plugs straight into Rack::Builder / Rack::Handler, and `App.run!(9292)` boots
# the shipped raw-TCP handler.
#
# --- How this maps onto rubylang's object model -----------------------------
#
# Sinatra runs each route block in a fresh per-request *instance* (via
# instance_exec / a generated instance method) so that `params`, `request`, etc.
# resolve as instance methods. rubylang does not rebind `self` for blocks:
# neither `instance_exec`, `instance_eval`, nor `define_method` change what
# `self` a block sees (verified — a block keeps the `self` of its defining
# scope). Inside `get "/x" do ... end` written in the class body, that `self` is
# the subclass itself, and bare calls like `params` resolve to *class* methods.
#
# So AppKit is built the way that model actually allows: the request helpers
# (`params`, `request`, `response`, `status`, `headers`, `content_type`,
# `redirect`, `halt`, `erb`) are class methods on AppKit::Base, and the
# per-request state lives in class-level ivars set at the top of `call`. This is
# effectively Sinatra's "classic" single-namespace style. Dispatch is
# synchronous and one-request-at-a-time (matching Rack::Handler::Simple), so a
# single current-request slot is correct here.
#
# require_relative pulls in the Rack layer this builds on.

require_relative "rack"
require "erb"
require "json"

module AppKit
  # AppKit::Base — subclass it, declare routes, get a Rack app.
  #
  # Everything is a class method because a route block's `self` is the subclass
  # (see the file header). Subclasses inherit these; per-request state is stored
  # in ivars on the subclass during `call`.
  class Base
    # ---- Route + hook registration (class-def time) ------------------------

    # The ordered list of [method, pattern, block] route entries.
    def self.routes
      @routes ||= []
    end

    def self.before_hooks
      @before_hooks ||= []
    end

    def self.after_hooks
      @after_hooks ||= []
    end

    # Configuration bag (Sinatra's `set`). `:views` is the ERB template dir.
    def self.settings
      @settings ||= { "views" => "views" }
    end

    # `set :views, "path"` — store a config value. Accepts symbol or string keys
    # (stored stringified for indifferent lookup).
    def self.set(key, value)
      settings[key.to_s] = value
      value
    end

    def self.views_dir
      settings["views"]
    end

    # The route DSL. `get "/users/:id" do ... end`. A `:segment` in the pattern
    # is a named parameter available as `params[:segment]`.
    def self.get(pattern, &block)
      route("GET", pattern, &block)
    end

    def self.post(pattern, &block)
      route("POST", pattern, &block)
    end

    def self.put(pattern, &block)
      route("PUT", pattern, &block)
    end

    def self.delete(pattern, &block)
      route("DELETE", pattern, &block)
    end

    def self.patch(pattern, &block)
      route("PATCH", pattern, &block)
    end

    def self.route(method, pattern, &block)
      routes << [method, pattern, block]
    end

    # `before { ... }` / `after { ... }` — filters run around every request.
    def self.before(&block)
      before_hooks << block
    end

    def self.after(&block)
      after_hooks << block
    end

    # `not_found { ... }` — handler invoked when no route matches. Its block runs
    # like a route block (status is forced to 404 first).
    def self.not_found(&block)
      @not_found_hook = block
    end

    def self.not_found_hook
      @not_found_hook
    end

    # ---- Per-request helpers (called with implicit self inside route blocks) --

    # The merged, indifferent-access params hash: query string + form body
    # (from Rack::Request) overlaid with named route segments. Both string and
    # symbol keys are present, so `params[:id]` and `params["id"]` both work.
    def self.params
      @params
    end

    # The Rack::Request for this request.
    def self.request
      @request
    end

    # The Rack::Response accumulating status/headers/body for this request.
    def self.response
      @response
    end

    # `status` reads the current code; `status 201` sets it.
    def self.status(code = nil)
      return @response.status if code.nil?
      @response.status = code
    end

    # `headers` returns the mutable header hash; `headers "X" => "Y"` merges in.
    def self.headers(hash = nil)
      hash.each { |k, v| @response.headers[k] = v } unless hash.nil?
      @response.headers
    end

    # Set the Content-Type. A symbol is looked up in MIME_TYPES; a string is
    # used verbatim.
    def self.content_type(type)
      value = type.is_a?(Symbol) ? (MIME_TYPES[type] || "text/plain") : type.to_s
      @response.headers["Content-Type"] = value
      value
    end

    MIME_TYPES = {
      :html => "text/html",
      :text => "text/plain",
      :txt  => "text/plain",
      :json => "application/json",
      :xml  => "application/xml",
      :css  => "text/css",
      :js   => "application/javascript",
    }

    # Redirect to `url` (default 302). Ends the request immediately via halt.
    def self.redirect(url, code = 302)
      @response.headers["Location"] = url
      @response.status = code
      throw(:halt, [code, @response.headers, []])
    end

    # Abort the request with an explicit response. Sinatra-style overloads:
    #   halt                       -> current status, empty body
    #   halt 404                   -> status 404, empty body
    #   halt "gone"                -> current status, body "gone"
    #   halt 404, "missing"        -> status 404, body "missing"
    def self.halt(*args)
      code = @response.status
      body = ""
      if args.length == 1
        arg = args[0]
        arg.is_a?(Integer) ? (code = arg) : (body = arg.to_s)
      elsif args.length >= 2
        code = args[0]
        body = args[1].to_s
      end
      @response.status = code
      throw(:halt, [code, @response.headers, [body]])
    end

    # Render an ERB template.
    #   erb :show                       -> views/show.erb
    #   erb :show, locals: { id: 1 }    -> with locals exposed as bare names
    #   erb "<%= 1 + 1 %>"              -> inline (a String is the template source)
    #   erb "<%= x %>", :inline, locals: {x: 5}  -> the :inline marker is accepted
    #
    # A String template renders inline; a Symbol reads views/<name>.erb. If a
    # layout template resolves (views/layout.erb, or :layout option), the page is
    # wrapped in it with `<%= yield %>` replaced by the rendered page. Pass
    # `layout: false` to skip.
    #
    # NOTE: rubylang's ERB does not expose `@ivars` through `binding` (verified —
    # `ERB#result(binding)` renders ivars as empty), so templates read values as
    # bare locals supplied via `locals:` (a first-class Sinatra option), e.g.
    # `<%= title %>` with `erb :page, locals: { title: "Home" }`.
    def self.erb(template, *rest)
      opts = rest.last.is_a?(Hash) ? rest.last : {}
      locals = symbolize(opts["locals"] || opts[:locals] || {})

      source = template.is_a?(String) ? template : read_template(template)
      inner = ERB.new(source).result_with_hash(locals)

      layout_name = if opts.key?(:layout)
        opts[:layout]
      elsif opts.key?("layout")
        opts["layout"]
      else
        :layout
      end
      return inner if layout_name == false

      layout_path = template_path(layout_name)
      return inner unless File.exist?(layout_path)

      layout_src = File.read(layout_path).gsub(/<%=\s*yield\s*%>/, "<%= _appkit_yield %>")
      wrap_locals = locals.merge({ :_appkit_yield => inner })
      ERB.new(layout_src).result_with_hash(wrap_locals)
    end

    # ---- Rack app contract -------------------------------------------------

    # AppKit::Base.call(env) — the Rack entry point. Returns [status, headers,
    # body]. Sets up per-request state, runs before-filters, dispatches to the
    # first matching route (or the not_found handler), then runs after-filters.
    # `redirect`/`halt` short-circuit via `throw :halt`.
    def self.call(env)
      reset_request(env)

      result = catch(:halt) do
        run_hooks(before_hooks)
        block, route_params = match_route(env)
        if block
          merge_params(route_params)
          finalize(block.call)
        else
          finalize_not_found
        end
      end

      # After-filters run for every request, including halted ones (Sinatra
      # semantics). They may still mutate the shared response header hash, which
      # is the same object referenced by `result`.
      run_hooks(after_hooks)
      result
    end

    # AppKit::Base.run!(port) — serve this app over real HTTP via the Rack
    # handler. Blocks forever (the handler loops on accept).
    def self.run!(port = 9292, host = "127.0.0.1")
      Rack::Handler::Simple.run(self, port, host)
    end

    # ---- Internals ---------------------------------------------------------

    def self.reset_request(env)
      @request = Rack::Request.new(env)
      @response = Rack::Response.new
      @params = {}
      # Seed with query/form params from Rack::Request (string keys), adding a
      # symbol alias for each so both access forms work.
      @request.params.each do |k, v|
        @params[k] = v
        @params[k.to_sym] = v
      end
    end

    def self.merge_params(route_params)
      route_params.each do |k, v|
        @params[k] = v
        @params[k.to_sym] = v
      end
    end

    # Find the first route whose method + segment pattern matches. Returns
    # [block, {param => value}] or [nil, {}].
    def self.match_route(env)
      method = env["REQUEST_METHOD"]
      path = env["PATH_INFO"] || "/"
      request_segments = split_path(path)

      routes.each do |route_method, pattern, block|
        next unless route_method == method
        pattern_segments = split_path(pattern)
        next unless pattern_segments.length == request_segments.length

        captured = {}
        matched = true
        i = 0
        while i < pattern_segments.length
          pseg = pattern_segments[i]
          rseg = request_segments[i]
          if pseg.start_with?(":")
            captured[pseg[1..-1]] = rseg
          elsif pseg != rseg
            matched = false
            break
          end
          i += 1
        end

        return [block, captured] if matched
      end

      [nil, {}]
    end

    # Split a path into non-empty segments. "/a/b" -> ["a", "b"]; "/" -> [].
    def self.split_path(path)
      path.split("/").reject { |s| s.empty? }
    end

    def self.run_hooks(hooks)
      hooks.each { |hook| hook.call }
    end

    # Turn a route block's return value into a Rack triple, honoring anything the
    # block set on @response (status, headers, content-type).
    #   String              -> body, default Content-Type text/html
    #   Hash / Array (+json content-type) -> JSON.generate
    #   full Rack triple    -> passed through
    #   nil                 -> empty body
    def self.finalize(result)
      # A block may return an explicit Rack triple; pass it through untouched.
      if result.is_a?(Array) && result.length == 3 &&
         result[0].is_a?(Integer) && result[1].is_a?(Hash)
        return result
      end

      ctype = @response.headers["Content-Type"]

      body =
        if result.is_a?(String)
          result
        elsif (result.is_a?(Hash) || result.is_a?(Array)) &&
              ctype == "application/json"
          JSON.generate(result)
        elsif result.nil?
          ""
        else
          result.to_s
        end

      @response.headers["Content-Type"] = "text/html" if @response.headers["Content-Type"].nil?
      [@response.status, @response.headers, [body]]
    end

    def self.finalize_not_found
      @response.status = 404
      if not_found_hook
        finalize(not_found_hook.call)
      else
        @response.headers["Content-Type"] = "text/plain" if @response.headers["Content-Type"].nil?
        [404, @response.headers, ["Not Found"]]
      end
    end

    def self.template_path(name)
      File.join(views_dir, "#{name}.erb")
    end

    def self.read_template(name)
      path = template_path(name)
      raise "AppKit: template not found: #{path}" unless File.exist?(path)
      File.read(path)
    end

    # Return a new hash with all keys converted to symbols (result_with_hash
    # exposes each key as a bare local variable in the template).
    def self.symbolize(hash)
      out = {}
      hash.each { |k, v| out[k.to_sym] = v }
      out
    end
  end
end
