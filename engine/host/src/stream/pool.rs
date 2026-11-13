//! Which of a stream's buffers is where. Only the host holds this: nothing here lives in memory a
//! peer can write.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BufferState {
    /// Waiting to be handed to the producer.
    Free,
    /// The producer has it, granted while `epoch` was current.
    Producer { epoch: u64 },
    /// Published: on the ring or with the consumer, until it comes back with `fence`.
    Consumer { fence: u32 },
    /// Published with no consumer attached, on memory kept on standby: the newest frame, held
    /// for the next consumer.
    Kept,
}

#[derive(Debug)]
pub(super) struct BufferPool {
    states: Vec<BufferState>,
    /// How many times each buffer has been published: its current fence.
    generation: Vec<u32>,
    free: VecDeque<u32>,
}

/// A buffer that came back wasn't due back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NotDue;

impl BufferPool {
    /// `buffers` free buffers. Everything is sized here, so nothing grows later.
    pub(super) fn new(buffers: u32) -> Self {
        let count = buffers as usize;
        Self {
            states: vec![BufferState::Free; count],
            generation: vec![0; count],
            free: (0..buffers).collect(),
        }
    }

    pub(super) fn state(&self, buffer: u32) -> Option<BufferState> {
        self.states.get(buffer as usize).copied()
    }

    /// The next free buffer, granted to the producer under `epoch`.
    pub(super) fn take(&mut self, epoch: u64) -> Option<u32> {
        let buffer = self.free.pop_front()?;
        if let Some(state) = self.states.get_mut(buffer as usize) {
            *state = BufferState::Producer { epoch };
        }
        Some(buffer)
    }

    /// Grants a published buffer back to the producer under `epoch`: the publisher took it back
    /// because no consumer holds it. The caller has checked the ring agrees.
    pub(super) fn grant(&mut self, buffer: u32, epoch: u64) {
        self.free.retain(|free| *free != buffer);
        if let Some(state) = self.states.get_mut(buffer as usize) {
            *state = BufferState::Producer { epoch };
        }
    }

    /// Marks a buffer the producer holds as published, returning its fence. The caller has
    /// checked it was held.
    pub(super) fn publish(&mut self, buffer: u32) -> u32 {
        let index = buffer as usize;
        let fence = self
            .generation
            .get(index)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        if let Some(generation) = self.generation.get_mut(index) {
            *generation = fence;
        }
        if let Some(state) = self.states.get_mut(index) {
            *state = BufferState::Consumer { fence };
        }
        fence
    }

    /// Takes a buffer off the producer without it having reached anyone.
    pub(super) fn drop_held(&mut self, buffer: u32) {
        self.make_free(buffer);
    }

    /// Keeps a buffer the producer holds for the next consumer. The caller has checked it was
    /// held.
    pub(super) fn keep(&mut self, buffer: u32) {
        if let Some(state) = self.states.get_mut(buffer as usize) {
            *state = BufferState::Kept;
        }
    }

    /// Frees a kept buffer that a newer frame took the place of.
    pub(super) fn drop_kept(&mut self, buffer: u32) {
        if self.state(buffer) == Some(BufferState::Kept) {
            self.make_free(buffer);
        }
    }

    /// A buffer the consumer gave back. It must be out with exactly this fence.
    pub(super) fn release(&mut self, buffer: u32, fence: u32) -> Result<(), NotDue> {
        match self.states.get(buffer as usize) {
            Some(BufferState::Consumer { fence: due }) if *due == fence => {
                self.make_free(buffer);
                Ok(())
            }
            _ => Err(NotDue),
        }
    }

    /// Takes back every published buffer. The consumer has gone.
    pub(super) fn reclaim_published(&mut self) -> u32 {
        self.reclaim(|state| matches!(state, BufferState::Consumer { .. }))
    }

    /// Takes back every buffer the producer held. The producer has gone.
    pub(super) fn reclaim_producer(&mut self) -> u32 {
        self.reclaim(|state| matches!(state, BufferState::Producer { .. }))
    }

    fn reclaim(&mut self, matches: impl Fn(BufferState) -> bool) -> u32 {
        let mut count = 0;
        for buffer in 0..self.states.len() as u32 {
            if self.state(buffer).is_some_and(&matches) {
                self.make_free(buffer);
                count += 1;
            }
        }
        count
    }

    fn make_free(&mut self, buffer: u32) {
        if let Some(state) = self.states.get_mut(buffer as usize)
            && *state != BufferState::Free
        {
            *state = BufferState::Free;
            self.free.push_back(buffer);
        }
    }

    fn count(&self, matches: impl Fn(BufferState) -> bool) -> u32 {
        self.states.iter().filter(|s| matches(**s)).count() as u32
    }

    /// Free, held by the producer, held by the consumer, kept for the next consumer.
    pub(super) fn counts(&self) -> (u32, u32, u32, u32) {
        (
            self.count(|s| s == BufferState::Free),
            self.count(|s| matches!(s, BufferState::Producer { .. })),
            self.count(|s| matches!(s, BufferState::Consumer { .. })),
            self.count(|s| s == BufferState::Kept),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_moves_free_producer_consumer_free() {
        let mut pool = BufferPool::new(2);
        assert_eq!(pool.counts(), (2, 0, 0, 0));
        let buffer = pool.take(1).unwrap();
        assert_eq!(pool.counts(), (1, 1, 0, 0));
        let fence = pool.publish(buffer);
        assert_eq!(fence, 1);
        assert_eq!(pool.counts(), (1, 0, 1, 0));
        assert_eq!(pool.release(buffer, fence), Ok(()));
        assert_eq!(pool.counts(), (2, 0, 0, 0));
    }

    #[test]
    fn a_second_return_of_the_same_buffer_is_refused() {
        let mut pool = BufferPool::new(1);
        let buffer = pool.take(1).unwrap();
        let fence = pool.publish(buffer);
        assert_eq!(pool.release(buffer, fence), Ok(()));
        assert_eq!(pool.release(buffer, fence), Err(NotDue));
    }

    #[test]
    fn a_return_with_the_wrong_fence_or_number_is_refused() {
        let mut pool = BufferPool::new(1);
        let buffer = pool.take(1).unwrap();
        let fence = pool.publish(buffer);
        assert_eq!(pool.release(buffer, fence + 1), Err(NotDue));
        assert_eq!(pool.release(9, fence), Err(NotDue));
        assert_eq!(pool.counts(), (0, 0, 1, 0), "unchanged");
    }

    #[test]
    fn a_stale_fence_from_an_earlier_publish_is_refused() {
        let mut pool = BufferPool::new(1);
        let buffer = pool.take(1).unwrap();
        let old = pool.publish(buffer);
        pool.release(buffer, old).unwrap();
        pool.take(1).unwrap();
        let new = pool.publish(buffer);
        assert_ne!(old, new);
        assert_eq!(pool.release(buffer, old), Err(NotDue));
        assert_eq!(pool.release(buffer, new), Ok(()));
    }

    #[test]
    fn a_buffer_the_producer_holds_cant_be_returned() {
        let mut pool = BufferPool::new(2);
        let buffer = pool.take(1).unwrap();
        assert_eq!(pool.release(buffer, 1), Err(NotDue));
        assert_eq!(pool.state(buffer), Some(BufferState::Producer { epoch: 1 }));
    }

    #[test]
    fn reclaiming_takes_back_only_what_it_says() {
        let mut pool = BufferPool::new(3);
        let a = pool.take(1).unwrap();
        let _b = pool.take(1).unwrap();
        pool.publish(a);
        assert_eq!(pool.reclaim_published(), 1);
        assert_eq!(pool.counts(), (2, 1, 0, 0));
        assert_eq!(pool.reclaim_producer(), 1);
        assert_eq!(pool.counts(), (3, 0, 0, 0));
    }

    #[test]
    fn a_kept_buffer_is_neither_side_s_until_it_is_published_or_dropped() {
        let mut pool = BufferPool::new(2);
        let a = pool.take(1).unwrap();
        pool.keep(a);
        assert_eq!(pool.counts(), (1, 0, 0, 1));
        assert_eq!(pool.release(a, 0), Err(NotDue), "nobody was handed it");
        assert_eq!(pool.reclaim_published(), 0);
        assert_eq!(pool.reclaim_producer(), 0);
        let fence = pool.publish(a);
        assert_eq!(pool.counts(), (1, 0, 1, 0));
        pool.release(a, fence).unwrap();
        let b = pool.take(1).unwrap();
        pool.keep(b);
        pool.drop_kept(b);
        assert_eq!(pool.counts(), (2, 0, 0, 0));
        // Only a kept buffer is dropped that way.
        let c = pool.take(1).unwrap();
        pool.drop_kept(c);
        assert_eq!(pool.counts(), (1, 1, 0, 0));
    }
}
