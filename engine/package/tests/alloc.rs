//! Steady-state calls into a package allocate nothing, on either side of the boundary, and a
//! package's driver gives back everything it was given.
//!
//! On its own in this file: the count is the whole process's.

mod common;
#[path = "common/quiet.rs"]
mod quiet;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{EXERCISE_ROOM, count, exercise_args, fixtures, index, kind, method, scratch};
use lemnos_core::Status;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options};
use lemnos_measure::test_alloc::{Counting, TOTAL};
use lemnos_package::{Loader, Mode, resources_held};
use lemnos_sim::Layout;
use lemnos_sim::SimBoard;
use quiet::QuietBoard;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const DEVICES: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "t"

[[devices]]
id = "user"
driver = "package-fixture"
uses = { peer = "chip" }

[[devices]]
id = "chip"
driver = "package-fixture"
uses = { bus = "i2c0" }

[[devices]]
id = "all"
driver = "package-fixture"

[devices.uses]
spi = "q-spi"
raw_spi = "q-raw-spi"
line = "q-line"
pwm = "q-pwm"
uart = "q-uart"
usb = "q-usb"
function = "q-function"
registers = "q-registers"
interrupt = "q-interrupt"
dma = "q-dma"
clocks = "q-clocks"
reset = "q-reset"
stream = "q-stream"
"#;

/// The package's own count of its allocations.
fn package_allocations(host: &Host, device: u32) -> u64 {
    let mut reply = [0; 8];
    assert_eq!(host.call(device, method(5), &[], &mut reply), Ok(8));
    u64::from_le_bytes(reply)
}

/// One call of the fixture's `exercise`, its arguments made ahead of time.
struct Step {
    kind: u8,
    step: u8,
    args: Vec<u8>,
    fails: bool,
}

fn step(kind: u8, step: u8, data: &[u8]) -> Step {
    let mut args = vec![0; data.len() + 10];
    let len = exercise_args(&mut args, kind, step, data);
    args.truncate(len);
    Step {
        kind,
        step,
        args,
        fails: false,
    }
}

/// Every call the fixture makes on every kind of resource, each resource used the way a driver
/// uses it in steady state, and one call that fails.
fn steps() -> Vec<Step> {
    let le32 = |value: u32| value.to_le_bytes();
    let le64 = |value: u64| value.to_le_bytes();
    let setup = [0x80, 6, 0, 1, 0, 0];
    let mut steps = vec![
        step(kind::SPI, 0, &[1, 2, 3]),
        step(kind::SPI, 1, &[4, 5]),
        step(kind::SPI, 2, &[6, 7]),
        step(kind::RAW_SPI, 0, &[3, 0x55]),
        step(kind::LINE, 0, &[1]),
        step(kind::LINE, 3, &[0]),
        step(kind::LINE, 2, &[]),
        step(kind::LINE, 1, &[]),
        step(kind::LINE, 4, &[]),
        step(
            kind::PWM,
            0,
            &[le64(20), le64(10), [1, 1, 0, 0, 0, 0, 0, 0]].concat(),
        ),
        step(kind::PWM, 1, &[]),
        step(kind::UART, 0, &le32(9600)),
        step(kind::UART, 1, b"ping"),
        step(kind::UART, 2, &[]),
        step(kind::REGISTERS, 0, &[]),
        step(kind::REGISTERS, 1, &le32(8)),
        step(kind::REGISTERS, 2, &[le32(4), le32(7)].concat()),
        step(kind::REGISTERS, 3, &[le32(4), le32(1), le32(8)].concat()),
        step(kind::REGISTERS, 4, &[le32(16), le32(1), le32(2)].concat()),
        step(kind::INTERRUPT, 0, &[]),
        step(kind::INTERRUPT, 2, &[]),
        step(kind::INTERRUPT, 3, &[]),
        step(kind::INTERRUPT, 1, &[]),
        step(kind::DMA, 0, &le64(64)),
        step(kind::DMA, 1, &[&le64(4)[..], &[1, 2, 3]].concat()),
        step(kind::DMA, 3, &[]),
        step(kind::DMA, 4, &[]),
        step(kind::DMA, 2, &[&le64(4)[..], &le32(3)].concat()),
        step(kind::DMA, 5, &[]),
        step(kind::CLOCKS, 0, &le32(2)),
        step(kind::CLOCKS, 3, &[&le32(2)[..], &le64(1_000_000)].concat()),
        step(kind::CLOCKS, 2, &le32(2)),
        step(kind::CLOCKS, 1, &le32(2)),
        step(kind::RESET, 0, &le32(1)),
        step(kind::RESET, 1, &le32(1)),
        step(kind::STREAM, 0, &[]),
        step(kind::STREAM, 1, &[]),
        step(kind::STREAM, 2, &[2, 5, 6, 7]),
        step(kind::STREAM, 3, &[0, 5]),
        step(kind::STREAM, 6, &[5]),
        step(kind::STREAM, 7, &[]),
        step(kind::STREAM, 8, &[]),
        step(kind::STREAM, 9, &[]),
        step(kind::STREAM, 5, &[]),
        step(kind::STREAM, 4, &[5, 3]),
        step(kind::USB_DEVICE, 0, &[]),
        step(kind::USB_DEVICE, 4, &[0]),
        step(kind::USB_DEVICE, 1, &[0]),
        step(kind::USB_DEVICE, 3, &[0, 1]),
        step(kind::USB_DEVICE, 6, &setup),
        step(kind::USB_DEVICE, 7, &[&setup[..], &[7, 7]].concat()),
        step(kind::USB_DEVICE, 8, &[0x81]),
        step(kind::USB_DEVICE, 9, &[0x02, 1, 2, 3]),
        step(kind::USB_DEVICE, 10, &[0x83]),
        step(kind::USB_DEVICE, 11, &[0x81]),
        step(kind::USB_DEVICE, 5, &[]),
        step(kind::USB_DEVICE, 2, &[0]),
        step(kind::USB_FUNCTION, 0, &[1, 2, 3]),
        step(kind::USB_FUNCTION, 2, &[]),
        step(kind::USB_FUNCTION, 3, &[9; 4]),
        step(kind::USB_FUNCTION, 4, &[3]),
        step(kind::USB_FUNCTION, 5, &[]),
        step(kind::USB_FUNCTION, 6, &[0x81, 1]),
        step(kind::USB_FUNCTION, 7, &[0x02]),
        step(kind::USB_FUNCTION, 8, &[0x81, 1, 2]),
        step(kind::USB_FUNCTION, 1, &[]),
    ];
    // A failure is steady state too: a register outside the window.
    let mut outside = step(kind::REGISTERS, 1, &le32(0x1000));
    outside.fails = true;
    steps.push(outside);
    steps
}

#[test]
fn steady_state_calls_through_a_package_allocate_nothing() {
    let mut loader = Loader::new(Mode::Open);
    loader.load(&fixtures().good).unwrap();
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c0");
    bus.attach(0x48, Layout::Bytes);
    bus.set_bytes(0x48, 0x10, &[0xa5]);
    let dir = scratch("alloc");
    std::fs::write(dir.join("buffer"), [7u8; 64]).unwrap();
    let clock = sim.clock();
    let board = QuietBoard {
        sim,
        file: std::fs::File::open(dir.join("buffer")).unwrap(),
    };
    let host = Host::start(
        &parse(DEVICES).unwrap(),
        &loader.entries(),
        Box::new(board),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    let (user, chip, all) = (
        index(&host, "user"),
        index(&host, "chip"),
        index(&host, "all"),
    );
    // One device taken, one bus, and one of each of the other thirteen kinds.
    assert_eq!(resources_held(), 15);

    let steps = steps();
    // Every kind of resource is in the round, and every function of every table.
    let mut kinds: Vec<u8> = steps.iter().map(|step| step.kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), 13);
    assert_eq!(steps.len(), 67);

    let mut args = Vec::new();
    args.extend_from_slice(&40u32.to_le_bytes());
    args.extend_from_slice(&2u32.to_le_bytes());
    let mut reply = [0; EXERCISE_ROOM];
    let round = |reply: &mut [u8; EXERCISE_ROOM]| {
        assert_eq!(count(&host, chip).map(|_| ()), Ok(()));
        assert_eq!(host.call(chip, method(2), &args, reply), Ok(4));
        // Through a device handle (a call, and its health), and through a bus handle.
        assert_eq!(host.call(user, method(6), &[], reply), Ok(4));
        assert_eq!(host.call(user, method(11), &[], reply), Ok(4));
        assert_eq!(host.call(chip, method(7), &[0x48, 0x10], reply), Ok(1));
        // A failure is steady state too.
        assert_eq!(
            host.call(chip, method(7), &[0x21, 0x10], reply),
            Err(Status::IO)
        );
        // Through a handle of every other kind.
        for step in &steps {
            let result = host.call(all, method(10), &step.args, reply);
            assert_eq!(
                result.is_err(),
                step.fails,
                "kind {} step {}: {result:?}",
                step.kind,
                step.step
            );
        }
        host.run_ticks();
    };
    // Warm up: lanes start, thread-locals are made.
    for _ in 0..3 {
        round(&mut reply);
    }
    assert_eq!(host.device("all").map(|_| ()), Some(()));
    let _ = host.devices();

    let inside = package_allocations(&host, chip);
    let before = TOTAL.load(Ordering::Relaxed);
    for _ in 0..200 {
        round(&mut reply);
    }
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "the host allocated in steady state");
    assert_eq!(
        package_allocations(&host, chip) - inside,
        0,
        "the package allocated in steady state"
    );

    // Letting go of the drivers gives back what they held.
    host.shutdown();
    assert_eq!(resources_held(), 0);
    let _ = std::fs::remove_dir_all(dir);
}
