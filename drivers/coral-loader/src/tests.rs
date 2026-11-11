//! The driver against a mock USB device that runs the DFU state machine. Time is simulated: the
//! rig calls `tick` at exactly the time the driver asked for, so a test can see every wait.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_core::{
    ConfigValue, Driver, ErrorKind, LocalCaller, Provider, Resources, Tag, UsbDeviceHandle,
};

use super::*;
use crate::dfu::{self, DfuInterface, find_interface};
use crate::loader::{
    BACKOFF_CAP_NS, BACKOFF_FIRST_NS, BUSY_BUDGET_NS, CLAIM_BUSY, GONE, MAX_BLOCK, MAX_POLL_MS,
    NOT_DFU_MODE, NOT_IDLE, REFUSED, RESET_FAILED, SHORT_DOWNLOAD, SILENT, STAYED_BUSY,
    UNKNOWN_STATE, WRONG_STATE,
};
use crate::mock::{ERR_VERIFY, ERR_WRITE, MockUsb, Transfer};

const MS: u64 = 1_000_000;
const SECOND: u64 = 1_000 * MS;

/// `bmAttributes`: can download.
const ATTR_DOWNLOAD: u8 = 0x01;
/// `bmAttributes`: can download, and manifestation tolerant.
const ATTR_TOLERANT: u8 = 0x05;

fn interface(number: u8, alternate: u8, protocol: u8) -> [u8; 9] {
    [9, 0x04, number, alternate, 0, 0xfe, 0x01, protocol, 0]
}

fn functional(attributes: u8, transfer_size: u16) -> [u8; 9] {
    let [low, high] = transfer_size.to_le_bytes();
    [9, 0x21, attributes, 0xe8, 0x03, low, high, 0x10, 0x01]
}

/// A configuration descriptor holding `parts`.
fn config(parts: &[&[u8]]) -> Vec<u8> {
    let mut blob = vec![9, 0x02, 0, 0, 1, 1, 0, 0x80, 50];
    for part in parts {
        blob.extend_from_slice(part);
    }
    let total = blob.len() as u16;
    blob[2..4].copy_from_slice(&total.to_le_bytes());
    blob
}

fn dfu_config(attributes: u8, transfer_size: u16) -> Vec<u8> {
    config(&[&interface(0, 0, 2), &functional(attributes, transfer_size)])
}

/// A device in `dfuIDLE` that ends in `MANIFEST_WAIT_RESET`.
fn device(transfer_size: u16) -> MockUsb {
    MockUsb::new(
        dfu_config(ATTR_DOWNLOAD, transfer_size),
        usize::from(transfer_size),
        false,
    )
}

/// A manifestation-tolerant device, which ends back in `dfuIDLE`.
fn tolerant_device(transfer_size: u16) -> MockUsb {
    MockUsb::new(
        dfu_config(ATTR_TOLERANT, transfer_size),
        usize::from(transfer_size),
        true,
    )
}

/// A temporary directory holding files for one test.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-coral-loader-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, data).unwrap();
        path
    }

    fn sparse(&self, name: &str, length: u64) -> PathBuf {
        let path = self.0.join(name);
        fs::File::create(&path).unwrap().set_len(length).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Board(Option<MockUsb>);

impl Provider for Board {
    fn provided(&self, _need: &str) -> bool {
        true
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_device(&mut self, _need: &str) -> Result<UsbDeviceHandle> {
        let usb = self.0.take().ok_or(Status::NOT_FOUND)?;
        Ok(lemnos_core::Held::boxed(Box::new(usb)))
    }
}

fn bind(usb: &MockUsb, path: &Path) -> Result<CoralLoader> {
    let text = path.to_str().unwrap();
    let entries = [("firmware", ConfigValue::Str(text))];
    bind_config(usb, &Config::new(&entries))
}

fn bind_config(usb: &MockUsb, config: &Config<'_>) -> Result<CoralLoader> {
    let mut board = Board(Some(usb.clone()));
    let mut resources = Resources::new(&mut board, None);
    CoralLoader::bind(&mut resources, config)
}

/// The driver and its device at a simulated time.
struct Rig {
    driver: CoralLoader,
    usb: MockUsb,
    now: u64,
    _dir: TempDir,
}

impl Rig {
    fn new(usb: MockUsb, image: &[u8]) -> Self {
        let dir = TempDir::new();
        let path = dir.write("image.bin", image);
        let driver = bind(&usb, &path).expect("bind");
        Self {
            driver,
            usb,
            now: 0,
            _dir: dir,
        }
    }

    /// One tick at the current time.
    fn tick(&mut self) -> Option<u64> {
        self.usb.state().now_ns = self.now;
        Driver::tick(&mut self.driver, self.now)
    }

    /// One tick, then the clock goes to the time the driver asked for.
    fn step(&mut self) -> Option<u64> {
        let next = self.tick()?;
        self.now = next.max(self.now);
        Some(next)
    }

    /// Ticks until the driver has nothing more to do.
    fn run(&mut self) {
        for _ in 0..4_000_000 {
            if self.step().is_none() {
                return;
            }
        }
        panic!("the download did not end");
    }

    /// Ticks until `done` says the device has seen enough.
    fn run_until(&mut self, done: impl Fn(&[Transfer]) -> bool) {
        for _ in 0..4_000_000 {
            if done(&self.usb.state().transfers) {
                return;
            }
            assert!(self.step().is_some(), "the download ended first");
        }
        panic!("the condition never came true");
    }

    /// Ticks until the driver reports a fault.
    fn run_to_fault(&mut self) -> &'static str {
        for _ in 0..4_000_000 {
            if let Some(fault) = self.driver.loader.fault() {
                return fault;
            }
            assert!(self.step().is_some(), "the download ended first");
        }
        panic!("no fault came");
    }

    fn health(&self) -> Health {
        Driver::health(&self.driver)
    }

    fn transfers(&self) -> Vec<Transfer> {
        self.usb.state().transfers.clone()
    }

    fn downloads(&self) -> Vec<(u16, usize)> {
        self.usb
            .state()
            .transfers
            .iter()
            .filter(|transfer| transfer.request == dfu::DNLOAD)
            .map(|transfer| (transfer.value, transfer.data.len()))
            .collect()
    }

    fn received(&self) -> Vec<u8> {
        self.usb.state().received.clone()
    }

    /// The download was confirmed, and one reset went out after everything else and took the
    /// device off the bus. Nothing was asked of the dead handle.
    fn assert_loaded(&self) {
        assert_eq!(self.health(), Health::Ok);
        assert_eq!(self.driver.loader.ended(), Some(Ended::Loaded));
        let device = self.usb.state();
        assert_eq!(device.early_polls, Vec::<u64>::new());
        assert_eq!(device.resets.len(), 1, "exactly one reset");
        assert_eq!(device.resets[0].0, device.transfers.len(), "after it all");
        assert!(device.gone);
        assert_eq!(device.calls_after_gone, Vec::<&str>::new());
    }
}

fn dnload_count(transfers: &[Transfer]) -> usize {
    transfers
        .iter()
        .filter(|transfer| transfer.request == dfu::DNLOAD)
        .count()
}

#[test]
fn the_download_is_the_exact_sequence_dfu_defines() {
    let image: Vec<u8> = (0..10).collect();
    let mut rig = Rig::new(device(4), &image);
    rig.run();
    assert_eq!(
        rig.transfers(),
        vec![
            Transfer::get_status(),
            Transfer::dnload(0, &[0, 1, 2, 3]),
            Transfer::get_status(),
            Transfer::dnload(1, &[4, 5, 6, 7]),
            Transfer::get_status(),
            Transfer::dnload(2, &[8, 9]),
            Transfer::get_status(),
            Transfer::dnload(3, &[]),
            Transfer::get_status(),
        ]
    );
    assert_eq!(rig.received(), image);
    assert_eq!(rig.usb.state().state, dfu::MANIFEST_WAIT_RESET);
    assert_eq!(rig.usb.state().claimed, vec![0]);
    rig.assert_loaded();
}

#[test]
fn requests_address_the_interface_the_descriptors_name() {
    let blob = config(&[&interface(3, 0, 2), &functional(ATTR_DOWNLOAD, 4)]);
    let mut rig = Rig::new(MockUsb::new(blob, 4, false), &[9]);
    rig.run();
    assert_eq!(rig.usb.state().claimed, vec![3]);
    assert!(rig.transfers().iter().all(|transfer| transfer.index == 3));
}

#[test]
fn a_dfu_interface_on_another_alternate_is_selected() {
    let blob = config(&[&interface(0, 2, 2), &functional(ATTR_DOWNLOAD, 4)]);
    let mut rig = Rig::new(MockUsb::new(blob, 4, false), &[9]);
    rig.run();
    assert_eq!(rig.usb.state().alternates, vec![(0, 2)]);
}

#[test]
fn a_probe_that_fails_after_the_claim_does_not_claim_twice() {
    let blob = config(&[&interface(0, 1, 2), &functional(ATTR_DOWNLOAD, 4)]);
    let usb = MockUsb::new(blob, 4, false);
    usb.state().alternate_error_once = Some(ErrorKind::Timeout);
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), SILENT);
    rig.run();
    assert_eq!(rig.usb.state().claimed, vec![0], "claimed once");
    assert_eq!(rig.usb.state().alternates, vec![(0, 1)]);
    assert_eq!(rig.received(), vec![1]);
}

#[test]
fn alternate_zero_is_left_alone() {
    let mut rig = Rig::new(device(4), &[9]);
    rig.run();
    assert_eq!(rig.usb.state().alternates, vec![]);
}

#[test]
fn blocks_are_the_size_the_device_reports() {
    let image = vec![0xa5; 1000];
    let mut rig = Rig::new(device(256), &image);
    rig.run();
    assert_eq!(
        rig.downloads(),
        vec![(0, 256), (1, 256), (2, 256), (3, 232), (4, 0)]
    );
    assert_eq!(rig.received(), image);
}

#[test]
fn an_image_of_exact_blocks_still_ends_with_a_zero_length_block() {
    let image = vec![1; 8];
    let mut rig = Rig::new(device(4), &image);
    rig.run();
    assert_eq!(rig.downloads(), vec![(0, 4), (1, 4), (2, 0)]);
    assert_eq!(rig.received(), image);
}

#[test]
fn the_block_number_wraps_at_65535_as_dfu_defines() {
    let image: Vec<u8> = (0..65_538u32).map(|i| i as u8).collect();
    let mut rig = Rig::new(device(1), &image);
    rig.run();
    let downloads = rig.downloads();
    assert_eq!(downloads.len(), 65_539);
    assert_eq!(downloads[65_534], (65_534, 1));
    assert_eq!(downloads[65_535], (65_535, 1));
    assert_eq!(downloads[65_536], (0, 1));
    assert_eq!(downloads[65_537], (1, 1));
    assert_eq!(downloads[65_538], (2, 0), "the ending block carries on");
    assert_eq!(rig.received(), image);
    rig.assert_loaded();
}

#[test]
fn an_absurd_transfer_size_is_capped_to_what_a_control_transfer_carries() {
    let image = vec![7; 10_000];
    let mut rig = Rig::new(device(u16::MAX), &image);
    rig.run();
    assert_eq!(
        rig.downloads(),
        vec![
            (0, MAX_BLOCK),
            (1, MAX_BLOCK),
            (2, 10_000 - 2 * MAX_BLOCK),
            (3, 0)
        ]
    );
    assert_eq!(rig.received(), image);
}

#[test]
fn a_zero_transfer_size_stops_before_any_claim_and_keeps_retrying() {
    let mut rig = Rig::new(device(0), &[1, 2, 3]);
    let first = rig.step().expect("retrying");
    assert_eq!(rig.health(), Health::Failed(dfu::ZERO_TRANSFER));
    assert_eq!(first, BACKOFF_FIRST_NS);
    rig.step();
    rig.step();
    let device = rig.usb.state();
    assert!(device.descriptor_reads >= 3, "it kept reading");
    assert_eq!(device.claimed, Vec::<u8>::new());
    assert_eq!(device.transfers, vec![]);
}

#[test]
fn descriptors_from_the_device_are_not_trusted() {
    let good_interface = interface(0, 0, 2);
    let good_functional = functional(ATTR_DOWNLOAD, 64);

    assert_eq!(
        find_interface(&config(&[&good_interface, &good_functional])),
        Ok(DfuInterface {
            interface: 0,
            alternate: 0,
            transfer_size: 64,
            manifestation_tolerant: false,
        })
    );

    // A zero length, which would spin a naive walk forever.
    assert_eq!(find_interface(&[0, 0x21]), Err(dfu::MALFORMED));
    // A length past the end of what was read.
    assert_eq!(find_interface(&[9, 0x21, 1, 2]), Err(dfu::MALFORMED));
    // One byte left over, too short to be a header.
    assert_eq!(find_interface(&[7]), Err(dfu::MALFORMED));
    // An interface descriptor too short to hold its class.
    assert_eq!(find_interface(&[4, 0x04, 0, 0]), Err(dfu::MALFORMED));
    // A functional descriptor too short to hold wTransferSize.
    assert_eq!(
        find_interface(&config(&[&good_interface, &[4, 0x21, 1, 0]])),
        Err(dfu::MALFORMED)
    );

    // Nothing to download to.
    assert_eq!(find_interface(&config(&[])), Err(dfu::NO_DFU_INTERFACE));
    assert_eq!(find_interface(&[]), Err(dfu::NO_DFU_INTERFACE));
    assert_eq!(
        find_interface(&config(&[&good_interface])),
        Err(dfu::NO_DFU_INTERFACE),
        "a DFU interface with no functional descriptor"
    );
    assert_eq!(
        find_interface(&config(&[&interface(0, 0, 1), &good_functional])),
        Err(dfu::NO_DFU_INTERFACE),
        "the runtime interface takes no download"
    );

    // Type 0x21 outside a DFU interface is a HID descriptor, not ours.
    let hid_interface = [9, 0x04, 1, 0, 1, 0x03, 0, 0, 0];
    let hid = [9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x10, 0];
    assert_eq!(
        find_interface(&config(&[
            &hid_interface,
            &hid,
            &good_interface,
            &good_functional
        ]))
        .unwrap()
        .transfer_size,
        64
    );

    // A DFU 1.0 functional descriptor stops after wTransferSize.
    assert_eq!(
        find_interface(&config(&[
            &good_interface,
            &[7, 0x21, ATTR_DOWNLOAD, 0, 0, 32, 0]
        ]))
        .unwrap()
        .transfer_size,
        32
    );

    assert_eq!(
        find_interface(&config(&[&good_interface, &functional(0x02, 64)])),
        Err(dfu::CANNOT_DOWNLOAD)
    );
    assert_eq!(
        find_interface(&config(&[&good_interface, &functional(ATTR_DOWNLOAD, 0)])),
        Err(dfu::ZERO_TRANSFER)
    );
}

#[test]
fn a_status_reply_is_parsed_field_by_field_and_a_short_one_is_refused() {
    assert_eq!(
        dfu::DfuStatus::parse(&[3, 0x34, 0x12, 0x01, 4, 0]),
        Ok(dfu::DfuStatus {
            status: 3,
            poll_ms: 0x01_1234,
            state: 4,
        })
    );
    assert_eq!(dfu::DfuStatus::parse(&[]), Err(dfu::SHORT_STATUS));
    assert_eq!(
        dfu::DfuStatus::parse(&[0, 0, 0, 0, 2]),
        Err(dfu::SHORT_STATUS)
    );
}

#[test]
fn the_poll_timeout_is_waited_out_between_ticks_not_inside_them() {
    let usb = device(4);
    usb.state().busy_polls = 2;
    usb.state().poll_ms = 30;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run();
    assert_eq!(rig.usb.state().early_polls, Vec::<u64>::new());
    assert_eq!(rig.now, 60 * MS, "two polls of 30 ms and no other wait");
    assert_eq!(rig.received(), vec![1, 2, 3, 4]);
}

#[test]
fn an_early_tick_does_nothing() {
    let usb = device(4);
    usb.state().busy_polls = 1;
    usb.state().poll_ms = 30;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run_until(|transfers| dnload_count(transfers) == 1);
    // The GETSTATUS that gets the busy reply.
    let due = rig.tick().expect("polling");
    assert_eq!(due, rig.now + 30 * MS);
    let before = rig.transfers().len();
    rig.now += 29 * MS;
    assert_eq!(rig.tick(), Some(due), "not yet");
    assert_eq!(rig.transfers().len(), before);
}

#[test]
fn a_poll_timeout_of_hours_is_capped() {
    let usb = device(4);
    usb.state().busy_polls = 1;
    usb.state().poll_ms = 0x00ff_ffff;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run_until(|transfers| dnload_count(transfers) == 1);
    let due = rig.tick().expect("polling");
    assert_eq!(due, rig.now + u64::from(MAX_POLL_MS) * MS);
    rig.run();
    assert_eq!(rig.received(), vec![1, 2, 3, 4]);
}

#[test]
fn a_poll_timeout_of_zero_still_leaves_time_between_polls() {
    let usb = device(4);
    usb.state().busy_polls = 1;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run_until(|transfers| dnload_count(transfers) == 1);
    let due = rig.tick().expect("polling");
    assert_eq!(due, rig.now + MS);
}

#[test]
fn a_device_busy_forever_runs_out_its_budget_and_is_retried() {
    let usb = device(4);
    usb.state().busy_forever = true;
    usb.state().poll_ms = 5_000;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    assert_eq!(rig.run_to_fault(), STAYED_BUSY);
    assert!(rig.now >= BUSY_BUDGET_NS);
    assert_eq!(rig.health(), Health::Failed(STAYED_BUSY));
    assert_eq!(rig.usb.state().early_polls, Vec::<u64>::new());

    // Still trying, and it finishes when the device comes round.
    for _ in 0..50 {
        assert!(rig.step().is_some(), "never gives up");
    }
    rig.usb.state().busy_forever = false;
    rig.run();
    assert_eq!(rig.received(), vec![1, 2, 3, 4]);
    rig.assert_loaded();
}

#[test]
fn a_short_status_reply_restarts_the_download() {
    let image = vec![5; 12];
    let mut rig = Rig::new(device(4), &image);
    rig.run_until(|transfers| dnload_count(transfers) == 2);
    rig.usb.state().short_status_once = Some(3);
    assert_eq!(rig.run_to_fault(), dfu::SHORT_STATUS);
    rig.run();

    assert_eq!(
        rig.downloads(),
        vec![(0, 4), (1, 4), (0, 4), (1, 4), (2, 4), (3, 0)],
        "from block 0, after the two that went out before the fault"
    );
    assert_eq!(rig.received(), image);
    assert!(
        rig.transfers()
            .iter()
            .any(|transfer| transfer.request == dfu::ABORT),
        "the leftover download was aborted first"
    );
    rig.assert_loaded();
}

#[test]
fn a_block_the_device_only_partly_took_restarts_the_download() {
    let usb = device(4);
    usb.state().short_dnload_once = true;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4, 5]);
    assert_eq!(rig.run_to_fault(), SHORT_DOWNLOAD);
    rig.run();
    assert_eq!(rig.received(), vec![1, 2, 3, 4, 5]);
}

#[test]
fn a_block_the_device_refuses_clears_the_error_and_starts_again() {
    let usb = device(4);
    usb.state().reject_block_once = Some(1);
    let image = vec![8; 10];
    let mut rig = Rig::new(usb, &image);
    assert_eq!(rig.run_to_fault(), REFUSED);
    assert_eq!(rig.health(), Health::Failed(REFUSED));
    assert_eq!(rig.usb.state().status, ERR_WRITE);
    rig.run();

    assert_eq!(
        rig.downloads(),
        vec![(0, 4), (1, 4), (0, 4), (1, 4), (2, 2), (3, 0)]
    );
    let transfers = rig.transfers();
    // Sync, block 0, poll, block 1, the poll that is refused, sync, then this.
    let refused_at = 6;
    assert_eq!(
        transfers[refused_at],
        Transfer::out(dfu::CLRSTATUS, 0, &[]),
        "not {:?}",
        &transfers[..refused_at + 1]
    );
    assert_eq!(rig.received(), image);
    rig.assert_loaded();
}

#[test]
fn an_image_the_device_rejects_when_manifesting_is_sent_again() {
    let usb = device(4);
    usb.state().reject_manifest_once = Some(ERR_VERIFY);
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    assert_eq!(rig.run_to_fault(), REFUSED);
    rig.run();
    assert_eq!(rig.downloads(), vec![(0, 4), (1, 0), (0, 4), (1, 0)]);
    rig.assert_loaded();
}

#[test]
fn a_device_left_in_error_is_cleared_first() {
    let usb = device(4);
    usb.state().state = dfu::ERROR;
    usb.state().status = ERR_WRITE;
    let mut rig = Rig::new(usb, &[1]);
    rig.run();
    assert_eq!(
        rig.transfers()[..3],
        [
            Transfer::get_status(),
            Transfer::out(dfu::CLRSTATUS, 0, &[]),
            Transfer::get_status(),
        ]
    );
    assert_eq!(rig.downloads(), vec![(0, 1), (1, 0)]);
    rig.assert_loaded();
}

#[test]
fn a_download_left_over_from_before_a_restart_is_aborted_first() {
    let usb = device(4);
    usb.state().state = dfu::DNLOAD_IDLE;
    let mut rig = Rig::new(usb, &[1]);
    rig.run();
    assert_eq!(
        rig.transfers()[..3],
        [
            Transfer::get_status(),
            Transfer::out(dfu::ABORT, 0, &[]),
            Transfer::get_status(),
        ]
    );
    assert_eq!(rig.downloads(), vec![(0, 1), (1, 0)]);
    rig.assert_loaded();
}

#[test]
fn a_device_already_waiting_for_reset_is_reset_and_not_downloaded_to_again() {
    let usb = device(4);
    usb.state().state = dfu::MANIFEST_WAIT_RESET;
    let mut rig = Rig::new(usb, &[1, 2, 3]);
    rig.run();
    assert_eq!(rig.downloads(), vec![]);
    rig.assert_loaded();
}

#[test]
fn a_device_that_cannot_be_brought_to_idle_is_reported_and_retried() {
    let usb = device(4);
    usb.state().state = dfu::ERROR;
    usb.state().status = ERR_WRITE;
    usb.state().ignore_recovery = true;
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), NOT_IDLE);
    assert!(rig.tick().is_some());
    assert_eq!(rig.downloads(), vec![]);
}

#[test]
fn a_device_that_jumps_out_of_the_download_states_is_reported_and_retried() {
    let mut rig = Rig::new(device(4), &[1, 2, 3, 4, 5]);
    rig.run_until(|transfers| dnload_count(transfers) == 1);
    rig.usb.state().state = dfu::IDLE;
    assert_eq!(rig.run_to_fault(), WRONG_STATE);
    rig.run();
    assert_eq!(rig.received(), vec![1, 2, 3, 4, 5]);
    rig.assert_loaded();
}

#[test]
fn a_device_in_application_mode_is_reported_and_retried() {
    let usb = device(4);
    usb.state().state = dfu::APP_IDLE;
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), NOT_DFU_MODE);
    assert!(rig.step().is_some());
    assert_eq!(rig.downloads(), vec![]);
}

#[test]
fn a_state_outside_the_specification_is_reported_and_retried() {
    let usb = device(4);
    usb.state().state = 200;
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), UNKNOWN_STATE);
    assert!(rig.step().is_some());
    assert_eq!(rig.downloads(), vec![]);
}

#[test]
fn a_device_that_stops_responding_backs_off_to_the_cap_and_never_gives_up() {
    let usb = device(4);
    usb.state().silent_after = Some((0, ErrorKind::Timeout));
    let image = vec![6; 8];
    let mut rig = Rig::new(usb, &image);
    let mut waits = Vec::new();
    for _ in 0..8 {
        let before = rig.now;
        let next = rig.step().expect("never gives up");
        waits.push((next - before) / SECOND);
    }
    assert_eq!(rig.health(), Health::Failed(SILENT));
    // Reading the descriptors and the first status need no wait; the DNLOAD is the first to fail.
    assert_eq!(waits, vec![0, 0, 1, 2, 4, 8, 8, 8]);
    assert_eq!(BACKOFF_CAP_NS, 8 * SECOND);

    // It comes back: the download finishes.
    rig.usb.state().silent_after = None;
    rig.run();
    assert_eq!(rig.received(), image);
    rig.assert_loaded();
}

#[test]
fn the_backoff_clears_once_the_device_takes_a_block() {
    let usb = device(4);
    usb.state().silent_after = Some((0, ErrorKind::Timeout));
    let mut rig = Rig::new(usb, &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    for _ in 0..4 {
        rig.step();
    }
    assert_eq!(rig.driver.loader.fault(), Some(SILENT));

    rig.usb.state().silent_after = None;
    rig.run_until(|transfers| dnload_count(transfers) == 2);

    let reached = rig.transfers().len();
    rig.usb.state().silent_after = Some((reached, ErrorKind::Timeout));
    loop {
        let before = rig.now;
        let next = rig.step().expect("retrying");
        if rig.driver.loader.fault().is_some() {
            assert_eq!(next - before, SECOND, "the backoff started over");
            break;
        }
    }
}

#[test]
fn a_device_that_disappears_is_retried_and_reported() {
    let usb = device(4);
    usb.state().silent_after = Some((0, ErrorKind::Unavailable));
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), GONE);
    // This is what has the host bind the driver again. Left in place, it keeps trying.
    assert_eq!(rig.health(), Health::Gone(GONE));
    for _ in 0..20 {
        assert!(rig.step().is_some());
    }
}

#[test]
fn a_held_interface_is_reported_as_held() {
    let usb = device(4);
    usb.state().claim_error_once = Some(ErrorKind::Busy);
    let mut rig = Rig::new(usb, &[1]);
    assert_eq!(rig.run_to_fault(), CLAIM_BUSY);
    assert_eq!(rig.usb.state().claimed, Vec::<u8>::new());
    rig.run();
    assert_eq!(rig.usb.state().claimed, vec![0]);
    assert_eq!(rig.received(), vec![1]);
}

#[test]
fn a_tolerant_device_ends_idle_and_is_reset_too() {
    let mut rig = Rig::new(tolerant_device(4), &[1, 2, 3, 4, 5]);
    rig.run();
    assert_eq!(rig.usb.state().state, dfu::IDLE);
    assert_eq!(rig.received(), vec![1, 2, 3, 4, 5]);
    rig.assert_loaded();
}

#[test]
fn manifestation_is_polled_at_the_rate_the_device_asks() {
    let usb = device(4);
    usb.state().manifest_polls = 3;
    usb.state().poll_ms = 10;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run();
    assert_eq!(rig.now, 30 * MS);
    assert_eq!(rig.usb.state().early_polls, Vec::<u64>::new());
    rig.assert_loaded();
}

#[test]
fn a_device_silent_while_manifesting_ends_unconfirmed_and_says_so() {
    let usb = device(4);
    usb.state().manifest_status_error = Some(ErrorKind::Timeout);
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run();
    assert_eq!(rig.driver.loader.ended(), Some(Ended::Unconfirmed));
    assert_eq!(rig.health(), Health::Degraded(UNCONFIRMED));
    assert_eq!(rig.downloads(), vec![(0, 4), (1, 0)], "nothing resent");
    assert_eq!(rig.usb.state().resets.len(), 1, "it is still reset");
}

#[test]
fn a_tolerant_device_that_goes_silent_while_manifesting_is_retried() {
    let usb = tolerant_device(4);
    usb.state().manifest_status_error = Some(ErrorKind::Timeout);
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    assert_eq!(rig.run_to_fault(), SILENT);
    assert_eq!(rig.driver.loader.ended(), None);
}

#[test]
fn a_device_that_leaves_the_bus_while_manifesting_ends_unconfirmed() {
    let usb = tolerant_device(4);
    usb.state().manifest_status_error = Some(ErrorKind::Unavailable);
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run();
    assert_eq!(rig.driver.loader.ended(), Some(Ended::Unconfirmed));
}

#[test]
fn nothing_is_sent_once_the_download_has_ended_and_the_image_is_released() {
    let mut rig = Rig::new(device(4), &[1, 2, 3, 4]);
    assert_eq!(rig.driver.loader.image_len(), 4);
    rig.run();
    assert_eq!(rig.driver.loader.image_len(), 0);
    let before = rig.transfers().len();
    for _ in 0..5 {
        rig.now += SECOND;
        assert_eq!(rig.tick(), None);
    }
    assert_eq!(rig.transfers().len(), before);
    rig.assert_loaded();
}

#[test]
fn health_claims_success_only_once_the_reset_has_taken_the_device_away() {
    let mut rig = Rig::new(device(4), &[1, 2, 3, 4]);
    assert_eq!(rig.health(), Health::Degraded(IN_PROGRESS));
    while rig.step().is_some() {
        assert_ne!(rig.health(), Health::Ok);
        assert!(!rig.usb.state().gone);
    }
    assert_eq!(rig.health(), Health::Ok);
    assert!(rig.usb.state().gone);
}

#[test]
fn the_reset_is_issued_once_after_the_final_dfu_stage() {
    let mut rig = Rig::new(device(4), &[1, 2, 3, 4, 5]);
    rig.run();
    // Two blocks and the zero-length one, each with its GETSTATUS, after the first GETSTATUS.
    assert_eq!(rig.transfers().len(), 7);
    assert_eq!(
        rig.usb.state().resets,
        vec![(7, dfu::MANIFEST_WAIT_RESET)],
        "one reset, after the last transfer, with the device waiting for it"
    );
    for _ in 0..5 {
        rig.now += 10 * SECOND;
        assert_eq!(rig.tick(), None);
    }
    assert_eq!(rig.usb.state().resets.len(), 1, "never a second reset");
    rig.assert_loaded();
}

#[test]
fn no_reset_goes_out_before_the_download_is_over() {
    let usb = device(4);
    usb.state().reject_block_once = Some(1);
    usb.state().reject_manifest_once = Some(ERR_VERIFY);
    let mut rig = Rig::new(usb, &[1, 2, 3, 4, 5, 6, 7, 8]);
    while rig.step().is_some() {
        let device = rig.usb.state();
        if device.state != dfu::MANIFEST_WAIT_RESET {
            assert_eq!(device.resets, vec![], "in DFU state {}", device.state);
        }
    }
    rig.assert_loaded();
}

#[test]
fn a_reset_that_fails_is_retried_backing_off_to_the_cap_and_never_gives_up() {
    let usb = device(4);
    usb.state().reset_errors = [ErrorKind::Timeout; 6].into();
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    assert_eq!(rig.run_to_fault(), RESET_FAILED);
    assert_eq!(rig.health(), Health::Failed(RESET_FAILED));
    let sent = rig.transfers().len();

    // `run_to_fault` has already moved the clock past the first wait.
    let mut waits = vec![rig.now / SECOND];
    while rig.usb.state().resets.len() < 6 {
        let before = rig.now;
        let next = rig.step().expect("never gives up");
        waits.push((next - before) / SECOND);
    }
    assert_eq!(waits, vec![1, 2, 4, 8, 8, 8]);
    assert_eq!(rig.health(), Health::Failed(RESET_FAILED));
    assert_eq!(rig.transfers().len(), sent, "only the reset is made again");

    rig.run();
    assert_eq!(rig.usb.state().resets.len(), 7);
    assert_eq!(rig.transfers().len(), sent);
    assert_eq!(
        rig.downloads(),
        vec![(0, 4), (1, 0)],
        "nothing downloaded again"
    );
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.driver.loader.ended(), Some(Ended::Loaded));
}

#[test]
fn a_reset_refused_for_any_reason_is_still_being_retried_much_later() {
    let usb = device(4);
    let refusals = [
        ErrorKind::Busy,
        ErrorKind::PermissionDenied,
        ErrorKind::Unsupported,
        ErrorKind::Failed,
        ErrorKind::NotFound,
    ];
    usb.state().reset_errors = refusals.iter().copied().cycle().take(500).collect();
    let mut rig = Rig::new(usb, &[1]);
    while rig.usb.state().resets.len() < 500 {
        let before = rig.now;
        let next = rig.step().expect("never gives up");
        assert!(next - before <= BACKOFF_CAP_NS);
    }
    assert_eq!(rig.health(), Health::Failed(RESET_FAILED));
    rig.run();
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn the_backoff_of_the_download_does_not_carry_into_the_reset() {
    let usb = device(4);
    usb.state().manifest_polls = 1;
    usb.state().reject_manifest_once = Some(ERR_VERIFY);
    usb.state().reset_errors = [ErrorKind::Failed].into();
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    assert_eq!(rig.run_to_fault(), REFUSED);
    rig.step();
    assert_eq!(rig.driver.loader.fault(), None);
    loop {
        let before = rig.now;
        let next = rig.step().expect("retrying");
        if rig.driver.loader.fault() == Some(RESET_FAILED) {
            assert_eq!(next - before, BACKOFF_FIRST_NS, "the backoff started over");
            break;
        }
    }
    rig.run();
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn a_device_that_comes_back_from_the_reset_unchanged_is_reported_and_left_alone() {
    let usb = device(4);
    usb.state().reset_keeps_identity = true;
    let mut rig = Rig::new(usb, &[1, 2, 3, 4]);
    rig.run();
    assert_eq!(rig.driver.loader.ended(), Some(Ended::Unchanged));
    assert_eq!(rig.health(), Health::Degraded(UNCHANGED));
    let sent = rig.transfers().len();
    rig.now += 60 * SECOND;
    assert_eq!(rig.tick(), None);
    assert_eq!(rig.usb.state().resets.len(), 1);
    assert_eq!(rig.transfers().len(), sent);
}

#[test]
fn binding_touches_no_usb_device() {
    let usb = device(4);
    let rig = Rig::new(usb, &[1, 2, 3]);
    let device = rig.usb.state();
    assert_eq!(device.descriptor_reads, 0);
    assert_eq!(device.claimed, Vec::<u8>::new());
    assert_eq!(device.transfers, vec![]);
}

#[test]
fn the_firmware_setting_is_required_and_must_be_a_string() {
    let usb = device(4);
    assert_eq!(
        bind_config(&usb, &Config::new(&[])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    let entries = [("firmware", ConfigValue::Int(3))];
    assert_eq!(
        bind_config(&usb, &Config::new(&entries)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_firmware_file_that_cannot_be_used_fails_the_bind() {
    let usb = device(4);
    let dir = TempDir::new();

    assert_eq!(
        bind(&usb, &dir.0.join("missing.bin")).err(),
        Some(Status::NOT_FOUND)
    );
    assert_eq!(
        bind(&usb, Path::new("relative/image.bin")).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        bind(&usb, &dir.write("empty.bin", &[])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(bind(&usb, &dir.0).err(), Some(Status::INVALID_ARGUMENT));
    assert_eq!(
        bind(&usb, &dir.sparse("huge.bin", MAX_IMAGE_BYTES + 1)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn an_image_at_the_limit_is_accepted() {
    let usb = device(4);
    let dir = TempDir::new();
    let path = dir.sparse("max.bin", MAX_IMAGE_BYTES);
    let driver = bind(&usb, &path).expect("exactly the limit fits");
    assert_eq!(driver.loader.image_len() as u64, MAX_IMAGE_BYTES);
}
