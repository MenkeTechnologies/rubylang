//! Offline generator for `docs/reference.html` — the builtin method/kernel
//! reference page. Run before publishing GitHub Pages; keeps the reference in
//! sync with the LSP corpus (`rubylang::lsp::corpus`) so the two never drift.

use std::fmt::Write as _;

fn main() {
    let mut rows = String::new();
    for (name, doc) in rubylang::lsp::corpus() {
        let _ = writeln!(
            rows,
            "<tr><td><code>{}</code></td><td>{}</td></tr>",
            html_escape(name),
            html_escape(doc)
        );
    }

    let page = format!(
        r#"<main class="wrap">
  <h1>rubylang builtin reference</h1>
  <p>Kernel functions and core methods available in the current rubylang build.
  Generated from the language-server corpus — run <code>gen-docs</code> to refresh.</p>
  <table>
    <thead><tr><th>method</th><th>description</th></tr></thead>
    <tbody>
{rows}    </tbody>
  </table>
</main>
"#
    );

    let out = "docs/reference.body.html";
    if let Err(e) = std::fs::write(out, page) {
        eprintln!("gen-docs: cannot write {out}: {e}");
        std::process::exit(1);
    }
    println!("wrote {out} ({} entries)", rubylang::lsp::corpus().len());
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
