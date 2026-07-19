# orm_blog.rb — a two-model Rails-style blog on the pure-Ruby AR::Base ORM
# (stdlib/active_record.rb) backed by rubylang's bundled SQLite3::Database.
#
# Exercises the full expanded surface: belongs_to / has_many associations,
# presence + uniqueness validations (with a real save-returns-false failure),
# before_create / after_create callbacks firing in order, a lazy
# where.order.limit query chain, pluck, find_by, update, where.not, dirty
# tracking, and destroy_all.
#
# Output is deterministic: no timestamps, no tmp paths, no run-order-dependent
# ids beyond the deterministic insert sequence. Runs unmodified on both
# rubylang (target/debug/ruby) and MRI (portable Ruby with the sqlite3 gem).

require "sqlite3"
require_relative "../stdlib/active_record"

DB_PATH = "/tmp/rubylang_orm_blog_demo.db"
File.delete(DB_PATH) if File.exist?(DB_PATH)

AR::Base.establish_connection(DB_PATH)

AR::Schema.define do |db|
  db.create_table :authors do |t|
    t.string :name
    t.string :email
  end
  db.create_table :posts, timestamps: true do |t|
    t.string  :title
    t.integer :author_id
    t.integer :views
    t.integer :published
  end
end

class Author < AR::Base
  has_many :posts

  validates_presence_of :name
  validates_uniqueness_of :email
end

class Post < AR::Base
  belongs_to :author

  validates_presence_of :title

  before_create :apply_defaults
  after_create  :announce

  def apply_defaults
    self["views"] = 0 if self["views"].nil?
    self["published"] = 1 if self["published"].nil?
  end

  def announce
    puts "  [after_create] post ##{id} \"#{title}\" published=#{self["published"]}"
  end
end

puts "== schema =="
puts "authors columns: #{Author.columns.inspect}"
puts "posts columns:   #{Post.columns.inspect}"

puts
puts "== create authors =="
alice = Author.create(name: "Alice", email: "alice@example.com")
bob   = Author.create(name: "Bob",   email: "bob@example.com")
puts "alice id=#{alice.id} bob id=#{bob.id}"

puts
puts "== validation: presence failure =="
ghost = Author.new(email: "ghost@example.com")
saved = ghost.save
puts "save returned: #{saved.inspect}"
puts "errors: #{ghost.errors.full_messages.inspect}"

puts
puts "== validation: uniqueness failure =="
dup = Author.new(name: "Alice II", email: "alice@example.com")
puts "save returned: #{dup.save.inspect}"
puts "errors: #{dup.errors.full_messages.inspect}"

puts
puts "== create! raises RecordInvalid =="
begin
  Author.create!(name: "")
rescue AR::RecordInvalid => e
  puts "raised: #{e.message}"
end

puts
puts "== create posts (callbacks fire in order) =="
p1 = Post.create(title: "Hello World",  author_id: alice.id, views: 10)
p2 = Post.create(title: "Rails on Ruby", author_id: alice.id, views: 55)
p3 = Post.create(title: "SQLite Deep Dive", author_id: bob.id, views: 30, published: 0)

puts
puts "== belongs_to / has_many =="
puts "p1.author.name = #{p1.author.name}"
puts "alice.posts.count = #{alice.posts.count}"
alice.posts.order(:title).each { |p| puts "  alice post: #{p.title}" }
puts "alice post titles = #{alice.posts.pluck(:title).inspect}"

puts
puts "== where.order.limit chaining (lazy) =="
top = Post.where(published: 1).order("views DESC").limit(2)
puts "sql = #{top.to_sql}"
top.each { |p| puts "  top post: #{p.title} (#{p.views} views)" }
puts "top titles = #{top.pluck(:title).inspect}"

puts
puts "== pluck / find_by / first / last / exists? =="
puts "all titles = #{Post.pluck(:title).inspect}"
found = Post.find_by(title: "Rails on Ruby")
puts "find_by title -> ##{found.id} by author #{found.author.name}"
puts "first title = #{Post.order(:id).first.title}"
puts "last title  = #{Post.order(:id).last.title}"
puts "default last title = #{Post.last.title}"
puts "exists published=0 ? #{Post.where(published: 0).exists?}"

puts
puts "== where.not =="
Post.where.not(published: 0).order(:id).each { |p| puts "  published: #{p.title}" }

puts
puts "== update + dirty tracking =="
puts "before: changed?=#{p1.changed?} views=#{p1.views}"
p1.views = 999
puts "after assign: changed?=#{p1.changed?} changed=#{p1.changed.inspect}"
p1.update(views: 42)
puts "after update: changed?=#{p1.changed?} views=#{Post.find(p1.id).views}"

puts
puts "== reload =="
p2.title = "LOCAL EDIT"
p2.reload
puts "p2 title after reload = #{p2.title}"

puts
puts "== update_all =="
affected = Post.where(author_id: alice.id).update_all(published: 1)
puts "update_all affected = #{affected}"

puts
puts "== destroy_all =="
puts "posts before = #{Post.count}"
Post.where(author_id: bob.id).destroy_all
puts "posts after destroying bob's = #{Post.count}"

AR::Base.connection.close

# --- assertions (self-contained: assert exact expected values) ---
AR::Base.establish_connection(DB_PATH)
raise "author count"  unless Author.count == 2
raise "post count"    unless Post.count == 2
raise "assoc name"    unless Post.find(1).author.name == "Alice"
raise "has_many"      unless Author.find(1).posts.count == 2
raise "pluck"         unless Author.find(1).posts.pluck(:title) == ["Hello World", "Rails on Ruby"]
raise "chain order"   unless Post.where(published: 1).order("views DESC").limit(2).pluck(:title) == ["Rails on Ruby", "Hello World"]
raise "find_by"       unless Post.find_by(title: "Rails on Ruby").id == 2
raise "where.not"     unless Post.where.not(published: 0).count == 2
raise "update"        unless Post.find(1).views == 42
raise "order last"    unless Post.order(:id).last.title == "Rails on Ruby"
AR::Base.connection.close

File.delete(DB_PATH) if File.exist?(DB_PATH)
puts
puts "OK"
