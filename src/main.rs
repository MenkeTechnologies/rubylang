//! The `ruby` binary entry point.
//!
//! Dispatch: `--lsp`/`--dap` speak their protocols over stdio; `--repl` (or no
//! file on a TTY) starts the interactive loop; `--build` AOT-compiles into the
//! cache; `--dump-bytecode` prints the lowered fusevm chunk; otherwise a file or
//! `-e` one-liner is run. Errors go to stderr in zsh-terse `ruby: <reason>`
//! form; nothing else is printed.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = rubylang::cli::parse();

    if cli.lsp {
        return match rubylang::lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }
    if cli.dap {
        return match rubylang::dap::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    if let Some(src) = cli.eval {
        return run_source(&src, "-e");
    }

    if let Some(file) = cli.file {
        if cli.dump_bytecode {
            return finish(dump(&file));
        }
        if cli.dump_tokens {
            return finish(dump_tokens(&file));
        }
        if cli.dump_ast {
            return finish(dump_ast(&file));
        }
        if cli.disasm {
            return finish(disasm(&file));
        }
        if cli.build {
            if cli.native {
                return match rubylang::aot::build_native(&file) {
                    Ok(msg) => {
                        // A build report is explicit user-requested output.
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => fail(&e),
                };
            }
            return match rubylang::aot::build(&file) {
                Ok(msg) => {
                    // A build report is explicit user-requested output.
                    println!("{msg}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e),
            };
        }
        return match rubylang::eval_file(&file) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    if cli.repl || atty_stdin() {
        rubylang::repl::run();
        return ExitCode::SUCCESS;
    }

    // No file and non-interactive stdin: run stdin as a script.
    let src = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    run_source(&src, "stdin")
}

fn run_source(src: &str, _label: &str) -> ExitCode {
    match rubylang::eval_str(src) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn dump(file: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let prog = rubylang::compile(&src)?;
    println!("== main ==\n{:#?}", prog.main.ops);
    for (name, m) in &prog.methods {
        println!(
            "== def {name} ({}) ==\n{:#?}",
            m.params.join(", "),
            m.chunk.ops
        );
    }
    for (i, p) in prog.procs.iter().enumerate() {
        println!(
            "== block #{i} (|{}|) ==\n{:#?}",
            p.params.join(", "),
            p.chunk.ops
        );
    }
    Ok(())
}

/// `--dump-tokens`: print the lexer token stream (after `rust { }` desugaring),
/// one `line<TAB>Tok` per line.
fn dump_tokens(file: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let src = rubylang::rust_ffi::desugar(&src);
    for t in rubylang::lexer::lex(&src)? {
        println!("{}\t{:?}", t.line, t.kind);
    }
    Ok(())
}

/// `--dump-ast`: print the parsed Ruby AST.
fn dump_ast(file: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let stmts = rubylang::parser::parse(&src)?;
    println!("{stmts:#?}");
    Ok(())
}

/// `--disasm`: print a fusevm bytecode disassembly of the main chunk plus every
/// compiled method and block, via the shared `fusevm::Chunk::disassemble`.
fn disasm(file: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let prog = rubylang::compile(&src)?;
    println!("; ruby fusevm — main\n{}", prog.main.disassemble());
    for (name, m) in &prog.methods {
        println!(
            "; ruby fusevm — def {name}({})\n{}",
            m.params.join(", "),
            m.chunk.disassemble()
        );
    }
    for (i, p) in prog.procs.iter().enumerate() {
        println!(
            "; ruby fusevm — block #{i} (|{}|)\n{}",
            p.params.join(", "),
            p.chunk.disassemble()
        );
    }
    Ok(())
}

fn finish(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn atty_stdin() -> bool {
    // SAFETY: isatty is a pure query on the stdin fd.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("ruby: {msg}");
    ExitCode::FAILURE
}
