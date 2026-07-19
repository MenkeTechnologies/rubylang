//! `Fiddle` (FFI) end-to-end tests. Each drives the whole pipeline (parse →
//! compile → run on fusevm) and makes a *real* C call through libffi against
//! symbols resolved from the current process (`Fiddle.dlopen(nil)` →
//! `dlopen(NULL)`), so no external library file is required. libc symbols
//! (`strlen`, `abs`, `strdup`, `getenv`) are always present in the test binary;
//! `sqrt` (libm) may not be linked on every host, so its test gates cleanly.
//!
//! The gating pattern: each C-call test resolves the symbol inside a `rescue`
//! and returns the sentinel `"SKIP"` when the symbol is unresolvable, so the
//! test passes on a stripped/minimal libc rather than failing spuriously. The
//! pure-Ruby surface (type-code constants, `require`, `Fiddle::Pointer[str]`)
//! is asserted unconditionally.

use rubylang::eval_to_string as ev;

/// `require "fiddle"` is a no-op that succeeds (returns true), like a builtin lib.
#[test]
fn fiddle_require_is_a_noop() {
    assert_eq!(ev(r#"require "fiddle""#).expect("eval"), "true");
}

/// The MRI type-code constants have their exact MRI integer values (a negative
/// code is the unsigned variant of its magnitude).
#[test]
fn fiddle_type_constants() {
    let src = r#"
        require "fiddle"
        [Fiddle::TYPE_VOID, Fiddle::TYPE_VOIDP, Fiddle::TYPE_CHAR,
         Fiddle::TYPE_SHORT, Fiddle::TYPE_INT, Fiddle::TYPE_LONG,
         Fiddle::TYPE_LONG_LONG, Fiddle::TYPE_FLOAT, Fiddle::TYPE_DOUBLE,
         Fiddle::TYPE_SIZE_T]
    "#;
    assert_eq!(ev(src).expect("eval"), "[0, 1, 2, 3, 4, 5, 6, 7, 8, -5]");
}

/// `Fiddle.dlopen(nil)` returns a `Fiddle::Handle`, and `handle[sym]` resolves a
/// symbol to an Integer address.
#[test]
fn fiddle_dlopen_self_and_sym() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        [libc.class.to_s, libc["strlen"].is_a?(Integer)]
    "#;
    assert_eq!(ev(src).expect("eval"), r#"["Fiddle::Handle", true]"#);
}

/// Real call: `strlen("hello")` → 5. Argument String marshals to a C `char*`;
/// the `size_t` result marshals back to an Integer.
#[test]
fn fiddle_strlen() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          f = Fiddle::Function.new(libc["strlen"], [Fiddle::TYPE_VOIDP], Fiddle::TYPE_SIZE_T)
          f.call("hello")
        rescue Fiddle::DLError
          "SKIP"
        end
    "#;
    let out = ev(src).expect("eval");
    assert!(out == "5" || out == "\"SKIP\"", "got {out:?}");
}

/// Real call: `abs(-7)` → 7. A signed `int` argument and return.
#[test]
fn fiddle_abs() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          abs = Fiddle::Function.new(libc["abs"], [Fiddle::TYPE_INT], Fiddle::TYPE_INT)
          abs.call(-7)
        rescue Fiddle::DLError
          "SKIP"
        end
    "#;
    let out = ev(src).expect("eval");
    assert!(out == "7" || out == "\"SKIP\"", "got {out:?}");
}

/// Real call: `sqrt(16.0)` → 4.0. A `double` argument and return (gated — libm
/// may not be resolvable via dlopen(NULL) on a minimal host).
#[test]
fn fiddle_sqrt_double() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          sq = Fiddle::Function.new(libc["sqrt"], [Fiddle::TYPE_DOUBLE], Fiddle::TYPE_DOUBLE)
          sq.call(16.0)
        rescue Fiddle::DLError
          "SKIP"
        end
    "#;
    let out = ev(src).expect("eval");
    assert!(out == "4.0" || out == "\"SKIP\"", "got {out:?}");
}

/// Real call returning a `char*`: `strdup("world")` returns a `Fiddle::Pointer`,
/// and `#to_s` reads the C string back to a Ruby String.
#[test]
fn fiddle_strdup_returns_readable_pointer() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          sd = Fiddle::Function.new(libc["strdup"], [Fiddle::TYPE_VOIDP], Fiddle::TYPE_VOIDP)
          p = sd.call("world")
          r = [p.class.to_s, p.to_s]
          p.free
          r
        rescue Fiddle::DLError
          "SKIP"
        end
    "#;
    let out = ev(src).expect("eval");
    assert!(
        out == r#"["Fiddle::Pointer", "world"]"# || out == "\"SKIP\"",
        "got {out:?}"
    );
}

/// `Fiddle::Pointer[str]` / `.to_ptr(str)` wrap bytes; `#to_s`/`#to_str(len)`
/// read them back. No C call — pure host memory, always runs.
#[test]
fn fiddle_pointer_wrap_and_read() {
    let src = r#"
        require "fiddle"
        a = Fiddle::Pointer["abc"]
        b = Fiddle::Pointer.to_ptr("hello")
        [a.class.to_s, a.to_s, a.size, b.to_str(3)]
    "#;
    assert_eq!(
        ev(src).expect("eval"),
        r#"["Fiddle::Pointer", "abc", 3, "hel"]"#
    );
}

/// `Fiddle::Pointer.malloc(n)` yields a zeroed buffer (`#to_s` is empty, `#size`
/// is n), and `#free` releases it without error.
#[test]
fn fiddle_pointer_malloc_and_free() {
    let src = r#"
        require "fiddle"
        p = Fiddle::Pointer.malloc(8)
        r = [p.class.to_s, p.size, p.to_s]
        p.free
        r
    "#;
    assert_eq!(ev(src).expect("eval"), r#"["Fiddle::Pointer", 8, ""]"#);
}

/// A bad `dlopen` path raises `Fiddle::DLError`, which a bare `rescue` (and an
/// explicit `rescue Fiddle::DLError`) both catch.
#[test]
fn fiddle_dlopen_bad_path_raises_rescuable() {
    let src = r#"
        require "fiddle"
        begin
          Fiddle.dlopen("/no/such/library/definitely-missing.so")
          "no-error"
        rescue Fiddle::DLError
          "caught"
        end
    "#;
    assert_eq!(ev(src).expect("eval"), "\"caught\"");
}
