# Real database persistence with SQLite3::Database (backed by a bundled SQLite,
# no external gem/FFI). A tiny `User` model writes rows to an on-disk database,
# closes the connection, then reopens the SAME file in a fresh connection and
# reads the rows back — proving the data lives on disk, not in memory. This is
# the substrate a Rails-like app stands on.

require "sqlite3"

DB_PATH = "/tmp/rubylang_sqlite_persistence_demo.db"
File.delete(DB_PATH) if File.exist?(DB_PATH)

# A minimal ActiveRecord-shaped model: class methods that issue SQL through a
# connection handed to them (kept explicit so the persistence is easy to follow).
class User
  def self.setup(db)
    db.execute(<<~SQL)
      CREATE TABLE IF NOT EXISTS users (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        age  INTEGER
      )
    SQL
  end

  def self.create(db, name, age)
    db.execute("INSERT INTO users (name, age) VALUES (?, ?)", [name, age])
    db.last_insert_row_id
  end

  def self.count(db)
    db.get_first_value("SELECT COUNT(*) FROM users")
  end

  def self.all(db)
    db.results_as_hash = true
    rows = db.execute("SELECT id, name, age FROM users ORDER BY id")
    db.results_as_hash = false
    rows
  end

  def self.oldest(db)
    db.get_first_row("SELECT name, age FROM users ORDER BY age DESC LIMIT 1")
  end
end

# --- Pass 1: open the file, write rows, then close (a process ending) ---
db = SQLite3::Database.new(DB_PATH)
User.setup(db)
id1 = User.create(db, "Alice", 30)
id2 = User.create(db, "Bob", 25)
puts "inserted ids: #{id1}, #{id2}"
written = User.count(db)
puts "count before close: #{written}"
db.close

# --- Pass 2: reopen the SAME file with a brand-new connection ---
db = SQLite3::Database.new(DB_PATH)
reread = User.count(db)
puts "count after reopen: #{reread}"
User.all(db).each do |row|
  puts "user ##{row["id"]}: #{row["name"]} (age #{row["age"]})"
end

name, age = User.oldest(db)
puts "oldest: #{name} at #{age}"

# A SQL error surfaces as a rescuable SQLite3::SQLException. (The exact message
# text differs between backends, so assert on its content, not the raw string.)
handled = false
begin
  db.execute("SELECT * FROM nonexistent")
rescue SQLite3::SQLException => e
  handled = e.message.include?("no such table")
end
puts "handled SQLite3::SQLException: #{handled}"

db.close
File.delete(DB_PATH) if File.exist?(DB_PATH)

# Self-checks: the reopened count must equal what we wrote, proving persistence.
raise "persistence lost rows" unless reread == written
raise "expected 2 rows" unless reread == 2
raise "wrong oldest" unless name == "Alice" && age == 30
raise "error not handled" unless handled
puts "OK"
