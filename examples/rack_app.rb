# rack_app.rb — a runnable demo of the pure-Ruby Rack library on rubylang.
#
# Builds a Router with three routes (HTML via ERB, JSON, and a dynamic greeting),
# wraps it in a logging middleware through Rack::Builder, then DRIVES the app by
# calling `app.call(env)` on three hand-built env hashes and printing each
# `[status, headers, body]` triple. Because a real server blocks forever on
# `accept`, this demo dispatches in-process so it terminates with deterministic
# output. The commented `Rack::Handler::Simple.run` line at the bottom shows how
# the identical `app` would be served over real HTTP.
#
#     target/debug/ruby examples/rack_app.rb

require_relative "../stdlib/rack"
require "erb"
require "json"

# --- Routes -----------------------------------------------------------------

# An ERB template rendered per request. `result_with_hash` binds the locals.
# (Kept on one line: rubylang cannot yet parse call args split across newlines.)
HOME_TEMPLATE = ERB.new("<html><body><h1><%= title %></h1><p>Visitors: <%= count %></p></body></html>")

router = Rack::Router.new do |r|
  # 1. HTML route rendering an ERB template.
  r.get("/") do
    html = HOME_TEMPLATE.result_with_hash(title: "Welcome to Rack on rubylang", count: 42)
    [200, { "Content-Type" => "text/html" }, [html]]
  end

  # 2. JSON API route.
  r.get("/api/status") do
    payload = JSON.generate({ "status" => "ok", "engine" => "rubylang", "rack" => 1 })
    [200, { "Content-Type" => "application/json" }, [payload]]
  end

  # 3. Dynamic route reading query params via Rack::Request; returns a String,
  #    which the Router wraps as a 200 text/html response.
  r.get("/hello") do |env|
    name = Rack::Request.new(env).params.fetch("name", "stranger")
    "<p>Hello, #{name}!</p>"
  end
end

# --- Middleware stack -------------------------------------------------------

# Wrap the router in the logging middleware. `to_app` returns the Rack app the
# server (or the in-process driver below) calls.
app = Rack::Builder.new do |b|
  b.use Rack::Logger
  b.run router
end.to_app

# --- Drive the app in-process (deterministic, terminating) ------------------

# Hand-built Rack env hashes — exactly what Rack::Handler::Simple would build
# from a raw HTTP request off a socket.
requests = [
  { "REQUEST_METHOD" => "GET", "PATH_INFO" => "/",           "QUERY_STRING" => "" },
  { "REQUEST_METHOD" => "GET", "PATH_INFO" => "/api/status", "QUERY_STRING" => "" },
  { "REQUEST_METHOD" => "GET", "PATH_INFO" => "/hello",      "QUERY_STRING" => "name=Ada" },
  { "REQUEST_METHOD" => "GET", "PATH_INFO" => "/nowhere",    "QUERY_STRING" => "" },
]

puts "=== Rack app driven in-process (#{requests.length} requests) ==="
requests.each do |env|
  status, headers, body = app.call(env)
  puts
  puts "#{env['REQUEST_METHOD']} #{env['PATH_INFO']}#{env['QUERY_STRING'].empty? ? '' : "?#{env['QUERY_STRING']}"}"
  puts "  status:  #{status}"
  puts "  headers: #{headers.inspect}"
  puts "  body:    #{body.join.inspect}"
end

puts
puts "=== done ==="

# --- Serving for real (commented — blocks forever) --------------------------
#
# The very same `app` serves over real HTTP with one line. Uncomment to run a
# blocking server, then in another terminal:
#
#     curl http://127.0.0.1:9292/
#     curl http://127.0.0.1:9292/api/status
#     curl 'http://127.0.0.1:9292/hello?name=Grace'
#
# Rack::Handler::Simple.run(app, 9292)
