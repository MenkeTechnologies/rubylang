//! End-to-end inline Rust FFI: a `rust { ... }` block is desugared, compiled to
//! a cdylib via `rustc`, dlopened, and its exports called by bareword from Ruby.
//!
//! These tests used to `return` early when `rustc` was missing — one of them
//! silently, with no message at all. That is a test that PASSES having executed
//! zero assertions, and nothing in a green run distinguishes it from a real one.
//! It cannot legitimately skip either: this file is compiled and linked by the
//! very `rustc` it was checking for, so an unavailable `rustc` here means the
//! environment is broken, not unsupported. `require_rustc` therefore FAILS.

use rubylang::eval_to_string as ev;

/// Fail loudly if the compiler that built this test binary cannot be invoked.
/// A broken `$RUSTC` override is a real defect and must not read as a pass.
fn require_rustc() {
    let name = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let ok = std::process::Command::new(&name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        ok,
        "rustc ({name}) must be runnable: this test binary was built by it, so \
         its absence is a broken environment, not an unsupported one"
    );
}

#[test]
fn rust_block_exports_are_callable_across_all_v1_signatures() {
    require_rustc();
    // Distinct names so this test's registry entries never collide with another
    // test's. Exercises int-arity, float-arity, and string→int marshalling
    // (Ruby String args are host heap handles, marshalled to native strings).
    let src = r#"
rust {
    pub extern "C" fn ffi_addi(a: i64, b: i64) -> i64 { a + b }
    pub extern "C" fn ffi_mulf(x: f64, y: f64, z: f64) -> f64 { x * y * z }
    pub extern "C" fn ffi_slen(s: *const c_char) -> i64 {
        unsafe { CStr::from_ptr(s).to_bytes().len() as i64 }
    }
}
[ffi_addi(21, 21), ffi_mulf(1.5, 2.0, 3.0), ffi_slen("hello world")]
"#;
    let out = ev(src).expect("FFI program should run");
    assert_eq!(out, "[42, 9.0, 11]");
}

#[test]
fn rust_block_with_no_exports_errors() {
    require_rustc();
    // A block with no `pub extern "C" fn` is a hard error — v1 requires at least
    // one exported function.
    let src = "rust { fn helper() -> i64 { 1 } }\n1\n";
    let err = ev(src).expect_err("empty-export block must error");
    assert!(err.contains("rust FFI"), "unexpected error: {err}");
}
