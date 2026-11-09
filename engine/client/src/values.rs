//! Values that arrived while something else was being waited for.

const CAPACITY: usize = 64 * 1024;
const HEADER: usize = 8 + 8 + 4;

/// Bounded, and allocates only when created. When full, new values are dropped
/// and counted: the oldest are already the ones being consumed.
#[derive(Debug)]
pub(crate) struct ValueQueue {
    buf: Vec<u8>,
    head: usize,
    tail: usize,
    dropped: u64,
}

impl ValueQueue {
    pub(crate) fn new() -> Self {
        Self {
            buf: vec![0; CAPACITY],
            head: 0,
            tail: 0,
            dropped: 0,
        }
    }

    pub(crate) const fn dropped(&self) -> u64 {
        self.dropped
    }

    pub(crate) fn push(&mut self, subscription: u64, stamp_ns: u64, bytes: &[u8]) {
        let need = HEADER + bytes.len();
        if self.head == self.tail {
            self.head = 0;
            self.tail = 0;
        }
        if self.tail + need > self.buf.len() {
            self.buf.copy_within(self.head..self.tail, 0);
            self.tail -= self.head;
            self.head = 0;
        }
        let Some(entry) = self.buf.get_mut(self.tail..self.tail + need) else {
            self.dropped += 1;
            return;
        };
        entry[..8].copy_from_slice(&subscription.to_le_bytes());
        entry[8..16].copy_from_slice(&stamp_ns.to_le_bytes());
        entry[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        entry[20..].copy_from_slice(bytes);
        self.tail += need;
    }

    /// Hands the oldest value to `read`.
    pub(crate) fn pop<R>(&mut self, read: impl FnOnce(u64, u64, &[u8]) -> R) -> Option<R> {
        if self.head == self.tail {
            return None;
        }
        let entry = &self.buf[self.head..self.tail];
        let word = |at: usize| u64::from_le_bytes(entry[at..at + 8].try_into().unwrap_or([0; 8]));
        let len = u32::from_le_bytes(entry[16..20].try_into().unwrap_or([0; 4])) as usize;
        let out = read(word(0), word(8), &entry[HEADER..HEADER + len]);
        self.head += HEADER + len;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pop(queue: &mut ValueQueue) -> Option<(u64, u64, Vec<u8>)> {
        queue.pop(|s, t, b| (s, t, b.to_vec()))
    }

    #[test]
    fn values_come_out_in_order() {
        let mut queue = ValueQueue::new();
        queue.push(1, 10, &[1]);
        queue.push(2, 20, &[2, 3]);
        assert_eq!(pop(&mut queue), Some((1, 10, vec![1])));
        assert_eq!(pop(&mut queue), Some((2, 20, vec![2, 3])));
        assert_eq!(pop(&mut queue), None);
    }

    #[test]
    fn a_full_queue_drops_the_new_value_and_counts_it() {
        let mut queue = ValueQueue::new();
        let big = vec![0; CAPACITY - HEADER];
        queue.push(1, 0, &big);
        queue.push(2, 0, &[1]);
        assert_eq!(queue.dropped(), 1);
        assert_eq!(pop(&mut queue).unwrap().0, 1);
    }

    #[test]
    fn room_freed_at_the_front_is_reused() {
        let mut queue = ValueQueue::new();
        let half = vec![0; CAPACITY / 2 - HEADER];
        queue.push(1, 0, &half);
        queue.push(2, 0, &half);
        assert_eq!(pop(&mut queue).unwrap().0, 1);
        queue.push(3, 0, &half);
        assert_eq!(queue.dropped(), 0);
        assert_eq!(pop(&mut queue).unwrap().0, 2);
        assert_eq!(pop(&mut queue).unwrap().0, 3);
    }
}
