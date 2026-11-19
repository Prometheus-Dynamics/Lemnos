//! A driver's notes reach the daemon's log, each as `lemnosd: <device id>: <text>`, once per note,
//! and the notes a full ring drops are reported as a count.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::start_board_logged;
use lemnos_core::{CallerId, Config, Lifecycle, MethodId, Note, Notes, Result, contract, driver};
use lemnos_host::entry;
use lemnos_sim::SimBoard;

#[contract("noter", version = 1)]
pub trait Noting {
    /// Pushes ten notes, valued 0 to 9, in one call.
    #[method(1)]
    fn burst(&mut self) -> Result<()>;
}

#[driver("noter", provides = [Noting])]
pub struct Noter {
    notes: Notes,
}

impl Lifecycle for Noter {
    type Needs = NoterNeeds;

    fn start(_: NoterNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            notes: Notes::new(),
        })
    }

    fn notes(&mut self) -> Option<&mut Notes> {
        Some(&mut self.notes)
    }
}

impl Noting for Noter {
    fn burst(&mut self) -> Result<()> {
        for value in 0..10 {
            self.notes.push(Note::with_value("burst", value));
        }
        Ok(())
    }
}

#[test]
fn a_note_reaches_the_log_with_its_device_id_and_a_full_ring_is_counted() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    let running = start_board_logged(
        "[[devices]]\nid = \"n\"\ndriver = \"noter\"\n",
        vec![entry::<Noter>()],
        SimBoard::new(),
        None,
        Some(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string());
        })),
    );
    let host = running.service.as_ref().unwrap().host();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(info) = host.device("n")
            && host
                .call_as(
                    CallerId::LOCAL,
                    info.index,
                    MethodId {
                        contract: 0,
                        method: 1,
                    },
                    &[],
                    &mut [],
                )
                .is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline, "the noter never came up");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The log is read on the daemon's look, a quarter second apart: wait for the lines, then look
    // again to see that none is repeated.
    let wanted = |lines: &[String]| {
        lines
            .iter()
            .filter(|l| l.starts_with("lemnosd: n: burst: "))
            .count()
            == 8
            && lines
                .iter()
                .any(|l| l == "lemnosd: n: 2 notes dropped before they were logged")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !wanted(&lines.lock().unwrap()) {
        assert!(
            Instant::now() < deadline,
            "the notes never reached the log: {:?}",
            lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(600));
    let lines = lines.lock().unwrap();
    let burst: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("lemnosd: n: burst: "))
        .collect();
    assert_eq!(
        burst.len(),
        8,
        "each note once, the two dropped ones absent: {burst:?}"
    );
    for (at, line) in burst.iter().enumerate() {
        assert_eq!(**line, format!("lemnosd: n: burst: {at}"));
    }
}
