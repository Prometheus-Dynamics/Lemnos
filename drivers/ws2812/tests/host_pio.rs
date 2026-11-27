//! A strip on a PIO output, bound through the host as the daemon binds it: the host hands the
//! backend's PIO machine to the driver. The driver's other tests give it the machine straight from
//! a backend, which skips the host; a host that does not pass the machine on refuses every
//! `pio-out:` strip with "no SPI bus" while the backend has the machine.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_core::{Held, LocalCaller, Pio, Provider, Result, Status, Tag};
use lemnos_driver_ws2812::Ws2812;
use lemnos_hal::ErrorKind;
use lemnos_hal::pio::{PioJob, PioMachine, PioOperation};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, entry};
use lemnos_sim::SimBoard;

const OUTPUT: &str = "pio-out:gpio=13";

#[derive(Default)]
struct Seen {
    loads: usize,
    transactions: usize,
}

struct Machine(Arc<Mutex<Seen>>);

impl PioMachine for Machine {
    fn load(&mut self, _job: &PioJob<'_>) -> std::result::Result<(), ErrorKind> {
        self.0.lock().unwrap().loads += 1;
        Ok(())
    }

    fn transaction(
        &mut self,
        _operations: &mut [PioOperation<'_>],
    ) -> std::result::Result<(), ErrorKind> {
        self.0.lock().unwrap().transactions += 1;
        Ok(())
    }
}

/// The sim's devices, one PIO output by its board-file name, and no SPI device (as the board that
/// showed the fault has none).
struct PioBoard {
    sim: SimBoard,
    seen: Arc<Mutex<Seen>>,
}

impl Provider for PioBoard {
    fn provided(&self, need: &str) -> bool {
        need == OUTPUT || self.sim.provided(need)
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        self.sim.device(need, tag)
    }

    fn pio(&mut self, need: &str) -> Result<Pio> {
        if need != OUTPUT {
            return Err(Status::NOT_FOUND);
        }
        Ok(Held::boxed(Box::new(Machine(Arc::clone(&self.seen)))))
    }
}

fn board(owner: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"pio\"\n\
         [[devices]]\nid = \"ring\"\ndriver = \"ws2812\"\nbus = \"{OUTPUT}\"\n{owner}\
         config = {{ count = 16, wire = \"rgb\" }}\n"
    )
}

fn start(owner: &str) -> (Host, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&board(owner)).unwrap(),
        &[entry::<Ws2812>()],
        Box::new(PioBoard {
            sim,
            seen: Arc::clone(&seen),
        }),
        Arc::new(clock.clone()),
        lemnos_test_options::options(),
    )
    .unwrap();
    clock.advance(Duration::from_millis(10));
    (host, seen)
}

#[test]
fn a_strip_on_a_pio_output_binds_through_the_host_and_loads_its_program() {
    let (host, seen) = start("");
    let state = host.device("ring").unwrap().state;
    assert!(
        matches!(state, DeviceState::Running),
        "the strip should run on the PIO output, not {state:?}"
    );
    assert_eq!(seen.lock().unwrap().loads, 1, "the program is loaded once");
}

#[test]
fn a_host_owned_strip_on_a_pio_output_never_loads_or_runs_the_machine() {
    let (host, seen) = start("owner = \"host\"\n");
    let state = host.device("ring").unwrap().state;
    let seen = seen.lock().unwrap();
    assert_eq!(
        (seen.loads, seen.transactions),
        (0, 0),
        "a host-owned PIO output is driven by nothing here ({state:?})"
    );
}
