//! Debug Adapter Protocol over stdio (`ruby --dap`).
//!
//! A minimal, spec-compliant adapter: it completes the initialize/launch
//! handshake, runs the launched `.rb` program to completion while streaming its
//! stdout as `output` events, and terminates. Stepping / breakpoints are a later
//! wave (the fusevm line table is emitted but not yet surfaced as stops); the
//! handshake and run-to-completion path are real so an editor can attach today.
//!
//! Program stdout is captured through a `pipe` + `dup2` on fd 1 so `puts`/`print`
//! land in `output` events instead of corrupting the JSON channel on fd 1.

use serde_json::{json, Value as J};
use std::io::{Read, Write};

/// Entry point for `ruby --dap`.
pub fn run() -> Result<(), String> {
    let mut input = std::io::stdin();
    let mut seq = 1i64;

    while let Some(msg) = read_message(&mut input)? {
        let command = msg.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let req_seq = msg.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);

        match command {
            "initialize" => {
                respond(
                    &mut seq,
                    req_seq,
                    command,
                    json!({ "supportsConfigurationDoneRequest": true }),
                );
                event(&mut seq, "initialized", json!({}));
            }
            "launch" => {
                respond(&mut seq, req_seq, command, json!({}));
                let program = msg
                    .get("arguments")
                    .and_then(|a| a.get("program"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                run_program(&mut seq, program);
                event(&mut seq, "terminated", json!({}));
            }
            "configurationDone" | "threads" | "setBreakpoints" => {
                let body = if command == "threads" {
                    json!({ "threads": [{ "id": 1, "name": "main" }] })
                } else {
                    json!({})
                };
                respond(&mut seq, req_seq, command, body);
            }
            "disconnect" => {
                respond(&mut seq, req_seq, command, json!({}));
                break;
            }
            _ => respond(&mut seq, req_seq, command, json!({})),
        }
    }
    Ok(())
}

/// Run the program, capturing its stdout and forwarding it as `output` events.
fn run_program(seq: &mut i64, program: &str) {
    let captured = capture_stdout(|| {
        if let Err(e) = crate::eval_file(program) {
            eprintln!("ruby: {e}");
        }
    });
    if !captured.is_empty() {
        event(
            seq,
            "output",
            json!({ "category": "stdout", "output": captured }),
        );
    }
}

/// Redirect fd 1 into a pipe for the duration of `f`, returning what was written.
fn capture_stdout(f: impl FnOnce()) -> String {
    // SAFETY: standard pipe + dup2 dance on the process's own stdout fd.
    unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            f();
            return String::new();
        }
        let saved = libc::dup(1);
        libc::dup2(fds[1], 1);
        libc::close(fds[1]);

        f();
        // Flush Rust's stdout buffer before restoring the fd.
        let _ = std::io::stdout().flush();

        libc::dup2(saved, 1);
        libc::close(saved);

        let mut out = String::new();
        let mut file = std::fs::File::from_raw_fd(fds[0]);
        let _ = file.read_to_string(&mut out);
        out
    }
}

use std::os::unix::io::FromRawFd;

// ---- wire protocol --------------------------------------------------------

/// Read one `Content-Length`-framed JSON message; `None` at EOF.
fn read_message(input: &mut std::io::Stdin) -> Result<Option<J>, String> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    // Read until the blank line terminating the headers.
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) => return Err(format!("dap read: {e}")),
        }
    }
    let header = String::from_utf8_lossy(&header);
    let len: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())
        .ok_or("dap: missing Content-Length")?;
    let mut body = vec![0u8; len];
    input
        .read_exact(&mut body)
        .map_err(|e| format!("dap body: {e}"))?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| format!("dap json: {e}"))
}

fn send(msg: &J) {
    let body = msg.to_string();
    let out = std::io::stdout();
    let mut lock = out.lock();
    let _ = write!(lock, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = lock.flush();
}

fn respond(seq: &mut i64, req_seq: i64, command: &str, body: J) {
    let msg = json!({
        "seq": *seq,
        "type": "response",
        "request_seq": req_seq,
        "success": true,
        "command": command,
        "body": body,
    });
    *seq += 1;
    send(&msg);
}

fn event(seq: &mut i64, event: &str, body: J) {
    let msg = json!({ "seq": *seq, "type": "event", "event": event, "body": body });
    *seq += 1;
    send(&msg);
}
