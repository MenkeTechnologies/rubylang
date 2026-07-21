//! Ruby wiring for inline Rust FFI (`rust { ... }` blocks).
//!
//! The heavy lifting lives in fusevm: [`fusevm::rust_sugar`] scans and rewrites
//! the block at the source level, and [`fusevm::ffi`] compiles/loads/marshals
//! it. This module only supplies the Ruby-flavored [`fusevm::RustSugar`] config
//! and the desugar entry the parser calls. The emitted `__rust_compile(...)`
//! call and every exported bareword are resolved in
//! [`crate::builtins`] (`dispatch_call`).

use fusevm::RustSugar;

/// Emit the Ruby statement a `rust { ... }` block desugars to: a call to the
/// `__rust_compile` builtin carrying the base64-encoded block body and its line.
/// A bare call (no `;`) — Ruby statements end at the newline; the desugarer pads
/// the replacement with the block's newline count so line numbers are preserved.
fn emit(b64: &str, line: usize) -> String {
    format!("__rust_compile(\"{b64}\", {line})")
}

/// Ruby desugar config: `#` line comments, no block comments, and
/// `newline_boundary = true` (Ruby statements end at a newline / `;`), so a
/// `rust { ... }` block on its own line is recognized. `rust {` at a statement
/// boundary is never idiomatic Ruby, so this only ever matches an intended FFI
/// block.
pub const SUGAR: RustSugar = RustSugar {
    keyword: "rust",
    line_comments: &["#"],
    block_comment: None,
    newline_boundary: true,
    emit,
};

/// Rewrite every top-level `rust { ... }` block in Ruby source into a
/// `__rust_compile(...)` call, before lexing. No-op when the source has no
/// `rust` token.
pub fn desugar(src: &str) -> String {
    SUGAR.desugar(src)
}

#[cfg(test)]
mod tests {
    #[test]
    fn desugars_ruby_block() {
        let src =
            "rust { pub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b } }\nputs add(2, 3)\n";
        let out = super::desugar(src);
        assert!(out.contains("__rust_compile("), "no builtin call: {out}");
        assert!(!out.contains("pub extern"), "Rust body leaked: {out}");
        assert!(out.contains("puts add(2, 3)"));
    }

    #[test]
    fn leaves_ordinary_ruby_untouched() {
        let src = "x = \"hi\".length\nputs x\n";
        assert_eq!(super::desugar(src), src);
    }

    #[test]
    fn keyword_in_hash_comment_not_expanded() {
        let src = "# rust { not a block }\nputs 1\n";
        assert_eq!(super::desugar(src), src);
    }
}
