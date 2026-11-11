//! Tests for the embedder entry point (`eval_str_captured`), which an in-process
//! host uses instead of `eval_str`: bindings are seeded after the host reset, and
//! program output is captured rather than written to the process streams.

/// A seeded binding is a real `String`, not merely something that prints like
/// one: it answers `String` methods. Strings live on the host heap, so this is
/// the property a `Value`-shaped API would silently get wrong.
#[test]
fn seeded_bindings_are_real_strings() {
    let (result, out) = rubylang::eval_str_captured("puts stdin.class", &[("stdin", "hi")]);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(out, "String\n");
}

/// A seeded binding is visible to the program. `eval_str` cannot do this — it
/// resets the host, so anything installed beforehand is gone by the time the
/// program runs.
#[test]
fn seeded_bindings_survive_the_host_reset() {
    let (result, out) = rubylang::eval_str_captured("puts stdin.upcase", &[("stdin", "hello")]);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(out, "HELLO\n");
}

/// Every Kernel writer is captured — `puts` (newline-completing), `print` (raw),
/// `p` (inspect) and `$stdout.write` (the IO layer) all funnel through the same
/// host sink, so none of them can reach a terminal the embedder owns.
#[test]
fn every_write_path_is_captured() {
    let (result, out) = rubylang::eval_str_captured(
        "print 'raw '\nputs 'line'\np 'inspected'\n$stdout.write(\"io\\n\")\n",
        &[],
    );
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(out, "raw line\n\"inspected\"\nio\n");
}

/// A program that prints and then raises produced both: the error and the
/// output it managed to write are returned side by side.
#[test]
fn output_before_a_raise_is_kept() {
    let (result, out) = rubylang::eval_str_captured("puts 'before'\nraise 'boom'", &[]);
    // The MESSAGE, not just `is_err`: any failure at all satisfied that, so the
    // test passed whether the raise surfaced or the program failed to parse.
    assert_eq!(
        result.err().as_deref(),
        Some("-e:2:in '<main>': boom (RuntimeError)")
    );
    assert_eq!(out, "before\n");
}

/// Capture is per-run: a second run does not see the first run's output.
#[test]
fn capture_resets_between_runs() {
    let (_, first) = rubylang::eval_str_captured("puts 'one'", &[]);
    let (_, second) = rubylang::eval_str_captured("puts 'two'", &[]);
    assert_eq!(first, "one\n");
    assert_eq!(second, "two\n");
}

/// A second run's blocks are its OWN. Block bodies are recycled through a
/// thread-local VM pool keyed by `ChunkId::Proc(i)`, and `i` is an index into
/// the host's proc table — which a fresh host restarts at 0. Without clearing
/// that pool on reset, the second program's block reused the first program's
/// pooled VM and ran the FIRST program's body: this exact pair answered
/// `undefined method '<' for nil` (the recursive lambda's `n < 2`) instead of
/// `[1, 2]`.
///
/// The first program must recurse deep enough to leave its block's VM in the
/// pool, and the second must reach the same proc index, which is why both are
/// written the way they are rather than as one-liners.
#[test]
fn a_second_runs_blocks_do_not_reuse_the_first_runs_bodies() {
    let (first, out1) = rubylang::eval_str_captured(
        "f = lambda { |n| n < 2 ? n : f.call(n - 1) }\np f.call(10)",
        &[],
    );
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(out1, "1\n");

    let (second, out2) =
        rubylang::eval_str_captured("p Enumerator.new { |y| y << 1; y << 2 }.to_a", &[]);
    assert!(second.is_ok(), "{second:?}");
    assert_eq!(out2, "[1, 2]\n");
}

// `run_program(src, input)`: the uniform embedding entry point.

#[test]
fn run_program_captures_stdout() {
    assert_eq!(
        rubylang::run_program("puts 'hello'", "").unwrap(),
        "hello\n"
    );
}

/// `input` is the program's stdin for every read path: `gets` (Kernel, line by
/// line), then `$stdin.read` (the rest), and `nil` at EOF.
#[test]
fn run_program_feeds_input_as_stdin() {
    let src = "a = gets\nrest = $stdin.read\np gets\nputs a.upcase\nputs rest.reverse";
    assert_eq!(
        rubylang::run_program(src, "one\ntwo\n").unwrap(),
        "nil\nONE\n\nowt\n"
    );
}

/// An uncaught exception and a nonzero `exit` are errors; `exit 0` after output
/// is success with that output, and `exit!` skips `ensure` without ending the
/// process (this test would not finish if it did).
#[test]
fn run_program_errors_and_exit_status() {
    let raised = rubylang::run_program("raise 'boom'", "").unwrap_err();
    assert!(raised.contains("boom (RuntimeError)"), "{raised}");
    assert_eq!(
        rubylang::run_program("puts 1\nexit 3", "").unwrap_err(),
        "exit status 3"
    );
    assert_eq!(
        rubylang::run_program("puts 1\nexit\nputs 2", "").unwrap(),
        "1\n"
    );
    assert_eq!(
        rubylang::run_program("begin\n  puts 1\n  exit!(0)\nensure\n  puts 2\nend", "").unwrap(),
        "1\n"
    );
    assert!(rubylang::run_program("def (", "").is_err());
}

/// Concurrent runs each see their own stdin and return only their own output.
#[test]
fn run_program_concurrent_runs_do_not_mix() {
    let src = "n = gets.to_i\n200.times { |i| puts n + i }";
    let runs: Vec<_> = [1000, 5000]
        .into_iter()
        .map(|n| std::thread::spawn(move || (n, rubylang::run_program(src, &format!("{n}\n")))))
        .collect();
    for run in runs {
        let (n, out) = run.join().unwrap();
        let want: String = (0..200).map(|i| format!("{}\n", n + i)).collect();
        assert_eq!(out.unwrap(), want);
    }
}
