//! A free list of values the caller gives back, handed out again: buffers and
//! tables that a steady stream reuses instead of allocating per transfer.

use std::sync::{Mutex, MutexGuard};

/// Up to `cap` spare values, last given first taken.
#[derive(Debug)]
pub struct Spares<T> {
    items: Mutex<Vec<T>>,
    cap: usize,
}

impl<T> Spares<T> {
    /// An empty pool that keeps at most `cap` values.
    pub const fn new(cap: usize) -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            cap,
        }
    }

    /// A spare value, if one is kept.
    pub fn take(&self) -> Option<T> {
        self.lock().pop()
    }

    /// Keeps `item` for a later `take`; dropped when the pool is full.
    pub fn give(&self, item: T) {
        let mut items = self.lock();
        if items.len() < self.cap {
            items.push(item);
        }
    }

    /// The number of values kept.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no value is kept.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<T>> {
        self.items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_hands_back_what_was_given_up_to_its_capacity() {
        let pool = Spares::new(2);
        assert!(pool.take().is_none());
        pool.give(vec![1u8]);
        pool.give(vec![2u8]);
        pool.give(vec![3u8]);
        assert_eq!(pool.len(), 2, "the third is dropped");
        assert_eq!(pool.take(), Some(vec![2u8]), "last given, first taken");
        assert_eq!(pool.take(), Some(vec![1u8]));
        assert!(pool.is_empty());
    }
}
