//! Running per-function code generation on several threads.
//!
//! Functions are independent given the read-only [`ModuleCtx`], so workers claim
//! them in index order and park each finished piece; the caller's thread hands
//! the pieces to `consume` strictly in index order. The output (and which error
//! surfaces first) is therefore the same as a serial run. Workers run at most
//! [`WINDOW_PER_JOB`] functions (and, once the parked pieces exceed
//! [`PARKED_BYTES_PER_JOB`], no further than the next piece the consumer needs)
//! ahead of the consumer, which bounds the pieces held in memory. A wide window
//! absorbs the spread in function cost; the byte budget keeps it from costing
//! memory when functions are large.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::GenMeta;
use super::driver::{ModuleCtx, ModuleParts, generate_function_into};
use crate::TranspileError;

/// Stack of a generation worker: lowering and flattening recurse on control-flow
/// nesting, and (unlike the CLI's own worker) a library caller cannot be asked
/// to provide a big stack. Only address space is reserved up front.
const WORKER_STACK: usize = 256 << 20;
/// How many functions a worker may run ahead of the consumer, per job. Wide on
/// purpose: function cost varies by orders of magnitude, and a narrow window
/// leaves workers idle behind one slow piece (googlesql, 8 jobs: 32 -> 190 ms,
/// 256 -> 126 ms of generation).
const WINDOW_PER_JOB: usize = 256;
/// Generated source a job may have parked, unconsumed, before it is throttled
/// regardless of the window. Keeps the memory in flight proportional to the
/// thread count even when single functions are megabytes. A throttle point, not
/// a hard cap: it is checked before generating, so in-flight pieces add to it.
const PARKED_BYTES_PER_JOB: usize = 16 << 20;
/// Functions claimed per `fetch_add`: with ~40k tiny functions a claim per
/// function makes the shared counter a hot cache line.
const CLAIM_CHUNK: usize = 16;
/// Throttled workers are woken once per this fraction of the window, not per
/// piece, which would make every consumed piece wake every waiting worker.
const WAKE_DIVISOR: usize = 4;
/// With the default `jobs`, modules with fewer live functions stay serial: the
/// threads would cost more than they save.
const AUTO_MIN_FUNCS: usize = 512;
/// The default never exceeds this many threads: beyond it the speed-up is small
/// while every thread adds allocator and memory overhead.
const AUTO_MAX_JOBS: usize = 8;

type Piece = Result<(String, GenMeta), TranspileError>;

/// How far workers may run ahead of the consumer.
#[derive(Clone, Copy)]
struct Limits {
    /// Functions, counted from the first one not yet consumed.
    window: usize,
    /// Parked source bytes above which only the next piece the consumer needs may
    /// still be generated (it can never be withheld, or the run would stall).
    byte_budget: usize,
}

impl Limits {
    fn for_jobs(jobs: usize) -> Self {
        Self {
            window: jobs.saturating_mul(WINDOW_PER_JOB),
            byte_budget: jobs.saturating_mul(PARKED_BYTES_PER_JOB),
        }
    }
}

/// Blocking state only; the hot path (parking a piece, taking one) touches the
/// per-slot mutexes and atomics in [`Shared`] instead of this lock.
struct Shared {
    /// Ring of `window` slots: function `k` parks in `k % window`. A worker only
    /// produces `k < consumed + window`, so the slot of `k - window` is free.
    slots: Vec<Mutex<Option<Piece>>>,
    /// Functions the consumer has taken so far.
    consumed: AtomicUsize,
    /// Source bytes of the pieces parked and not yet taken.
    parked: AtomicUsize,
    consumer_waiting: AtomicBool,
    abort: AtomicBool,
    /// Held only to sleep and wake on the condvars below.
    gate: Mutex<()>,
    /// Signalled when a piece lands (wakes the consumer).
    ready: Condvar,
    /// Signalled when the consumer advances or aborts (wakes throttled workers).
    room: Condvar,
}

impl Shared {
    fn new(window: usize) -> Self {
        Self {
            slots: (0..window).map(|_| Mutex::new(None)).collect(),
            consumed: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            consumer_waiting: AtomicBool::new(false),
            abort: AtomicBool::new(false),
            gate: Mutex::new(()),
            ready: Condvar::new(),
            room: Condvar::new(),
        }
    }

    fn slot(&self, k: usize) -> Option<&Mutex<Option<Piece>>> {
        self.slots.get(k.checked_rem(self.slots.len())?)
    }

    fn gate(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes and releases the gate before a notify, so the stores preceding it
    /// are ordered before any sleeper's re-check and the wake-up is not lost.
    fn pass_gate(&self) {
        drop(self.gate());
    }
}

fn lock_slot(slot: &Mutex<Option<Piece>>) -> MutexGuard<'_, Option<Piece>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops the others if a worker unwinds, so the consumer never waits for a piece
/// that will not come (the scope then re-raises the panic).
struct AbortOnUnwind<'a>(&'a Shared);
impl Drop for AbortOnUnwind<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            abort(self.0);
        }
    }
}

fn abort(shared: &Shared) {
    shared.abort.store(true, Ordering::SeqCst);
    shared.pass_gate();
    shared.ready.notify_all();
    shared.room.notify_all();
}

/// Consumed pieces between wake-ups of throttled workers.
fn wake_interval(window: usize) -> usize {
    (window / WAKE_DIVISOR).max(1)
}

/// The worker count for `requested` jobs (`0` = auto) over `live` functions.
pub(super) fn resolve_jobs(requested: usize, live: usize) -> usize {
    let jobs = if requested == 0 {
        if live < AUTO_MIN_FUNCS {
            1
        } else {
            thread::available_parallelism().map_or(1, |n| n.get().min(AUTO_MAX_JOBS))
        }
    } else {
        requested
    };
    jobs.min(live)
}

/// Generate every live function and pass each one's source (prefixed by
/// `line_prefix`) and dependencies to `consume`, in function-index order.
pub(super) fn for_each_function<F>(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    line_prefix: &str,
    mut consume: F,
) -> Result<(), TranspileError>
where
    F: FnMut(&str, GenMeta) -> Result<(), TranspileError>,
{
    let base = parts.imports.len();
    let live: Vec<usize> = (0..parts.funcs.len())
        .filter(|&i| parts.live.get(i).copied().unwrap_or(false))
        .collect();
    let generate =
        |k: usize, out: &mut String| -> Result<GenMeta, TranspileError> {
            let index = live.get(k).copied().ok_or_else(|| {
                TranspileError::Internal("function slot out of range".to_string())
            })?;
            let input = parts.funcs.get(index).ok_or_else(|| {
                TranspileError::Internal(format!("function {index} has no input"))
            })?;
            // Defined functions are named by their full function index, i.e.
            // after the imported functions in the shared index space.
            let func_index = base + index;
            generate_function_into(func_index, input, ctx, line_prefix, out).map_err(|e| {
                TranspileError::InFunction {
                    index: func_index,
                    source: Box::new(e),
                }
            })
        };

    let jobs = resolve_jobs(parts.opts.jobs, live.len());
    run_ordered(
        live.len(),
        jobs,
        Limits::for_jobs(jobs),
        &generate,
        &mut consume,
    )
}

/// Runs `generate` for `0..total` on up to `jobs` threads and feeds the results
/// to `consume` in index order, stopping at the first error in that order.
fn run_ordered<G, F>(
    total: usize,
    jobs: usize,
    limits: Limits,
    generate: &G,
    consume: &mut F,
) -> Result<(), TranspileError>
where
    G: Fn(usize, &mut String) -> Result<GenMeta, TranspileError> + Sync,
    F: FnMut(&str, GenMeta) -> Result<(), TranspileError>,
{
    let serial = |consume: &mut F| -> Result<(), TranspileError> {
        let mut scratch = String::new();
        for k in 0..total {
            scratch.clear();
            let meta = generate(k, &mut scratch)?;
            consume(&scratch, meta)?;
        }
        Ok(())
    };
    if jobs <= 1 {
        return serial(consume);
    }

    let Limits {
        window,
        byte_budget,
    } = limits;
    let shared = Shared::new(window);
    let next = AtomicUsize::new(0);
    let wake_every = wake_interval(window);
    thread::scope(|scope| {
        let worker = || {
            let _guard = AbortOnUnwind(&shared);
            // Reused across functions: generation grows it to the largest
            // function once, instead of regrowing a fresh buffer per function.
            // Each piece is then an exact-size copy.
            let mut scratch = String::new();
            loop {
                let start = next.fetch_add(CLAIM_CHUNK, Ordering::Relaxed);
                if start >= total {
                    return;
                }
                for k in start..start.saturating_add(CLAIM_CHUNK).min(total) {
                    if !wait_for_room(&shared, k, window, byte_budget) {
                        return;
                    }
                    scratch.clear();
                    let piece = generate(k, &mut scratch)
                        .map(|meta| (String::from(scratch.as_str()), meta));
                    if let Ok((src, _)) = &piece {
                        shared.parked.fetch_add(src.len(), Ordering::SeqCst);
                    }
                    if let Some(slot) = shared.slot(k) {
                        *lock_slot(slot) = Some(piece);
                    }
                    // Pairs with the fence in the consumer: either it sees the
                    // piece, or this sees `consumer_waiting` and wakes it.
                    fence(Ordering::SeqCst);
                    if shared.consumer_waiting.load(Ordering::SeqCst) {
                        shared.pass_gate();
                        shared.ready.notify_one();
                    }
                }
            }
        };
        let mut spawned = 0usize;
        for _ in 0..jobs {
            if thread::Builder::new()
                .stack_size(WORKER_STACK)
                .spawn_scoped(scope, worker)
                .is_ok()
            {
                spawned += 1;
            }
        }
        if spawned == 0 {
            return serial(consume);
        }
        // `consume` runs the caller's code (a split sink); if it unwinds, the
        // throttled workers must be released or the scope would never join.
        let _consumer_guard = AbortOnUnwind(&shared);
        for k in 0..total {
            let Some(piece) = take_piece(&shared, k, wake_every, byte_budget) else {
                // A worker panicked; the scope re-raises it on join.
                return Ok(());
            };
            if let Err(e) = piece.and_then(|(src, meta)| consume(&src, meta)) {
                abort(&shared);
                return Err(e);
            }
        }
        Ok(())
    })
}

/// Blocks until function `k` is within the window and, unless it is the very
/// next piece the consumer needs, the parked bytes are under budget; `false`
/// means aborted.
fn wait_for_room(shared: &Shared, k: usize, window: usize, byte_budget: usize) -> bool {
    let has_room = || {
        let consumed = shared.consumed.load(Ordering::SeqCst);
        k < consumed.saturating_add(window)
            && (k <= consumed || shared.parked.load(Ordering::SeqCst) < byte_budget)
    };
    if !has_room() {
        let _gate = shared
            .room
            .wait_while(shared.gate(), |()| {
                !shared.abort.load(Ordering::SeqCst) && !has_room()
            })
            .unwrap_or_else(PoisonError::into_inner);
    }
    !shared.abort.load(Ordering::SeqCst)
}

/// Takes function `k`'s piece, sleeping until it lands; `None` means aborted.
fn take_piece(shared: &Shared, k: usize, wake_every: usize, byte_budget: usize) -> Option<Piece> {
    let slot = shared.slot(k)?;
    let try_take = || lock_slot(slot).take();
    let piece = loop {
        if let Some(piece) = try_take() {
            break piece;
        }
        let mut gate = shared.gate();
        shared.consumer_waiting.store(true, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        // Re-check after announcing the wait, under the gate, so a worker that
        // parked just before cannot be missed.
        let found = try_take();
        if found.is_none() && !shared.abort.load(Ordering::SeqCst) {
            gate = shared
                .ready
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        shared.consumer_waiting.store(false, Ordering::SeqCst);
        drop(gate);
        if let Some(piece) = found {
            break piece;
        }
        if shared.abort.load(Ordering::SeqCst) {
            // One last look: a piece may have landed alongside the abort.
            return try_take();
        }
    };
    let freed = piece.as_ref().map_or(0, |(src, _)| src.len());
    let parked_before = shared.parked.fetch_sub(freed, Ordering::SeqCst);
    shared.consumed.store(k + 1, Ordering::SeqCst);
    // While over budget a worker may be waiting for exactly the piece that just
    // became the next one, so every step must wake it; otherwise once a quarter
    // window has gone by.
    if parked_before >= byte_budget || (k + 1).is_multiple_of(wake_every) {
        shared.pass_gate();
        shared.room.notify_all();
    }
    Some(piece)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    fn meta() -> GenMeta {
        GenMeta {
            helpers: HashSet::new(),
            rt: HashSet::new(),
            simd: HashSet::new(),
            dispatch_sigs: HashSet::new(),
            uses_eh: false,
            state_structs: Vec::new(),
        }
    }

    /// Function `k` yields `k` repeated `1 + k % 7` times: sizes differ so a
    /// reordering or a reused buffer leaking into a piece shows up.
    fn body(k: usize) -> String {
        format!("f{k};").repeat(1 + k % 7)
    }

    fn write_body(k: usize, out: &mut String) -> Result<GenMeta, TranspileError> {
        out.push_str(&body(k));
        Ok(meta())
    }

    /// How long a run that must finish may take. A lost wake-up leaves the consumer and
    /// the workers asleep for good, and nothing inside the run can fail it.
    const HANG: Duration = Duration::from_secs(15);

    /// Ends the test process if the returned sender is still alive [`HANG`] after it was
    /// armed (a wall-clock limit, not a progress check): a deadlocked run would otherwise
    /// stall the whole suite (and a `cargo mutants` run) instead of failing. Dropping the
    /// sender disarms it.
    fn arm_watchdog(what: &'static str) -> mpsc::Sender<()> {
        let (disarm, disarmed) = mpsc::channel::<()>();
        thread::spawn(move || {
            if disarmed.recv_timeout(HANG) == Err(RecvTimeoutError::Timeout) {
                eprintln!("{what}: did not finish within {HANG:?}");
                std::process::exit(1);
            }
        });
        disarm
    }

    fn collect<G>(total: usize, jobs: usize, limits: Limits, generate: G) -> Vec<String>
    where
        G: Fn(usize, &mut String) -> Result<GenMeta, TranspileError> + Sync,
    {
        let _watchdog = arm_watchdog("collect");
        let mut got = Vec::new();
        run_ordered(total, jobs, limits, &generate, &mut |src, _| {
            got.push(src.to_string());
            Ok(())
        })
        .unwrap();
        got
    }

    fn expected(total: usize) -> Vec<String> {
        (0..total).map(body).collect()
    }

    #[test]
    fn pieces_arrive_in_index_order_under_every_limit() {
        for (window, byte_budget) in [(256, usize::MAX), (4, usize::MAX), (256, 1), (1, 1)] {
            for jobs in [1, 2, 8] {
                let limits = Limits {
                    window,
                    byte_budget,
                };
                assert_eq!(
                    collect(300, jobs, limits, write_body),
                    expected(300),
                    "{window}/{byte_budget}/{jobs}"
                );
            }
        }
    }

    #[test]
    fn a_slow_head_function_does_not_change_the_output() {
        let limits = Limits {
            window: 8,
            byte_budget: 64,
        };
        let got = collect(200, 4, limits, |k, out: &mut String| {
            if k == 0 {
                thread::sleep(Duration::from_millis(30));
            }
            write_body(k, out)
        });
        assert_eq!(got, expected(200));
    }

    #[test]
    fn the_lowest_failing_index_surfaces_first() {
        let _watchdog = arm_watchdog("the_lowest_failing_index_surfaces_first");
        let result = run_ordered(
            100,
            8,
            Limits {
                window: 16,
                byte_budget: 32,
            },
            &|k, out: &mut String| {
                if k == 40 || k == 70 {
                    return Err(TranspileError::Internal(k.to_string()));
                }
                write_body(k, out)
            },
            &mut |_, _| Ok(()),
        );
        assert!(matches!(result, Err(TranspileError::Internal(m)) if m == "40"));
    }

    #[test]
    fn a_consumer_error_stops_the_run() {
        let _watchdog = arm_watchdog("a_consumer_error_stops_the_run");
        let mut seen = 0usize;
        let result = run_ordered(
            100,
            4,
            Limits {
                window: 16,
                byte_budget: 32,
            },
            &write_body,
            &mut |_, _| {
                seen += 1;
                if seen == 10 {
                    Err(TranspileError::Internal("stop".into()))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(TranspileError::Internal(m)) if m == "stop"));
        assert_eq!(seen, 10);
    }

    #[test]
    fn the_byte_budget_bounds_how_far_workers_run_ahead() {
        let _watchdog = arm_watchdog("the_byte_budget_bounds_how_far_workers_run_ahead");
        let generated = AtomicUsize::new(0);
        let mut consumed = 0usize;
        let mut max_ahead = 0usize;
        let jobs = 4;
        run_ordered(
            200,
            jobs,
            Limits {
                window: 1024,
                byte_budget: 100,
            },
            &|_, out: &mut String| {
                out.push_str(&"x".repeat(60));
                generated.fetch_add(1, Ordering::SeqCst);
                Ok(meta())
            },
            &mut |_, _| {
                consumed += 1;
                // Slow consumer: unthrottled workers would race far ahead.
                thread::sleep(Duration::from_millis(1));
                max_ahead = max_ahead.max(generated.load(Ordering::SeqCst) - consumed);
                Ok(())
            },
        )
        .unwrap();
        // Budget fits two pieces; each worker may add one it was already past
        // the check for, plus the exempt next-to-consume piece.
        assert!(max_ahead <= 2 + jobs + 1, "ran {max_ahead} ahead");
    }

    /// Long enough for a thread that can proceed to be scheduled, even on a loaded
    /// runner; the polls below return as soon as it has.
    const PROMPT: Duration = Duration::from_secs(5);
    /// How long a thread that must stay blocked is watched. A too-short wait can only
    /// miss a bug, never fail a correct build.
    const BLOCKED: Duration = Duration::from_millis(150);

    fn poll(done: &AtomicBool, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        done.load(Ordering::SeqCst)
    }

    /// Runs `f` on a thread and reports `(returned within grace, its answer)`. A call
    /// still blocked after `grace` is released by aborting `shared`.
    fn run_or_abort<T: Send>(
        shared: &Shared,
        grace: Duration,
        f: impl FnOnce() -> T + Send,
    ) -> (bool, T) {
        // Aborting `shared` is meant to release `f`; if it does not (the abort check
        // was lost), the join below would wait for ever.
        let _watchdog = arm_watchdog("run_or_abort");
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let answer = f();
                done.store(true, Ordering::SeqCst);
                answer
            });
            let finished = poll(&done, grace);
            if !finished {
                abort(shared);
            }
            (finished, handle.join().unwrap())
        })
    }

    fn wait_outcome(shared: &Shared, k: usize, budget: usize, grace: Duration) -> (bool, bool) {
        run_or_abort(shared, grace, || wait_for_room(shared, k, WINDOW, budget))
    }

    /// The window of every `Shared` these tests build.
    const WINDOW: usize = 4;

    fn proceeds(shared: &Shared, k: usize, budget: usize) -> bool {
        wait_outcome(shared, k, budget, PROMPT) == (true, true)
    }

    fn blocks(shared: &Shared, k: usize, budget: usize) -> bool {
        // Released by the abort that ends the grace period.
        wait_outcome(shared, k, budget, BLOCKED) == (false, false)
    }

    /// A shared state whose consumer is at function 3 with `parked` bytes parked.
    fn mid_run(parked: usize) -> Shared {
        let shared = Shared::new(WINDOW);
        shared.consumed.store(3, Ordering::SeqCst);
        shared.parked.store(parked, Ordering::SeqCst);
        shared
    }

    #[test]
    fn a_worker_waits_for_the_window_edge() {
        // 3 + 4 = 7 is the first function outside the window.
        assert!(
            proceeds(&mid_run(0), 6, 100),
            "last function inside the window"
        );
        assert!(
            blocks(&mid_run(0), 7, 100),
            "first function outside the window"
        );
        assert!(blocks(&mid_run(0), 9, 100), "well outside the window");
    }

    #[test]
    fn the_byte_budget_throttles_everything_but_the_next_piece() {
        // Under budget: a function ahead of the consumer may run.
        assert!(proceeds(&mid_run(99), 4, 100));
        // At the budget it may not (the limit is exclusive)...
        assert!(blocks(&mid_run(100), 4, 100));
        assert!(blocks(&mid_run(101), 4, 100));
        // ...except the piece the consumer needs next, or the run would stall.
        assert!(proceeds(&mid_run(100), 3, 100));
        assert!(proceeds(&mid_run(1_000), 3, 100));
    }

    #[test]
    fn a_throttled_worker_resumes_when_the_consumer_advances() {
        let _watchdog = arm_watchdog("a_throttled_worker_resumes_when_the_consumer_advances");
        let shared = mid_run(0);
        let answer = thread::scope(|scope| {
            let handle = scope.spawn(|| wait_for_room(&shared, 7, WINDOW, 100));
            thread::sleep(Duration::from_millis(20));
            shared.consumed.store(4, Ordering::SeqCst);
            shared.pass_gate();
            shared.room.notify_all();
            handle.join().unwrap()
        });
        assert!(answer);
    }

    #[test]
    fn an_abort_releases_a_throttled_worker_with_false() {
        let shared = mid_run(0);
        shared.abort.store(true, Ordering::SeqCst);
        // Even with room, an aborted run tells the worker to stop...
        assert_eq!(wait_outcome(&shared, 4, 100, PROMPT), (true, false));
        // ...and without room it does not wait.
        assert_eq!(wait_outcome(&shared, 7, 100, PROMPT), (true, false));
    }

    #[test]
    fn pass_gate_takes_the_gate() {
        let shared = Shared::new(1);
        let held = shared.gate();
        let passed = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                shared.pass_gate();
                passed.store(true, Ordering::SeqCst);
            });
            // A sleeper's re-check runs under the gate, so a notify may only follow
            // once the gate has been free.
            assert!(
                !poll(&passed, BLOCKED),
                "pass_gate returned while the gate was held"
            );
            drop(held);
            assert!(poll(&passed, PROMPT));
        });
    }

    fn parked_piece(shared: &Shared, k: usize, src: &str) {
        let slot = shared.slot(k).unwrap();
        *lock_slot(slot) = Some(Ok((src.to_string(), meta())));
        shared.parked.fetch_add(src.len(), Ordering::SeqCst);
    }

    #[test]
    fn taking_a_piece_advances_the_consumer_and_frees_its_bytes() {
        let shared = Shared::new(WINDOW);
        shared.consumed.store(5, Ordering::SeqCst);
        // Function 5 parks in slot 5 % 4.
        parked_piece(&shared, 5, "abcd");
        parked_piece(&shared, 6, "xy");
        let (src, _) = take_piece(&shared, 5, 1_000, usize::MAX).unwrap().unwrap();
        assert_eq!(src, "abcd");
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 6);
        assert_eq!(shared.parked.load(Ordering::SeqCst), 2);
        assert!(
            lock_slot(shared.slot(5).unwrap()).is_none(),
            "the slot is emptied"
        );
    }

    #[test]
    fn taking_a_failed_piece_frees_no_bytes() {
        let shared = Shared::new(WINDOW);
        shared.parked.store(7, Ordering::SeqCst);
        *lock_slot(shared.slot(0).unwrap()) = Some(Err(TranspileError::Internal("boom".into())));
        let piece = take_piece(&shared, 0, 1_000, usize::MAX).unwrap();
        assert!(matches!(piece, Err(TranspileError::Internal(m)) if m == "boom"));
        assert_eq!(shared.parked.load(Ordering::SeqCst), 7);
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 1);
    }

    fn take_outcome(shared: &Shared, k: usize, grace: Duration) -> (bool, Option<Piece>) {
        run_or_abort(shared, grace, || take_piece(shared, k, 1_000, usize::MAX))
    }

    #[test]
    fn an_aborted_consumer_does_not_wait_for_a_piece() {
        let shared = Shared::new(WINDOW);
        shared.abort.store(true, Ordering::SeqCst);
        let (finished, piece) = take_outcome(&shared, 0, PROMPT);
        assert!(finished, "take_piece slept although the run was aborted");
        assert!(piece.is_none());
    }

    #[test]
    fn a_consumer_waits_for_its_piece_until_one_lands() {
        let _watchdog = arm_watchdog("a_consumer_waits_for_its_piece_until_one_lands");
        let shared = Shared::new(WINDOW);
        let (finished, piece) = take_outcome(&shared, 0, BLOCKED);
        assert!(!finished, "take_piece returned without a piece");
        assert!(piece.is_none(), "an abort ends the wait with no piece");

        let shared = Shared::new(WINDOW);
        let piece = thread::scope(|scope| {
            let handle = scope.spawn(|| take_piece(&shared, 0, 1_000, usize::MAX));
            thread::sleep(Duration::from_millis(20));
            parked_piece(&shared, 0, "late");
            shared.pass_gate();
            shared.ready.notify_one();
            handle.join().unwrap()
        });
        assert_eq!(piece.unwrap().unwrap().0, "late");
    }

    /// Takes function `k` (a piece of `len` bytes) while a worker, which has no room
    /// until the consumer advances, is throttled; reports whether the take woke it.
    fn take_wakes_throttled_worker(
        k: usize,
        len: usize,
        wake_every: usize,
        budget: usize,
        expect_wake: bool,
    ) -> bool {
        let _watchdog = arm_watchdog("take_wakes_throttled_worker");
        let shared = Shared::new(WINDOW);
        shared.consumed.store(k, Ordering::SeqCst);
        parked_piece(&shared, k, &"x".repeat(len));
        // Function k + WINDOW is outside the window until k has been taken.
        let worker_k = k + WINDOW;
        // A wake-up is awaited until it comes; its absence is watched for briefly.
        let grace = if expect_wake { PROMPT } else { BLOCKED };
        let started = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            let handle = scope.spawn(|| {
                started.store(true, Ordering::SeqCst);
                wait_for_room(&shared, worker_k, WINDOW, budget);
                done.store(true, Ordering::SeqCst);
            });
            // Only once the worker runs can the take be told apart from its start-up.
            assert!(poll(&started, PROMPT));
            thread::sleep(Duration::from_millis(20));
            assert!(
                !done.load(Ordering::SeqCst),
                "the worker had room before the take"
            );
            take_piece(&shared, k, wake_every, budget).unwrap().unwrap();
            let woken = poll(&done, grace);
            abort(&shared);
            handle.join().unwrap();
            woken
        })
    }

    #[test]
    fn the_consumer_wakes_throttled_workers_every_quarter_window() {
        // Taking function 1 completes the second of every two (`wake_every` = 2).
        assert!(take_wakes_throttled_worker(1, 1, 2, 1_000, true));
    }

    #[test]
    fn the_consumer_wakes_throttled_workers_at_every_step_over_budget() {
        // Parked bytes equal to the budget count as over it, whatever the step.
        assert!(take_wakes_throttled_worker(0, 3, 1_000, 3, true));
    }

    #[test]
    fn the_consumer_stays_quiet_between_wake_points_under_budget() {
        assert!(!take_wakes_throttled_worker(0, 3, 1_000, 1_000, false));
    }

    #[test]
    fn wake_points_are_a_quarter_of_the_window_and_never_zero() {
        assert_eq!(wake_interval(256), 64);
        assert_eq!(wake_interval(8), 2);
        assert_eq!(wake_interval(7), 1);
        assert_eq!(wake_interval(1), 1);
        assert_eq!(wake_interval(0), 1);
    }

    #[test]
    fn limits_scale_with_the_job_count() {
        // The documented per-job figures, not whatever the constants happen to hold.
        let one = Limits::for_jobs(1);
        assert_eq!(one.window, 256);
        assert_eq!(one.byte_budget, 16 * 1024 * 1024);
        let four = Limits::for_jobs(4);
        assert_eq!(four.window, 4 * one.window);
        assert_eq!(four.byte_budget, 4 * one.byte_budget);
        // Saturates instead of wrapping.
        assert_eq!(Limits::for_jobs(usize::MAX).window, usize::MAX);
    }

    #[test]
    fn explicit_jobs_are_capped_only_by_the_live_function_count() {
        assert_eq!(resolve_jobs(3, 1_000), 3);
        assert_eq!(
            resolve_jobs(64, 1_000),
            64,
            "no auto cap on an explicit count"
        );
        assert_eq!(resolve_jobs(8, 3), 3);
        assert_eq!(resolve_jobs(1, 1_000), 1);
        assert_eq!(resolve_jobs(5, 0), 0);
    }

    #[test]
    fn auto_jobs_stay_serial_below_the_threshold_and_cap_at_eight() {
        let auto = thread::available_parallelism().map_or(1, |n| n.get().min(AUTO_MAX_JOBS));
        assert_eq!(resolve_jobs(0, 0), 0);
        assert_eq!(resolve_jobs(0, 1), 1);
        assert_eq!(resolve_jobs(0, AUTO_MIN_FUNCS - 1), 1);
        assert_eq!(resolve_jobs(0, AUTO_MIN_FUNCS), auto);
        assert_eq!(resolve_jobs(0, 100_000), auto);
        assert!(auto <= AUTO_MAX_JOBS);
    }

    #[test]
    fn one_job_generates_on_the_callers_thread_and_more_on_workers() {
        let caller = thread::current().id();
        for (jobs, on_caller) in [(1, true), (2, false), (4, false)] {
            let seen = Mutex::new(Vec::new());
            let got = collect(40, jobs, Limits::for_jobs(jobs), |k, out: &mut String| {
                seen.lock().unwrap().push(thread::current().id());
                write_body(k, out)
            });
            assert_eq!(got, expected(40));
            let ids = seen.into_inner().unwrap();
            assert_eq!(ids.len(), 40);
            assert!(
                ids.iter().all(|id| (*id == caller) == on_caller),
                "jobs = {jobs}"
            );
        }
    }

    /// Recurses through ~8 MiB of frames: far beyond a default thread stack's 2 MiB
    /// but well inside `WORKER_STACK`.
    fn burn_stack(depth: usize) -> usize {
        let frame = [u8::try_from(depth % 251).unwrap(); 64 * 1024];
        let below = if depth == 0 { 0 } else { burn_stack(depth - 1) };
        usize::from(std::hint::black_box(&frame)[0]) + below
    }

    #[test]
    fn workers_have_room_for_deep_recursion() {
        let got = collect(4, 2, Limits::for_jobs(2), |k, out: &mut String| {
            out.push_str(&burn_stack(128 + k).to_string());
            Ok(meta())
        });
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn a_failing_function_is_named_by_its_index_after_the_imports() {
        // Two imported functions, then two defined ones of which the second fails:
        // its index in the shared index space is 2 + 1.
        let wasm = wat::parse_str(
            r#"(module
                (import "env" "a" (func))
                (import "env" "b" (func))
                (tag $e (param v128))
                (func (export "ok"))
                (func (export "bad")
                  v128.const i32x4 0 0 0 0
                  throw $e))"#,
        )
        .unwrap();
        let err = crate::transpile(&wasm).unwrap_err();
        assert!(
            matches!(&err, TranspileError::InFunction { index: 3, .. }),
            "{err}"
        );
    }
}
