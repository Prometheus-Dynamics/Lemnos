//! The `self-test/1` contract: named checks a device runs on itself, which report pass, fail,
//! skipped or not-supported with a reason code and up to four measured values.
//!
//! Any driver may serve it beside its own contract, and the built-in `board-self-test` serves it
//! for a whole board. A UI lists the tests with `tests`, starts one with `run` (a long job), and
//! reads what each test last found with `last`. Nothing persists across a restart.
//!
//! Every byte layout is fixed (little-endian, no padding); see `docs/drivers.md`
//! ("Self-tests") and `docs/daemon.md` for the bounds of the two out-buffer methods.
//!
//! [`Runner`] is the bookkeeping every driver shares: it validates a run, refuses an intrusive
//! one while another caller uses the device, drives the test's steps from the driver's tick,
//! puts back what a test changed on every end (and retries that under the failure rule), and
//! keeps the last result per test. A driver supplies only [`Bench`], the test's own steps.

#![no_std]

use lemnos_core::{
    CallerId, Decode, Encode, JobId, JobState, Reader, Reply, Result, Status, Writer, contract,
};

/// Most tests one device lists.
pub const MAX_TESTS: usize = 8;
/// Most entries a `tests` or `last` list carries: a board's list names up to 16 members' tests
/// (8 each), and `all`.
pub const MAX_LISTED: usize = 16 * MAX_TESTS + 1;
/// Most bytes in a name a list carries: a test's own name (at most [`MAX_TEST_NAME`]), or a
/// board's `member:test`, which is a member's id, a colon and a test name.
pub const MAX_NAME: usize = 80;
/// Most bytes in one test's own name.
pub const MAX_TEST_NAME: usize = 32;
/// Most bytes in a test's description.
pub const MAX_DESCRIPTION: usize = 96;
/// Most bytes in a test's fixture text.
pub const MAX_FIXTURE: usize = 64;
/// Most bytes in a result's detail.
pub const MAX_DETAIL: usize = 96;
/// Most measured values one result carries.
pub const MAX_MEASURES: usize = 4;
/// Most bytes in a measure's name.
pub const MAX_MEASURE_NAME: usize = 32;
/// Most bytes in a measure's unit.
pub const MAX_UNIT: usize = 8;
/// Callers a device keeps track of for the in-use rule. More than this counts as in use.
pub const MAX_USERS: usize = 8;

/// Bytes one test record takes in `tests`.
pub const TEST_BYTES: usize = 1 + MAX_NAME + 1 + MAX_DESCRIPTION + 1 + 1 + 1 + MAX_FIXTURE + 4;
/// Bytes one measure takes in a result.
pub const MEASURE_BYTES: usize = 1 + MAX_MEASURE_NAME + 8 + 1 + 1 + MAX_UNIT + 8 + 8 + 1;
/// Bytes one result record takes: the `run` job's result, and the body of a `last` entry.
pub const RECORD_BYTES: usize = 1 + 2 + 1 + MAX_DETAIL + 1 + MAX_MEASURES * MEASURE_BYTES;
/// Bytes one `last` entry takes: a name, then its result.
pub const LAST_BYTES: usize = 1 + MAX_NAME + RECORD_BYTES;

/// The reason codes a result carries. Numbers never change; a new reason takes the next number.
pub mod reason {
    /// Within its limits, or answered as expected (pass).
    pub const OK: u16 = 0;
    /// An operator confirmed what the test shows (pass).
    pub const OPERATOR_CONFIRMED: u16 = 1;
    /// The test needs an operator to confirm; run it again with `confirmed = true` (skipped).
    pub const NEEDS_OPERATOR: u16 = 2;
    /// The board file gives no setting the test needs (skipped).
    pub const NOT_CONFIGURED: u16 = 3;
    /// The hardware the test needs is not there, or not running (skipped).
    pub const ABSENT: u16 = 4;
    /// A board member refused to start because another caller uses it (skipped).
    pub const IN_USE: u16 = 5;
    /// A read or write of the device failed (fail).
    pub const NO_RESPONSE: u16 = 6;
    /// The chip answered with another identity (fail).
    pub const IDENTITY: u16 = 7;
    /// A measured value is outside its limits (fail).
    pub const OUT_OF_RANGE: u16 = 8;
    /// The device could not be put back as the test found it; retried (fail).
    pub const RESTORE_FAILED: u16 = 9;
    /// The device does not offer the test named (not-supported).
    pub const UNKNOWN_TEST: u16 = 10;
    /// The device is owned by the operating system, and the test would write (skipped).
    pub const READ_ONLY: u16 = 11;
}

/// What a test found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Skipped,
    NotSupported,
}

impl Outcome {
    /// The number this outcome has on the wire.
    pub const fn code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Skipped => 2,
            Self::NotSupported => 3,
        }
    }

    /// The outcome for a wire number, or `None` for one this build does not know.
    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Pass,
            1 => Self::Fail,
            2 => Self::Skipped,
            3 => Self::NotSupported,
            _ => return None,
        })
    }
}

/// A short text kept inline, at most `N` bytes. Longer text is cut at `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    len: u8,
    bytes: [u8; N],
}

impl<const N: usize> Text<N> {
    /// `text`, cut to `N` bytes (at a character boundary).
    pub const fn new(text: &str) -> Self {
        let text = text.as_bytes();
        let mut len = if text.len() < N { text.len() } else { N };
        // Never cut inside a character: back up to its first byte.
        while len > 0 && len < text.len() && (text[len] & 0xC0) == 0x80 {
            len -= 1;
        }
        let mut bytes = [0u8; N];
        let mut at = 0;
        while at < len {
            bytes[at] = text[at];
            at += 1;
        }
        Self {
            len: len as u8,
            bytes,
        }
    }

    /// The empty text.
    pub const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; N],
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    fn put(&self, out: &mut Writer<'_>) -> Result<()> {
        out.write(&[self.len])?;
        out.write(&self.bytes)
    }

    fn take(from: &mut Reader<'_>) -> Result<Self> {
        let len = u8::decode(from)?;
        let raw = from.take(N)?;
        if usize::from(len) > N {
            return Err(Status::INVALID_ARGUMENT);
        }
        // Only the bytes the length names are text; the rest is padding and must be zero.
        if raw[usize::from(len)..].iter().any(|byte| *byte != 0) {
            return Err(Status::INVALID_ARGUMENT);
        }
        core::str::from_utf8(&raw[..usize::from(len)]).map_err(|_| Status::INVALID_ARGUMENT)?;
        let mut bytes = [0u8; N];
        bytes.copy_from_slice(raw);
        Ok(Self { len, bytes })
    }
}

/// One named check a device offers, as a driver declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The stable name: `[a-z0-9_]`, 1 to [`MAX_TEST_NAME`] bytes, not `all`.
    pub name: &'static str,
    /// One line for a person.
    pub description: &'static str,
    /// The test changes what the device does while it runs, and puts it back after.
    pub intrusive: bool,
    /// The test needs an operator to confirm a fixture (`run(name, confirmed = true)`).
    pub operator: bool,
    /// What the test needs in the room or on the board, empty for none.
    pub fixture: &'static str,
    /// An upper bound on the run, in milliseconds. Data for a UI, not a promise.
    pub expect_ms: u32,
}

impl Spec {
    /// A test that only reads the device.
    pub const fn read(name: &'static str, description: &'static str, expect_ms: u32) -> Self {
        Self {
            name,
            description,
            intrusive: false,
            operator: false,
            fixture: "",
            expect_ms,
        }
    }

    /// A test that changes the device while it runs, given the fixture it needs.
    pub const fn intrusive(
        name: &'static str,
        description: &'static str,
        fixture: &'static str,
        expect_ms: u32,
    ) -> Self {
        Self {
            name,
            description,
            intrusive: true,
            operator: false,
            fixture,
            expect_ms,
        }
    }

    /// Whether `name` is a name a test can have: `[a-z0-9_]`, 1 to [`MAX_TEST_NAME`] bytes, not
    /// `all`.
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= MAX_TEST_NAME
            && name != "all"
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }
}

/// A measured value: `value * 10^exponent` in `unit`, and the limits it was compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measure {
    pub name: Text<MAX_MEASURE_NAME>,
    pub value: i64,
    pub exponent: i8,
    pub unit: Text<MAX_UNIT>,
    pub low: Option<i64>,
    pub high: Option<i64>,
}

impl Measure {
    /// A value with no limits.
    pub const fn new(name: &str, value: i64, exponent: i8, unit: &str) -> Self {
        Self {
            name: Text::new(name),
            value,
            exponent,
            unit: Text::new(unit),
            low: None,
            high: None,
        }
    }

    /// The same value, compared against `low` and `high`.
    pub const fn within(mut self, low: i64, high: i64) -> Self {
        self.low = Some(low);
        self.high = Some(high);
        self
    }

    fn put(&self, out: &mut Writer<'_>) -> Result<()> {
        self.name.put(out)?;
        self.value.encode(out)?;
        self.exponent.encode(out)?;
        self.unit.put(out)?;
        self.low.unwrap_or(0).encode(out)?;
        self.high.unwrap_or(0).encode(out)?;
        let flags = u8::from(self.low.is_some()) | (u8::from(self.high.is_some()) << 1);
        flags.encode(out)
    }

    fn take(from: &mut Reader<'_>) -> Result<Self> {
        let name = Text::take(from)?;
        let value = i64::decode(from)?;
        let exponent = i8::decode(from)?;
        let unit = Text::take(from)?;
        let low = i64::decode(from)?;
        let high = i64::decode(from)?;
        let flags = u8::decode(from)?;
        if flags & !0b11 != 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            name,
            value,
            exponent,
            unit,
            low: (flags & 1 != 0).then_some(low),
            high: (flags & 2 != 0).then_some(high),
        })
    }
}

/// What a test found: the body of every result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub outcome: Outcome,
    /// A code from [`reason`].
    pub reason: u16,
    pub detail: Text<MAX_DETAIL>,
    measures: [Option<Measure>; MAX_MEASURES],
}

impl Record {
    pub const fn new(outcome: Outcome, reason: u16) -> Self {
        Self {
            outcome,
            reason,
            detail: Text::empty(),
            measures: [None; MAX_MEASURES],
        }
    }

    /// A passing result.
    pub const fn pass(reason: u16) -> Self {
        Self::new(Outcome::Pass, reason)
    }

    /// A failing result.
    pub const fn fail(reason: u16) -> Self {
        Self::new(Outcome::Fail, reason)
    }

    /// A skipped result: the board lacks what the test needs.
    pub const fn skipped(reason: u16) -> Self {
        Self::new(Outcome::Skipped, reason)
    }

    /// A result that says the device does not offer the test.
    pub const fn not_supported(reason: u16) -> Self {
        Self::new(Outcome::NotSupported, reason)
    }

    /// Sets the short text. Cut to [`MAX_DETAIL`] bytes.
    pub const fn said(mut self, detail: &str) -> Self {
        self.detail = Text::new(detail);
        self
    }

    /// Adds a measured value. A fifth is not kept: [`MAX_MEASURES`] is the most one result holds.
    pub fn measure(mut self, measure: Measure) -> Self {
        if let Some(slot) = self.measures.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(measure);
        }
        self
    }

    /// The measured values, in the order they were added.
    pub fn measures(&self) -> impl Iterator<Item = &Measure> {
        self.measures.iter().flatten()
    }

    /// Writes the record: [`RECORD_BYTES`] bytes.
    pub fn write(&self, out: &mut Writer<'_>) -> Result<()> {
        self.outcome.code().encode(out)?;
        self.reason.encode(out)?;
        self.detail.put(out)?;
        let count = self.measures().count() as u8;
        count.encode(out)?;
        for slot in &self.measures {
            match slot {
                Some(measure) => measure.put(out)?,
                None => Measure::new("", 0, 0, "").put(out)?,
            }
        }
        Ok(())
    }

    /// Reads a record written by [`Record::write`]. A record with an unknown outcome, a count
    /// above [`MAX_MEASURES`], or unused measure slots that are not empty is refused.
    pub fn read(from: &mut Reader<'_>) -> Result<Self> {
        let outcome = Outcome::from_code(u8::decode(from)?).ok_or(Status::INVALID_ARGUMENT)?;
        let reason = u16::decode(from)?;
        let detail = Text::take(from)?;
        let count = usize::from(u8::decode(from)?);
        if count > MAX_MEASURES {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut measures = [None; MAX_MEASURES];
        for (index, slot) in measures.iter_mut().enumerate() {
            let measure = Measure::take(from)?;
            if index < count {
                *slot = Some(measure);
            } else if measure != Measure::new("", 0, 0, "") {
                // An unused slot is written empty, so a record that is not is malformed.
                return Err(Status::INVALID_ARGUMENT);
            }
        }
        Ok(Self {
            outcome,
            reason,
            detail,
            measures,
        })
    }
}

/// The `tests` list: a count, then one record per test.
pub fn write_tests(specs: &[Spec], out: &mut [u8]) -> Result<usize> {
    let mut writer = Writer::new(out);
    (specs.len() as u8).encode(&mut writer)?;
    for spec in specs {
        write_test(
            &mut writer,
            spec.name,
            spec.description,
            spec.intrusive,
            spec.operator,
            spec.fixture,
            spec.expect_ms,
        )?;
    }
    Ok(writer.len())
}

/// One test record of the `tests` list, under `name`: a board writes its members' tests under
/// `member:test` names with this.
pub fn write_test(
    out: &mut Writer<'_>,
    name: &str,
    description: &str,
    intrusive: bool,
    operator: bool,
    fixture: &str,
    expect_ms: u32,
) -> Result<()> {
    Text::<MAX_NAME>::new(name).put(out)?;
    Text::<MAX_DESCRIPTION>::new(description).put(out)?;
    u8::from(intrusive).encode(out)?;
    u8::from(operator).encode(out)?;
    Text::<MAX_FIXTURE>::new(fixture).put(out)?;
    expect_ms.encode(out)
}

/// One `last` entry, under `name`: see [`write_test`].
pub fn write_last_entry(out: &mut Writer<'_>, name: &str, record: &Record) -> Result<()> {
    Text::<MAX_NAME>::new(name).put(out)?;
    record.write(out)
}

/// One entry of the `tests` list, as a client reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestEntry {
    pub name: Text<MAX_NAME>,
    pub description: Text<MAX_DESCRIPTION>,
    pub intrusive: bool,
    pub operator: bool,
    pub fixture: Text<MAX_FIXTURE>,
    pub expect_ms: u32,
}

/// The entries of a `tests` reply, read in order.
pub fn read_tests(bytes: &[u8]) -> Result<impl Iterator<Item = TestEntry> + '_> {
    let mut check = Reader::new(bytes);
    let count = u8::decode(&mut check)?;
    if usize::from(count) > MAX_LISTED {
        return Err(Status::INVALID_ARGUMENT);
    }
    for _ in 0..count {
        read_entry(&mut check)?;
    }
    let mut from = Reader::new(bytes);
    let count = u8::decode(&mut from)?;
    Ok((0..count).filter_map(move |_| read_entry(&mut from).ok()))
}

fn read_entry(from: &mut Reader<'_>) -> Result<TestEntry> {
    let name = Text::take(from)?;
    let description = Text::take(from)?;
    let intrusive = u8::decode(from)? != 0;
    let operator = u8::decode(from)? != 0;
    let fixture = Text::take(from)?;
    let expect_ms = u32::decode(from)?;
    Ok(TestEntry {
        name,
        description,
        intrusive,
        operator,
        fixture,
        expect_ms,
    })
}

/// One `last` entry: a name and its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastEntry {
    pub name: Text<MAX_NAME>,
    pub record: Record,
}

/// The entries of a `last` reply, read in order. Malformed bytes end the list.
pub fn read_last(bytes: &[u8]) -> Result<impl Iterator<Item = LastEntry> + '_> {
    let mut from = Reader::new(bytes);
    let count = u8::decode(&mut from)?;
    Ok((0..count).map_while(move |_| {
        let name = Text::take(&mut from).ok()?;
        let record = Record::read(&mut from).ok()?;
        Some(LastEntry { name, record })
    }))
}

/// The `last` list: a count, then each test that has a result, with its name.
pub fn write_last(entries: &[(&str, Record)], out: &mut [u8]) -> Result<usize> {
    let mut writer = Writer::new(out);
    (entries.len() as u8).encode(&mut writer)?;
    for (name, record) in entries {
        write_last_entry(&mut writer, name, record)?;
    }
    Ok(writer.len())
}

/// Serves `self-test/1`. Methods are numbered as the contract declares them; a method's number
/// never changes.
#[contract("self-test", version = 1)]
pub trait SelfTest {
    /// Writes the device's tests, [`write_tests`] form. `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(1)]
    fn tests(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Starts test `name` as a long job. Progress is milliseconds of the test's `expect_ms`; its
    /// result is a [`Record`], written with [`Record::write`]. `INVALID_ARGUMENT` for a name the
    /// device does not list. `BUSY` while another test runs, or for an intrusive test while
    /// another caller uses the device. `confirmed` is the operator's word for a test that needs one.
    #[method(2, job)]
    fn run(&mut self, name: &str, confirmed: bool) -> Result<JobId>;
    /// Writes the last result of each test that has one, [`write_last`] form.
    #[method(3)]
    fn last(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}

/// The test's own steps, which a driver writes. [`Runner`] calls them from the driver's tick.
pub trait Bench {
    /// Does one step of test `test` (an index into the driver's specs) at `now_ns`, and says
    /// when the next step is due or that the test is done. Keeps what it must restore in
    /// `scratch`.
    fn step(&mut self, test: usize, now_ns: u64, scratch: &mut Scratch) -> Flow;
    /// Puts back what the test changed, from `scratch`. `Ok` when nothing was changed or it is
    /// all back. A failed write is an `Err`: the runner tries again later.
    fn restore(&mut self, scratch: &mut Scratch) -> Result<()>;
}

/// What a step says next.
// A record is fixed size and copied by value on purpose: boxing it would need an allocator.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Not done: call again at this time (the host's clock, in nanoseconds).
    Wait(u64),
    /// Done, with this result. The runner puts back what the test changed before it answers.
    Done(Record),
}

/// What a test keeps between its steps, and what it restores from. The bench owns its meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scratch {
    /// Which step the test is on.
    pub step: u8,
    /// Values read before the test changed anything, as the bench needs them.
    pub saved: [i64; 4],
}

/// The callers a device has seen, for the in-use rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Users {
    ids: [Option<CallerId>; MAX_USERS],
}

impl Users {
    /// `caller` has called the device: it is a user until it goes.
    pub fn note(&mut self, caller: CallerId) {
        if self.ids.contains(&Some(caller)) {
            return;
        }
        // A caller that does not fit is not kept: a full set already holds at least seven callers
        // other than any one asker, so `others_than` answers the same without it.
        if let Some(slot) = self.ids.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(caller);
        }
    }

    /// `caller` has gone.
    pub fn gone(&mut self, caller: CallerId) {
        for slot in &mut self.ids {
            if *slot == Some(caller) {
                *slot = None;
            }
        }
    }

    /// Whether any caller other than `asker` is using the device. More callers than the set
    /// holds count as using it.
    pub fn others_than(&self, asker: CallerId) -> bool {
        self.ids.iter().flatten().any(|caller| *caller != asker)
    }
}

/// The restore that is still owed, after a test was cancelled and its put-back failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Owed {
    scratch: Scratch,
    next_ns: u64,
    backoff_ns: u64,
}

/// The first put-back retry after a failure, and the longest wait between tries. The failure
/// rule's cap, so a test's restore is never abandoned.
pub const RESTORE_FIRST_NS: u64 = 100_000_000;
pub const RESTORE_CAP_NS: u64 = 8_000_000_000;

/// The run in progress, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Steps are being taken.
    Running,
    /// The test answered, but its put-back failed: the answer waits for it.
    Restoring {
        record: Record,
        next_ns: u64,
        backoff_ns: u64,
    },
    /// Over: answered on the next `job` call.
    Over(Answer),
}

// A record is fixed size and copied by value on purpose: boxing it would need an allocator.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Record(Record),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Active {
    job: JobId,
    test: usize,
    started_ns: Option<u64>,
    /// When the next step is due: a tick before it takes no step.
    due_ns: u64,
    scratch: Scratch,
    stage: Stage,
}

/// The bookkeeping a driver keeps for `self-test/1`. Fixed size; allocates nothing.
#[derive(Debug, Clone)]
pub struct Runner {
    specs: &'static [Spec],
    last: [Option<Record>; MAX_TESTS],
    active: Option<Active>,
    owed: Option<Owed>,
    users: Users,
    next_job: u64,
    now_ns: u64,
}

impl Runner {
    /// A runner for the tests `specs` (at most [`MAX_TESTS`], with valid names).
    pub const fn new(specs: &'static [Spec]) -> Self {
        Self {
            specs,
            last: [None; MAX_TESTS],
            active: None,
            owed: None,
            users: Users {
                ids: [None; MAX_USERS],
            },
            next_job: 0,
            now_ns: 0,
        }
    }

    /// The tests this runner was built for.
    pub fn specs(&self) -> &'static [Spec] {
        self.specs
    }

    /// Records that `caller` has called the device. Call from the driver's `caller`.
    pub fn caller(&mut self, caller: CallerId) {
        self.users.note(caller);
    }

    /// Records that `caller` has gone. Call from the driver's `caller_gone`.
    pub fn caller_gone(&mut self, caller: CallerId) {
        self.users.gone(caller);
    }

    /// Whether a test is running, or owes a put-back. A driver refuses its own normal changes
    /// while this is true, so no one's write is overwritten by a put-back.
    pub fn busy(&self) -> bool {
        self.active.is_some() || self.owed.is_some()
    }

    /// Why the device is not as it should be, for its health: a put-back that is owed.
    pub fn health(&self) -> Option<&'static str> {
        self.owed
            .map(|_| "a self-test could not put the device back: retried")
    }

    /// The first tick a test or a put-back is due, or `None` when nothing is. Costs nothing and
    /// allocates nothing while no test runs.
    pub fn next(&self) -> Option<u64> {
        if let Some(owed) = self.owed {
            return Some(owed.next_ns);
        }
        match self.active?.stage {
            Stage::Running => Some(self.now_ns),
            Stage::Restoring { next_ns, .. } => Some(next_ns),
            // Answered by the `job` call that follows each tick, so no tick is owed for it.
            Stage::Over(_) => None,
        }
    }

    /// Starts test `name` for `asker`, and returns its job. See [`SelfTest::run`] for the errors.
    /// A test that needs an operator who has not confirmed is answered `skipped` at once, without
    /// touching the device.
    pub fn start(&mut self, name: &str, confirmed: bool, asker: CallerId) -> Result<JobId> {
        let test = self
            .specs
            .iter()
            .position(|spec| spec.name == name)
            .ok_or(Status::INVALID_ARGUMENT)?;
        if self.busy() {
            return Err(Status::BUSY);
        }
        let spec = self.specs[test];
        // An operator test that is not confirmed is answered `skipped` without touching the device,
        // so it is not refused for use: the skip comes first.
        let unconfirmed = spec.operator && !confirmed;
        if spec.intrusive && !unconfirmed && self.users.others_than(asker) {
            return Err(Status::BUSY);
        }
        self.next_job = self.next_job.wrapping_add(1).max(1);
        let job = JobId(self.next_job);
        let stage = if unconfirmed {
            let record = Record::skipped(reason::NEEDS_OPERATOR)
                .said("the test needs an operator to confirm; run it with confirmed = true");
            self.last[test] = Some(record);
            Stage::Over(Answer::Record(record))
        } else {
            Stage::Running
        };
        self.active = Some(Active {
            job,
            test,
            started_ns: None,
            due_ns: 0,
            scratch: Scratch::default(),
            stage,
        });
        Ok(job)
    }

    /// Takes the steps due at `now_ns`, and puts back a test's changes when the test is over.
    /// Returns when to be called next, as [`Lifecycle::tick`](lemnos_core::Lifecycle::tick) does.
    pub fn tick(&mut self, now_ns: u64, bench: &mut dyn Bench) -> Option<u64> {
        self.now_ns = now_ns;
        if let Some(owed) = self.owed
            && now_ns >= owed.next_ns
        {
            let mut scratch = owed.scratch;
            self.owed = if bench.restore(&mut scratch).is_ok() {
                None
            } else {
                Some(Owed {
                    scratch,
                    next_ns: now_ns + owed.backoff_ns,
                    backoff_ns: (owed.backoff_ns * 2).min(RESTORE_CAP_NS),
                })
            };
        }
        let Some(mut active) = self.active else {
            return self.next();
        };
        match active.stage {
            Stage::Over(_) => {}
            Stage::Running => {
                // A tick can come early (the host asks at the time it was given, and at any call):
                // a step is taken only once its time has come.
                if now_ns < active.due_ns {
                    self.active = Some(active);
                    return Some(active.due_ns);
                }
                active.started_ns.get_or_insert(now_ns);
                match bench.step(active.test, now_ns, &mut active.scratch) {
                    Flow::Wait(next) => {
                        active.due_ns = next.max(now_ns);
                        self.active = Some(active);
                        return Some(active.due_ns);
                    }
                    Flow::Done(record) => {
                        if bench.restore(&mut active.scratch).is_ok() {
                            active.stage = Stage::Over(Answer::Record(record));
                        } else {
                            active.stage = Stage::Restoring {
                                record: as_restore_failed(record),
                                next_ns: now_ns + RESTORE_FIRST_NS,
                                backoff_ns: RESTORE_FIRST_NS * 2,
                            };
                        }
                    }
                }
            }
            Stage::Restoring {
                record,
                next_ns,
                backoff_ns,
            } => {
                if now_ns >= next_ns {
                    if bench.restore(&mut active.scratch).is_ok() {
                        active.stage = Stage::Over(Answer::Record(record));
                    } else {
                        active.stage = Stage::Restoring {
                            record,
                            next_ns: now_ns + backoff_ns,
                            backoff_ns: (backoff_ns * 2).min(RESTORE_CAP_NS),
                        };
                    }
                }
            }
        }
        if let Stage::Over(Answer::Record(record)) = active.stage {
            self.last[active.test] = Some(record);
        }
        self.active = Some(active);
        self.next()
    }

    /// How the job stands, for the driver's `job`. A finished test's record is written to
    /// `result`, and the job is then forgotten.
    pub fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(active) = self.active.filter(|active| active.job == job) else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        match active.stage {
            Stage::Over(Answer::Record(record)) => {
                self.active = None;
                match record.write(result) {
                    Ok(()) => JobState::Finished(Status::OK),
                    Err(status) => JobState::Finished(status),
                }
            }
            Stage::Over(Answer::Cancelled) => {
                self.active = None;
                JobState::Finished(Status::CANCELLED)
            }
            Stage::Running | Stage::Restoring { .. } => {
                let spec = self.specs[active.test];
                let total = u64::from(spec.expect_ms);
                let started = active.started_ns.unwrap_or(self.now_ns);
                let done = (self.now_ns.saturating_sub(started) / 1_000_000).min(total);
                JobState::Running { done, total }
            }
        }
    }

    /// Stops job `job`: the test's changes are put back at once, and the job answers
    /// `CANCELLED`. A put-back that fails is retried on the ticks. A cancelled test leaves `last`
    /// as it was.
    pub fn cancel(&mut self, job: JobId, bench: &mut dyn Bench) {
        let Some(active) = self.active.filter(|active| active.job == job) else {
            return;
        };
        // A test that already answered keeps its answer: the host reads it as it really ended.
        if let Stage::Over(_) = active.stage {
            return;
        }
        let mut scratch = active.scratch;
        if bench.restore(&mut scratch).is_err() {
            self.owed = Some(Owed {
                scratch,
                next_ns: self.now_ns + RESTORE_FIRST_NS,
                backoff_ns: RESTORE_FIRST_NS * 2,
            });
        }
        self.active = Some(Active {
            stage: Stage::Over(Answer::Cancelled),
            ..active
        });
    }

    /// Writes the `tests` list.
    pub fn write_tests(&self, out: &mut [u8]) -> Result<usize> {
        write_tests(self.specs, out)
    }

    /// Writes the `last` list.
    pub fn write_last(&self, out: &mut [u8]) -> Result<usize> {
        let mut entries: [(&str, Record); MAX_TESTS] =
            [("", Record::new(Outcome::Pass, 0)); MAX_TESTS];
        let mut count = 0;
        for (index, spec) in self.specs.iter().enumerate() {
            if let Some(record) = self.last[index] {
                entries[count] = (spec.name, record);
                count += 1;
            }
        }
        write_last(&entries[..count], out)
    }

    /// The last result of test `name`, if it has one.
    pub fn last(&self, name: &str) -> Option<Record> {
        let index = self.specs.iter().position(|spec| spec.name == name)?;
        self.last[index]
    }
}

/// A test's answer when its put-back failed: a fail, keeping the test's own reason when it
/// already failed.
fn as_restore_failed(record: Record) -> Record {
    if record.outcome == Outcome::Fail {
        return record.said("the device could not be put back as it was found: retried");
    }
    Record::fail(reason::RESTORE_FAILED)
        .said("the device could not be put back as it was found: retried")
}
