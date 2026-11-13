//! A driver that watches the board gets it before each tick: every device the board names, with
//! where it came from, whether it is bound, and its health (a dead device's `Gone` kept apart from
//! `Failed`). Reading it allocates nothing, and a driver that does not watch never gets it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_core::{
    BoardDevice, BoardView, Config, Health, Lifecycle, Origin, Result, UsbDeviceHandle, driver,
};
use lemnos_hal::usb_match::UsbCodes;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What the watcher saw at its last look, one line per device.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Allocations made by reading the board, at the watcher's last look.
static READ_ALLOCATIONS: AtomicUsize = AtomicUsize::new(usize::MAX);
/// The board file's `health` list, as the watcher's start was given it.
static LISTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// How many times the plain driver was handed the board. It never should be.
static PLAIN_HANDED: AtomicUsize = AtomicUsize::new(0);

fn health_word(health: Health) -> String {
    match health {
        Health::Ok => "Ok".into(),
        Health::Degraded(why) => format!("Degraded({why})"),
        Health::Failed(why) => format!("Failed({why})"),
        Health::Gone(why) => format!("Gone({why})"),
    }
}

fn line(device: BoardDevice<'_>) -> String {
    let origin = match device.origin {
        Origin::BuiltIn => "builtin",
        Origin::Hotplug => "hotplug",
    };
    format!(
        "{} {origin} {} {}",
        device.id,
        if device.bound { "bound" } else { "unbound" },
        health_word(device.health)
    )
}

/// Watches the board on every tick.
#[driver("watch", provides = [])]
pub struct Watch {}

impl Lifecycle for Watch {
    type Needs = WatchNeeds;
    const TICKS: bool = true;
    const WATCHES_BOARD: bool = true;

    fn start(_: WatchNeeds, cfg: &Config<'_>) -> Result<Self> {
        let listed = cfg.list("health").unwrap_or(&[]);
        *LISTED.lock().unwrap() = listed.iter().map(|item| item.to_string()).collect();
        Ok(Self {})
    }

    fn board(&mut self, view: &dyn BoardView) {
        // Only counting here: reading the view must allocate nothing.
        let (_, allocated) = allocations(|| {
            let mut devices = 0usize;
            view.each(&mut |_device| devices += 1);
            std::hint::black_box(devices)
        });
        READ_ALLOCATIONS.store(allocated, Ordering::SeqCst);
        let mut lines = Vec::new();
        view.each(&mut |device| lines.push(line(device)));
        *SEEN.lock().unwrap() = lines;
    }

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }
}

/// Does not watch the board: the host never hands it one.
#[driver("plain", provides = [])]
pub struct Plain {}

impl Lifecycle for Plain {
    type Needs = PlainNeeds;
    const TICKS: bool = true;

    fn start(_: PlainNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn board(&mut self, _view: &dyn BoardView) {
        PLAIN_HANDED.fetch_add(1, Ordering::SeqCst);
    }

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }
}

/// Reports itself failed.
#[driver("sick", provides = [])]
pub struct Sick {}

impl Lifecycle for Sick {
    type Needs = SickNeeds;

    fn start(_: SickNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn health(&self) -> Health {
        Health::Failed("sick")
    }
}

/// Reports itself dead for good.
#[driver("dead", provides = [])]
pub struct Dead {}

impl Lifecycle for Dead {
    type Needs = DeadNeeds;

    fn start(_: DeadNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn health(&self) -> Health {
        Health::Gone("unplugged")
    }
}

/// Takes a USB device an `[[each]]` entry makes it for.
#[driver("cam", provides = [])]
pub struct Cam {
    #[needs]
    usb: UsbDeviceHandle,
}

impl Lifecycle for Cam {
    type Needs = CamNeeds;

    fn start(needs: CamNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { usb: needs.usb })
    }

    fn health(&self) -> Health {
        let _ = &self.usb;
        Health::Ok
    }
}

const CAMERA: UsbCodes = UsbCodes {
    vendor: 0x1a2b,
    product: 1,
    class: 0xef,
    subclass: 2,
    protocol: 1,
};
const VIDEO: [u8; 27] = [
    9, 2, 27, 0, 2, 1, 0, 0x80, 50, //
    9, 4, 0, 0, 0, 0x0e, 1, 0, 0, //
    9, 4, 1, 0, 0, 0x0e, 2, 0, 0,
];

fn start(board: SimBoard, devices: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}\n\
         [[each]]\nid = \"camera\"\ndriver = \"cam\"\nmatch = \"usb:class:0e\"\nneed = \"usb\"\n"
    );
    let clock = board.clock();
    clock.advance(Duration::from_secs(100));
    Host::start(
        &parse(&text).unwrap(),
        &[
            entry::<Watch>(),
            entry::<Plain>(),
            entry::<Sick>(),
            entry::<Dead>(),
            entry::<Cam>(),
        ],
        Box::new(board),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

const BOARD: &str = "[[devices]]\nid = \"w\"\ndriver = \"watch\"\n\
    config = { health = [\"device:s\", \"file:/run/x\"] }\n\
    [[devices]]\nid = \"s\"\ndriver = \"sick\"\n\
    [[devices]]\nid = \"g\"\ndriver = \"dead\"\n\
    [[devices]]\nid = \"off\"\ndriver = \"plain\"\ndisabled = true\n";

fn seen() -> Vec<String> {
    SEEN.lock().unwrap().clone()
}

#[test]
fn a_watching_driver_sees_every_device_with_origin_bound_and_health() {
    let board = SimBoard::new();
    board.add_usb_device_as("usb:1-1", CAMERA, &VIDEO);
    let host = start(board, BOARD);
    host.tick();
    let lines = seen();
    assert!(
        lines.contains(&"w builtin bound Ok".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"s builtin bound Failed(sick)".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"g builtin bound Gone(unplugged)".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"off builtin unbound Failed(off)".to_string()),
        "a switched-off device is unbound: {lines:?}"
    );
    assert!(
        lines.contains(&"camera@usb:1-1 hotplug bound Ok".to_string()),
        "a device an each entry made is hotplug: {lines:?}"
    );
    assert_eq!(lines.len(), 5, "every device the board names: {lines:?}");
}

#[test]
fn reading_the_board_allocates_nothing() {
    let board = SimBoard::new();
    board.add_usb_device_as("usb:1-1", CAMERA, &VIDEO);
    let host = start(board, BOARD);
    host.tick();
    assert_eq!(READ_ALLOCATIONS.load(Ordering::SeqCst), 0);
}

#[test]
fn a_driver_that_does_not_watch_is_never_handed_the_board() {
    let board = SimBoard::new();
    let host = start(board, BOARD);
    host.tick();
    host.tick();
    assert_eq!(PLAIN_HANDED.load(Ordering::SeqCst), 0);
}

#[test]
fn a_board_file_list_of_strings_reaches_the_driver_and_other_arrays_are_refused() {
    let board = SimBoard::new();
    let _host = start(board, BOARD);
    assert_eq!(*LISTED.lock().unwrap(), ["device:s", "file:/run/x"]);
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"w\"\ndriver = \"watch\"\nconfig = { n = [1, 2] }\n";
    assert!(parse(text).is_err(), "a list of numbers is not a setting");
}
