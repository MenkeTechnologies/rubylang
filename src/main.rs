//! The `ruby` binary entry point.
//!
//! Dispatch: `--lsp`/`--dap` speak their protocols over stdio; `--repl` (or no
//! file on a TTY) starts the interactive loop; `--build` AOT-compiles into the
//! cache; `--dump-bytecode` prints the lowered fusevm chunk; otherwise a file or
//! `-e` one-liner is run. Errors go to stderr in zsh-terse `ruby: <reason>`
//! form; nothing else is printed.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match rubylang::cli::parse() {
        Ok(c) => c,
        // MRI prints option errors to stderr and exits 1.
        Err(e) => return fail(&e),
    };

    // Informational, exit immediately (MRI prints to stdout and exits 0).
    if cli.show_version {
        println!("{}", rubylang::version_banner());
        return ExitCode::SUCCESS;
    }
    if cli.show_help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    // A bare `-v` with no program prints the banner and exits; with a program it
    // prints the banner first, then runs.
    if cli.verbose_version {
        println!("{}", rubylang::version_banner());
        if cli.file.is_none() && cli.eval.is_empty() {
            return ExitCode::SUCCESS;
        }
    }

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

    // The text-processing loop switches need ARGF + the Kernel `$_` method family
    // (`gsub`/`sub`/`chomp`/no-arg `print`), which are not built yet. Reject
    // explicitly rather than silently mis-run.
    if cli.loop_n || cli.loop_p || cli.autosplit || cli.chomp {
        return fail("-n/-p/-a/-l/-F (ARGF line loops) are not implemented yet");
    }

    let cfg = rubylang::RunConfig {
        argv: cli.argv.clone(),
        includes: cli.includes.clone(),
        requires: cli.requires.clone(),
        script_name: if !cli.eval.is_empty() {
            "-e".to_string()
        } else {
            cli.file.clone().unwrap_or_default()
        },
        warn_level: cli.warn_level,
        debug: cli.debug,
    };

    // `-e` one-liners: MRI joins repeated `-e` with newlines.
    if !cli.eval.is_empty() {
        let src = cli.eval.join("\n");
        if cli.check_syntax {
            return finish(check_syntax(&src));
        }
        return match rubylang::eval_str_cfg(&src, &cfg) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    if let Some(mut file) = cli.file.clone() {
        // `-S`: resolve a bare program name against `$PATH`.
        if cli.search_path {
            file = search_path(&file).unwrap_or(file);
        }
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
        if cli.check_syntax {
            let src = match std::fs::read_to_string(&file) {
                Ok(s) => s,
                Err(e) => return fail(&format!("cannot read {file}: {e}")),
            };
            return finish(check_syntax(&src));
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
        return match rubylang::eval_file_cfg(&file, &cfg) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    if cli.repl || atty_stdin() {
        rubylang::repl::run();
        return ExitCode::SUCCESS;
    }

    // No file and non-interactive stdin: run stdin as a script (`$0` == "-").
    let src = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    if cli.check_syntax {
        return finish(check_syntax(&src));
    }
    let cfg = rubylang::RunConfig {
        script_name: "-".to_string(),
        ..cfg
    };
    match rubylang::eval_str_cfg(&src, &cfg) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

/// `--help` / `-h` usage text.
const USAGE: &str = "\
Usage: ruby [switches] [--] [programfile] [arguments]
  -0[octal]       specify record separator (accepted, inert)
  -a              autosplit mode with -n or -p (splits $_ into $F)
  -c              check syntax only
  -Cdirectory     accepted, inert
  -d, --debug     set $DEBUG to true
  -e 'command'    one line of script; several -e's allowed; omit programfile
  -Fpattern       split() pattern for autosplit (-a)
  -i[extension]   accepted, inert
  -Idirectory     prepend directory to $LOAD_PATH
  -l              enable line-ending processing with -n/-p
  -n              assume 'while gets(); ... end' loop around your script
  -p              assume loop like -n but print line also, like sed
  -rlibrary       require the library before executing your script
  -S              look for the script using PATH environment variable
  -v              print the version number, then run in verbose mode
  -w              turn warnings on for your script
  -Wlevel         set warning level; 0=silence, 1=medium, 2=verbose
  --version       print the version number, then exit
  --repl          start the interactive REPL
  --lsp / --dap   speak the Language / Debug Adapter Protocol over stdio
  --build         AOT-compile the script into the on-disk cache
  --dump-tokens / --dump-ast / --dump-bytecode / --disasm   introspection
";

/// `-c`: parse and lower the source without running; MRI prints `Syntax OK`.
fn check_syntax(src: &str) -> Result<(), String> {
    rubylang::compile(src).map(|_| {
        // Explicit user-requested output (matches MRI's `ruby -c`).
        println!("Syntax OK");
    })
}

/// `-S`: locate `name` on `$PATH` if it is not already a path. Returns the
/// resolved path, or `None` to fall back to the name as given.
fn search_path(name: &str) -> Option<String> {
    if name.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
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
