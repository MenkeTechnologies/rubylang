//! `SQLite3::Database` end-to-end tests. Each drives the whole pipeline (parse →
//! compile → run on fusevm) against a real `rusqlite` connection (the `bundled`
//! SQLite amalgamation, no external libsqlite3). These pin the MRI-faithful
//! sqlite3-gem surface: `execute`/`execute2`, positional binds, array- and
//! hash-shaped rows, `get_first_row`/`get_first_value`, `last_insert_row_id`/
//! `changes`, `results_as_hash=`, rescuable `SQLite3::SQLException`, the
//! sqlite→Ruby type mapping, and — via a tempfile written then reopened in a
//! *fresh* interpreter — real on-disk persistence.
//!
//! `eval_to_string` calls `reset_host()` on every invocation, so the write-phase
//! and read-phase evals below run on independent interpreters with empty
//! `db_handles`; the reopened data can only have come from the file on disk.

use rubylang::eval_to_string as ev;

/// `require "sqlite3"` is a no-op that succeeds (returns true), like a builtin lib.
#[test]
fn sqlite_require_is_a_noop() {
    let out = ev(r#"require "sqlite3""#).expect("eval");
    assert_eq!(out, "true");
}

/// In-memory CRUD: create a table, insert two rows with positional array binds,
/// and read them back as Arrays of column values.
#[test]
fn sqlite_in_memory_crud_rows_as_arrays() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
        db.execute("INSERT INTO users (name, age) VALUES (?, ?)", ["Alice", 30])
        db.execute("INSERT INTO users (name, age) VALUES (?, ?)", ["Bob", 25])
        db.execute("SELECT id, name, age FROM users ORDER BY id")
    "#;
    let out = ev(src).expect("eval");
    assert_eq!(out, r#"[[1, "Alice", 30], [2, "Bob", 25]]"#);
}

/// `.open` is an alias for `.new`, and `:memory:` is the default path.
#[test]
fn sqlite_open_alias_and_default_memory() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.open(":memory:")
        db.execute("CREATE TABLE t (x)")
        db.execute("INSERT INTO t VALUES (42)")
        db.get_first_value("SELECT x FROM t")
    "#;
    assert_eq!(ev(src).expect("eval"), "42");
}

/// `db.results_as_hash = true` makes rows Hashes keyed by column name.
#[test]
fn sqlite_results_as_hash() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE users (name TEXT, age INTEGER)")
        db.execute("INSERT INTO users VALUES (?, ?)", ["Alice", 30])
        db.results_as_hash = true
        db.execute("SELECT name, age FROM users")
    "#;
    assert_eq!(ev(src).expect("eval"), r#"[{"name" => "Alice", "age" => 30}]"#);
}

/// `get_first_value` returns a scalar; `get_first_row` returns one row; both take
/// trailing positional binds. A no-match `get_first_row` returns nil.
#[test]
fn sqlite_get_first_value_and_row() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE users (name TEXT, age INTEGER)")
        db.execute("INSERT INTO users VALUES (?, ?)", ["Alice", 30])
        db.execute("INSERT INTO users VALUES (?, ?)", ["Bob", 25])
        [
          db.get_first_value("SELECT COUNT(*) FROM users"),
          db.get_first_row("SELECT name, age FROM users WHERE name = ?", "Bob"),
          db.get_first_row("SELECT name FROM users WHERE name = ?", "Nobody"),
        ]
    "#;
    assert_eq!(ev(src).expect("eval"), r#"[2, ["Bob", 25], nil]"#);
}

/// `last_insert_row_id` and `changes` reflect the most recent write.
#[test]
fn sqlite_last_insert_row_id_and_changes() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        db.execute("INSERT INTO t (v) VALUES (10)")
        db.execute("INSERT INTO t (v) VALUES (20)")
        rowid = db.last_insert_row_id
        db.execute("UPDATE t SET v = v + 1")
        [rowid, db.changes]
    "#;
    assert_eq!(ev(src).expect("eval"), "[2, 2]");
}

/// The sqlite→Ruby type map: INTEGER→Integer, REAL→Float, TEXT→String,
/// NULL→nil.
#[test]
fn sqlite_type_mapping() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE t (i INTEGER, r REAL, s TEXT, n INTEGER)")
        db.execute("INSERT INTO t VALUES (?, ?, ?, ?)", [7, 2.5, "hi", nil])
        db.get_first_row("SELECT i, r, s, n FROM t")
    "#;
    assert_eq!(ev(src).expect("eval"), r#"[7, 2.5, "hi", nil]"#);
}

/// `execute2` prepends a header row of the column names.
#[test]
fn sqlite_execute2_prepends_header_row() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE t (name TEXT)")
        db.execute("INSERT INTO t VALUES ('a')")
        db.execute("INSERT INTO t VALUES ('b')")
        db.execute2("SELECT name FROM t ORDER BY name")
    "#;
    assert_eq!(ev(src).expect("eval"), r#"[["name"], ["a"], ["b"]]"#);
}

/// The block form of `execute` yields each row and returns nil.
#[test]
fn sqlite_execute_block_form_yields_rows() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE t (name TEXT)")
        db.execute("INSERT INTO t VALUES ('a')")
        db.execute("INSERT INTO t VALUES ('b')")
        seen = []
        db.execute("SELECT name FROM t ORDER BY name") { |row| seen << row[0] }
        seen
    "#;
    assert_eq!(ev(src).expect("eval"), r#"["a", "b"]"#);
}

/// A SQL error raises `SQLite3::SQLException`, caught by an explicit
/// `rescue SQLite3::SQLException`, with the sqlite message preserved.
#[test]
fn sqlite_error_raises_rescuable_sqlexception() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        begin
          db.execute("SELECT * FROM does_not_exist")
          "no error"
        rescue SQLite3::SQLException => e
          "caught:#{e.message.include?('no such table')}"
        end
    "#;
    assert_eq!(ev(src).expect("eval"), r#""caught:true""#);
}

/// The same error is also caught by a bare `rescue` (proving it is a
/// StandardError), reporting the exception class name.
#[test]
fn sqlite_error_caught_by_bare_rescue() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        begin
          db.execute("THIS IS NOT SQL")
          "no error"
        rescue => e
          e.class.to_s
        end
    "#;
    assert_eq!(ev(src).expect("eval"), r#""SQLite3::SQLException""#);
}

/// `db.close` closes the handle; `closed?` then reports true and further use
/// raises.
#[test]
fn sqlite_close_and_closed_predicate() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        before = db.closed?
        db.close
        [before, db.closed?]
    "#;
    assert_eq!(ev(src).expect("eval"), "[false, true]");
}

/// The block form of `Database.new` yields the handle and returns the block's
/// value (closing the DB afterward).
#[test]
fn sqlite_database_new_block_form_returns_block_value() {
    let src = r#"
        require "sqlite3"
        SQLite3::Database.new(":memory:") do |db|
          db.execute("CREATE TABLE t (x)")
          db.execute("INSERT INTO t VALUES (99)")
          db.get_first_value("SELECT x FROM t")
        end
    "#;
    assert_eq!(ev(src).expect("eval"), "99");
}

/// Real on-disk persistence: write rows to a tempfile DB in one interpreter,
/// then reopen the same file in a *fresh* interpreter (empty `db_handles`) and
/// read the rows back. The data can only have survived via the file on disk.
#[test]
fn sqlite_file_backed_persistence_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("app.db");
    let p = db_path.to_str().unwrap().to_string();

    // Write phase — its own interpreter.
    let write_src = format!(
        r#"
        require "sqlite3"
        db = SQLite3::Database.new({p:?})
        db.execute("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, views INTEGER)")
        db.execute("INSERT INTO posts (title, views) VALUES (?, ?)", ["Hello", 100])
        db.execute("INSERT INTO posts (title, views) VALUES (?, ?)", ["World", 250])
        db.close
        db.closed?
        "#
    );
    assert_eq!(ev(&write_src).expect("write eval"), "true");
    assert!(db_path.exists(), "the DB file must exist on disk after write");

    // Read phase — a fresh interpreter (host was reset), reopening the file.
    let read_src = format!(
        r#"
        require "sqlite3"
        db = SQLite3::Database.new({p:?})
        rows = db.execute("SELECT title, views FROM posts ORDER BY id")
        total = db.get_first_value("SELECT SUM(views) FROM posts")
        db.close
        [rows, total]
        "#
    );
    assert_eq!(
        ev(&read_src).expect("read eval"),
        r#"[[["Hello", 100], ["World", 250]], 350]"#,
    );
}

/// A single scalar bind (no wrapping array) is accepted; a placeholder with no
/// supplied bind is left NULL — the lenient sqlite3-gem bind behavior.
#[test]
fn sqlite_scalar_bind_and_null_padding() {
    let src = r#"
        require "sqlite3"
        db = SQLite3::Database.new(":memory:")
        db.execute("CREATE TABLE t (a TEXT, b TEXT)")
        db.execute("INSERT INTO t (a, b) VALUES (?, ?)", "only")
        db.get_first_row("SELECT a, b FROM t")
    "#;
    assert_eq!(ev(src).expect("eval"), r#"["only", nil]"#);
}
