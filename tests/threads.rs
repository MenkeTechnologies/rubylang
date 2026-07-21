//! Thread semantics on the GVL, checked by running programs through the built
//! `ruby` binary (each subprocess has its own process-global host, so the cases
//! stay isolated). These lock in the guarantees the GVL model must uphold:
//! shared-heap visibility, one-thread-at-a-time atomicity, join/value ordering,
//! and cross-thread exception propagation. All are deterministic and CI-safe
//! (real OS threads, no reliance on timing/interleaving).

use std::process::Command;

/// Run `src` through the `ruby` binary and return its stdout.
fn run(src: &str) -> String {
    let ruby = env!("CARGO_BIN_EXE_ruby");
    let out = Command::new(ruby)
        .arg("-e")
        .arg(src)
        .output()
        .expect("run ruby binary");
    assert!(
        out.status.success(),
        "program exited non-zero: {src}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn thread_value_returns_block_result() {
    assert_eq!(run("p Thread.new { 6 * 7 }.value"), "42\n");
}

#[test]
fn thread_join_returns_the_thread() {
    assert_eq!(run("t = Thread.new { 1 }; p t.join.equal?(t)"), "true\n");
}

#[test]
fn threads_share_the_object_heap() {
    // A mutation inside the thread is visible to the joiner — one shared heap.
    assert_eq!(run("a = []; Thread.new { a << :x }.join; p a"), "[:x]\n");
}

#[test]
fn gvl_serializes_read_modify_write() {
    // 20 threads each do 1000 non-atomic `+= 1`. Without the GVL making each
    // Ruby step exclusive this loses updates; with it the total is exact.
    let out = run("c = [0]
         ts = (1..20).map { Thread.new { 1000.times { c[0] += 1 } } }
         ts.each(&:join)
         p c[0]");
    assert_eq!(out, "20000\n");
}

#[test]
fn many_threads_each_compute_independently() {
    assert_eq!(
        run("p (1..5).map { |i| Thread.new { i * i } }.map(&:value)"),
        "[1, 4, 9, 16, 25]\n"
    );
}

#[test]
fn thread_exception_propagates_through_value() {
    // The raised object (not just its message) crosses the thread boundary, so
    // the rescue binds it with #class and #message intact.
    assert_eq!(
        run("t = Thread.new { raise ArgumentError, 'boom' }
             begin
               t.value
             rescue => e
               p [e.class, e.message]
             end"),
        "[ArgumentError, \"boom\"]\n"
    );
}

#[test]
fn nested_threads() {
    assert_eq!(
        run("outer = Thread.new { Thread.new { 10 }.value + 5 }; p outer.value"),
        "15\n"
    );
}

#[test]
fn thread_local_variables_do_not_leak_to_the_spawner() {
    // A local defined inside the thread's block is not visible outside it.
    assert_eq!(
        run("x = 1; Thread.new { y = 99 }.join; p defined?(y)"),
        "nil\n"
    );
}

#[test]
fn mutex_synchronize_guards_a_counter() {
    let out = run("m = Mutex.new
         c = [0]
         ts = (1..10).map { Thread.new { 100.times { m.synchronize { c[0] += 1 } } } }
         ts.each(&:join)
         p c[0]");
    assert_eq!(out, "1000\n");
}

#[test]
fn queue_blocks_consumer_until_producer_pushes() {
    // The consumer thread pops an empty queue — it must block (releasing the GVL)
    // until the main thread pushes, then wake and return the item.
    assert_eq!(
        run("q = Queue.new; t = Thread.new { q.pop }; q.push(:hello); p t.value"),
        ":hello\n"
    );
}

#[test]
fn queue_work_sharing_across_threads() {
    // Four workers drain a closed queue of 1..100; every item is handled once, so
    // the totals sum to 5050 regardless of how the work is split.
    let out = run("q = Queue.new
         (1..100).each { |i| q.push(i) }
         q.close
         ts = (1..4).map { Thread.new { s = 0; while (x = q.pop); s += x; end; s } }
         p ts.map(&:value).sum");
    assert_eq!(out, "5050\n");
}

#[test]
fn condition_variable_wait_and_signal() {
    // The waiter parks on the condvar until the main thread flips the predicate
    // and signals; the predicate-loop pattern tolerates spurious wakeups.
    assert_eq!(
        run("m = Mutex.new
             cv = ConditionVariable.new
             ready = [false]
             t = Thread.new { m.synchronize { cv.wait(m) until ready[0]; :done } }
             ready[0] = true
             cv.signal
             p t.value"),
        ":done\n"
    );
}
