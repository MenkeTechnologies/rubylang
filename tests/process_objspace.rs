//! `Process.clock_gettime` clock selection and `ObjectSpace.each_object` over
//! program-defined classes, driven through the `ruby` binary.

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
fn cpu_time_clock_is_not_the_wall_clock() {
    let out = run("p Process.clock_gettime(Process::CLOCK_PROCESS_CPUTIME_ID) < 1000, \
         Process.clock_gettime(Process::CLOCK_REALTIME, :second) > 1_600_000_000");
    assert_eq!(out, "true\ntrue\n");
}

#[test]
fn monotonic_clock_never_goes_backwards_and_honours_units() {
    let out = run("a = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond); \
         b = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond); \
         p a.class, b >= a, Process.clock_gettime(Process::CLOCK_MONOTONIC, :float_millisecond).class");
    assert_eq!(out, "Integer\ntrue\nFloat\n");
}

#[test]
fn bad_clock_or_unit_raises_einval_naming_the_clock() {
    let out = run("[[99], [1, :bad]].each { |a| begin; Process.clock_gettime(*a); \
         rescue => e; p e.class, e.message; end }");
    assert_eq!(
        out,
        "Errno::EINVAL\n\"Invalid argument - clock_gettime(99)\"\n\
         Errno::EINVAL\n\"Invalid argument - clock_gettime(1)\"\n"
    );
}

#[test]
fn each_object_enumerates_defined_classes_and_modules() {
    let out = run("class Base; end; class A < Base; end; class B < Base; end; module Mx; end; \
         p ObjectSpace.each_object(Class).select { |c| c < Base }.sort_by(&:name), \
         ObjectSpace.each_object(Base.singleton_class).to_a.sort_by(&:name), \
         ObjectSpace.each_object(Module).include?(Mx), \
         ObjectSpace.each_object(Class).include?(Mx)");
    assert_eq!(out, "[A, B]\n[A, B, Base]\ntrue\nfalse\n");
}
