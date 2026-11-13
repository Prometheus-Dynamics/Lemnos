//! A board file's `[[each]]` entry: one device for each resource that fits its pattern, made
//! when the host starts and as hardware comes, with an id that stays with the resource. A board
//! entry keeps what it names, by any name, and an entry's `except` keeps what it names.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{Config, ErrorKind, Health, Lifecycle, Result, UsbDeviceHandle, driver};
use lemnos_hal::usb_match::UsbCodes;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// A configuration with a video control and a video streaming interface.
const VIDEO: [u8; 27] = [
    9, 2, 27, 0, 2, 1, 0, 0x80, 50, // configuration
    9, 4, 0, 0, 0, 0x0e, 1, 0, 0, // video control
    9, 4, 1, 0, 0, 0x0e, 2, 0, 0, // video streaming
];
/// A configuration with one mass-storage interface.
const DISK: [u8; 18] = [
    9, 2, 18, 0, 1, 1, 0, 0x80, 50, // configuration
    9, 4, 0, 0, 0, 0x08, 6, 0x50, 0, // mass storage
];

const fn ids(vendor: u16, product: u16) -> UsbCodes {
    UsbCodes {
        vendor,
        product,
        class: 0xef,
        subclass: 2,
        protocol: 1,
    }
}

const GONE: &str = "the camera is gone";

/// Takes a USB device and looks at it on every tick.
#[driver("cam", provides = [])]
pub struct Cam {
    #[needs]
    usb: UsbDeviceHandle,
    gone: bool,
}

impl Lifecycle for Cam {
    type Needs = CamNeeds;
    const TICKS: bool = true;

    fn start(needs: CamNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            usb: needs.usb,
            gone: false,
        })
    }

    fn health(&self) -> Health {
        if self.gone {
            Health::Gone(GONE)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        let mut header = [0u8; 9];
        self.gone = self.usb.config_descriptor(0, &mut header) == Err(ErrorKind::Unavailable);
        None
    }
}

const CAMERAS: &str = r#"
[[each]]
id = "camera"
driver = "cam"
match = "usb:class:0e"
need = "usb"
[each.streams.frames]
buffers = 2
buffer_bytes = 64
"#;

struct Rig {
    host: Host,
    clock: SimClock,
    board: SimBoard,
}

impl Rig {
    /// A host on `board` with `text` as its board file's devices and entries.
    fn start(board: SimBoard, text: &str) -> Self {
        let text =
            format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{text}");
        let clock = board.clock();
        clock.advance(Duration::from_secs(100));
        let host = Host::start(
            &parse(&text).unwrap(),
            &[entry::<Cam>()],
            Box::new(board.clone()),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        Self { host, clock, board }
    }

    /// Every device, by id, with its state.
    fn devices(&self) -> Vec<(String, DeviceState)> {
        self.host
            .devices()
            .into_iter()
            .map(|device| (device.id, device.state))
            .collect()
    }

    fn ids(&self) -> Vec<String> {
        self.devices().into_iter().map(|(id, _)| id).collect()
    }

    fn running(&self, id: &str) -> bool {
        self.host.device(id).map(|device| device.state) == Some(DeviceState::Running)
    }

    /// Hands the host what the board has said since last time, and lets it act once the gap a
    /// hint keeps has passed.
    fn deliver(&self) {
        self.board.hints().drain(|hint| self.host.hint(hint));
        self.clock.advance(Options::default().hint_gap);
        self.host.tick();
    }
}

fn two_cameras_and_a_disk() -> SimBoard {
    let board = SimBoard::new();
    board.add_usb_device_as("usb:1-2", ids(0x1a2b, 1), &VIDEO);
    board.add_usb_device_as("usb:1-1", ids(0x1a2b, 1), &VIDEO);
    board.add_usb_device_as(
        "usb:1-3",
        UsbCodes {
            vendor: 0x0001,
            product: 0x0002,
            class: 0,
            subclass: 0,
            protocol: 0,
        },
        &DISK,
    );
    board
}

#[test]
fn each_camera_plugged_in_at_start_binds_with_no_entry_of_its_own() {
    let rig = Rig::start(two_cameras_and_a_disk(), CAMERAS);
    assert_eq!(rig.ids(), ["camera@usb:1-1", "camera@usb:1-2"]);
    assert!(rig.running("camera@usb:1-1"));
    assert!(rig.running("camera@usb:1-2"));
    assert!(rig.board.is_claimed("usb:1-1"));
    assert!(!rig.board.is_claimed("usb:1-3"), "the disk is no camera");
    // Each has the streams the entry gives it.
    for device in rig.host.devices() {
        let stats = rig.host.stream_stats(device.index, "frames").unwrap();
        assert!(!stats.attached);
    }
    // Nothing more is made by looking again.
    rig.clock.advance(Duration::from_secs(60));
    rig.host.tick();
    assert_eq!(rig.ids().len(), 2);
}

#[test]
fn a_camera_plugged_in_later_binds_at_the_hint_or_at_the_next_look_without_one() {
    let rig = Rig::start(SimBoard::new(), CAMERAS);
    assert!(rig.ids().is_empty());
    assert_eq!(
        rig.host.next_retry_ns(),
        Some(rig.clock.now_ns() + Options::default().retry_cap.as_nanos() as u64),
        "an entry is looked at again a retry cap later"
    );

    rig.board
        .add_usb_device_as("usb:2-1", ids(0x1a2b, 1), &VIDEO);
    rig.deliver();
    assert_eq!(rig.ids(), ["camera@usb:2-1"]);
    assert!(rig.running("camera@usb:2-1"));

    // A hint that is lost costs only the wait.
    rig.board
        .add_usb_device_as("usb:2-2", ids(0x1a2b, 1), &VIDEO);
    assert_eq!(rig.board.hints().lose(), 1);
    rig.host.tick();
    assert_eq!(rig.ids().len(), 1);
    rig.clock.advance(Options::default().retry_cap);
    rig.host.tick();
    assert_eq!(rig.ids(), ["camera@usb:2-1", "camera@usb:2-2"]);
    assert!(rig.running("camera@usb:2-2"));
}

#[test]
fn a_camera_that_comes_back_to_its_port_gets_its_id_and_index_back() {
    let rig = Rig::start(two_cameras_and_a_disk(), CAMERAS);
    let index = rig.host.device("camera@usb:1-2").unwrap().index;
    let first = rig.board.faults("usb:1-2").unwrap();
    first.outage(
        Duration::from_nanos(rig.clock.now_ns()),
        None,
        ErrorKind::Unavailable,
    );
    rig.board.remove("usb:1-2");
    rig.deliver();
    assert!(matches!(
        rig.host.device("camera@usb:1-2").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    assert_eq!(rig.ids().len(), 2, "a device that left stays listed");

    rig.board
        .add_usb_device_as("usb:1-2", ids(0x1a2b, 1), &VIDEO);
    rig.clock.advance(Duration::from_secs(1));
    rig.deliver();
    let back = rig.host.device("camera@usb:1-2").unwrap();
    assert_eq!(back.index, index);
    assert_eq!(back.state, DeviceState::Running);
    assert_eq!(rig.ids().len(), 2);
}

#[test]
fn a_board_entry_keeps_the_device_it_names_by_any_name() {
    let text = format!(
        "{CAMERAS}\n\
         [[devices]]\nid = \"front\"\ndriver = \"cam\"\nuses = {{ usb = \"usb:1a2b:0007\" }}\n\
         [[devices]]\nid = \"back\"\ndriver = \"cam\"\nbus = \"usb:1-2\"\ndisabled = true\n"
    );
    let board = two_cameras_and_a_disk();
    board.add_usb_device_as("usb:1-4", ids(0x1a2b, 7), &VIDEO);
    let rig = Rig::start(board, &text);
    // `front` has the camera it names by vendor and product, and `back`, switched off, keeps
    // the one on its port from every entry: that is how a board forbids one.
    assert_eq!(rig.ids(), ["front", "back", "camera@usb:1-1"]);
    assert!(rig.running("front"));
    assert!(rig.board.is_claimed("usb:1-4"));
    assert!(!rig.board.is_claimed("usb:1-2"));
}

#[test]
fn an_entry_takes_nothing_its_except_reaches_and_nothing_an_earlier_entry_took() {
    let text = r#"
[[each]]
id = "camera"
driver = "cam"
match = "usb:class:0e"
need = "usb"
except = ["usb:1-1"]

[[each]]
id = "anything"
driver = "cam"
match = "usb:1a2b:0001"
"#;
    let rig = Rig::start(two_cameras_and_a_disk(), text);
    assert_eq!(rig.ids(), ["camera@usb:1-2", "anything@usb:1-1"]);
    assert!(rig.running("anything@usb:1-1"));
    // Without a `need`, the resource is the device's bus.
    assert!(rig.board.is_claimed("usb:1-1"));
}

#[test]
fn an_entry_for_a_driver_there_is_not_leaves_its_devices_retrying_with_the_reason() {
    let text = "[[each]]\nid = \"x\"\ndriver = \"none\"\nmatch = \"usb:class:0e\"\n";
    let rig = Rig::start(two_cameras_and_a_disk(), text);
    let devices = rig.devices();
    assert_eq!(devices.len(), 2);
    for (_, state) in devices {
        let DeviceState::Backoff { reason, .. } = state else {
            panic!("{state:?}");
        };
        assert!(reason.contains("none"), "{reason}");
    }
}
