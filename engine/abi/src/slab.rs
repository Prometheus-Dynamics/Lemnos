use alloc::vec::Vec;

use crate::{Handle, Status};

/// The objects behind a surface's handles. Released slots are reused, with
/// their generation bumped; a slot whose generation would wrap is retired
/// instead, so no handle is ever issued twice.
#[derive(Debug)]
pub struct Slab<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
}

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slab<T> {
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }

    /// Room for `capacity` objects before the slab allocates.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free: Vec::with_capacity(capacity),
            len: 0,
        }
    }

    /// Stores `value` and returns its handle. [`Status::NO_MEMORY`] once
    /// every one of the 2^32 slot indices is in use or retired.
    pub fn insert(&mut self, value: T) -> Result<Handle, Status> {
        let handle = if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            Handle::new(index, slot.generation)
        } else {
            let index = u32::try_from(self.slots.len()).map_err(|_| Status::NO_MEMORY)?;
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            Handle::new(index, 1)
        };
        self.len += 1;
        Ok(handle)
    }

    fn slot(&self, handle: Handle) -> Option<&Slot<T>> {
        self.slots
            .get(handle.index() as usize)
            .filter(|s| s.generation == handle.generation() && s.value.is_some())
    }

    pub fn get(&self, handle: Handle) -> Result<&T, Status> {
        self.slot(handle)
            .and_then(|s| s.value.as_ref())
            .ok_or(Status::STALE_HANDLE)
    }

    pub fn get_mut(&mut self, handle: Handle) -> Result<&mut T, Status> {
        self.slots
            .get_mut(handle.index() as usize)
            .filter(|s| s.generation == handle.generation())
            .and_then(|s| s.value.as_mut())
            .ok_or(Status::STALE_HANDLE)
    }

    /// Takes the object out; the handle, and every copy of it, is stale from
    /// now on.
    pub fn remove(&mut self, handle: Handle) -> Result<T, Status> {
        let index = handle.index();
        let slot = self
            .slots
            .get_mut(index as usize)
            .filter(|s| s.generation == handle.generation())
            .ok_or(Status::STALE_HANDLE)?;
        let value = slot.value.take().ok_or(Status::STALE_HANDLE)?;
        self.len -= 1;
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(index);
        }
        Ok(value)
    }

    /// Objects stored.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The stored objects with their handles.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.value
                .as_ref()
                .map(|v| (Handle::new(i as u32, s.generation), v))
        })
    }
}
