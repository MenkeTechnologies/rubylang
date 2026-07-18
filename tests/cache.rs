//! The rkyv/bincode bytecode cache round-trips a compiled program: store then
//! load must reproduce a program that runs identically. Uses an isolated HOME so
//! a developer's real `~/.rubyrs` shard is untouched.

use rubyrs::{cache, compiler, host};

#[test]
fn store_then_load_reproduces_the_program() {
    let tmp = tempfile::tempdir().unwrap();
    // Point the cache at an isolated home for the duration of this test.
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let src = "def double(x); x * 2; end; double(21)";
    let prog = rubyrs::compile(src).expect("compile");
    cache::store(src, &prog).expect("store");

    let loaded = cache::load(src).expect("cached program present");
    // A different source must miss.
    assert!(cache::load("puts 1").is_none());

    // The loaded program runs to the same value as a fresh compile.
    host::reset_host();
    let compiler::Program {
        main,
        methods,
        procs,
    } = loaded;
    host::with_host(|h| h.load_program(methods, procs));
    let v = host::run_main(main).expect("run cached");
    let got = host::with_host(|h| h.inspect(&v));
    assert_eq!(got, "42");

    match prev {
        Some(p) => std::env::set_var("HOME", p),
        None => std::env::remove_var("HOME"),
    }
}
