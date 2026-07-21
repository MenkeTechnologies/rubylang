//! Integration test for the DAP debugger (`ruby --dap`), driven over stdio with
//! no external `ruby` and no editor. The load-bearing assertion: a breakpoint on
//! a line INSIDE a method body fires, the stack trace names the method and line,
//! and a local is visible; then a step lands on the next line and `continue`
//! runs the program to completion (its stdout arriving as an `output` event).

use serde_json::{json, Value};
use std::io::{BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Read one `Content-Length`-framed JSON message from `r`.
fn read_msg(r: &mut impl Read) -> Option<Value> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let header = String::from_utf8_lossy(&header);
    let len: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(w: &mut impl Write, seq: &mut i64, command: &str, args: Value) {
    let msg = json!({ "seq": *seq, "type": "request", "command": command, "arguments": args });
    *seq += 1;
    let body = msg.to_string();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    w.flush().unwrap();
}

#[test]
fn dap_breakpoint_inside_method_fires_and_steps() {
    // A program whose breakpoint line (2) is INSIDE a method body — the case
    // that top-level-only line tracking could never hit.
    let dir = std::env::temp_dir();
    let prog = dir.join(format!("rubylang_dap_{}.rb", std::process::id()));
    std::fs::write(
        &prog,
        "def greet(name)\n  msg = \"hi \" + name\n  puts msg\nend\ngreet(\"world\")\n",
    )
    .unwrap();
    let prog_path = prog.to_str().unwrap().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg("--dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ruby --dap");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Drive the protocol on a worker thread so a hang fails via timeout instead
    // of blocking the test runner.
    let (tx, rx) = mpsc::channel::<Result<Vec<u32>, String>>();
    let handle = std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        let mut seq = 1i64;
        send(&mut stdin, &mut seq, "initialize", json!({}));
        send(
            &mut stdin,
            &mut seq,
            "setBreakpoints",
            json!({ "source": { "path": prog_path }, "breakpoints": [{ "line": 2 }] }),
        );
        send(&mut stdin, &mut seq, "configurationDone", json!({}));
        send(
            &mut stdin,
            &mut seq,
            "launch",
            json!({ "program": prog_path }),
        );

        let mut verified = false;
        let mut stop_lines: Vec<u32> = Vec::new();
        let mut top_name = String::new();
        let mut saw_name_var = false;
        let mut output = String::new();
        let mut terminated = false;

        while let Some(m) = read_msg(&mut r) {
            let ty = m.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ev = m.get("event").and_then(|t| t.as_str()).unwrap_or("");
            let cmd = m.get("command").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "response" && cmd == "setBreakpoints" {
                verified = m
                    .pointer("/body/breakpoints/0/verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            if ty == "event" && ev == "stopped" {
                send(&mut stdin, &mut seq, "stackTrace", json!({ "threadId": 1 }));
            }
            if ty == "response" && cmd == "stackTrace" {
                let line = m
                    .pointer("/body/stackFrames/0/line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                top_name = m
                    .pointer("/body/stackFrames/0/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                stop_lines.push(line);
                if stop_lines.len() == 1 {
                    // Inspect locals, then step to the next line.
                    send(
                        &mut stdin,
                        &mut seq,
                        "variables",
                        json!({ "variablesReference": 1 }),
                    );
                } else {
                    send(&mut stdin, &mut seq, "continue", json!({ "threadId": 1 }));
                }
            }
            if ty == "response" && cmd == "variables" {
                if let Some(vars) = m.pointer("/body/variables").and_then(|v| v.as_array()) {
                    saw_name_var = vars.iter().any(|v| {
                        v.get("name").and_then(|n| n.as_str()) == Some("name")
                            && v.get("value").and_then(|n| n.as_str()) == Some("\"world\"")
                    });
                }
                send(&mut stdin, &mut seq, "next", json!({ "threadId": 1 }));
            }
            if ty == "event" && ev == "output" {
                output.push_str(
                    m.pointer("/body/output")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
            }
            if ty == "event" && ev == "terminated" {
                terminated = true;
                break;
            }
        }

        let mut errs = Vec::new();
        if !verified {
            errs.push("breakpoint on line 2 not verified".to_string());
        }
        if stop_lines.first() != Some(&2) {
            errs.push(format!("first stop not line 2: {stop_lines:?}"));
        }
        if top_name != "greet" {
            errs.push(format!("top frame not `greet`: {top_name:?}"));
        }
        if !saw_name_var {
            errs.push("local `name = \"world\"` not visible".to_string());
        }
        if stop_lines.get(1) != Some(&3) {
            errs.push(format!("step did not land on line 3: {stop_lines:?}"));
        }
        if !output.contains("hi world") {
            errs.push(format!("program output missing: {output:?}"));
        }
        if !terminated {
            errs.push("no terminated event".to_string());
        }
        if errs.is_empty() {
            tx.send(Ok(stop_lines)).ok();
        } else {
            tx.send(Err(errs.join("; "))).ok();
        }
    });

    let result = rx.recv_timeout(Duration::from_secs(15));
    let _ = child.kill();
    let _ = child.wait();
    let _ = handle.join();
    let _ = std::fs::remove_file(&prog);

    match result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("DAP debugger assertion failed: {e}"),
        Err(_) => panic!("DAP debugger timed out (no result within 15s)"),
    }
}

#[test]
fn dap_function_breakpoint_stops_on_entry_and_evaluates() {
    // A `setFunctionBreakpoints` on `greet` must stop at the method's first
    // marker (reason `function breakpoint`) with `greet` on top of the stack, and
    // `evaluate` of the bound parameter must resolve against the paused frame.
    let dir = std::env::temp_dir();
    let prog = dir.join(format!("rubylang_dap_fn_{}.rb", std::process::id()));
    std::fs::write(
        &prog,
        "def greet(name)\n  msg = \"hi \" + name\n  puts msg\nend\ngreet(\"world\")\n",
    )
    .unwrap();
    let prog_path = prog.to_str().unwrap().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg("--dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ruby --dap");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let handle = std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        let mut seq = 1i64;
        send(&mut stdin, &mut seq, "initialize", json!({}));
        send(
            &mut stdin,
            &mut seq,
            "setFunctionBreakpoints",
            json!({ "breakpoints": [{ "name": "greet" }] }),
        );
        send(&mut stdin, &mut seq, "configurationDone", json!({}));
        send(
            &mut stdin,
            &mut seq,
            "launch",
            json!({ "program": prog_path }),
        );

        let mut fn_bp_verified = false;
        let mut stop_reason = String::new();
        let mut top_name = String::new();
        let mut eval_result = String::new();
        let mut terminated = false;

        while let Some(m) = read_msg(&mut r) {
            let ty = m.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ev = m.get("event").and_then(|t| t.as_str()).unwrap_or("");
            let cmd = m.get("command").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "response" && cmd == "setFunctionBreakpoints" {
                fn_bp_verified = m
                    .pointer("/body/breakpoints/0/verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            if ty == "event" && ev == "stopped" {
                stop_reason = m
                    .pointer("/body/reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                send(&mut stdin, &mut seq, "stackTrace", json!({ "threadId": 1 }));
            }
            if ty == "response" && cmd == "stackTrace" {
                top_name = m
                    .pointer("/body/stackFrames/0/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                send(
                    &mut stdin,
                    &mut seq,
                    "evaluate",
                    json!({ "expression": "name", "context": "watch" }),
                );
            }
            if ty == "response" && cmd == "evaluate" {
                eval_result = m
                    .pointer("/body/result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                send(&mut stdin, &mut seq, "continue", json!({ "threadId": 1 }));
            }
            if ty == "event" && ev == "terminated" {
                terminated = true;
                break;
            }
        }

        let mut errs = Vec::new();
        if !fn_bp_verified {
            errs.push("function breakpoint on `greet` not verified".to_string());
        }
        if stop_reason != "function breakpoint" {
            errs.push(format!(
                "stop reason not function breakpoint: {stop_reason:?}"
            ));
        }
        if top_name != "greet" {
            errs.push(format!("top frame not `greet`: {top_name:?}"));
        }
        if eval_result != "\"world\"" {
            errs.push(format!("evaluate `name` != \"world\": {eval_result:?}"));
        }
        if !terminated {
            errs.push("no terminated event".to_string());
        }
        if errs.is_empty() {
            tx.send(Ok(())).ok();
        } else {
            tx.send(Err(errs.join("; "))).ok();
        }
    });

    let result = rx.recv_timeout(Duration::from_secs(15));
    let _ = child.kill();
    let _ = child.wait();
    let _ = handle.join();
    let _ = std::fs::remove_file(&prog);

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("DAP function-breakpoint assertion failed: {e}"),
        Err(_) => panic!("DAP function-breakpoint test timed out (no result within 15s)"),
    }
}
