//! Embedding entry point: run a Ruby program from a host and capture its output.

use crate::host::{self, StdinSource};

/// Native stack for the interpreter thread; the same reservation the `ruby`
/// binary runs its program on (`src/main.rs`).
const INTERPRETER_STACK: usize = 1024 * 1024 * 1024;

/// Unwind payload `exit!` raises inside an embedded run in place of
/// `process::exit`: it skips `ensure`/`rescue` and is caught by [`run_program`].
pub(crate) struct ExitNow(pub i32);

/// Run `src` as a complete program with `input` as its stdin and return
/// everything it wrote to stdout.
///
/// `input` is what `gets`, `$stdin`/`STDIN` (`read`, `gets`, `each_line`,
/// `readlines`) see. Each call runs on its own thread with a fresh host;
/// output goes to that host's capture buffer ([`crate::eval_str_captured`]), so
/// concurrent calls never share output and nothing reaches the process stdout.
/// Writes to `$stderr` land in the same buffer; `abort`'s message and an
/// uncaught exception's report go to the process stderr as in `ruby`.
///
/// A syntax error, an uncaught exception (MRI's report is the message), an
/// `exit`/`exit!`/`abort` with a nonzero status, and a panic in the interpreter
/// return `Err`. `exit`/`exit(0)`/`exit!(0)` return `Ok` with the output so far.
/// `at_exit` handlers run only when the program calls `exit`.
pub fn run_program(src: &str, input: &str) -> Result<String, String> {
    let src = src.to_owned();
    let stdin = StdinSource::Fed(std::io::Cursor::new(input.as_bytes().to_vec()));
    std::thread::Builder::new()
        .name("rubylang-embed".into())
        .stack_size(INTERPRETER_STACK)
        .spawn(move || run_captured(&src, stdin))
        .map_err(|e| format!("spawn interpreter thread: {e}"))?
        .join()
        .unwrap_or_else(|panic| Err(panic_message(panic.as_ref())))
}

fn run_captured(src: &str, stdin: StdinSource) -> Result<String, String> {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::eval_captured(src, &[], stdin)
    }));
    let (result, output) = match run {
        Ok(done) => done,
        Err(panic) => match panic.downcast::<ExitNow>() {
            Ok(exit) => {
                let output = host::with_host(|h| h.end_capture());
                return exit_outcome(exit.0, "exit", output);
            }
            Err(panic) => return Err(panic_message(panic.as_ref())),
        },
    };
    match (result, host::with_host(|h| h.exit_status)) {
        (Ok(_), _) => Ok(output),
        (Err(msg), Some(code)) => exit_outcome(code, &msg, output),
        (Err(msg), None) => Err(msg),
    }
}

/// The outcome of a program that ended through `exit`/`exit!`/`abort`: `Ok` on
/// status 0, else `Err` with the `SystemExit` message (`abort`'s text).
fn exit_outcome(code: i32, msg: &str, output: String) -> Result<String, String> {
    match (code, msg) {
        (0, _) => Ok(output),
        (_, "exit") => Err(format!("exit status {code}")),
        _ => Err(msg.to_string()),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    let msg = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic");
    format!("interpreter panicked: {msg}")
}
