//! Running per-function code generation on several threads.
//!
//! Functions are independent given the read-only [`ModuleCtx`], so workers claim
//! them in index order and park each finished piece; the caller's thread hands
//! the pieces to `consume` strictly in index order. The output (and which error
//! surfaces first) is therefore the same as a serial run. Workers run at most
//! [`WINDOW_PER_JOB`] functions ahead of the consumer, which bounds the pieces
//! held in memory.

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
/// How many functions a worker may run ahead of the consumer, per job.
const WINDOW_PER_JOB: usize = 32;
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

/// Blocking state only; the hot path (parking a piece, taking one) touches the
/// per-slot mutexes and atomics in [`Shared`] instead of this lock.
struct Shared {
    /// Ring of `window` slots: function `k` parks in `k % window`. A worker only
    /// produces `k < consumed + window`, so the slot of `k - window` is free.
    slots: Vec<Mutex<Option<Piece>>>,
    /// Functions the consumer has taken so far.
    consumed: AtomicUsize,
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
fn resolve_jobs(requested: usize, live: usize) -> usize {
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
                TranspileError::Unsupported("function slot out of range".to_string())
            })?;
            let input = parts.funcs.get(index).ok_or_else(|| {
                TranspileError::Unsupported(format!("function {index} has no input"))
            })?;
            // Defined functions are named by their full function index, i.e.
            // after the imported functions in the shared index space.
            generate_function_into(base + index, input, ctx, line_prefix, out)
        };

    let jobs = resolve_jobs(parts.jobs, live.len());
    let serial = |consume: &mut F| -> Result<(), TranspileError> {
        let mut scratch = String::new();
        for k in 0..live.len() {
            scratch.clear();
            let meta = generate(k, &mut scratch)?;
            consume(&scratch, meta)?;
        }
        Ok(())
    };
    if jobs <= 1 {
        return serial(&mut consume);
    }

    let window = jobs.saturating_mul(WINDOW_PER_JOB);
    let shared = Shared {
        slots: (0..window).map(|_| Mutex::new(None)).collect(),
        consumed: AtomicUsize::new(0),
        consumer_waiting: AtomicBool::new(false),
        abort: AtomicBool::new(false),
        gate: Mutex::new(()),
        ready: Condvar::new(),
        room: Condvar::new(),
    };
    let next = AtomicUsize::new(0);
    let wake_every = (window / WAKE_DIVISOR).max(1);
    let total = live.len();
    thread::scope(|scope| {
        let worker = || {
            let _guard = AbortOnUnwind(&shared);
            loop {
                let start = next.fetch_add(CLAIM_CHUNK, Ordering::Relaxed);
                if start >= total {
                    return;
                }
                for k in start..start.saturating_add(CLAIM_CHUNK).min(total) {
                    if !wait_for_room(&shared, k, window) {
                        return;
                    }
                    let mut out = String::new();
                    let piece = generate(k, &mut out).map(|meta| (out, meta));
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
            return serial(&mut consume);
        }
        // `consume` runs the caller's code (a split sink); if it unwinds, the
        // throttled workers must be released or the scope would never join.
        let _consumer_guard = AbortOnUnwind(&shared);
        for k in 0..total {
            let Some(piece) = take_piece(&shared, k, wake_every) else {
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

/// Blocks until function `k` is within the window; `false` means aborted.
fn wait_for_room(shared: &Shared, k: usize, window: usize) -> bool {
    let has_room = || {
        k < shared
            .consumed
            .load(Ordering::SeqCst)
            .saturating_add(window)
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
fn take_piece(shared: &Shared, k: usize, wake_every: usize) -> Option<Piece> {
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
    shared.consumed.store(k + 1, Ordering::SeqCst);
    if (k + 1).is_multiple_of(wake_every) {
        shared.pass_gate();
        shared.room.notify_all();
    }
    Some(piece)
}
