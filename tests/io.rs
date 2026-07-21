//! File / IO / Dir end-to-end tests. Each drives the whole pipeline (parse →
//! compile → run on fusevm), writing real files under an isolated `tempfile`
//! directory and reading them back. Values are asserted as their Ruby `inspect`
//! form. These pin the MRI-faithful surface: `File.read/write`, the path
//! helpers (`basename`/`dirname`/`extname`/`join`/`expand_path`), `File.open`
//! with a block, the IO instance methods, `Dir.pwd/glob/entries`, and the
//! standard-stream objects.

use rubylang::eval_to_string as ev;

/// Assert `src` evaluates to `expected` (its inspect form).
fn eq(src: &str, expected: &str) {
    match ev(src) {
        Ok(got) => assert_eq!(got, expected, "for source: {src}"),
        Err(e) => panic!("eval error for `{src}`: {e}"),
    }
}

/// A unique temp directory for one test, dropped at scope end.
fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn file_write_returns_byte_count_and_read_round_trips() {
    let d = tmp();
    let p = d.path().join("rt.txt");
    let p = p.to_str().unwrap();
    // File.write returns the number of bytes written.
    eq(&format!("File.write({p:?}, \"hello world\")"), "11");
    eq(&format!("File.read({p:?})"), "\"hello world\"");
    eq(&format!("File.size({p:?})"), "11");
}

#[test]
fn file_predicates() {
    let d = tmp();
    let f = d.path().join("a.txt");
    let f = f.to_str().unwrap();
    let sub = d.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let sub = sub.to_str().unwrap();
    std::fs::write(f, b"x").unwrap();
    eq(&format!("File.exist?({f:?})"), "true");
    eq(&format!("File.exist?({f:?} + \"_nope\")"), "false");
    eq(&format!("File.file?({f:?})"), "true");
    eq(&format!("File.file?({sub:?})"), "false");
    eq(&format!("File.directory?({sub:?})"), "true");
    eq(&format!("File.directory?({f:?})"), "false");
}

#[test]
fn file_basename_dirname_extname() {
    eq("File.basename(\"/a/b/c.txt\")", "\"c.txt\"");
    eq("File.basename(\"/a/b/c.txt\", \".txt\")", "\"c\"");
    eq("File.basename(\"/a/b/c.txt\", \".*\")", "\"c\"");
    eq("File.basename(\"/a/b/\")", "\"b\"");
    eq("File.basename(\"/\")", "\"/\"");
    eq("File.dirname(\"/a/b/c.txt\")", "\"/a/b\"");
    eq("File.dirname(\"c.txt\")", "\".\"");
    eq("File.dirname(\"/\")", "\"/\"");
    // extname MRI edge cases: leading-dot name, trailing dot, all-dots.
    eq("File.extname(\"foo.tar.gz\")", "\".gz\"");
    eq("File.extname(\"foo.\")", "\".\"");
    eq("File.extname(\".foo\")", "\"\"");
    eq("File.extname(\"a..\")", "\".\"");
    eq("File.extname(\"...\")", "\"\"");
    eq("File.extname(\"/x/.z\")", "\"\"");
}

#[test]
fn file_join_and_expand_path() {
    eq("File.join(\"a\", \"b\", \"c\")", "\"a/b/c\"");
    eq("File.join(\"a/\", \"/b\", \"c\")", "\"a/b/c\"");
    eq("File.join(\"/a\", \"b\")", "\"/a/b\"");
    // expand_path is purely lexical against an explicit base.
    eq("File.expand_path(\"foo\", \"/bar\")", "\"/bar/foo\"");
    eq("File.expand_path(\"../a\", \"/x/y\")", "\"/x/a\"");
    eq("File.expand_path(\"a/../b\", \"/x\")", "\"/x/b\"");
}

#[test]
fn file_open_with_block_round_trips_and_closes() {
    let d = tmp();
    let p = d.path().join("lines.txt");
    let p = p.to_str().unwrap();
    std::fs::write(p, b"l1\nl2\nl3\n").unwrap();
    // The block's value is returned, and the handle is closed on exit.
    eq(
        &format!("File.open({p:?}) {{ |f| f.read }}"),
        "\"l1\\nl2\\nl3\\n\"",
    );
    eq(&format!("File.open({p:?}) {{ |f| f.gets }}"), "\"l1\\n\"");
    // Block-less open returns the IO; inspect shows the path, closed? tracks state.
    eq(
        &format!("f = File.open({p:?}); r = f.inspect; f.close; [r, f.closed?, f.inspect]"),
        &format!("[\"#<File:{p}>\", true, \"#<File:{p} (closed)>\"]"),
    );
}

#[test]
fn io_instance_write_read_gets_readlines() {
    let d = tmp();
    let p = d.path().join("io.txt");
    let p = p.to_str().unwrap();
    // write returns the byte count; << returns the IO; puts/print append/none.
    eq(
        &format!(
            "f = File.open({p:?}, \"w\"); n = f.write(\"abc\"); f << \"de\"; f.close; [n, File.read({p:?})]"
        ),
        "[3, \"abcde\"]",
    );
    std::fs::write(p, b"x\ny\nz\n").unwrap();
    eq(
        &format!("File.open({p:?}) {{ |f| [f.gets, f.gets, f.readlines] }}"),
        "[\"x\\n\", \"y\\n\", [\"z\\n\"]]",
    );
    eq(
        &format!("File.readlines({p:?})"),
        "[\"x\\n\", \"y\\n\", \"z\\n\"]",
    );
}

#[test]
fn file_foreach_yields_each_line() {
    let d = tmp();
    let p = d.path().join("fe.txt");
    let p = p.to_str().unwrap();
    std::fs::write(p, b"one\ntwo\n").unwrap();
    eq(
        &format!("acc = []; File.foreach({p:?}) {{ |l| acc << l }}; acc"),
        "[\"one\\n\", \"two\\n\"]",
    );
}

#[test]
fn file_delete_removes_the_file() {
    let d = tmp();
    let p = d.path().join("del.txt");
    let p = p.to_str().unwrap();
    std::fs::write(p, b"x").unwrap();
    eq(
        &format!("n = File.delete({p:?}); [n, File.exist?({p:?})]"),
        "[1, false]",
    );
}

#[test]
fn dir_pwd_is_absolute() {
    // Dir.pwd returns the process cwd (absolute path). Shared across parallel
    // tests, so assert only its shape, not a specific value.
    eq("Dir.pwd.start_with?(\"/\")", "true");
}

#[test]
fn dir_glob_sorted_and_dotfile_excluded() {
    let d = tmp();
    for n in ["b.txt", "a.txt", "c.rb", ".hidden"] {
        std::fs::write(d.path().join(n), b"x").unwrap();
    }
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("sub").join("d.txt"), b"x").unwrap();
    let base = d.path().to_str().unwrap();
    // `*` sorts and excludes the leading-dot file.
    eq(
        &format!("Dir.glob({base:?} + \"/*\")"),
        &format!("[\"{base}/a.txt\", \"{base}/b.txt\", \"{base}/c.rb\", \"{base}/sub\"]"),
    );
    // `*.txt` filters by extension, sorted.
    eq(
        &format!("Dir.glob({base:?} + \"/*.txt\")"),
        &format!("[\"{base}/a.txt\", \"{base}/b.txt\"]"),
    );
    // `**` recurses; results sorted lexicographically.
    eq(
        &format!("Dir.glob({base:?} + \"/**/*.txt\")"),
        &format!("[\"{base}/a.txt\", \"{base}/b.txt\", \"{base}/sub/d.txt\"]"),
    );
    // Brace alternation: each group globbed+sorted, concatenated in brace order.
    eq(
        &format!("Dir.glob({base:?} + \"/*.{{txt,rb}}\")"),
        &format!("[\"{base}/a.txt\", \"{base}/b.txt\", \"{base}/c.rb\"]"),
    );
    // Dir[] is an alias for Dir.glob.
    eq(
        &format!("Dir[{base:?} + \"/*.rb\"]"),
        &format!("[\"{base}/c.rb\"]"),
    );
}

#[test]
fn dir_entries_includes_dot_and_dotdot() {
    let d = tmp();
    for n in ["a.txt", "b.txt"] {
        std::fs::write(d.path().join(n), b"x").unwrap();
    }
    let base = d.path().to_str().unwrap();
    eq(
        &format!("Dir.entries({base:?}).sort"),
        "[\".\", \"..\", \"a.txt\", \"b.txt\"]",
    );
}

#[test]
fn dir_exist_and_mkdir() {
    let d = tmp();
    let nd = d.path().join("made");
    let nd = nd.to_str().unwrap();
    eq(&format!("Dir.exist?({nd:?})"), "false");
    eq(&format!("Dir.mkdir({nd:?}); Dir.exist?({nd:?})"), "true");
}

#[test]
fn dir_home_matches_env() {
    let home = std::env::var("HOME").unwrap();
    eq("Dir.home", &format!("{home:?}"));
}

#[test]
fn standard_streams_are_io_objects() {
    eq("$stdout.class.to_s", "\"IO\"");
    eq("STDOUT.class.to_s", "\"IO\"");
    eq("$stderr.class.to_s", "\"IO\"");
    eq("$stdin.class.to_s", "\"IO\"");
    eq("STDOUT.inspect", "\"#<IO:<STDOUT>>\"");
    eq("STDERR.inspect", "\"#<IO:<STDERR>>\"");
    eq("STDIN.inspect", "\"#<IO:<STDIN>>\"");
    // $stdout and STDOUT are the same object.
    eq("$stdout.equal?(STDOUT)", "true");
}

#[test]
fn stdout_write_returns_byte_count() {
    // write returns bytes written; << returns the IO; puts/print return nil.
    eq("$stdout.write(\"ab\")", "2");
    eq("STDOUT.write(\"a\", \"b\", \"c\\n\")", "4");
    eq("$stdout.puts(\"x\")", "nil");
    eq("$stdout.print(\"y\")", "nil");
    eq("($stdout << \"z\").equal?(STDOUT)", "true");
    eq("$stdout.flush.equal?(STDOUT)", "true");
}

#[test]
fn kernel_open_delegates_to_file_open() {
    let d = tmp();
    let p = d.path().join("k.txt");
    let p = p.to_str().unwrap();
    std::fs::write(p, b"kernel").unwrap();
    eq(&format!("open({p:?}) {{ |f| f.read }}"), "\"kernel\"");
}
