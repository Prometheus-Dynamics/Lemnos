//! The stage's USB device path, against a scripted host that fails the ways hardware does: no host,
//! a stalled endpoint, a bus reset in the middle of a write, a short packet, and a core that does not
//! come up. The watchdog rule is checked with a counting feed: the device layer never feeds, and the
//! stage feeds once per verified piece.

use std::collections::VecDeque;

use lemnos_k230_usbboot_stage::format::{self, PACKET, STAGE_IDENT, cmd};
use lemnos_k230_usbboot_stage::ram::RamMedium;
use lemnos_k230_usbboot_stage::stage::Stage;
use lemnos_k230_usbboot_stage::usb_dev::{
    self, BULK_IN, BULK_OUT, DEVICE, Device, Event, HwError, Step, UsbHw,
};
use lemnos_k230_usbboot_stage::wdt::Feed;

/// What the scripted host sends next.
#[derive(Clone, Debug)]
enum Scripted {
    Setup([u8; 8]),
    Out { ep: u8, data: Vec<u8> },
    Reset,
}

/// A device-side hardware model: a host that sends a script, and the device's replies.
struct Model {
    script: VecDeque<Scripted>,
    current: Vec<u8>,
    /// What the device sent on IN endpoints, in order.
    sent: Vec<(u8, Vec<u8>)>,
    /// IN endpoints with a transfer the host has not taken yet.
    busy: Vec<u8>,
    stalled: Vec<u8>,
    address: Option<u8>,
    core_up: bool,
}

impl Model {
    fn new(script: Vec<Scripted>) -> Self {
        Self {
            script: script.into(),
            current: Vec::new(),
            sent: Vec::new(),
            busy: Vec::new(),
            stalled: Vec::new(),
            address: None,
            core_up: true,
        }
    }

    /// The host takes the device's last IN transfer on `ep`: its bytes, or a stall.
    fn host_take(&mut self, ep: u8) -> Result<Vec<u8>, HwError> {
        if self.stalled.contains(&ep) {
            return Err(HwError::Stalled);
        }
        self.busy.retain(|&e| e != ep);
        Ok(self
            .sent
            .iter()
            .rev()
            .find(|(e, _)| *e == ep)
            .map(|(_, d)| d.clone())
            .unwrap_or_default())
    }
}

impl UsbHw for Model {
    fn core_reset(&mut self) -> Result<(), HwError> {
        if self.core_up {
            Ok(())
        } else {
            Err(HwError::Timeout)
        }
    }

    fn poll(&mut self) -> Event {
        match self.script.pop_front() {
            None => Event::Idle,
            Some(Scripted::Reset) => {
                // A bus reset flushes the endpoints: a reply the host never took is gone.
                self.busy.clear();
                Event::BusReset
            }
            Some(Scripted::Setup(s)) => Event::Setup(s),
            Some(Scripted::Out { ep, data }) => {
                let len = data.len();
                self.current = data;
                Event::Out { ep, len }
            }
        }
    }

    fn read_out(&mut self, _ep: u8, buf: &mut [u8]) -> Result<usize, HwError> {
        if self.current.len() > buf.len() {
            return Err(HwError::Overrun);
        }
        buf[..self.current.len()].copy_from_slice(&self.current);
        Ok(self.current.len())
    }

    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), HwError> {
        if self.stalled.contains(&ep) {
            return Err(HwError::Stalled);
        }
        self.sent.push((ep, data.to_vec()));
        if ep != 0 {
            self.busy.push(ep);
        }
        Ok(())
    }

    fn in_ready(&self, ep: u8) -> bool {
        !self.busy.contains(&ep) && !self.stalled.contains(&ep)
    }

    fn stall(&mut self, ep: u8) {
        self.stalled.push(ep);
    }

    fn set_address(&mut self, address: u8) {
        self.address = Some(address);
    }
}

/// Counts the stage's feeds of the watchdogs.
#[derive(Default)]
struct Counter {
    feeds: u32,
}

impl Feed for Counter {
    fn feed(&mut self) {
        self.feeds += 1;
    }

    fn fast_reset(&mut self) {}

    fn count_for_test(&self) -> u32 {
        self.feeds
    }
}

/// A stage over a RAM medium, with a counting feed.
fn stage(bytes: &mut [u8]) -> Stage<RamMedium<'_>, Counter> {
    Stage::new(RamMedium::new(bytes, 512, 4096), Counter::default())
}

fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Scripted {
    let mut s = [0u8; 8];
    s[0] = request_type;
    s[1] = request;
    s[2..4].copy_from_slice(&value.to_le_bytes());
    s[4..6].copy_from_slice(&index.to_le_bytes());
    s[6..8].copy_from_slice(&length.to_le_bytes());
    Scripted::Setup(s)
}

fn bring_up(script: Vec<Scripted>) -> Device<Model> {
    Device::start(Model::new(script)).expect("the core comes up")
}

#[test]
fn the_device_presents_the_download_ids_and_its_identity_and_never_feeds() {
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![
        setup(0x80, 6, 0x0100, 0, 64),  // GET_DESCRIPTOR device
        setup(0x80, 6, 0x0200, 0, 255), // GET_DESCRIPTOR configuration
        setup(0x00, 5, 9, 0, 0),        // SET_ADDRESS 9
        setup(0x00, 9, 1, 0, 0),        // SET_CONFIGURATION 1
        setup(0xc0, 0, 0, 0, 32),       // GET_CPU_INFO: the identity
        setup(0xc0, 0, 1, 0, 4),        // the loader version
    ]);
    for _ in 0..6 {
        assert!(matches!(device.step(&mut stage), Ok(Step::Control)));
    }
    let sent = &device.hw().sent;
    // The device descriptor carries 29f1:0230.
    assert_eq!(sent[0].1, DEVICE.to_vec());
    assert_eq!(&sent[0].1[8..12], &[0xf1, 0x29, 0x30, 0x02]);
    // The identity: the stage's text, NUL padded to 32 bytes, as the host reads it.
    let identity = &sent[4].1;
    assert_eq!(&identity[..STAGE_IDENT.len()], STAGE_IDENT.as_bytes());
    assert!(identity[STAGE_IDENT.len()..].iter().all(|&b| b == 0));
    assert_eq!(sent[5].1, format::LOADER_VERSION.to_le_bytes().to_vec());
    // The address and configuration took effect.
    assert_eq!(device.hw().address, Some(9));
    assert_eq!(device.address(), 9);
    assert!(device.configured());
    // Watchdog rule: enumeration and the identity feed nothing.
    assert_eq!(stage.feed_count_for_test(), 0);
}

#[test]
fn a_command_comes_in_and_its_reply_goes_out_with_the_stage_feeding_nothing_for_it() {
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![Scripted::Out {
        ep: BULK_OUT,
        data: format::packet(cmd::NOP, &[]).to_vec(),
    }]);
    assert_eq!(device.step(&mut stage), Ok(Step::Bulk));
    // The reply is waiting for the host; the host takes it.
    let reply = device.hw_mut().host_take(BULK_IN).unwrap();
    assert_eq!(
        reply,
        format::reply(cmd::NOP, format::RESULT_OK, &[]).to_vec()
    );
    assert_eq!(stage.feed_count_for_test(), 0);
}

#[test]
fn with_no_host_nothing_is_sent_and_nothing_is_fed() {
    // Real-failing for the watchdog rule: a device with no host still runs the loop, and must not
    // feed the watchdogs while it waits.
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![Scripted::Out {
        ep: BULK_OUT,
        data: format::packet(cmd::PROBE, &[0, 0]).to_vec(),
    }]);
    assert_eq!(device.step(&mut stage), Ok(Step::Bulk));
    let mut quiet = 0;
    for _ in 0..1000 {
        match device.step(&mut stage) {
            Ok(Step::Idle) | Ok(Step::Quiet) => quiet += 1,
            other => panic!("with no host the device did {other:?}"),
        }
    }
    assert_eq!(quiet, 1000);
    // The reply is still waiting, not lost, and nothing has been fed.
    assert!(device.hw().sent.len() == 1);
    assert_eq!(stage.feed_count_for_test(), 0);
}

#[test]
fn a_stalled_endpoint_refuses_the_host_and_an_unknown_request_stalls_ep0() {
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![
        // An endpoint the device does not have.
        Scripted::Out {
            ep: 0x02,
            data: vec![1, 2, 3],
        },
        // A vendor request the device does not answer.
        setup(0xc0, 77, 0, 0, 8),
    ]);
    assert_eq!(device.step(&mut stage), Ok(Step::Stalled));
    assert_eq!(device.hw_mut().host_take(0x02), Err(HwError::Stalled));
    assert_eq!(device.step(&mut stage), Ok(Step::Stalled));
    assert_eq!(device.hw().stalled, vec![0x02, 0x00]);
    assert_eq!(stage.feed_count_for_test(), 0);
}

#[test]
fn a_bus_reset_in_the_middle_of_a_write_abandons_it_and_the_data_after_it_is_refused() {
    // Real-failing: without the abandon, the page after the reset is stored as data.
    let mut medium = vec![0xffu8; 1 << 16];
    let before = medium.clone();
    let mut stage = stage(&mut medium);
    let mut write = [0u8; 32];
    write[0..8].copy_from_slice(&8192u64.to_le_bytes()); // offset
    write[8..16].copy_from_slice(&4096u64.to_le_bytes()); // total
    let mut device = bring_up(vec![
        Scripted::Out {
            ep: BULK_OUT,
            data: format::packet(cmd::WRITE, &write).to_vec(),
        },
        Scripted::Reset,
        Scripted::Out {
            ep: BULK_OUT,
            data: vec![0x5a; 1000],
        },
    ]);
    assert_eq!(device.step(&mut stage), Ok(Step::Bulk));
    assert_eq!(device.step(&mut stage), Ok(Step::Reset));
    assert_eq!(device.step(&mut stage), Ok(Step::Bulk));
    // The page is not data any more: it is a command of the wrong size, and the host is told so.
    let reply = device.hw_mut().host_take(BULK_IN).unwrap();
    assert_eq!(reply[2..4], format::RESULT_MESSAGE.to_le_bytes());
    let text = String::from_utf8_lossy(&reply[6..]).to_string();
    assert!(text.contains("60-byte"), "{text}");
    assert_eq!(medium, before, "the abandoned write stored nothing");
    assert_eq!(
        device.address(),
        0,
        "a reset returns the device to address 0"
    );
}

#[test]
fn a_short_packet_on_the_bulk_pipe_is_a_refused_command_and_feeds_nothing() {
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![Scripted::Out {
        ep: BULK_OUT,
        data: vec![0x11; 10],
    }]);
    assert_eq!(device.step(&mut stage), Ok(Step::Bulk));
    let reply = device.hw_mut().host_take(BULK_IN).unwrap();
    assert_eq!(reply.len(), PACKET);
    assert_eq!(reply[2..4], format::RESULT_MESSAGE.to_le_bytes());
    assert_eq!(stage.feed_count_for_test(), 0);
}

#[test]
fn a_core_that_does_not_come_up_is_an_error_before_any_host_sees_the_device() {
    let mut model = Model::new(vec![]);
    model.core_up = false;
    let error = Device::start(model)
        .err()
        .expect("the core does not come up");
    assert_eq!(error, HwError::Timeout);
}

#[test]
fn a_write_that_is_verified_feeds_once_per_piece_and_the_device_adds_no_feed() {
    // The watchdog rule, the other way: the stage feeds per verified piece (one 4 KiB write here),
    // and nothing else on the path feeds.
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut write = [0u8; 32];
    write[0..8].copy_from_slice(&8192u64.to_le_bytes());
    write[8..16].copy_from_slice(&4096u64.to_le_bytes());
    let mut device = bring_up(vec![
        setup(0xc0, 0, 0, 0, 32),
        Scripted::Out {
            ep: BULK_OUT,
            data: format::packet(cmd::WRITE, &write).to_vec(),
        },
        Scripted::Out {
            ep: BULK_OUT,
            data: vec![0x77; 4096],
        },
    ]);
    for _ in 0..3 {
        device.step(&mut stage).unwrap();
    }
    assert_eq!(stage.feed_count_for_test(), 1);
    assert_eq!(&medium[8192..8192 + 4096], &vec![0x77u8; 4096][..]);
}

#[test]
fn the_reboot_mark_asks_for_the_fast_reset_through_the_device() {
    let mut medium = vec![0xffu8; 1 << 16];
    let mut stage = stage(&mut medium);
    let mut device = bring_up(vec![Scripted::Out {
        ep: BULK_OUT,
        data: format::packet(cmd::REBOOT, &format::REBOOT_MARK.to_le_bytes()).to_vec(),
    }]);
    assert_eq!(device.step(&mut stage), Ok(Step::Reboot));
}

#[test]
fn the_device_module_names_the_download_pipes_it_serves() {
    assert_eq!((BULK_OUT, BULK_IN), (0x01, 0x81));
    assert_eq!(usb_dev::descriptor(3, 0), Some(&usb_dev::LANGUAGE[..]));
    assert!(usb_dev::descriptor(9, 0).is_none());
}
