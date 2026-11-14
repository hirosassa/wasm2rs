//! The slab that hands out and recycles the guest-visible handles.

use super::fs::FIRST_FILE_STREAM;

/// Traps the guest. The generated `Host` methods have no error channel for a bad handle or an
/// exhausted host resource, so a trap is a panic that names the cause (the contract in the
/// crate docs).
#[cold]
#[expect(
    clippy::panic,
    reason = "the one place a trap is raised: `Host` signatures leave no other channel"
)]
pub(super) fn trap(msg: impl std::fmt::Display) -> ! {
    panic!("{msg}")
}

/// Locks the stdin buffer; one poisoned by a panicking thread traps like any broken invariant.
pub(super) fn lock_stdin<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock()
        .unwrap_or_else(|_| trap("wasi:cli: the stdin buffer is poisoned"))
}

/// The slab index a handle names. One too large for `usize` is as unknown as any other
/// handle that is not in the table, so it saturates to a slot that never exists.
pub(super) fn idx(handle: u32) -> usize {
    usize::try_from(handle).unwrap_or(usize::MAX)
}

/// A length as the `u64` that WASI counts in (`usize` is never wider than 64 bits here).
pub(super) fn to_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// A WASI length as a `usize`; one beyond the address space saturates, which the callers'
/// own limits (already applied) and the allocator reject.
pub(super) fn to_usize(len: u64) -> usize {
    usize::try_from(len).unwrap_or(usize::MAX)
}

/// The handle for a slab slot; a table with over `u32::MAX` slots traps.
pub(super) fn to_handle(slot: usize) -> u32 {
    u32::try_from(slot).unwrap_or_else(|_| trap("handle table exhausted"))
}

/// A table of live handles: a handle is a slot's index, and the slot of a dropped handle
/// is reused by the next `insert`. A guest must not use a handle after dropping it; the
/// host does not detect that, and the stale handle may then name a newer resource.
pub(super) struct Slab<T> {
    pub(super) slots: Vec<Option<T>>,
    pub(super) free: Vec<usize>,
    /// How often each slot was freed: a `SlotRef` taken earlier no longer matches once it grew.
    pub(super) epochs: Vec<u32>,
}

/// A slot of a `Slab` together with the epoch it had when the reference was made, so a
/// pollable made for a resource cannot be confused with whatever is put in its slot later.
#[derive(Clone, Copy, Debug)]
pub(super) struct SlotRef {
    pub(super) slot: usize,
    pub(super) epoch: u32,
}

impl<T> Slab<T> {
    pub(super) fn new() -> Self {
        Slab {
            slots: Vec::new(),
            free: Vec::new(),
            epochs: Vec::new(),
        }
    }

    pub(super) fn insert(&mut self, value: T) -> usize {
        match self.free.pop() {
            Some(slot) => {
                // every freed slot index came from `slots`
                match self.slots.get_mut(slot) {
                    Some(entry) => *entry = Some(value),
                    None => trap("handle table: a freed slot is missing"),
                }
                // a new occupant: a ref made while the slot was free must not match it
                if let Some(epoch) = self.epochs.get_mut(slot) {
                    *epoch = epoch.wrapping_add(1);
                }
                slot
            }
            None => {
                let slot = self.slots.len();
                self.slots.push(Some(value));
                self.epochs.push(1);
                slot
            }
        }
    }

    pub(super) fn get(&self, slot: usize) -> Option<&T> {
        self.slots.get(slot)?.as_ref()
    }

    pub(super) fn get_mut(&mut self, slot: usize) -> Option<&mut T> {
        self.slots.get_mut(slot)?.as_mut()
    }

    /// A reference to what the slot holds now. One made for a free or unknown slot (epoch 0
    /// never occurs) matches nothing, not even what is inserted there later.
    pub(super) fn slot_ref(&self, slot: usize) -> SlotRef {
        SlotRef {
            slot,
            epoch: self.epochs.get(slot).copied().unwrap_or(0),
        }
    }

    /// What `r` was made for, `None` once it was freed (even if the slot is in use again).
    pub(super) fn get_ref(&self, r: SlotRef) -> Option<&T> {
        if self.epochs.get(r.slot) != Some(&r.epoch) {
            return None;
        }
        self.get(r.slot)
    }

    /// Frees a slot and hands back its value; `None` for an unknown or already free one.
    pub(super) fn take(&mut self, slot: usize) -> Option<T> {
        let value = self.slots.get_mut(slot)?.take()?;
        if let Some(epoch) = self.epochs.get_mut(slot) {
            *epoch = epoch.wrapping_add(1);
        }
        self.free.push(slot);
        Some(value)
    }

    /// Frees a slot; an unknown or already free one is ignored.
    pub(super) fn remove(&mut self, slot: usize) {
        drop(self.take(slot));
    }
}

/// The stream handle of a file stream's slot: file streams count up from `FIRST_FILE_STREAM`.
pub(super) fn file_stream_handle(slot: usize) -> u32 {
    FIRST_FILE_STREAM
        .checked_add(to_handle(slot))
        .unwrap_or_else(|| trap("stream handle table exhausted"))
}
