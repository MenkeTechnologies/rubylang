# orm_app.rb — a Rails-style app running on the pure-Ruby AR::Base ORM
# (stdlib/active_record.rb) backed by rubylang's bundled SQLite3::Database.
#
# It defines a model, migrates a schema, exercises the full CRUD + query API,
# then CLOSES the connection and REOPENS the same on-disk file in a fresh
# connection to prove the data persists to disk (not just in memory).
#
# Output is deterministic: no tmp path, no timestamps, no row ids that depend
# on run order beyond the deterministic 1,2 sequence.

require "sqlite3"
require_relative "../stdlib/active_record"

DB_PATH = "/tmp/rubylang_orm_app_demo.db"
File.delete(DB_PATH) if File.exist?(DB_PATH)

# A Rails-style model. table_name is inferred as "users" (User -> user -> +s).
class User < AR::Base
end

puts "== schema =="
AR::Base.establish_connection(DB_PATH)
AR::Schema.define do |db|
  db.create_table :users do |t|
    t.string  :name
    t.integer :age
  end
end
puts "table_name inferred: #{User.table_name}"
puts "columns: #{User.columns.inspect}"

puts
puts "== create (INSERT) =="
ann = User.create(name: "Ann", age: 30)
bob = User.create(name: "Bob", age: 25)
puts "created Ann id=#{ann.id} persisted=#{ann.persisted?}"
puts "created Bob id=#{bob.id} persisted=#{bob.persisted?}"

puts
puts "== read (all / find / where / count / first) =="
puts "count: #{User.count}"
User.all.each { |u| puts "  all -> ##{u.id} #{u.name} (#{u.age})" }
found = User.find(ann.id)
puts "find(#{ann.id}) -> #{found.name} (#{found.age})"
User.where(age: 30).each { |u| puts "  where(age:30) -> #{u.name}" }
first = User.first
puts "first -> #{first.name}"
puts "attributes: #{found.attributes.inspect}"
puts "index access: found['name'] = #{found['name']}"

puts
puts "== update (save on persisted) =="
bob.age = 26
bob.save
puts "Bob after save: #{User.find(bob.id).age}"

puts
puts "== destroy (DELETE) =="
carol = User.create(name: "Carol", age: 40)
puts "count with Carol: #{User.count}"
carol.destroy
puts "count after destroy: #{User.count}"
puts "carol persisted after destroy: #{carol.persisted?}"

puts
puts "== RecordNotFound =="
begin
  User.find(9999)
rescue AR::RecordNotFound => e
  puts "raised: #{e.message}"
end

AR::Base.connection.close

puts
puts "== persistence: reopen a fresh connection to the SAME file =="
AR::Base.establish_connection(DB_PATH)
puts "count after reopen: #{User.count}"
User.all.each { |u| puts "  reread -> ##{u.id} #{u.name} (#{u.age})" }
AR::Base.connection.close

# --- assertions (self-contained ORM: assert exact expected values) ---
AR::Base.establish_connection(DB_PATH)
names = User.all.map { |u| u.name }
ages  = User.all.map { |u| u.age }
raise "expected 2 rows, got #{User.count}"        unless User.count == 2
raise "wrong names: #{names.inspect}"             unless names == ["Ann", "Bob"]
raise "wrong ages: #{ages.inspect}"               unless ages == [30, 26]
raise "find failed" unless User.find(1).name == "Ann"
raise "where failed" unless User.where(age: 30).map { |u| u.name } == ["Ann"]
raise "first failed" unless User.first.name == "Ann"
AR::Base.connection.close

File.delete(DB_PATH) if File.exist?(DB_PATH)
puts
puts "OK"
