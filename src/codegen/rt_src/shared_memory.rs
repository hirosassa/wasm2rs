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

// The bytes of a `SharedMemory` at one instant (see `SharedMemory::bytes`).
// It is `Copy` and borrows nothing: `base` is valid for `len <= cap` bytes for
// as long as the owning `SharedMemory` lives, which the generated code (holding
// `&self.memory`) guarantees. Plain accesses that race with another thread's
// access to the same bytes are wasm data races, as in any wasm engine.
#[derive(Clone, Copy)]
struct SharedBytes {
    base: *mut u8,
    len: usize,
}

#[allow(dead_code)]
impl SharedBytes {
    fn len(&self) -> usize {
        self.len
    }

    // `memory.copy`: overlapping ranges behave as a memmove; traps like
    // `<[u8]>::copy_within` when either range is out of bounds.
    fn copy_within(&mut self, src: std::ops::Range<usize>, dest: usize) {
        self.check(src.start, src.end);
        let n = src.end - src.start;
        let dest_end = dest.checked_add(n).unwrap_or(usize::MAX);
        self.check(dest, dest_end);
        // SAFETY: both ranges are within `len <= cap`; `ptr::copy` allows overlap.
        unsafe { std::ptr::copy(self.base.add(src.start), self.base.add(dest), n) }
    }

    // The traps of out-of-range indexing, reported like slice indexing.
    #[cold]
    fn out_of_range(&self, start: usize, end: usize) -> ! {
        if start > end {
            panic!("slice index starts at {start} but ends at {end}");
        }
        panic!(
            "range end index {end} out of range for slice of length {}",
            self.len
        );
    }

    #[inline(always)]
    fn check(&self, start: usize, end: usize) {
        if start > end || end > self.len {
            self.out_of_range(start, end);
        }
    }

    #[inline(always)]
    fn check_index(&self, i: usize) {
        if i >= self.len {
            panic!(
                "index out of bounds: the len is {} but the index is {i}",
                self.len
            );
        }
    }
}

impl std::ops::Index<usize> for SharedBytes {
    type Output = u8;

    #[inline(always)]
    fn index(&self, i: usize) -> &u8 {
        self.check_index(i);
        // SAFETY: `i < len <= cap`; the reference covers one byte for one access.
        unsafe { &*self.base.add(i) }
    }
}

impl std::ops::IndexMut<usize> for SharedBytes {
    #[inline(always)]
    fn index_mut(&mut self, i: usize) -> &mut u8 {
        self.check_index(i);
        // SAFETY: as in `index`.
        unsafe { &mut *self.base.add(i) }
    }
}

impl std::ops::Index<std::ops::Range<usize>> for SharedBytes {
    type Output = [u8];

    #[inline(always)]
    fn index(&self, r: std::ops::Range<usize>) -> &[u8] {
        self.check(r.start, r.end);
        // SAFETY: `start <= end <= len <= cap`; the reference covers only this range.
        unsafe { std::slice::from_raw_parts(self.base.add(r.start), r.end - r.start) }
    }
}

impl std::ops::IndexMut<std::ops::Range<usize>> for SharedBytes {
    #[inline(always)]
    fn index_mut(&mut self, r: std::ops::Range<usize>) -> &mut [u8] {
        self.check(r.start, r.end);
        // SAFETY: as in `index`.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(r.start), r.end - r.start) }
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
    // Aligned for `AtomicU64` so that an `addr % width == 0` check makes the
    // cell's address aligned for `Atomic{U16,U32,U64}` too (a `u8` layout only
    // promises 1). No more than that: an alignment above the allocator's minimum
    // (8 on 32-bit hosts) turns `alloc_zeroed` into allocate-then-memset, which
    // would touch the whole reservation instead of leaving it lazily backed.
    std::alloc::Layout::from_size_align(
        cap.max(1),
        std::mem::align_of::<std::sync::atomic::AtomicU64>(),
    )
    .unwrap()
}

fn shared_width_mask(width: usize) -> u64 {
    if width >= 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    }
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

    // A view of the current contents. Callers index it with a bounds check, which
    // is the wasm out-of-bounds trap. The view holds a raw pointer, never a
    // `&mut [u8]` over the whole memory: any number of views may coexist (and
    // overlap `&Atomic*` cells other threads use), and only the bytes of one
    // access are borrowed, for the length of that access.
    // `len` is read Relaxed: it only bounds the view, never shrinks, and the
    // bytes below `cap` are zeroed before any thread sees them, so there is no
    // data to acquire. A thread that learns of a grow through a synchronising
    // atomic op on memory it could already reach sees the new `len` via that
    // happens-before edge; as before, a flag placed inside the freshly grown
    // page itself gives no such edge. Relaxed avoids a load-acquire per access.
    fn bytes(&self) -> SharedBytes {
        let len = self.0.len.load(std::sync::atomic::Ordering::Relaxed);
        SharedBytes {
            base: self.0.base,
            len,
        }
    }

    // Raises the length by `delta` pages; returns the old page count, or -1 if
    // that would pass the maximum. The new bytes are already zero.
    fn grow(&self, delta: u32) -> i32 {
        let mut cur = self.0.len.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            let new = match (delta as usize)
                .checked_mul(65536)
                .and_then(|d| cur.checked_add(d))
            {
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
        use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering::SeqCst};
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
        use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering::SeqCst};
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
        use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering::SeqCst};
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
        use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering::SeqCst};
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
