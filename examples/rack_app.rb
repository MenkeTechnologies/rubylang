# rack_app.rb — a runnable demo of the pure-Ruby Rack library on rubylang.
#
# Builds a Router with literal, `:id`-param, and `*wildcard` routes; renders one
# route through ERB and another as JSON; wraps the whole thing in a middleware
# stack (CommonLogger + Session::Cookie); serves a real file through
# Rack::Static; and drives every path in-process with Rack::MockRequest so the
# program terminates with deterministic output instead of blocking on a socket.
# In-script `raise ... unless` assertions make a regression a non-zero exit.
#
#     target/debug/ruby examples/rack_app.rb
#
# The commented `Rack::Handler::Simple.run` at the bottom shows how the identical
# `app` would be served over real HTTP.

require_relative "../stdlib/rack"
require "erb"
require "json"
require "tmpdir"

# A tiny in-memory log sink so CommonLogger output is captured (and asserted)
# instead of racing to a live, timestamped stderr line.
class LogSink
  attr_reader :lines
  def initialize; @lines = []; end
  def write(line); @lines << line; end
end

# --- Routes -----------------------------------------------------------------

# An ERB template rendered per request. `result_with_hash` binds the locals.
# (Kept on one line: rubylang cannot yet parse call args split across newlines.)
HOME_TEMPLATE = ERB.new("<html><body><h1><%= title %></h1><p>Visits: <%= visits %></p></body></html>")

router = Rack::Router.new do |r|
  # 1. HTML route rendering an ERB template. Reads/increments a session counter.
  r.get("/") do |env|
    session = env["rack.session"]
    session["visits"] = (session["visits"] || 0) + 1
    html = HOME_TEMPLATE.result_with_hash(title: "Rack on rubylang", visits: session["visits"])
    [200, { "Content-Type" => "text/html" }, [html]]
  end

  # 2. `:id` PARAM route. The captured segment arrives as the block's 2nd arg AND
  #    inside Rack::Request#params — both shown here.
  r.get("/posts/:id") do |env, params|
    id = params["id"]
    also = Rack::Request.new(env).params["id"]
    JSON.generate({ "post_id" => id, "via_request_params" => also })
  end

  # 3. A more-specific literal route that must win over "/posts/:id".
  r.get("/posts/new") do |env|
    "<form>new post</form>"
  end

  # 4. Trailing WILDCARD route: everything after /files/ is captured as "splat".
  r.get("/files/*") do |env, params|
    "serving: #{params['splat']}"
  end

  # 5. JSON API route reading a query param via nested-decoding.
  r.get("/api/echo") do |env|
    params = Rack::Request.new(env).params
    JSON.generate({ "user" => params["user"], "tags" => params["tags"] })
  end
end

# --- Middleware stack -------------------------------------------------------

# CommonLogger (outermost) logs to our capture sink; Session::Cookie signs a
# per-client session hash into a cookie and re-reads it on the next request.
log_sink = LogSink.new
app = Rack::Builder.new do |b|
  b.use Rack::CommonLogger, log_sink
  b.use Rack::Session::Cookie, secret: "demo-secret"
  b.run router
end.to_app

mock = Rack::MockRequest.new(app)

# --- Drive the app in-process (deterministic, terminating) ------------------

puts "=== Router: literal / param / wildcard dispatch ==="

res = mock.get("/posts/42")
puts
puts "GET /posts/42"
puts "  status: #{res.status}"
puts "  body:   #{res.body}"
raise "param route failed: #{res.body}" unless res.body == '{"post_id":"42","via_request_params":"42"}'

res = mock.get("/posts/new")
puts
puts "GET /posts/new  (literal beats :id)"
puts "  status: #{res.status}"
puts "  body:   #{res.body}"
raise "specificity failed" unless res.body == "<form>new post</form>"

res = mock.get("/files/css/site.css")
puts
puts "GET /files/css/site.css  (wildcard splat)"
puts "  status: #{res.status}"
puts "  body:   #{res.body}"
raise "wildcard failed" unless res.body == "serving: css/site.css"

res = mock.get("/api/echo?user=ada&tags[]=x&tags[]=y")
puts
puts "GET /api/echo?user=ada&tags[]=x&tags[]=y  (nested params)"
puts "  status: #{res.status}"
puts "  body:   #{res.body}"
raise "nested params failed" unless res.body == '{"user":"ada","tags":["x","y"]}'

res = mock.get("/nowhere")
puts
puts "GET /nowhere"
puts "  status: #{res.status}"
puts "  body:   #{res.body}"
raise "404 failed" unless res.status == 404

# --- Cookie / session round-trip --------------------------------------------

puts
puts "=== Session cookie round-trip ==="

first = mock.get("/")
session_cookie = first.cookies["rack.session"]
puts
puts "1st GET /  -> #{first.body}"
puts "   Set-Cookie present: #{!session_cookie.nil?}"
raise "no session cookie set" if session_cookie.nil?
raise "1st visit wrong" unless first.body.include?("Visits: 1")

second = mock.get("/", "HTTP_COOKIE" => "rack.session=#{session_cookie}")
puts "2nd GET /  (replaying cookie) -> #{second.body}"
raise "session did not persist" unless second.body.include?("Visits: 2")

tampered = mock.get("/", "HTTP_COOKIE" => "rack.session=forged--deadbeef")
puts "3rd GET /  (forged cookie, signature rejected) -> #{tampered.body}"
raise "tampered cookie not reset" unless tampered.body.include?("Visits: 1")

# --- Rack::Static serving a real file ---------------------------------------

puts
puts "=== Rack::Static serving a file ==="

static_root = Dir.mktmpdir
File.write(File.join(static_root, "hello.txt"), "a static asset\n")

static_app = Rack::Static.new(nil, root: static_root)
static_mock = Rack::MockRequest.new(static_app)

hit = static_mock.get("/hello.txt")
puts
puts "GET /hello.txt"
puts "  status:       #{hit.status}"
puts "  content-type: #{hit.content_type}"
puts "  body:         #{hit.body.inspect}"
raise "static serve failed" unless hit.status == 200 && hit.body == "a static asset\n"
raise "static mime failed" unless hit.content_type == "text/plain"

miss = static_mock.get("/does-not-exist.png")
puts
puts "GET /does-not-exist.png"
puts "  status: #{miss.status}"
raise "static 404 failed" unless miss.status == 404

File.delete(File.join(static_root, "hello.txt"))
Dir.rmdir(static_root)

# --- CommonLogger output ----------------------------------------------------

puts
puts "=== CommonLogger captured #{log_sink.lines.length} lines (router requests) ==="
raise "logger captured nothing" if log_sink.lines.empty?

# --- MockRequest assertion block --------------------------------------------

puts
puts "=== MockRequest assertions ==="
checks = [
  ["param route id",      mock.get("/posts/7").body == '{"post_id":"7","via_request_params":"7"}'],
  ["wildcard depth",      mock.get("/files/a/b/c").body == "serving: a/b/c"],
  ["literal specificity", mock.get("/posts/new").body == "<form>new post</form>"],
  ["unknown is 404",      mock.get("/x/y/z").status == 404],
]
checks.each do |name, ok|
  puts "  [#{ok ? 'PASS' : 'FAIL'}] #{name}"
  raise "assertion failed: #{name}" unless ok
end

puts
puts "=== all assertions passed ==="

# --- Serving for real (commented — blocks forever) --------------------------
#
# The very same `app` serves over real HTTP with one line. Uncomment to run a
# blocking server, then in another terminal:
#
#     curl http://127.0.0.1:9292/
#     curl http://127.0.0.1:9292/posts/42
#     curl 'http://127.0.0.1:9292/api/echo?user=grace&tags[]=x'
#
# Rack::Handler::Simple.run(app, 9292)
