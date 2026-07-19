//! Offline generator for `docs/reference.html` — the builtin method / kernel
//! reference page, rendered with the same cyberpunk HUD chrome as
//! `docs/index.html`. Run before publishing GitHub Pages:
//!
//! ```sh
//! cargo run --bin gen-docs
//! ```
//!
//! Source of truth: the LSP corpus in `rubylang::lsp` (`corpus()`), the exact
//! `(name, chapter, doc)` table the editor completion/hover path renders from.
//! The static page and the language server therefore never drift — a method is
//! documented here only if the runtime actually dispatches it in `builtins.rs`.
//!
//! Chapters are the corpus's own class/module grouping (`entry.1`), rendered in
//! first-seen order, one `<section>`/table per chapter.

use std::collections::BTreeSet;
use std::fmt::Write as _;

fn main() {
    let corpus = rubylang::lsp::corpus();
    let chapters: BTreeSet<&str> = corpus.iter().map(|(_, c, _)| *c).collect();

    let page = format!(
        "{head}{body}{foot}",
        head = HEAD,
        body = build_body(corpus, chapters.len()),
        foot = FOOT,
    )
    // Stamp the current crate version so the page never falls behind Cargo.toml
    // (the meta version-sync gate compares docs/*.html against the manifest).
    .replace("__RUBYLANG_VERSION__", env!("CARGO_PKG_VERSION"));

    let out = "docs/reference.html";
    if let Err(e) = std::fs::write(out, page) {
        eprintln!("gen-docs: cannot write {out}: {e}");
        std::process::exit(1);
    }
    println!("wrote {out} ({} methods, {} chapters)", corpus.len(), chapters.len());
}

/// Render the stat grid plus one section/table per chapter, in corpus order.
fn build_body(corpus: &[(&str, &str, &str)], chapter_count: usize) -> String {
    let mut out = String::new();

    let _ = write!(
        out,
        "\n      <div class=\"stat-grid\">\n\
         \x20       <div class=\"stat-card\"><div class=\"stat-val\">{methods}</div><div class=\"stat-label\">Documented methods</div></div>\n\
         \x20       <div class=\"stat-card\"><div class=\"stat-val accent\">{chapters}</div><div class=\"stat-label\">Classes &amp; modules</div></div>\n\
         \x20     </div>\n",
        methods = corpus.len(),
        chapters = chapter_count,
    );

    // Walk the corpus once, opening a new section each time the chapter changes.
    let mut current: Option<&str> = None;
    for (name, chapter, doc) in corpus {
        if current != Some(*chapter) {
            if current.is_some() {
                out.push_str("          </tbody>\n        </table>\n      </section>\n");
            }
            // The `id="ch-…"` marks this as a real reference chapter. The
            // reference-PDF pipeline keeps id-carrying sections and drops the
            // id-less ones (page chrome / link lists), so every chapter needs it.
            let _ = write!(
                out,
                "\n      <section class=\"tutorial-section\" id=\"ch-{slug}\">\n\
                 \x20       <h2>{title}</h2>\n\
                 \x20       <table class=\"file-table\">\n\
                 \x20         <thead><tr><th>Method</th><th>Description</th></tr></thead>\n\
                 \x20         <tbody>\n",
                slug = slugify(chapter),
                title = html_escape(chapter),
            );
            current = Some(*chapter);
        }
        let _ = writeln!(
            out,
            "<tr><td><code>{}</code></td><td>{}</td></tr>",
            html_escape(name),
            html_escape(doc),
        );
    }
    if current.is_some() {
        out.push_str("          </tbody>\n        </table>\n      </section>\n");
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Lowercase, non-alphanumeric runs collapsed to a single `-`, edges trimmed —
/// e.g. `Enumerator::Lazy` -> `enumerator-lazy`, `TrueClass/FalseClass` ->
/// `trueclass-falseclass`. Used for the `id="ch-…"` chapter anchors.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

const HEAD: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark light">
  <meta name="description" content="rubylang — Builtin reference. Kernel functions and core methods available in the current rubylang build. MIT licensed.">
  <title>rubylang &mdash; Builtin Reference</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Orbitron:wght@400;600;700;900&family=Share+Tech+Mono&display=swap" rel="stylesheet">
  <link rel="stylesheet" href="hud-static.css">
  <link rel="stylesheet" href="tutorial.css">
  <style>
    .tutorial-main { max-width: 76rem; }
    .file-table { width:100%;border-collapse:collapse;margin:0.6rem 0;font-size:12px; }
    .file-table th { background:var(--bg-secondary);color:var(--cyan);font-family:'Orbitron',sans-serif;font-size:10px;font-weight:700;letter-spacing:1.2px;text-transform:uppercase;text-align:left;padding:7px 10px;border:1px solid var(--border); }
    .file-table td { padding:6px 10px;border:1px solid var(--border);color:var(--text-dim);vertical-align:middle; }
    .file-table tr:hover td { background:var(--bg-hover); }
    .file-table td:first-child { font-family:'Share Tech Mono',monospace;color:var(--accent-light);font-weight:600;white-space:nowrap; }
    .file-table code { font-size:11px;color:var(--accent-light);background:var(--bg-primary);padding:1px 4px;border-radius:2px; }
    .stat-grid { display:grid;grid-template-columns:repeat(auto-fill,minmax(14rem,1fr));gap:0.75rem;margin:1.2rem 0; }
    .stat-card { border:1px solid var(--border);border-top:3px solid var(--cyan);background:var(--bg-card);padding:1rem 1.2rem;border-radius:2px;text-align:center; }
    .stat-card .stat-val { font-family:'Orbitron',sans-serif;font-size:28px;font-weight:900;color:var(--cyan);line-height:1.1;text-shadow:0 0 20px var(--cyan-glow); }
    .stat-card .stat-val.accent { color:var(--accent);text-shadow:0 0 20px var(--accent-glow); }
    .stat-card .stat-label { font-family:'Orbitron',sans-serif;font-size:9px;font-weight:700;letter-spacing:2px;text-transform:uppercase;color:var(--text-muted);margin-top:0.5rem; }
    .section-rule { border:none;border-top:1px dashed var(--border);margin:2rem 0; }
    .hub-scheme-strip { border-bottom:1px dashed var(--border);background:color-mix(in srgb, var(--bg-secondary) 85%, transparent);padding:0.55rem 1.5rem 0.65rem;position:relative; }
    .hub-scheme-strip-inner { max-width:76rem;margin:0 auto;display:flex;align-items:center;gap:0.85rem; }
    .hub-scheme-strip .hud-scheme-label { flex:0 0 auto;font-family:'Orbitron',sans-serif;font-size:9px;font-weight:700;letter-spacing:2px;text-transform:uppercase;color:var(--accent);text-align:left; }
    .hub-scheme-strip .scheme-grid { flex:1 1 auto;display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:6px; }
    @media (max-width:720px){ .hub-scheme-strip-inner{flex-direction:column;align-items:stretch}.hub-scheme-strip .scheme-grid{grid-template-columns:repeat(2,minmax(0,1fr))} }
    .docs-build-line { margin:0.35rem 0 0;font-family:'Share Tech Mono',ui-monospace,monospace;font-size:11px;color:var(--text-dim);letter-spacing:0.03em;max-width:42rem;opacity:0.75; }
  </style>
</head>
<body>
  <div class="app tutorial-app" id="docsApp">
    <div class="crt-scanline" id="crtH" aria-hidden="true"></div>
    <div class="crt-scanline-v" id="crtV" aria-hidden="true"></div>

    <header class="tutorial-header">
      <div class="tutorial-header-inner">
        <div>
          <h1 class="tutorial-brand">// RUBYLANG — BUILTIN REFERENCE</h1>
          <nav class="tutorial-crumbs" aria-label="Breadcrumb">
            <a href="index.html">Docs</a>
            <span class="sep">/</span>
            <span class="current">Builtin Reference</span>
            <span class="sep">/</span>
            <a href="https://github.com/MenkeTechnologies/rubylang" target="_blank" rel="noopener noreferrer">GitHub</a>
          </nav>
          <p class="docs-build-line">rubylang v__RUBYLANG_VERSION__ · Ruby on fusevm · lex/parse → AST → bytecode → Cranelift JIT · MIT · in active development</p>
        </div>
        <div class="tutorial-toolbar">
          <button type="button" class="btn btn-secondary" id="btnTheme" title="Toggle light/dark">Theme</button>
          <button type="button" class="btn btn-secondary active" id="btnCrt" title="CRT scanline overlay">CRT</button>
          <button type="button" class="btn btn-secondary active" id="btnNeon" title="Neon border pulse">Neon</button>
          <a class="btn btn-secondary" href="index.html">Docs</a>
          <a class="btn btn-secondary" href="https://github.com/MenkeTechnologies/rubylang" target="_blank" rel="noopener noreferrer">GitHub</a>
        </div>
      </div>
    </header>

    <div class="hub-scheme-strip">
      <div class="hub-scheme-strip-inner">
        <span class="hud-scheme-label">// Color scheme</span>
        <div class="scheme-grid" id="hudSchemeGrid"></div>
      </div>
    </div>

    <main class="tutorial-main">
      <h2 class="tutorial-title"><span class="step-hash">&gt;_</span>BUILTIN REFERENCE</h2>
      <p class="tutorial-subtitle">Every builtin method the current rubylang build dispatches, grouped by class and module. This page is generated from the language-server corpus (<code>src/lsp.rs</code>) by the <code>gen-docs</code> binary, so it stays in sync with what the runtime and editor tooling actually know about. Each row mirrors a real dispatch arm in <code>src/builtins.rs</code>.</p>
"#;

const FOOT: &str = r#"
      <section class="tutorial-section">
        <h2>More</h2>
        <ul>
          <li><strong>Docs</strong> — <a href="index.html">index.html</a> (overview, architecture, examples)</li>
          <li><strong>Engineering report</strong> — <a href="report.html">report.html</a> (value model, status, dependencies)</li>
          <li><strong>Source</strong> — <a href="https://github.com/MenkeTechnologies/rubylang">github.com/MenkeTechnologies/rubylang</a></li>
        </ul>
      </section>
    </main>

  </div>

  <script src="hud-theme.js"></script>
</body>
</html>
"#;
