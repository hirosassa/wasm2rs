//! The slab that hands out and recycles the guest-visible handles.

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
                self.slots[slot] = Some(value);
                // a new occupant: a ref made while the slot was free must not match it
                if let Some(epoch) = self.epochs.get_mut(slot) {
                    *epoch = epoch.wrapping_add(1);
                }
                slot
            }
            None => {
                self.slots.push(Some(value));
                self.epochs.push(1);
                self.slots.len() - 1
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
