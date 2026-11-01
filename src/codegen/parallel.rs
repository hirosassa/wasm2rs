//! Running per-function code generation on several threads.
//!
//! Functions are independent given the read-only [`ModuleCtx`], so workers claim
//! them in index order and park each finished piece; the caller's thread hands
//! the pieces to `consume` strictly in index order. The output (and which error
//! surfaces first) is therefore the same as a serial run. Workers run at most
//! [`WINDOW_PER_JOB`] functions ahead of the consumer, which bounds the pieces
//! held in memory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[derive(Default)]
struct State {
    done: HashMap<usize, Piece>,
    consumed: usize,
    consumer_waiting: bool,
    abort: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a piece lands (wakes the consumer).
    ready: Condvar,
    /// Signalled when the consumer advances or aborts (wakes throttled workers).
    room: Condvar,
}

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.state.lock().unwrap_or_else(PoisonError::into_inner)
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
    lock(shared).abort = true;
    shared.ready.notify_all();
    shared.room.notify_all();
}

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

    let shared = Shared {
        state: Mutex::new(State::default()),
        ready: Condvar::new(),
        room: Condvar::new(),
    };
    let next = AtomicUsize::new(0);
    let window = jobs.saturating_mul(WINDOW_PER_JOB);
    let wake_every = (window / WAKE_DIVISOR).max(1);
    let total = live.len();
    thread::scope(|scope| {
        let worker = || {
            let _guard = AbortOnUnwind(&shared);
            loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= total {
                    return;
                }
                {
                    let mut st = lock(&shared);
                    while !st.abort && k >= st.consumed.saturating_add(window) {
                        st = shared.room.wait(st).unwrap_or_else(PoisonError::into_inner);
                    }
                    if st.abort {
                        return;
                    }
                }
                let mut out = String::new();
                let piece = generate(k, &mut out).map(|meta| (out, meta));
                let wake = {
                    let mut st = lock(&shared);
                    st.done.insert(k, piece);
                    st.consumer_waiting
                };
                if wake {
                    shared.ready.notify_one();
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
        for k in 0..total {
            let piece = {
                let mut st = lock(&shared);
                loop {
                    if let Some(piece) = st.done.remove(&k) {
                        st.consumed = k + 1;
                        if (k + 1) % wake_every == 0 {
                            shared.room.notify_all();
                        }
                        break Some(piece);
                    }
                    if st.abort {
                        break None;
                    }
                    st.consumer_waiting = true;
                    st = shared
                        .ready
                        .wait(st)
                        .unwrap_or_else(PoisonError::into_inner);
                    st.consumer_waiting = false;
                }
            };
            // `None`: a worker panicked; the scope re-raises it on join.
            let Some(piece) = piece else { return Ok(()) };
            if let Err(e) = piece.and_then(|(src, meta)| consume(&src, meta)) {
                abort(&shared);
                return Err(e);
            }
        }
        Ok(())
    })
}
