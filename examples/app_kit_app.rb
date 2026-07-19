# app_kit_app.rb — a runnable demo of the AppKit Sinatra-style DSL on rubylang.
#
# Subclasses AppKit::Base, declares a handful of routes (a param route, an ERB
# page wrapped in a layout, an inline-ERB fragment, a JSON API, a redirect, and
# a form POST), then DRIVES the app in-process by calling `App.call(env)` on
# hand-built Rack env hashes and printing each [status, headers, body] triple.
# In-process dispatch keeps the output deterministic and terminating; the
# commented `App.run!(9292)` at the bottom shows how the identical class serves
# over real HTTP.
#
#     target/debug/ruby examples/app_kit_app.rb

require_relative "../stdlib/app_kit"

class App < AppKit::Base
  # ERB templates live next to this file. __dir__ resolves to examples/.
  set :views, File.join(__dir__, "views")

  # A before-filter runs around every request. Here it stamps a header so you
  # can see filters fire; after-filters work the same way.
  before do
    headers "X-Powered-By" => "AppKit/rubylang"
  end

  # 1. Named-parameter route. "/users/42" -> params[:id] == "42".
  #    Renders views/show.erb wrapped in views/layout.erb (<%= yield %>).
  get "/users/:id" do
    id = params[:id]
    erb :show, :locals => {
      :title => "User #{id}",
      :id    => id,
      :name  => "user_#{id}",
      :roles => ["reader", "writer"],
    }
  end

  # 2. Inline ERB (a String template is rendered inline). No layout is applied
  #    to inline templates unless one resolves; here we skip it explicitly.
  get "/greet/:who" do
    erb "<p>Hello, <%= who %>!</p>", :inline, :layout => false, :locals => { :who => params[:who] }
  end

  # 3. JSON API. `content_type :json` + returning a Hash -> JSON body.
  get "/api/users/:id" do
    content_type :json
    { :id => params[:id].to_i, :name => "user_#{params[:id]}", :active => true }
  end

  # 4. Redirect. Ends the request immediately with a 302 + Location header.
  get "/old" do
    redirect "/users/1"
  end

  # 5. Form POST reading a body param, setting a custom status.
  post "/users" do
    status 201
    "created user: #{params[:name]}"
  end

  # Fallback for unmatched paths.
  not_found do
    content_type :text
    "no such page"
  end
end

# --- Drive the app in-process (deterministic, terminating) ------------------

def show(label, env)
  status, headers, body = App.call(env)
  puts label
  puts "  status:  #{status}"
  puts "  headers: #{headers.inspect}"
  puts "  body:    #{body.join.inspect}"
  puts
end

puts "=== AppKit app driven in-process ==="
puts

show("GET /users/42", {
  "REQUEST_METHOD" => "GET", "PATH_INFO" => "/users/42", "QUERY_STRING" => "",
})

show("GET /greet/Ada", {
  "REQUEST_METHOD" => "GET", "PATH_INFO" => "/greet/Ada", "QUERY_STRING" => "",
})

show("GET /api/users/7", {
  "REQUEST_METHOD" => "GET", "PATH_INFO" => "/api/users/7", "QUERY_STRING" => "",
})

show("GET /old (redirect)", {
  "REQUEST_METHOD" => "GET", "PATH_INFO" => "/old", "QUERY_STRING" => "",
})

show("POST /users (form body)", {
  "REQUEST_METHOD" => "POST",
  "PATH_INFO"      => "/users",
  "QUERY_STRING"   => "",
  "CONTENT_TYPE"   => "application/x-www-form-urlencoded",
  "rack.input"     => "name=Grace",
})

show("GET /missing (not_found)", {
  "REQUEST_METHOD" => "GET", "PATH_INFO" => "/missing", "QUERY_STRING" => "",
})

puts "=== done ==="

# --- Serving for real (commented — blocks forever) --------------------------
#
# The very same class serves over real HTTP with one line. Uncomment, run, then
# from another terminal:
#
#     curl http://127.0.0.1:9292/users/42
#     curl 'http://127.0.0.1:9292/api/users/7'
#     curl -i http://127.0.0.1:9292/old
#     curl -d 'name=Grace' http://127.0.0.1:9292/users
#
# App.run!(9292)
