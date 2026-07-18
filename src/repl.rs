//! Interactive REPL (`ruby --repl`, or `ruby` on a TTY with no file).
//!
//! Each accepted line is compiled and run on a *persistent* host, so `def`s and
//! variables carry across prompts. The value of the line is echoed with
//! `inspect` (the `=>` form), like `irb`.

use crate::{banner, host};
use nu_ansi_term::Color;
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

/// Run the REPL until Ctrl-D / EOF.
pub fn run() {
    banner::print_banner();
    host::reset_host();

    let mut line_editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("ruby".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        match line_editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if matches!(line, "exit" | "quit") {
                    break;
                }
                eval_line(line);
            }
            Ok(Signal::CtrlC) => continue,
            Ok(Signal::CtrlD) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

/// Compile+run a single line on the live host, echoing the result.
fn eval_line(line: &str) {
    let prog = match crate::compile(line) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", Color::Red.paint(format!("ruby: {e}")));
            return;
        }
    };
    let crate::compiler::Program {
        main,
        methods,
        classes,
        begins,
        procs,
    } = prog;
    host::with_host(|h| h.load_program(methods, classes, begins, procs));
    match host::run_main(main) {
        Ok(v) => {
            let s = host::with_host(|h| h.inspect(&v));
            println!("{} {}", Color::DarkGray.paint("=>"), Color::Green.paint(s));
        }
        Err(e) => eprintln!("{}", Color::Red.paint(format!("ruby: {e}"))),
    }
}
