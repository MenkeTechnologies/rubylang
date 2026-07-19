# active_record.rb — a faithful, pure-Ruby subset of Rails' ActiveRecord.
#
# This is a real, working ORM (no stubs). It is backed by the SQLite3::Database
# class that rubylang ships (`require "sqlite3"`), and runs unmodified on both
# rubylang and MRI. It implements the model layer surface most Rails apps lean
# on:
#
#   * connection management + table-name inference + schema introspection with
#     dynamically-defined column accessors;
#   * a lazy, chainable query Relation (where / order / limit / offset / pluck /
#     first / last / count / exists? / find_each / where.not), built as SQL and
#     executed only on enumeration, every value ?-bound;
#   * finders (find / find_by / find_by!);
#   * persistence (create / create! / save / save! / update / update! / destroy /
#     destroy_all / update_all / reload) with dirty tracking + new_record?;
#   * validations (presence, uniqueness) with an errors object, save returning
#     false when invalid, and save!/create! raising AR::RecordInvalid;
#   * callbacks (before_save / after_save / before_create / after_create /
#     before_destroy) run in registration order;
#   * associations (belongs_to / has_many) resolved through define_method;
#   * automatic created_at / updated_at timestamps when those columns exist.
#
# All values flow through `?` bind parameters — table and column identifiers
# (which SQLite cannot bind) come only from the schema/model definitions, never
# from user input, so there is no SQL-injection surface.
#
# rubylang runtime notes (idioms deliberately avoided because they are not yet
# supported — see the mission report for the full list):
#   * class-level instance variables (`@x` in a `self.` method) read back nil,
#     so all per-model state lives in the shared class variable `@@registry`,
#     keyed by model name;
#   * `super("message")` inside an exception subclass's `initialize` breaks, so
#     AR::RecordInvalid overrides `#message` instead of calling super;
#   * `respond_to?` does not see `define_method`- or module-included methods, so
#     "blank?" checks test `is_a?(String)` rather than `respond_to?(:empty?)`;
#   * `Module#allocate` is absent, so records are built with `new(row_hash)`;
#   * bare `name` inside a `self.` method returns nil — `self.name` is used.

require "sqlite3"

module AR
  # Raised by `find`/`find_by!` when no row matches (Rails-compatible name).
  class RecordNotFound < StandardError; end

  # Raised by `save!`/`create!`/`update!` when validations fail. The message is
  # built in `initialize` and returned by an overridden `#message` because
  # calling `super("...")` from an exception subclass's initializer is not yet
  # supported by rubylang.
  class RecordInvalid < StandardError
    def initialize(record)
      @record  = record
      @message = "Validation failed: #{record.errors.full_messages.join(", ")}"
    end

    def record
      @record
    end

    def message
      @message
    end
  end

  # A minimal Rails-style errors collection. Keyed by attribute name (String),
  # each value an array of messages. `full_messages` humanizes the attribute
  # name and prefixes it, matching Rails ("Name can't be blank").
  class Errors
    def initialize
      @messages = {}
    end

    def add(attr, message)
      (@messages[attr.to_s] ||= []) << message
    end

    def [](attr)
      @messages[attr.to_s] || []
    end

    def empty?
      @messages.empty?
    end

    def any?
      !@messages.empty?
    end

    def clear
      @messages = {}
    end

    def count
      total = 0
      @messages.each { |_attr, msgs| total += msgs.length }
      total
    end

    def full_messages
      out = []
      @messages.each do |attr, msgs|
        human = attr.split("_").map { |p| p.capitalize }.join(" ")
        msgs.each { |m| out << "#{human} #{m}" }
      end
      out
    end
  end

  # A lazy, chainable query object. Nothing touches the database until the
  # relation is enumerated (each/to_a/map/...) or a terminal (count/first/last/
  # pluck/exists?/find_each) is called. Every chaining method returns a fresh
  # relation, so `rel.where(...)` never mutates `rel` (Rails semantics).
  class Relation
    include Enumerable

    def initialize(model)
      @model  = model
      @wheres = [] # array of [sql_fragment, [bound_values]] ANDed together
      @order  = nil
      @limit  = nil
      @offset = nil
    end

    # Copy this relation so chaining is non-destructive.
    def spawn
      r = Relation.new(@model)
      r.set_state(@wheres.dup, @order, @limit, @offset)
      r
    end

    # Used by `spawn` to seed a clone (arrays are already copied by the caller).
    def set_state(wheres, order, limit, offset)
      @wheres = wheres
      @order  = order
      @limit  = limit
      @offset = offset
      self
    end

    # `where(col: val, ...)` adds an equality group; nil becomes `IS NULL`.
    # `where` with no argument returns a WhereChain so `where.not(...)` works.
    def where(conds = nil)
      return WhereChain.new(self) if conds.nil?
      fragment, values = build_conditions(conds, false)
      return spawn if fragment.empty? # where({}) is a no-op, matching Rails
      r = spawn
      r.push_where(fragment, values)
      r
    end

    def where_not(conds)
      fragment, values = build_conditions(conds, true)
      return spawn if fragment.empty?
      r = spawn
      r.push_where(fragment, values)
      r
    end

    def push_where(fragment, values)
      @wheres << [fragment, values]
      self
    end

    # `order(:col)` or `order("col DESC")`. Later calls replace earlier order.
    def order(col)
      r = spawn
      r.set_order(col.to_s)
      r
    end

    def set_order(order)
      @order = order
      self
    end

    def limit(n)
      r = spawn
      r.set_limit(n)
      r
    end

    def set_limit(n)
      @limit = n
      self
    end

    def offset(m)
      r = spawn
      r.set_offset(m)
      r
    end

    def set_offset(m)
      @offset = m
      self
    end

    # ---- SQL assembly -----------------------------------------------------

    def where_sql
      return "" if @wheres.empty?
      " WHERE " + @wheres.map { |w| w[0] }.join(" AND ")
    end

    def bind_values
      values = []
      @wheres.each { |w| values.concat(w[1]) }
      values
    end

    # Everything after the column list: WHERE / ORDER BY / LIMIT / OFFSET.
    def tail_sql
      sql = where_sql
      sql += " ORDER BY #{@order}" if @order
      sql += " LIMIT #{@limit}"    if @limit
      sql += " OFFSET #{@offset}"  if @offset
      sql
    end

    def to_sql
      "SELECT * FROM #{@model.table_name}#{tail_sql}"
    end

    # ---- terminals --------------------------------------------------------

    def to_a
      @model.columns
      @model.connection.execute(to_sql, bind_values).map { |row| @model.new(row) }
    end

    def each(&block)
      to_a.each(&block)
    end

    # `pluck(:col)` returns the bare column values (no model objects).
    def pluck(col)
      @model.columns
      sql = "SELECT #{col} FROM #{@model.table_name}#{tail_sql}"
      @model.connection.execute(sql, bind_values).map { |row| row[col.to_s] }
    end

    # COUNT respects WHERE (order/limit/offset are irrelevant to a count).
    def count
      @model.columns
      sql = "SELECT COUNT(*) FROM #{@model.table_name}#{where_sql}"
      @model.connection.get_first_value(sql, bind_values)
    end

    def exists?
      count > 0
    end

    # `first` / `last` order by primary key unless an explicit order is set,
    # matching Rails.
    def first
      rel = @order ? self : order("id")
      rel.limit(1).to_a.first
    end

    def last
      rel = @order ? order(reverse_order_sql(@order)) : order("id DESC")
      rel.limit(1).to_a.first
    end

    def find_by(conds)
      where(conds).first
    end

    def find_by!(conds)
      record = find_by(conds)
      raise RecordNotFound, "Couldn't find #{@model.model_name}" if record.nil?
      record
    end

    # Batch iterator: pages by primary key so a huge table never fully loads.
    # Any existing conditions on the relation are preserved (order/limit/offset
    # are copied by `spawn`), so `Model.where(...).find_each` scopes correctly.
    def find_each(batch_size = 1000)
      current = 0
      loop do
        batch = order("id").limit(batch_size).offset(current).to_a
        break if batch.empty?
        batch.each { |record| yield record }
        break if batch.length < batch_size
        current += batch_size
      end
    end

    # DELETE every matched row (loads them so per-row before_destroy fires).
    def destroy_all
      to_a.each { |record| record.destroy }
    end

    # UPDATE every matched row in a single statement; returns the row count.
    def update_all(attrs)
      @model.columns
      affected = count
      assignments = attrs.keys.map { |k| "#{k} = ?" }.join(", ")
      sql = "UPDATE #{@model.table_name} SET #{assignments}#{where_sql}"
      @model.connection.execute(sql, attrs.values + bind_values)
      affected
    end

    private

    # Invert an ORDER BY clause so `last` on an ordered relation returns the
    # true tail (Rails reverses the order and takes the first row). Each
    # comma-separated term toggles ASC/DESC; a bare column defaults to DESC.
    def reverse_order_sql(order)
      order.split(",").map do |term|
        t   = term.strip
        low = t.downcase
        if low.end_with?(" desc")
          t[0...-5].strip + " ASC"
        elsif low.end_with?(" asc")
          t[0...-4].strip + " DESC"
        else
          t + " DESC"
        end
      end.join(", ")
    end

    # Turn a conditions hash into a "col = ? AND col2 IS NULL" fragment plus the
    # ordered bound values. `negate` flips = -> !=, IS NULL -> IS NOT NULL.
    def build_conditions(conds, negate)
      parts  = []
      values = []
      conds.each do |key, value|
        if value.nil?
          parts << (negate ? "#{key} IS NOT NULL" : "#{key} IS NULL")
        else
          parts << (negate ? "#{key} != ?" : "#{key} = ?")
          values << value
        end
      end
      [parts.join(" AND "), values]
    end
  end

  # Returned by `where` with no argument so `Model.where.not(active: true)` reads
  # naturally; `.not` forwards to the relation's negated where.
  class WhereChain
    def initialize(relation)
      @relation = relation
    end

    def not(conds)
      @relation.where_not(conds)
    end
  end

  class Base
    # Shared, process-wide state. rubylang class-level `@ivars` do not persist,
    # so a class variable (shared down the whole AR::Base hierarchy) holds it.
    @@connection = nil
    @@registry   = {} # model-name String => per-model config hash

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

    # Inverse of pluralize, for association target inference ("posts" => "post").
    def self.singularize(word)
      word = word.to_s
      if word.end_with?("ies")
        word[0...-3] + "y"
      elsif word.end_with?("s")
        word[0...-1]
      else
        word
      end
    end

    # "blog_post" / :author => "BlogPost" / "Author" (for const lookup).
    def self.camelize(word)
      word.to_s.split("_").map { |part| part.capitalize }.join
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

    def self.has_column?(name)
      columns.include?(name.to_s)
    end

    def self.define_column_accessors(cols)
      cols.each do |col|
        define_method(col) { @attributes[col] }
        define_method("#{col}=") { |value| @attributes[col] = value }
      end
    end

    # ---- validations ------------------------------------------------------

    def self.validators
      config["validators"] ||= []
    end

    # `validates_presence_of :name, :email` — rejects nil / blank strings.
    def self.validates_presence_of(*attrs)
      attrs.each { |attr| validators << ["presence", attr.to_s, nil] }
    end

    # `validates_uniqueness_of :email` — query-backed; excludes the record's own
    # row so re-saving an unchanged record does not flag itself.
    def self.validates_uniqueness_of(*attrs)
      attrs.each { |attr| validators << ["uniqueness", attr.to_s, nil] }
    end

    # ---- callbacks --------------------------------------------------------

    def self.callbacks(kind)
      config["callbacks"] ||= {}
      config["callbacks"][kind] ||= []
    end

    def self.before_save(method_name)
      callbacks("before_save") << method_name
    end

    def self.after_save(method_name)
      callbacks("after_save") << method_name
    end

    def self.before_create(method_name)
      callbacks("before_create") << method_name
    end

    def self.after_create(method_name)
      callbacks("after_create") << method_name
    end

    def self.before_destroy(method_name)
      callbacks("before_destroy") << method_name
    end

    # ---- associations -----------------------------------------------------

    # `belongs_to :author` defines `#author`, resolving `Author.find_by(id:
    # author_id)`. Options: :class_name, :foreign_key.
    def self.belongs_to(name, options = {})
      fk         = (options[:foreign_key] || "#{name}_id").to_s
      class_name = (options[:class_name] || camelize(name)).to_s
      define_method(name) do
        fk_value = @attributes[fk]
        return nil if fk_value.nil?
        Object.const_get(class_name).find_by(id: fk_value)
      end
    end

    # `has_many :posts` defines `#posts`, returning the relation
    # `Post.where(<owner>_id: id)`. Options: :class_name, :foreign_key.
    def self.has_many(name, options = {})
      class_name = (options[:class_name] || camelize(singularize(name))).to_s
      fk         = (options[:foreign_key] || "#{model_name.downcase}_id").to_s
      define_method(name) do
        conds = {}
        conds[fk] = id
        Object.const_get(class_name).where(conds)
      end
    end

    # ---- class query API (delegates to a fresh Relation) ------------------

    def self.all
      Relation.new(self)
    end

    def self.where(conds = nil)
      all.where(conds)
    end

    def self.order(col)
      all.order(col)
    end

    def self.limit(n)
      all.limit(n)
    end

    def self.offset(m)
      all.offset(m)
    end

    def self.pluck(col)
      all.pluck(col)
    end

    def self.count
      all.count
    end

    def self.exists?
      all.exists?
    end

    def self.first
      all.first
    end

    def self.last
      all.last
    end

    def self.find_by(conds)
      all.find_by(conds)
    end

    def self.find_by!(conds)
      all.find_by!(conds)
    end

    def self.find_each(batch_size = 1000, &block)
      all.find_each(batch_size, &block)
    end

    def self.destroy_all
      all.destroy_all
    end

    def self.update_all(attrs)
      all.update_all(attrs)
    end

    def self.find(id)
      columns
      row = connection.get_first_row("SELECT * FROM #{table_name} WHERE id = ?", [id])
      raise RecordNotFound, "Couldn't find #{model_name} with id=#{id}" if row.nil?
      new(row)
    end

    def self.create(attrs = {})
      record = new(attrs)
      record.save
      record
    end

    def self.create!(attrs = {})
      record = new(attrs)
      record.save!
      record
    end

    # ---- instance API -----------------------------------------------------

    # Build a record. `attrs` may use String or Symbol keys and comes either
    # from the caller (User.new(name: "Ann")) or from a result row (String
    # keys, includes id => the record is persisted). The dirty baseline
    # (@original) is the all-nil column map for a new record, or the loaded row
    # for a persisted one, so a freshly-read record is not reported as changed.
    def initialize(attrs = {})
      @attributes = {}
      self.class.columns.each { |c| @attributes[c] = nil }
      @errors   = Errors.new
      @original = @attributes.dup
      attrs.each { |k, v| @attributes[k.to_s] = v }
      @original = @attributes.dup if persisted?
    end

    def id
      @attributes["id"]
    end

    def persisted?
      !@attributes["id"].nil?
    end

    def new_record?
      !persisted?
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

    def errors
      @errors
    end

    # ---- dirty tracking (best-effort) ------------------------------------

    def changed?
      @attributes != @original
    end

    def changed
      @attributes.keys.select { |k| @attributes[k] != @original[k] }
    end

    # ---- validations ------------------------------------------------------

    def valid?
      @errors = Errors.new
      self.class.validators.each do |validator|
        kind = validator[0]
        attr = validator[1]
        run_validator(kind, attr)
      end
      @errors.empty?
    end

    def invalid?
      !valid?
    end

    # ---- persistence ------------------------------------------------------

    # INSERT (new) or UPDATE (persisted). Runs validations first and returns
    # false without touching the database when invalid. Fires callbacks in
    # registration order and stamps timestamps when those columns exist.
    def save
      return false unless valid?
      run_callbacks("before_save")
      stamp = current_timestamp
      if persisted?
        set_timestamp("updated_at", stamp)
        persist_update
      else
        set_timestamp("created_at", stamp)
        set_timestamp("updated_at", stamp)
        run_callbacks("before_create")
        persist_insert
        run_callbacks("after_create")
      end
      run_callbacks("after_save")
      @original = @attributes.dup
      true
    end

    def save!
      raise RecordInvalid.new(self) unless save
      true
    end

    def update(attrs)
      attrs.each { |k, v| @attributes[k.to_s] = v }
      save
    end

    def update!(attrs)
      attrs.each { |k, v| @attributes[k.to_s] = v }
      save!
    end

    # DELETE this record; clears its id so it reads as no-longer-persisted.
    def destroy
      return self unless persisted?
      run_callbacks("before_destroy")
      sql = "DELETE FROM #{self.class.table_name} WHERE id = ?"
      self.class.connection.execute(sql, [@attributes["id"]])
      @attributes["id"] = nil
      self
    end

    # Re-read this record's row from the database, discarding local changes.
    def reload
      row = self.class.connection.get_first_row(
        "SELECT * FROM #{self.class.table_name} WHERE id = ?", [id]
      )
      raise RecordNotFound, "Couldn't find #{self.class.model_name} with id=#{id}" if row.nil?
      @attributes = {}
      self.class.columns.each { |c| @attributes[c] = nil }
      row.each { |k, v| @attributes[k.to_s] = v }
      @original = @attributes.dup
      self
    end

    private

    def run_validator(kind, attr)
      value = @attributes[attr]
      if kind == "presence"
        blank = value.nil? || (value.is_a?(String) && value.strip.empty?)
        @errors.add(attr, "can't be blank") if blank
      elsif kind == "uniqueness"
        conds = {}
        conds[attr] = value
        others = self.class.where(conds).to_a
        taken = others.reject { |r| persisted? && r.id == id }
        @errors.add(attr, "has already been taken") unless taken.empty?
      end
    end

    def run_callbacks(kind)
      self.class.callbacks(kind).each { |method_name| send(method_name) }
    end

    # A stable-per-call timestamp. Time is available on rubylang; the string is
    # never printed in the demos, so determinism of output is unaffected.
    def current_timestamp
      Time.now.to_s
    end

    def set_timestamp(col, stamp)
      @attributes[col] = stamp if self.class.has_column?(col)
    end

    def writable_columns
      self.class.columns.reject { |c| c == "id" }
    end

    def persist_insert
      cols         = writable_columns
      placeholders = cols.map { "?" }.join(", ")
      values       = cols.map { |c| @attributes[c] }
      sql = "INSERT INTO #{self.class.table_name} (#{cols.join(", ")}) VALUES (#{placeholders})"
      self.class.connection.execute(sql, values)
      @attributes["id"] = self.class.connection.last_insert_row_id
    end

    def persist_update
      cols        = writable_columns
      assignments = cols.map { |c| "#{c} = ?" }.join(", ")
      values      = cols.map { |c| @attributes[c] }
      sql = "UPDATE #{self.class.table_name} SET #{assignments} WHERE id = ?"
      self.class.connection.execute(sql, values + [@attributes["id"]])
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
  # Every table gets an implicit `id INTEGER PRIMARY KEY`, matching Rails. Pass
  # `timestamps: true` to create_table (or call `t.timestamps`) to add the
  # created_at / updated_at columns AR stamps automatically.
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

    # Rails' `t.timestamps` shorthand for the two datetime bookkeeping columns.
    def timestamps
      @columns << "created_at TEXT"
      @columns << "updated_at TEXT"
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
    def create_table(name, options = {})
      table = TableDefinition.new(name)
      table.timestamps if options[:timestamps]
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
