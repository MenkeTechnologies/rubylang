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
            return match dump(&file) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            };
        }
        if cli.build {
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

fn atty_stdin() -> bool {
    // SAFETY: isatty is a pure query on the stdin fd.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("ruby: {msg}");
    ExitCode::FAILURE
}
