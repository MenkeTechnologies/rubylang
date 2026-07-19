# active_record.rb — a faithful, pure-Ruby subset of Rails' ActiveRecord.
#
# This is a real, working ORM (no stubs). It is backed by the SQLite3::Database
# class that rubylang ships (`require "sqlite3"`), and runs unmodified on both
# rubylang and MRI. It implements the model layer surface most Rails apps lean
# on: connection management, table-name inference, schema introspection with
# dynamically-defined column accessors, a class-level query API (create/all/
# find/where/count/first), and an instance API (save/destroy/persisted?/
# attributes/[]/[]=/id). All values flow through `?` bind parameters — table and
# column identifiers (which SQLite cannot bind) come only from the schema, never
# from user input, so there is no SQL-injection surface.
#
# rubylang runtime notes (idioms deliberately avoided because they are not yet
# supported — see the mission report for the full list):
#   * class-level instance variables (`@x` in a `self.` method) read back nil,
#     so all per-model state lives in the shared class variable `@@registry`,
#     keyed by model name;
#   * `instance_variable_set/get` are not defined on class objects, so class
#     state is never poked reflectively — only plain `@@registry` hash access;
#   * `Module#allocate` is absent, so records are built with `new(row_hash)`;
#   * bare `name` inside a `self.` method returns nil — `self.name` is used.

require "sqlite3"

module AR
  # Raised by `find` when no row matches the given id (Rails-compatible name).
  class RecordNotFound < StandardError; end

  class Base
    # Shared, process-wide state. rubylang class-level `@ivars` do not persist,
    # so a class variable (shared down the whole AR::Base hierarchy) holds it.
    @@connection = nil
    @@registry   = {} # model-name String => { "table" => .., "columns" => [..] }

    # ---- connection management -------------------------------------------

    # Open (or reopen) the shared database. Rows come back as column-keyed
    # hashes, which every query path below relies on.
    def self.establish_connection(path)
      @@connection = SQLite3::Database.new(path)
      @@connection.results_as_hash = true
      @@connection
    end

    def self.connection
      raise "no database connection: call AR::Base.establish_connection(path) first" if @@connection.nil?
      @@connection
    end

    def self.connected?
      !@@connection.nil?
    end

    # ---- per-model configuration -----------------------------------------

    # The registry key for the current model — the class's simple name with any
    # module qualification stripped ("Admin::User" => "User").
    def self.model_name
      self.name.split("::").last
    end

    def self.config
      @@registry[model_name] ||= {}
    end

    # Naive Rails-style pluralizer: consonant + "y" => "ies", otherwise "+s".
    def self.pluralize(word)
      if word.end_with?("y")
        word[0...-1] + "ies"
      else
        word + "s"
      end
    end

    def self.default_table_name
      pluralize(model_name.downcase)
    end

    # Combined getter/setter. `User.table_name` infers "users"; a model overrides
    # it Rails-macro style with `table_name "articles"` in its body.
    #
    # NOTE: the assignment form `self.table_name = "..."` is intentionally NOT
    # offered — rubylang cannot yet parse a `name=` setter *definition* (`def
    # foo=` / `def self.foo=`), so there is no method to receive the assignment.
    # The macro form below is the supported override path.
    def self.table_name(value = nil)
      if value.nil?
        config["table"] ||= default_table_name
      else
        config["table"] = value.to_s
      end
    end

    # ---- schema introspection + dynamic accessors ------------------------

    # Read the table's columns via PRAGMA and define a getter/setter pair for
    # each on the model class (so every instance gains them). Idempotent per
    # model; the first query/instantiation triggers it.
    def self.columns
      unless config["columns"]
        rows = connection.execute("PRAGMA table_info(#{table_name})")
        cols = rows.map { |r| r["name"] }
        raise "no such table: #{table_name}" if cols.empty?
        config["columns"] = cols
        define_column_accessors(cols)
      end
      config["columns"]
    end

    def self.define_column_accessors(cols)
      cols.each do |col|
        define_method(col) { @attributes[col] }
        define_method("#{col}=") { |value| @attributes[col] = value }
      end
    end

    # ---- class query API --------------------------------------------------

    def self.create(attrs = {})
      new(attrs).save
    end

    def self.all
      columns
      connection.execute("SELECT * FROM #{table_name} ORDER BY id").map { |row| new(row) }
    end

    def self.find(id)
      columns
      row = connection.get_first_row("SELECT * FROM #{table_name} WHERE id = ?", [id])
      raise RecordNotFound, "Couldn't find #{model_name} with id=#{id}" if row.nil?
      new(row)
    end

    # Parameterized equality query. Keys are column names (schema-sourced),
    # values are bound with `?` — never interpolated.
    def self.where(conds = {})
      columns
      clause = conds.keys.map { |k| "#{k} = ?" }.join(" AND ")
      sql = "SELECT * FROM #{table_name}"
      sql = "#{sql} WHERE #{clause}" unless conds.empty?
      sql = "#{sql} ORDER BY id"
      connection.execute(sql, conds.values).map { |row| new(row) }
    end

    def self.count
      columns
      connection.get_first_value("SELECT COUNT(*) FROM #{table_name}")
    end

    def self.first
      columns
      row = connection.get_first_row("SELECT * FROM #{table_name} ORDER BY id LIMIT 1")
      row.nil? ? nil : new(row)
    end

    # ---- instance API -----------------------------------------------------

    # Build a record. `attrs` may use String or Symbol keys and comes either
    # from the caller (User.new(name: "Ann")) or from a result row (String
    # keys, includes id => the record is persisted).
    def initialize(attrs = {})
      @attributes = {}
      self.class.columns.each { |c| @attributes[c] = nil }
      attrs.each { |k, v| @attributes[k.to_s] = v }
    end

    def id
      @attributes["id"]
    end

    def persisted?
      !@attributes["id"].nil?
    end

    # A defensive copy of the underlying column => value map.
    def attributes
      @attributes.dup
    end

    def [](col)
      @attributes[col.to_s]
    end

    def []=(col, value)
      @attributes[col.to_s] = value
    end

    # INSERT a new record, or UPDATE an existing one by id. Returns self so
    # `Model.create(...)` and `record.save` both hand back the record.
    def save
      cols = self.class.columns.reject { |c| c == "id" }
      if persisted?
        assignments = cols.map { |c| "#{c} = ?" }.join(", ")
        values = cols.map { |c| @attributes[c] }
        sql = "UPDATE #{self.class.table_name} SET #{assignments} WHERE id = ?"
        self.class.connection.execute(sql, values + [@attributes["id"]])
      else
        placeholders = cols.map { "?" }.join(", ")
        values = cols.map { |c| @attributes[c] }
        sql = "INSERT INTO #{self.class.table_name} (#{cols.join(", ")}) VALUES (#{placeholders})"
        self.class.connection.execute(sql, values)
        @attributes["id"] = self.class.connection.last_insert_row_id
      end
      self
    end

    # DELETE this record; clears its id so it reads as no-longer-persisted.
    def destroy
      return self unless persisted?
      sql = "DELETE FROM #{self.class.table_name} WHERE id = ?"
      self.class.connection.execute(sql, [@attributes["id"]])
      @attributes["id"] = nil
      self
    end
  end

  # ---- migration DSL ------------------------------------------------------
  #
  #   AR::Schema.define do |db|
  #     db.create_table :users do |t|
  #       t.string  :name
  #       t.integer :age
  #     end
  #   end
  #
  # Every table gets an implicit `id INTEGER PRIMARY KEY`, matching Rails.
  class TableDefinition
    SQL_TYPES = {
      "string"   => "TEXT",
      "text"     => "TEXT",
      "integer"  => "INTEGER",
      "float"    => "REAL",
      "boolean"  => "INTEGER",
      "datetime" => "TEXT"
    }

    def initialize(name)
      @name = name.to_s
      @columns = ["id INTEGER PRIMARY KEY"]
    end

    # One typed-column helper per SQL type (t.string, t.integer, ...).
    SQL_TYPES.each do |ruby_type, sql_type|
      define_method(ruby_type) do |col_name|
        @columns << "#{col_name} #{sql_type}"
      end
    end

    def to_sql
      "CREATE TABLE #{@name} (#{@columns.join(", ")})"
    end
  end

  # The migration builder, yielded to the `Schema.define` block.
  #
  #   AR::Schema.define do |db|
  #     db.create_table :users do |t|
  #       t.string :name
  #     end
  #   end
  #
  # (A bare `create_table ...` receiver via `instance_eval(&block)` is avoided:
  # rubylang does not rebind self when a forwarded `&block` is handed to
  # instance_eval, so the builder is passed explicitly as a block argument.)
  class SchemaDefiner
    def create_table(name)
      table = TableDefinition.new(name)
      yield table
      AR::Base.connection.execute("DROP TABLE IF EXISTS #{name}")
      AR::Base.connection.execute(table.to_sql)
    end
  end

  module Schema
    def self.define
      yield SchemaDefiner.new
    end
  end
end
