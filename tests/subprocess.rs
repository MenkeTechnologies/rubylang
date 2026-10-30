//! Command execution — backticks, `%x(…)`, `` <<`CMD` ``, `Kernel#system` and
//! `$?` / `Process::Status` — plus `Kernel#sleep` and `IO#sync`, driven through
//! the `ruby` binary because child processes write to the real stdout.
//!
//! Every expected output was captured from MRI 4.0.9 running the same source
//! (pids masked with `sub(/\d+/, "N")` where they appear).

use std::process::Command;

fn run(src: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .arg("-e")
        .arg(src)
        .output()
        .expect("spawn ruby");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn backticks_capture_stdout_and_set_last_status() {
    assert_eq!(
        run("p $?; x = `echo hi`; p x, $?.class, $?.success?, $?.exitstatus, $?.exited?, \
             $?.signaled?, $?.to_i"),
        "nil\n\"hi\\n\"\nProcess::Status\ntrue\n0\ntrue\nfalse\n0\n"
    );
    assert_eq!(
        run(r#"p `printf "a b"; exit 3`, $?.exitstatus, $?.success?, $?.to_i, $?.to_s.sub(/\d+/, "N"), $?.inspect.sub(/\d+/, "N")"#),
        "\"a b\"\n3\nfalse\n768\n\"pid N exit 3\"\n\"#<Process::Status: pid N exit 3>\"\n"
    );
}

#[test]
fn percent_x_and_backtick_heredoc_are_command_strings() {
    assert_eq!(
        run("v = 5; p %x(echo #{v}), %x{printf a}"),
        "\"5\\n\"\n\"a\"\n"
    );
    assert_eq!(run("s = <<~`CMD`\n  echo heredoc\nCMD\np s"), "\"heredoc\\n\"\n");
}

#[test]
fn a_user_backtick_method_overrides_the_command_string() {
    assert_eq!(
        run(r#"def `(c) = "custom #{c}"; p `ls`, %x(a b)"#),
        "\"custom ls\"\n\"custom a b\"\n"
    );
}

#[test]
fn a_program_that_cannot_run_is_enoent_for_backticks_and_nil_for_system() {
    // No metacharacter, so the line is exec'd directly rather than through sh,
    // which is why it is ENOENT and not sh's "command not found".
    assert_eq!(
        run("p((`nonexistent_cmd_zz arg` rescue $!), $?.exitstatus)"),
        "#<Errno::ENOENT: No such file or directory - nonexistent_cmd_zz>\n127\n"
    );
    assert_eq!(
        run(r#"p system("true"), system("false"), system("exit 4"), $?.exitstatus, system("nonexistent_cmd_zz"), $?.exitstatus, system("")"#),
        "true\nfalse\nfalse\n4\nnil\n127\nnil\n"
    );
}

#[test]
fn system_inherits_stdout_in_order_and_takes_env_argv_and_chdir() {
    assert_eq!(
        run(r#"puts "before"; system("echo from-child"); puts "after""#),
        "before\nfrom-child\nafter\n"
    );
    assert_eq!(
        run(r#"p system("echo", "a  b", "c"), system({"ZV" => "q"}, "echo $ZV"), system("pwd", chdir: "/")"#),
        "a  b c\nq\n/\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn a_signalled_child_reports_its_signal() {
    assert_eq!(
        run(r#"`kill -TERM $$`; p $?.signaled?, $?.termsig, $?.exitstatus, $?.success?, $?.inspect.sub(/\d+/, "N")"#),
        "true\n15\nnil\nnil\n\"#<Process::Status: pid N SIGTERM (signal 15)>\"\n"
    );
}

#[test]
fn system_argument_and_exception_errors() {
    assert_eq!(
        run(r#"p((system("nonexistent_cmd_zz", exception: true) rescue $!), (system("false", exception: true) rescue $!), (system("true", foo: 1) rescue $!), (system(1) rescue $!), (system rescue $!))"#),
        "#<Errno::ENOENT: No such file or directory - nonexistent_cmd_zz>\n\
         #<RuntimeError: Command failed with exit 1: false>\n\
         #<ArgumentError: wrong exec option symbol: foo>\n\
         #<TypeError: no implicit conversion of Integer into String>\n\
         #<ArgumentError: wrong number of arguments (given 0, expected 1+)>\n"
    );
}

#[test]
fn process_status_has_no_new_and_last_status_is_dollar_question() {
    assert_eq!(
        run("p((Process::Status.new rescue $!.class), Process.last_status); `true`; \
             p Process.last_status.equal?($?)"),
        "NoMethodError\nnil\ntrue\n"
    );
}

#[test]
fn command_bytes_and_output_bytes_pass_through_unchanged() {
    assert_eq!(
        run(r#"p `printf "\\377"`.valid_encoding?, `printf "\xff"`.bytes"#),
        "false\n[255]\n"
    );
}

#[test]
fn sleep_validates_its_interval() {
    assert_eq!(
        run(r#"p sleep(0), (sleep(-1) rescue $!), (sleep("1") rescue $!), (sleep(true) rescue $!), (sleep(1, 2) rescue $!), (sleep(Float::NAN) rescue $!)"#),
        "0\n#<ArgumentError: time interval must not be negative>\n\
         #<TypeError: can't convert String into time interval>\n\
         #<TypeError: can't convert TrueClass into time interval>\n\
         #<ArgumentError: wrong number of arguments (given 2, expected 0..1)>\n\
         #<RangeError: NaN out of Time range>\n"
    );
}

#[test]
fn sleep_actually_suspends() {
    let start = std::time::Instant::now();
    run("sleep 0.3");
    assert!(start.elapsed() >= std::time::Duration::from_millis(300));
}

#[test]
fn io_sync_is_a_per_stream_flag() {
    assert_eq!(
        run("p $stdout.sync, $stderr.sync, $stdin.sync; $stdout.sync = true; \
             p $stdout.sync, $stdout.fsync, ($stdout.sync = nil), $stdout.sync"),
        "false\ntrue\nfalse\ntrue\n0\nnil\nfalse\n"
    );
}
