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
    let shared = Shared {
        slots: (0..window).map(|_| Mutex::new(None)).collect(),
        consumed: AtomicUsize::new(0),
        parked: AtomicUsize::new(0),
        consumer_waiting: AtomicBool::new(false),
        abort: AtomicBool::new(false),
        gate: Mutex::new(()),
        ready: Condvar::new(),
        room: Condvar::new(),
    };
    let next = AtomicUsize::new(0);
    let wake_every = (window / WAKE_DIVISOR).max(1);
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
    use std::time::Duration;

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

    fn collect<G>(total: usize, jobs: usize, limits: Limits, generate: G) -> Vec<String>
    where
        G: Fn(usize, &mut String) -> Result<GenMeta, TranspileError> + Sync,
    {
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
}
