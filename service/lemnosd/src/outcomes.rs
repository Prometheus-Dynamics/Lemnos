//! The outcomes of calls made with a token (`docs/wire.md`, "CALL_OUTCOME"): a bounded table the
//! service keeps, so a client can ask what became of a call it gave up on, after its timeout or a
//! reconnect. A call is known by the name its client said in Hello and the token it chose, so the
//! outcome outlives the connection that made the call.
//!
//! The table is one fixed array, allocated once. Recording an outcome and answering a question
//! allocate nothing. A call the table has no room for evicts the oldest finished outcome; a pending
//! one is evicted only when nothing finished is left. An evicted or aged-out outcome reads as
//! unknown.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lemnos_host::CallSink;

/// How many calls the table keeps.
pub(crate) const CAPACITY: usize = 256;
/// How long a finished call's outcome is kept. A call still running is kept until it finishes.
pub(crate) const KEEP: Duration = Duration::from_secs(600);
/// The client name's bytes the table keeps: the first of the Hello name.
const NAME: usize = 32;

/// What the table knows of one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Begun; the driver's method has not started (the lane has not run it yet).
    Waiting,
    /// The driver's method is running.
    Started,
    /// The driver's method ran and returned this status.
    Applied(i32),
    /// Refused before the driver's method ran, with this status.
    NotApplied(i32),
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    used: bool,
    /// Bumped each time the slot is given to a new call, so a late report about an evicted call is
    /// recognised and dropped.
    generation: u32,
    client: [u8; NAME],
    client_len: usize,
    token: u64,
    phase: Phase,
    begun: Instant,
    ended: Instant,
}

impl Entry {
    fn empty(now: Instant) -> Self {
        Self {
            used: false,
            generation: 0,
            client: [0; NAME],
            client_len: 0,
            token: 0,
            phase: Phase::Waiting,
            begun: now,
            ended: now,
        }
    }

    fn is_for(&self, client: &str, token: u64) -> bool {
        self.used && self.token == token && self.client() == client_key(client)
    }

    fn client(&self) -> &[u8] {
        &self.client[..self.client_len]
    }

    fn pending(&self) -> bool {
        matches!(self.phase, Phase::Waiting | Phase::Started)
    }

    /// Finished, and kept for as long as the table keeps a finished call.
    fn aged(&self, now: Instant) -> bool {
        !self.pending() && now.saturating_duration_since(self.ended) >= KEEP
    }
}

/// The name as the table keeps it: its first bytes.
fn client_key(client: &str) -> &[u8] {
    let bytes = client.as_bytes();
    &bytes[..bytes.len().min(NAME)]
}

/// The answer to a question about one token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Unknown,
    Pending,
    Applied(i32),
    NotApplied(i32),
}

pub(crate) struct Outcomes {
    table: Mutex<Vec<Entry>>,
}

/// A call's ticket: its slot and the slot's generation, which the call's reports carry.
fn ticket(index: usize, generation: u32) -> u64 {
    (u64::from(generation) << 32) | index as u64
}

impl Outcomes {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            table: Mutex::new(vec![Entry::empty(now); CAPACITY]),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Begins a call from `client` with `token`, and returns its ticket. `None` when a call with
    /// this token from this client is still running: the new one is not started. A token whose
    /// earlier call finished is given to the new call, and its outcome is replaced.
    pub(crate) fn begin(&self, client: &str, token: u64) -> Option<u64> {
        self.begin_at(client, token, Instant::now())
    }

    fn begin_at(&self, client: &str, token: u64, now: Instant) -> Option<u64> {
        let mut table = self.lock();
        let index = match table.iter().position(|e| e.is_for(client, token)) {
            Some(index) if table[index].pending() => return None,
            Some(index) => index,
            None => victim(&table, now),
        };
        let key = client_key(client);
        let entry = &mut table[index];
        entry.used = true;
        entry.generation = entry.generation.wrapping_add(1);
        entry.client[..key.len()].copy_from_slice(key);
        entry.client_len = key.len();
        entry.token = token;
        entry.phase = Phase::Waiting;
        entry.begun = now;
        entry.ended = now;
        Some(ticket(index, entry.generation))
    }

    /// The driver's method of the call `ticket` names is running now.
    pub(crate) fn note_started(&self, ticket: u64) {
        let mut table = self.lock();
        if let Some(entry) = locate(&mut table, ticket)
            && entry.phase == Phase::Waiting
        {
            entry.phase = Phase::Started;
        }
    }

    /// The call `ticket` names is over with `status`, at `now`. It applied if its method started,
    /// and was refused otherwise.
    pub(crate) fn note_finished_at(&self, ticket: u64, status: i32, now: Instant) {
        let mut table = self.lock();
        let Some(entry) = locate(&mut table, ticket) else {
            return;
        };
        entry.phase = match entry.phase {
            Phase::Waiting => Phase::NotApplied(status),
            Phase::Started => Phase::Applied(status),
            done @ (Phase::Applied(_) | Phase::NotApplied(_)) => done,
        };
        if entry.phase != Phase::Waiting {
            entry.ended = now;
        }
    }

    /// What became of the call `client` made with `token`.
    pub(crate) fn query(&self, client: &str, token: u64) -> Answer {
        self.query_at(client, token, Instant::now())
    }

    fn query_at(&self, client: &str, token: u64, now: Instant) -> Answer {
        let table = self.lock();
        let Some(entry) = table.iter().find(|e| e.is_for(client, token)) else {
            return Answer::Unknown;
        };
        match entry.phase {
            Phase::Waiting | Phase::Started => Answer::Pending,
            _ if entry.aged(now) => Answer::Unknown,
            Phase::Applied(status) => Answer::Applied(status),
            Phase::NotApplied(status) => Answer::NotApplied(status),
        }
    }
}

/// The slot a new call takes: an unused one, then an aged finished one, then the oldest finished,
/// and the oldest pending one only when nothing else is left.
fn victim(table: &[Entry], now: Instant) -> usize {
    if let Some(index) = table.iter().position(|e| !e.used) {
        return index;
    }
    if let Some(index) = table.iter().position(|e| e.aged(now)) {
        return index;
    }
    let oldest = |finished: bool| {
        table
            .iter()
            .enumerate()
            .filter(|(_, e)| e.pending() != finished)
            .min_by_key(|(_, e)| if finished { e.ended } else { e.begun })
            .map(|(index, _)| index)
    };
    oldest(true).or_else(|| oldest(false)).unwrap_or(0)
}

/// The entry a ticket names, if it still names the same call.
fn locate(table: &mut [Entry], ticket: u64) -> Option<&mut Entry> {
    let index = usize::try_from(ticket & 0xffff_ffff).ok()?;
    let generation = (ticket >> 32) as u32;
    table
        .get_mut(index)
        .filter(|e| e.used && e.generation == generation)
}

impl CallSink for Outcomes {
    fn started(&self, ticket: u64) {
        self.note_started(ticket);
    }

    fn finished(&self, ticket: u64, status: lemnos_core::Status) {
        self.note_finished_at(ticket, status.0, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = "caller";

    #[test]
    fn a_call_is_pending_until_its_method_finishes_and_then_applied_with_its_status() {
        let now = Instant::now();
        let table = Outcomes::new();
        let ticket = table.begin_at(CLIENT, 7, now).unwrap();
        assert_eq!(table.query_at(CLIENT, 7, now), Answer::Pending);
        table.note_started(ticket);
        assert_eq!(table.query_at(CLIENT, 7, now), Answer::Pending);
        table.note_finished_at(ticket, -5, now);
        assert_eq!(table.query_at(CLIENT, 7, now), Answer::Applied(-5));
    }

    #[test]
    fn a_call_refused_before_its_method_ran_is_not_applied() {
        let now = Instant::now();
        let table = Outcomes::new();
        let ticket = table.begin_at(CLIENT, 8, now).unwrap();
        table.note_finished_at(ticket, 2, now);
        assert_eq!(table.query_at(CLIENT, 8, now), Answer::NotApplied(2));
    }

    #[test]
    fn an_unknown_token_and_another_client_are_unknown() {
        let now = Instant::now();
        let table = Outcomes::new();
        let ticket = table.begin_at(CLIENT, 9, now).unwrap();
        table.note_finished_at(ticket, 0, now);
        assert_eq!(table.query_at(CLIENT, 10, now), Answer::Unknown);
        assert_eq!(table.query_at("someone-else", 9, now), Answer::Unknown);
    }

    #[test]
    fn a_token_still_running_is_not_begun_again() {
        let now = Instant::now();
        let table = Outcomes::new();
        assert!(table.begin_at(CLIENT, 11, now).is_some());
        assert!(table.begin_at(CLIENT, 11, now).is_none());
    }

    #[test]
    fn a_finished_outcome_ages_out_and_a_pending_one_does_not() {
        let now = Instant::now();
        let later = now + KEEP;
        let table = Outcomes::new();
        let done = table.begin_at(CLIENT, 1, now).unwrap();
        table.note_finished_at(done, 0, now);
        let running = table.begin_at(CLIENT, 2, now).unwrap();
        table.note_started(running);
        assert_eq!(table.query_at(CLIENT, 1, later), Answer::Unknown);
        assert_eq!(table.query_at(CLIENT, 2, later), Answer::Pending);
    }

    #[test]
    fn a_full_table_evicts_the_oldest_finished_outcome_and_reads_it_as_unknown() {
        let now = Instant::now();
        let table = Outcomes::new();
        for token in 0..CAPACITY as u64 {
            let ticket = table
                .begin_at(CLIENT, token, now + Duration::from_millis(token))
                .unwrap();
            table.note_started(ticket);
            table.note_finished_at(ticket, 0, now + Duration::from_millis(token));
        }
        let fresh = now + Duration::from_secs(1);
        table.begin_at(CLIENT, 10_000, fresh).unwrap();
        assert_eq!(table.query_at(CLIENT, 0, fresh), Answer::Unknown);
        assert_eq!(
            table.query_at(CLIENT, CAPACITY as u64 - 1, fresh),
            Answer::Applied(0)
        );
        assert_eq!(table.query_at(CLIENT, 10_000, fresh), Answer::Pending);
    }

    #[test]
    fn a_report_about_an_evicted_pending_call_is_dropped_and_names_no_other_call() {
        let now = Instant::now();
        let table = Outcomes::new();
        let first = table.begin_at(CLIENT, 0, now).unwrap();
        for token in 1..CAPACITY as u64 {
            table.begin_at(CLIENT, token, now).unwrap();
        }
        // Every slot is pending: the next call takes the oldest, token 0's.
        let newest = table.begin_at(CLIENT, 9_999, now).unwrap();
        assert_eq!(table.query_at(CLIENT, 0, now), Answer::Unknown);
        table.note_finished_at(first, 0, now);
        assert_eq!(table.query_at(CLIENT, 9_999, now), Answer::Pending);
        table.note_started(newest);
        table.note_finished_at(newest, 3, now);
        assert_eq!(table.query_at(CLIENT, 9_999, now), Answer::Applied(3));
    }

    #[test]
    fn a_client_name_past_its_kept_bytes_shares_the_kept_part() {
        let now = Instant::now();
        let table = Outcomes::new();
        let long = "x".repeat(NAME + 8);
        let ticket = table.begin_at(&long, 4, now).unwrap();
        table.note_started(ticket);
        table.note_finished_at(ticket, 0, now);
        assert_eq!(
            table.query_at(&"x".repeat(NAME), 4, now),
            Answer::Applied(0)
        );
    }
}
