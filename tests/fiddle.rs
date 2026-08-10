//! `Fiddle` (FFI) end-to-end tests. Each drives the whole pipeline (parse →
//! compile → run on fusevm) and makes a *real* C call through libffi against
//! symbols resolved from the current process (`Fiddle.dlopen(nil)` →
//! `dlopen(NULL)`), so no external library file is required. libc symbols
//! (`strlen`, `abs`, `strdup`, `getenv`) are always present in the test binary;
//! `sqrt` (libm) may not be linked on every host, so its test gates cleanly.
//!
//! The gating pattern: each C-call test resolves the symbol inside a `rescue`
//! and answers the sentinel `:unresolved` when the SYMBOL LOOKUP fails, so the
//! test passes on a stripped/minimal libc rather than failing spuriously.
//!
//! Only the lookup is inside the rescue. Wrapping the whole call in it — which
//! is what these did — let a broken `Function.new`, a broken `call`, or a
//! marshalling bug take the skip branch too, so every one of these tests passed
//! on every host with the feature removed. The pure-Ruby surface (type-code
//! constants, `require`, `Fiddle::Pointer[str]`) is asserted unconditionally.

use rubylang::eval_to_string as ev;

/// Assert a real C call's result, or REPORT that the symbol did not resolve.
///
/// The `:unresolved` sentinel is a legitimate escape on a stripped libc, but a
/// SILENT one is indistinguishable from a verified call: every one of these
/// tests would stay green with the C-call path entirely broken and nothing
/// would say which branch ran. `fiddle_core_libc_symbols_resolve` below is the
/// backstop — it fails if the sentinel becomes the normal outcome.
fn assert_call(test: &str, out: &str, expected: &str) {
    if out == ":unresolved" {
        eprintln!(
            "SKIPPED C CALL: {test} took the :unresolved branch — dlopen(NULL) \
             could not resolve the symbol on this host, so the libffi call, the \
             marshalling and the return conversion were NOT exercised"
        );
        return;
    }
    assert_eq!(out, expected, "{test}");
}

/// The backstop for the `:unresolved` escape in the four C-call tests below.
///
/// Each of those may legitimately skip on a host that cannot resolve its
/// symbol. All four skipping at once is not a host quirk — it is `dlopen(NULL)`
/// or the symbol-lookup path being broken, which is exactly the failure the
/// per-test sentinel is shaped to hide. `strlen` is in the C runtime of every
/// Unix this crate builds for, so requiring at least it to resolve costs no
/// portability and turns a silent all-skip into a red test.
#[test]
fn fiddle_core_libc_symbols_resolve() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        %w[strlen].map do |sym|
          begin
            libc[sym]
            :ok
          rescue Fiddle::DLError
            :unresolved
          end
        end
    "#;
    let out = ev(src).expect("eval");
    assert_eq!(
        out, "[:ok]",
        "dlopen(NULL) could not resolve strlen — the :unresolved escape in the \
         C-call tests is then taken everywhere and they all pass having called \
         no C at all"
    );
}

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
          ptr = libc["strlen"]
        rescue Fiddle::DLError
          :unresolved
        else
          Fiddle::Function.new(ptr, [Fiddle::TYPE_VOIDP], Fiddle::TYPE_SIZE_T).call("hello")
        end
    "#;
    let out = ev(src).expect("eval");
    assert_call("fiddle_strlen", &out, "5");
}

/// Real call: `abs(-7)` → 7. A signed `int` argument and return.
#[test]
fn fiddle_abs() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          ptr = libc["abs"]
        rescue Fiddle::DLError
          :unresolved
        else
          Fiddle::Function.new(ptr, [Fiddle::TYPE_INT], Fiddle::TYPE_INT).call(-7)
        end
    "#;
    let out = ev(src).expect("eval");
    assert_call("fiddle_abs", &out, "7");
}

/// Real call: `sqrt(16.0)` → 4.0. A `double` argument and return (gated — libm
/// may not be resolvable via dlopen(NULL) on a minimal host).
#[test]
fn fiddle_sqrt_double() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          ptr = libc["sqrt"]
        rescue Fiddle::DLError
          :unresolved
        else
          Fiddle::Function.new(ptr, [Fiddle::TYPE_DOUBLE], Fiddle::TYPE_DOUBLE).call(16.0)
        end
    "#;
    let out = ev(src).expect("eval");
    assert_call("fiddle_sqrt_double", &out, "4.0");
}

/// Real call returning a `char*`: `strdup("world")` returns a `Fiddle::Pointer`,
/// and `#to_s` reads the C string back to a Ruby String.
#[test]
fn fiddle_strdup_returns_readable_pointer() {
    let src = r#"
        require "fiddle"
        libc = Fiddle.dlopen(nil)
        begin
          ptr = libc["strdup"]
        rescue Fiddle::DLError
          :unresolved
        else
          sd = Fiddle::Function.new(ptr, [Fiddle::TYPE_VOIDP], Fiddle::TYPE_VOIDP)
          p = sd.call("world")
          r = [p.class.to_s, p.to_s]
          p.free
          r
        end
    "#;
    let out = ev(src).expect("eval");
    assert_call(
        "fiddle_strdup_returns_readable_pointer",
        &out,
        r#"["Fiddle::Pointer", "world"]"#,
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
