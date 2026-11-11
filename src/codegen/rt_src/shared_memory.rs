#[derive(Clone)]
pub struct SharedMemory(std::sync::Arc<SharedMemInner>);

struct SharedMemInner {
    // `cap` zeroed bytes reserved up front (lazily backed by the OS) and never
    // reallocated (at least one byte is allocated, so `cap == 0` is fine); `len` is the current size, raised by `memory.grow`.
    base: *mut u8,
    cap: usize,
    len: std::sync::atomic::AtomicUsize,
    park: std::sync::Mutex<SharedPark>,
    cvar: std::sync::Condvar,
}

// SAFETY: `base` points at a heap block owned by this value and never moved or
// freed before drop; concurrent access to it is the shared-memory contract above.
unsafe impl Send for SharedMemInner {}
unsafe impl Sync for SharedMemInner {}

impl Drop for SharedMemInner {
    fn drop(&mut self) {
        // SAFETY: `base` was allocated in `with_max` with exactly this layout.
        unsafe { std::alloc::dealloc(self.base, shared_layout(self.cap)) }
    }
}

#[derive(Default)]
struct SharedPark {
    // addr -> FIFO of tickets of the waiters still parked there
    queues: std::collections::HashMap<u32, std::collections::VecDeque<u64>>,
    // tickets a `notify` has chosen to wake but whose waiter has not yet resumed
    woken: std::collections::HashSet<u64>,
    next_ticket: u64,
}

fn shared_layout(cap: usize) -> std::alloc::Layout {
    std::alloc::Layout::array::<u8>(cap.max(1)).unwrap()
}

fn shared_width_mask(width: usize) -> u64 {
    if width >= 8 { u64::MAX } else { (1u64 << (width * 8)) - 1 }
}

// RMW op codes: 0=add 1=sub 2=and 3=or 4=xor 5=xchg
fn shared_combine(op: u8, old: u64, val: u64, width: usize) -> u64 {
    let m = shared_width_mask(width);
    let r = match op {
        0 => old.wrapping_add(val),
        1 => old.wrapping_sub(val),
        2 => old & val,
        3 => old | val,
        4 => old ^ val,
        _ => val, // xchg
    };
    r & m
}

#[allow(dead_code)]
impl SharedMemory {
    // The host-created handle: room to grow to the wasm32 limit (4 GiB of
    // address space, only touched pages are backed).
    pub fn new(min_bytes: usize) -> Self {
        Self::with_max(min_bytes, usize::try_from(1u64 << 32).unwrap_or(usize::MAX))
    }

    // Reserves up to `max_bytes`. If the host cannot reserve that much address
    // space, the reservation is halved (down to `min_bytes`), which only lowers
    // how far `memory.grow` can go.
    pub fn with_max(min_bytes: usize, max_bytes: usize) -> Self {
        let mut cap = max_bytes.max(min_bytes);
        let base = loop {
            // SAFETY: the layout has a non-zero size.
            let p = unsafe { std::alloc::alloc_zeroed(shared_layout(cap)) };
            if !p.is_null() {
                break p;
            }
            if cap <= min_bytes {
                std::alloc::handle_alloc_error(shared_layout(cap));
            }
            cap = (cap / 2 / 65536 * 65536).max(min_bytes);
        };
        SharedMemory(std::sync::Arc::new(SharedMemInner {
            base,
            cap,
            len: std::sync::atomic::AtomicUsize::new(min_bytes),
            park: std::sync::Mutex::new(SharedPark::default()),
            cvar: std::sync::Condvar::new(),
        }))
    }

    // The current contents. Callers index it with a bounds check, which is the
    // wasm out-of-bounds trap; the slice is not synchronised (see above).
    // `len` is read Relaxed: it only bounds the slice, never shrinks, and the
    // bytes below `cap` are zeroed before any thread sees them, so there is no
    // data to acquire. A thread that learns of a grow through a synchronising
    // atomic op on memory it could already reach sees the new `len` via that
    // happens-before edge; as before, a flag placed inside the freshly grown
    // page itself gives no such edge. Relaxed avoids a load-acquire per access.
    #[allow(clippy::mut_from_ref)]
    fn bytes(&self) -> &mut [u8] {
        let len = self.0.len.load(std::sync::atomic::Ordering::Relaxed);
        // SAFETY: `base` is valid for `cap >= len` bytes for the life of `self`.
        unsafe { std::slice::from_raw_parts_mut(self.0.base, len) }
    }

    // Raises the length by `delta` pages; returns the old page count, or -1 if
    // that would pass the maximum. The new bytes are already zero.
    fn grow(&self, delta: u32) -> i32 {
        let mut cur = self.0.len.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            let new = match (delta as usize).checked_mul(65536).and_then(|d| cur.checked_add(d)) {
                Some(n) if n <= self.0.cap => n,
                _ => return -1,
            };
            match self.0.len.compare_exchange(
                cur,
                new,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return (cur / 65536) as i32,
                Err(actual) => cur = actual,
            }
        }
    }

    // The cell of an atomic access: traps (panics) when out of bounds or not
    // aligned to its width, as the threads proposal requires.
    fn atomic_cell(&self, addr: usize, width: usize) -> *mut u8 {
        let len = self.0.len.load(std::sync::atomic::Ordering::Relaxed);
        if addr % width != 0 {
            panic!("unaligned atomic access");
        }
        if addr.checked_add(width).is_none_or(|end| end > len) {
            panic!("out of bounds atomic access");
        }
        // SAFETY: `addr + width <= len <= cap`.
        unsafe { self.0.base.add(addr) }
    }

    // Sequentially consistent atomic load of `width` bytes, zero-extended.
    fn atomic_load(&self, addr: usize, width: usize) -> u64 {
        use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};
        let p = self.atomic_cell(addr, width);
        // SAFETY: `p` is in bounds and aligned for `width` (checked above).
        unsafe {
            match width {
                1 => (*(p as *const AtomicU8)).load(SeqCst) as u64,
                2 => (*(p as *const AtomicU16)).load(SeqCst) as u64,
                4 => (*(p as *const AtomicU32)).load(SeqCst) as u64,
                _ => (*(p as *const AtomicU64)).load(SeqCst),
            }
        }
    }

    // Sequentially consistent atomic store of the low `width` bytes of `val`.
    fn atomic_store(&self, addr: usize, width: usize, val: u64) {
        use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};
        let p = self.atomic_cell(addr, width);
        // SAFETY: `p` is in bounds and aligned for `width` (checked above).
        unsafe {
            match width {
                1 => (*(p as *const AtomicU8)).store(val as u8, SeqCst),
                2 => (*(p as *const AtomicU16)).store(val as u16, SeqCst),
                4 => (*(p as *const AtomicU32)).store(val as u32, SeqCst),
                _ => (*(p as *const AtomicU64)).store(val, SeqCst),
            }
        }
    }

    // Atomic read-modify-write (a compare-exchange loop, so one routine serves
    // every width and op). Returns the (zero-extended) old value.
    fn atomic_rmw(&self, addr: usize, width: usize, op: u8, val: u64) -> u64 {
        use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};
        let p = self.atomic_cell(addr, width);
        let val = val & shared_width_mask(width);
        macro_rules! rmw {
            ($atomic:ty, $int:ty) => {{
                // SAFETY: `p` is in bounds and aligned for `width` (checked above).
                let cell = unsafe { &*(p as *const $atomic) };
                let mut old = cell.load(SeqCst);
                loop {
                    let new = shared_combine(op, old as u64, val, width) as $int;
                    match cell.compare_exchange_weak(old, new, SeqCst, SeqCst) {
                        Ok(_) => break old as u64,
                        Err(actual) => old = actual,
                    }
                }
            }};
        }
        match width {
            1 => rmw!(AtomicU8, u8),
            2 => rmw!(AtomicU16, u16),
            4 => rmw!(AtomicU32, u32),
            _ => rmw!(AtomicU64, u64),
        }
    }

    // Stores `replacement` only if the (masked) current value equals `expected`.
    // Returns the (zero-extended) old value either way.
    fn atomic_cmpxchg(&self, addr: usize, width: usize, expected: u64, replacement: u64) -> u64 {
        use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};
        let p = self.atomic_cell(addr, width);
        let expected = expected & shared_width_mask(width);
        let replacement = replacement & shared_width_mask(width);
        macro_rules! cas {
            ($atomic:ty, $int:ty) => {{
                // SAFETY: `p` is in bounds and aligned for `width` (checked above).
                let cell = unsafe { &*(p as *const $atomic) };
                match cell.compare_exchange(expected as $int, replacement as $int, SeqCst, SeqCst) {
                    Ok(old) | Err(old) => old as u64,
                }
            }};
        }
        match width {
            1 => cas!(AtomicU8, u8),
            2 => cas!(AtomicU16, u16),
            4 => cas!(AtomicU32, u32),
            _ => cas!(AtomicU64, u64),
        }
    }

    // Wakes the `count` longest-parked waiters on `addr` (at most), by ticket.
    fn notify(&self, addr: u32, count: u32) -> u32 {
        let mut park = self.0.park.lock().unwrap();
        let mut n = 0;
        while n < count {
            let ticket = match park.queues.get_mut(&addr).and_then(|q| q.pop_front()) {
                Some(t) => t,
                None => break,
            };
            park.woken.insert(ticket);
            n += 1;
        }
        if n > 0 {
            drop(park);
            self.0.cvar.notify_all();
        }
        n
    }

    // Returns 0 (woken), 1 (value mismatch - did not block), or 2 (timed out).
    // `timeout_ns < 0` means wait forever. `width` is 4 (wait32) or 8 (wait64).
    fn wait(&self, addr: u32, expected: u64, width: usize, timeout_ns: i64) -> i32 {
        // The bounds/alignment trap comes first, outside the lock, so a trapping
        // wait cannot poison it for the other threads.
        self.atomic_cell(addr as usize, width);
        // Hold `park` across the value check so a concurrent `notify` (which also
        // takes `park`) cannot slip between the check and our registration -
        // closing the lost-wakeup window.
        let mut park = self.0.park.lock().unwrap();
        if self.atomic_load(addr as usize, width) != (expected & shared_width_mask(width)) {
            return 1;
        }
        let ticket = park.next_ticket;
        park.next_ticket = ticket.wrapping_add(1);
        park.queues.entry(addr).or_default().push_back(ticket);
        let deadline = if timeout_ns < 0 {
            None
        } else {
            Some(std::time::Instant::now() + std::time::Duration::from_nanos(timeout_ns as u64))
        };
        loop {
            if park.woken.remove(&ticket) {
                return 0;
            }
            match deadline {
                None => park = self.0.cvar.wait(park).unwrap(),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        // Timed out and not chosen by any `notify`: leave the queue.
                        if let Some(q) = park.queues.get_mut(&addr) {
                            q.retain(|t| *t != ticket);
                        }
                        return 2;
                    }
                    park = self.0.cvar.wait_timeout(park, d - now).unwrap().0;
                }
            }
        }
    }
}
