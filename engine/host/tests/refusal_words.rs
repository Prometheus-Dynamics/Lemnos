//! A refusal's words ([`Lifecycle::refusal`]) and a failed job's words: the host asks the driver
//! right after a failed method, keeps the words for that caller until it takes them or calls the
//! device again, bounds them, and passes a failed job's UTF-8 words on with its status.

use lemnos_core::{
    CallerId, Config, JobId, JobState, Lifecycle, MethodId, Reply, Result, Status, contract,
    driver, say,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, JobEvent, JobUpdate, Options, REFUSAL_WORDS, entry};
use lemnos_sim::SimBoard;

#[contract("example.words", version = 1)]
pub trait Words {
    /// Refused `NOT_PERMITTED`, with `words` as its cause; no words when `words` is empty, and
    /// twice the bound of two-byte characters when it is `long`.
    #[method(1)]
    fn refuse(&mut self, words: &str) -> Result<()>;
    /// Answers.
    #[method(2)]
    fn fine(&mut self) -> Result<()>;
    /// A job that fails at once; `how` 0 says words, 1 writes bytes that are not UTF-8, 2 writes
    /// words longer than the bound; 3 refuses the start `BUSY`, with words.
    #[method(3, job, stall = 60, result = bytes)]
    fn fail(&mut self, how: u8) -> Result<JobId>;
}

#[driver("words", provides = [Words])]
pub struct Sayer {
    why: Option<String>,
    how: Option<u8>,
}

impl Lifecycle for Sayer {
    type Needs = SayerNeeds;
    const JOBS: bool = true;

    fn start(_: SayerNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            why: None,
            how: None,
        })
    }

    fn refusal(&mut self, _status: Status, words: &mut Reply<'_>) {
        if let Some(why) = self.why.take() {
            say(words, &why);
        }
    }

    fn job(&mut self, _: JobId, result: &mut Reply<'_>) -> JobState {
        match self.how.take() {
            Some(0) => say(result, "the part it needs is missing"),
            Some(1) => {
                let _ = result.write(&[0xff, 0xfe, 0xfd]);
            }
            Some(_) => say(result, &"é".repeat(REFUSAL_WORDS)),
            None => {}
        }
        JobState::Finished(Status::IO)
    }
}

impl Words for Sayer {
    fn refuse(&mut self, words: &str) -> Result<()> {
        self.why = match words {
            "" => None,
            "long" => Some("ü".repeat(REFUSAL_WORDS)),
            words => Some(words.to_string()),
        };
        Err(Status::NOT_PERMITTED)
    }

    fn fine(&mut self) -> Result<()> {
        Ok(())
    }

    fn fail(&mut self, how: u8) -> Result<JobId> {
        if how == 3 {
            self.why = Some("a job is running".to_string());
            return Err(Status::BUSY);
        }
        self.how = Some(how);
        Ok(JobId(1))
    }
}

fn host() -> (Host, u32) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"w\"\ndriver = \"words\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Sayer>()],
        Box::new(sim),
        std::sync::Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    let index = host.device("w").unwrap().index;
    (host, index)
}

const REFUSE: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const FINE: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const FAIL: MethodId = MethodId {
    contract: 0,
    method: 3,
};

fn text_arg(text: &str) -> Vec<u8> {
    let mut args = (text.len() as u32).to_le_bytes().to_vec();
    args.extend_from_slice(text.as_bytes());
    args
}

fn refuse(host: &Host, index: u32, caller: CallerId, words: &str) {
    let status = host.call_as(caller, index, REFUSE, &text_arg(words), &mut []);
    assert_eq!(status, Err(Status::NOT_PERMITTED));
}

#[test]
fn a_refusals_words_are_its_callers_and_taken_once() {
    let (host, index) = host();
    let (one, two) = (CallerId(1), CallerId(2));
    refuse(&host, index, one, "the slot failed a trial");
    refuse(&host, index, two, "the slot is pinned");
    assert_eq!(
        host.take_refusal(index, two).as_deref(),
        Some("the slot is pinned")
    );
    assert_eq!(
        host.take_refusal(index, one).as_deref(),
        Some("the slot failed a trial")
    );
    assert_eq!(host.take_refusal(index, one), None, "taken once");
}

#[test]
fn a_refusal_without_words_says_none_and_never_an_earlier_ones() {
    let (host, index) = host();
    let caller = CallerId(3);
    refuse(&host, index, caller, "an earlier cause");
    // Not taken: the caller's next call forgets them, whether it fails or not.
    refuse(&host, index, caller, "");
    assert_eq!(host.take_refusal(index, caller), None);
    refuse(&host, index, caller, "a cause");
    host.call_as(caller, index, FINE, &[], &mut []).unwrap();
    assert_eq!(host.take_refusal(index, caller), None);
}

#[test]
fn a_refusals_words_are_bounded_at_a_character() {
    let (host, index) = host();
    let caller = CallerId(4);
    refuse(&host, index, caller, "long");
    let words = host.take_refusal(index, caller).unwrap();
    assert_eq!(words, "ü".repeat(REFUSAL_WORDS / 2));
    let mut out = [0u8; 9];
    let mut reply = Reply::new(&mut out);
    say(&mut reply, &words);
    // 9 bytes hold four two-byte characters; the fifth is never cut in half.
    assert_eq!(reply.written(), "üüüü".as_bytes());
}

fn failed_job(how: u8) -> (Status, Vec<u8>) {
    let (host, index) = host();
    host.start_job(CallerId(5), index, FAIL, &[how]).unwrap();
    host.run_ticks();
    let mut all: Vec<JobUpdate> = Vec::new();
    host.take_job_updates(&mut all);
    all.iter()
        .find_map(|u| match &u.event {
            JobEvent::Finished { status, result } => Some((*status, result.clone())),
            _ => None,
        })
        .expect("the job ended")
}

#[test]
fn a_failed_job_ends_with_its_words_bounded_and_only_if_they_are_text() {
    let (status, words) = failed_job(0);
    assert_eq!(status, Status::IO);
    assert_eq!(words, b"the part it needs is missing");
    let (status, bytes) = failed_job(1);
    assert_eq!(status, Status::IO);
    assert!(
        bytes.is_empty(),
        "bytes that are not UTF-8 are not passed on"
    );
    let (_, long) = failed_job(2);
    assert!(long.len() <= REFUSAL_WORDS, "{}", long.len());
    assert!(core::str::from_utf8(&long).is_ok());
}

#[test]
fn a_refused_job_start_keeps_its_words_for_its_caller() {
    let (host, index) = host();
    let caller = CallerId(6);
    assert_eq!(host.start_job(caller, index, FAIL, &[3]), Err(Status::BUSY));
    assert_eq!(host.take_refusal(index, CallerId(7)), None);
    assert_eq!(
        host.take_refusal(index, caller).as_deref(),
        Some("a job is running")
    );
}
