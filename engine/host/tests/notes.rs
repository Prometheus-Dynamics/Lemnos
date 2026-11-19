//! A driver's notes: the host takes them in order, counts what a full ring drops, and does not allocate
//! in the steady state.

use std::sync::Arc;

use lemnos_core::{CallerId, Config, Lifecycle, MethodId, Note, Notes, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimClock};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[contract("noter", version = 1)]
pub trait Noting {
    /// Pushes ten notes, valued 0 to 9, in one call: a ring of eight keeps the first eight.
    #[method(1)]
    fn burst(&mut self) -> Result<()>;
    /// Pushes one note valued 1.
    #[method(2)]
    fn one(&mut self) -> Result<()>;
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

    fn one(&mut self) -> Result<()> {
        self.notes.push(Note::with_value("one", 1));
        Ok(())
    }
}

fn host() -> (Host, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"n\"\ndriver = \"noter\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Noter>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

#[test]
fn a_burst_is_taken_in_order_and_what_a_full_ring_drops_is_counted_once() {
    let (host, _) = host();
    let index = host.device("n").unwrap().index;
    host.call_as(CallerId::LOCAL, index, method(1), &[], &mut [])
        .unwrap();
    let mut got = Vec::new();
    let dropped = host.take_notes(index, &mut |note| got.push(note)).unwrap();
    assert_eq!(dropped, 2, "the last two of the ten did not fit");
    let values: Vec<u32> = got.iter().map(|note| note.value.unwrap()).collect();
    assert_eq!(values, (0..8).collect::<Vec<u32>>());
    assert!(got.iter().all(|note| note.text == "burst"));
    assert_eq!(
        host.take_notes(index, &mut |_| {}),
        Some(0),
        "nothing is left, and the drop was reported"
    );
}

#[test]
fn a_device_with_no_notes_to_take_answers_none_for_a_missing_index() {
    let (host, _) = host();
    assert_eq!(host.take_notes(99, &mut |_| {}), None);
}

#[test]
fn steady_state_notes_allocate_nothing() {
    let (host, _) = host();
    let index = host.device("n").unwrap().index;
    let mut seen = 0u32;
    // The first round allocates whatever the host's own paths need once; the rounds after it allocate nothing.
    for _ in 0..16 {
        host.call_as(CallerId::LOCAL, index, method(2), &[], &mut [])
            .unwrap();
        host.take_notes(index, &mut |note| seen += note.value.unwrap());
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            host.call_as(CallerId::LOCAL, index, method(2), &[], &mut [])
                .unwrap();
            host.take_notes(index, &mut |note| seen += note.value.unwrap());
        }
    });
    assert_eq!(seen, 1016, "every note reached the take");
    assert_eq!(allocated, 0, "steady-state notes allocate nothing");
}
