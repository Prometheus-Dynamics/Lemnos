//! The notes a driver reports for the log: a fixed text, and at most one number after it. A driver
//! keeps a [`Notes`] ring in a field and pushes into it from its calls and its tick; the host takes
//! them out between calls and logs each one. Nothing here allocates.

/// How many notes a [`Notes`] ring holds before the host takes them. A note pushed when the ring is
/// full is counted as dropped instead.
pub const NOTE_SLOTS: usize = 8;

/// A note for the log: a fixed text, and at most one number, which the log shows after the text.
/// The text names its own unit. A note says what the driver did, not what it is about to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    pub text: &'static str,
    pub value: Option<u32>,
}

impl Note {
    /// A note with only its text.
    pub const fn new(text: &'static str) -> Self {
        Self { text, value: None }
    }

    /// A note with its text and one number.
    pub const fn with_value(text: &'static str, value: u32) -> Self {
        Self {
            text,
            value: Some(value),
        }
    }
}

/// A fixed ring of [`Note`]s that a driver fills and the host drains, oldest first. Holds up to
/// [`NOTE_SLOTS`]. A push into a full ring is counted, and [`Notes::take_dropped`] reports the count
/// once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notes {
    slots: [Option<Note>; NOTE_SLOTS],
    first: usize,
    len: usize,
    dropped: u32,
}

impl Default for Notes {
    fn default() -> Self {
        Self::new()
    }
}

impl Notes {
    /// An empty ring.
    pub const fn new() -> Self {
        Self {
            slots: [None; NOTE_SLOTS],
            first: 0,
            len: 0,
            dropped: 0,
        }
    }

    /// Keeps `note` for the host, or counts it as dropped when the ring is full.
    pub fn push(&mut self, note: Note) {
        if self.len == NOTE_SLOTS {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.slots[(self.first + self.len) % NOTE_SLOTS] = Some(note);
        self.len += 1;
    }

    /// The oldest note kept, or `None` when there is none.
    pub fn pop(&mut self) -> Option<Note> {
        if self.len == 0 {
            return None;
        }
        let note = self.slots[self.first].take();
        self.first = (self.first + 1) % NOTE_SLOTS;
        self.len -= 1;
        note
    }

    /// How many notes were dropped since this was last called, and starts the count again.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::take(&mut self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_come_out_oldest_first_and_an_empty_ring_gives_none() {
        let mut notes = Notes::new();
        assert_eq!(notes.pop(), None);
        notes.push(Note::new("first"));
        notes.push(Note::with_value("second", 7));
        assert_eq!(notes.pop(), Some(Note::new("first")));
        assert_eq!(notes.pop(), Some(Note::with_value("second", 7)));
        assert_eq!(notes.pop(), None);
    }

    #[test]
    fn a_full_ring_counts_what_it_cannot_keep_and_reports_it_once() {
        let mut notes = Notes::new();
        for at in 0..NOTE_SLOTS as u32 + 3 {
            notes.push(Note::with_value("n", at));
        }
        assert_eq!(notes.take_dropped(), 3);
        assert_eq!(notes.take_dropped(), 0, "reported once");
        // The first eight are kept, in order; the three after them were the ones dropped.
        for at in 0..NOTE_SLOTS as u32 {
            assert_eq!(notes.pop(), Some(Note::with_value("n", at)));
        }
        assert_eq!(notes.pop(), None);
    }

    #[test]
    fn the_ring_wraps_round_and_keeps_order() {
        let mut notes = Notes::new();
        for round in 0..3 * NOTE_SLOTS as u32 {
            notes.push(Note::with_value("n", round));
            assert_eq!(notes.pop(), Some(Note::with_value("n", round)));
        }
        assert_eq!(notes.take_dropped(), 0);
    }
}
