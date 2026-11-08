//! `wasi:clocks` on the typed host (monotonic `now`, `subscribe-instant`, wall-clock `now`)
//! and how `poll` and `block` wait for deadlines, with the imports declared in WIT text.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const IO: &str = r#"
package wasi:io@0.2.0;
interface poll {
  resource pollable {
    block: func();
    ready: func() -> bool;
  }
  poll: func(in: list<borrow<pollable>>) -> list<u32>;
}
"#;

const CLOCKS: &str = r#"
package wasi:clocks@0.2.0;
interface monotonic-clock {
  use wasi:io/poll@0.2.0.{pollable};
  now: func() -> u64;
  resolution: func() -> u64;
  subscribe-instant: func(when: u64) -> pollable;
  subscribe-duration: func(when: u64) -> pollable;
}
interface wall-clock {
  record datetime { seconds: u64, nanoseconds: u32 }
  now: func() -> datetime;
  resolution: func() -> datetime;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:clocks/monotonic-clock@0.2.0;
  import wasi:clocks/wall-clock@0.2.0;
  import wasi:io/poll@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $now (result i64)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-instant" (func $at (param i64) (result i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $dur (param i64) (result i32)))
  (import "wasi:clocks/wall-clock@0.2.0" "now" (func $wall (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.ready" (func $ready (param i32) (result i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "resolution" (func $mono_res (result i64)))
  (import "wasi:clocks/wall-clock@0.2.0" "resolution" (func $wall_res (param i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "now") (result i64) (call $now))
  ;; whether a timer `ns` nanoseconds away is ready, without waiting for it
  (func (export "ready_after") (param $ns i64) (result i32) (call $ready (call $dur (local.get $ns))))
  ;; `ready` on one timer before and after blocking on it: before times ten, plus after
  (func (export "ready_before_after") (param $ns i64) (result i32) (local $p i32)
    (local.set $p (call $dur (local.get $ns)))
    (i32.add (i32.mul (call $ready (local.get $p)) (i32.const 10))
      (block (result i32) (call $block (local.get $p)) (call $ready (local.get $p)))))
  (func (export "mono_resolution") (result i64) (call $mono_res))
  (func (export "wall_res_secs") (result i64) (call $wall_res (i32.const 136)) (i64.load (i32.const 136)))
  (func (export "wall_res_nanos") (result i32) (call $wall_res (i32.const 136)) (i32.load (i32.const 144)))
  ;; waits on a timer `ns` nanoseconds away
  (func (export "block_for") (param $ns i64) (call $block (call $dur (local.get $ns))))
  ;; waits on the instant `at` (on the monotonic clock)
  (func (export "block_at") (param $at i64) (call $block (call $at (local.get $at))))
  ;; polls a ten-second timer and the instant `at`: the number of ready pollables times
  ;; ten, plus the first ready index
  (func (export "poll_at") (param $at i64) (result i32)
    (i32.store (i32.const 96) (call $dur (i64.const 10000000000)))
    (i32.store (i32.const 100) (call $at (local.get $at)))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (i32.add (i32.mul (i32.load (i32.const 108)) (i32.const 10))
             (i32.load (i32.load (i32.const 104)))))
  ;; polls [a timer `ns` away from now, a 30 ms timer] (or [the instant `ns`, a 30 ms timer]
  ;; when `by_instant` is set): the number of ready pollables times ten, plus the first ready
  ;; index
  (func (export "poll_far_and_soon") (param $ns i64) (param $by_instant i32) (result i32)
    (i32.store (i32.const 160)
      (if (result i32) (local.get $by_instant)
        (then (call $at (local.get $ns)))
        (else (call $dur (local.get $ns)))))
    (i32.store (i32.const 164) (call $dur (i64.const 30000000)))
    (call $poll (i32.const 160) (i32.const 2) (i32.const 168))
    (i32.add (i32.mul (i32.load (i32.const 172)) (i32.const 10))
             (i32.load (i32.load (i32.const 168)))))
  ;; the datetime record: seconds (u64) at 120, nanoseconds (u32) at 128
  (func (export "wall_secs") (result i64) (call $wall (i32.const 120)) (i64.load (i32.const 120)))
  (func (export "wall_nanos") (result i32) (call $wall (i32.const 120)) (i32.load (i32.const 128))))
"#;

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, CLOCKS], WORLD, "w"),
        CORE,
        "TypedHost::new(Vec::new())",
        body,
    )
}

#[test]
fn the_monotonic_clock_starts_near_zero_and_does_not_go_back() {
    assert_succeeded(&run(
        "let a = inst.now() as u64; std::thread::sleep(std::time::Duration::from_millis(20)); \
         let b = inst.now() as u64; \
         assert!(a < 10_000_000_000, \"{a}\"); \
         assert!(b >= a + 20_000_000, \"{a} {b}\");",
    ));
}

#[test]
fn block_waits_for_a_duration_and_for_an_instant_ahead() {
    assert_succeeded(&run(
        "let start = std::time::Instant::now(); inst.block_for(40_000_000); \
         assert!(start.elapsed() >= std::time::Duration::from_millis(40)); \
         let at = inst.now() as u64 + 30_000_000; inst.block_at(at as i64); \
         assert!(inst.now() as u64 >= at);",
    ));
}

#[test]
fn an_instant_in_the_past_is_ready_at_once() {
    // read as a duration, an instant this large would wait that long
    assert_succeeded(&run(
        "std::thread::sleep(std::time::Duration::from_millis(60)); let past = inst.now(); \
         let start = std::time::Instant::now(); inst.block_at(past); \
         assert!(start.elapsed() < std::time::Duration::from_millis(30));",
    ));
}

#[test]
fn poll_returns_the_ready_instant_without_waiting_for_a_far_deadline() {
    // [far timer, instant now]: only index 1 is ready
    assert_succeeded(&run(
        "let start = std::time::Instant::now(); let now = inst.now(); \
         assert_eq!(inst.poll_at(now), 11); \
         assert!(start.elapsed() < std::time::Duration::from_secs(9));",
    ));
}

#[test]
fn poll_waits_for_an_instant_ahead() {
    assert_succeeded(&run(
        "let start = std::time::Instant::now(); let at = inst.now() + 50_000_000; \
         assert_eq!(inst.poll_at(at), 11); \
         assert!(start.elapsed() >= std::time::Duration::from_millis(50));",
    ));
}

#[test]
fn a_deadline_too_far_to_represent_never_arrives_and_does_not_panic() {
    // u64::MAX nanoseconds (about 584 years) is a deadline that never arrives: the 30 ms
    // timer is the only one ready. `Instant` does not document its range, so a platform may
    // overflow here (`deadline_after` guards that); the common ones do not, so this checks
    // the far deadline stays pending rather than the overflow itself
    assert_succeeded(&run("assert_eq!(inst.poll_far_and_soon(-1, 0), 11); \
         assert_eq!(inst.poll_far_and_soon(-1, 1), 11);"));
}

#[test]
fn the_wall_clock_reports_the_time_since_the_unix_epoch() {
    assert_succeeded(&run(
        "let secs = inst.wall_secs() as u64; let nanos = inst.wall_nanos() as u32; \
         let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(); \
         assert!(secs <= now && now - secs < 60, \"{secs} {now}\"); \
         assert!(nanos < 1_000_000_000, \"{nanos}\");",
    ));
}

#[test]
fn ready_reports_a_due_timer_and_not_a_far_one_without_waiting() {
    assert_succeeded(&run("let start = std::time::Instant::now(); \
         assert_eq!(inst.ready_after(0), 1); \
         assert_eq!(inst.ready_after(10_000_000_000), 0); \
         assert!(start.elapsed() < std::time::Duration::from_secs(9));"));
}

#[test]
fn ready_turns_true_on_the_same_timer_once_it_has_passed() {
    // not ready right after the subscription (tens digit 0), ready once blocked on (units 1)
    assert_succeeded(&run("assert_eq!(inst.ready_before_after(30_000_000), 1);"));
}

#[test]
fn the_clock_resolutions_match_the_precision_of_now() {
    // `now` counts nanoseconds from an `Instant`, and the wall clock reads nanoseconds
    assert_succeeded(&run("assert_eq!(inst.mono_resolution(), 1); \
         assert_eq!(inst.wall_res_secs(), 0); \
         assert_eq!(inst.wall_res_nanos(), 1);"));
}
