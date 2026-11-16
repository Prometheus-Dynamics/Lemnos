//! The dry run of the Stage L flow against the sim, configured with board B's identity (`29f1:0230`, the ROM
//! answering exactly `K230`) and the ROM that goes deaf when idle. The sim's report image answers the records
//! the stage builds, and its watchdog returns the board. Three cases: the flow with the keep-alive stopped 3 s
//! before (the identify is its first action), the record content, and a late host, which must get `Deaf` by
//! name and send no image.

use std::time::Duration;

use sha2::Digest;

use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::sim::{SimBoard, SimClock, SimConfig, sim_report_image};
use lemnos_k230_usbboot::stage_report::{Options, RunRecord, Timer, run};
use lemnos_k230_usbboot_stage::format::PACKET;
use lemnos_k230_usbboot_stage::pads::PAD_COUNT;
use lemnos_k230_usbboot_stage::report::{self, CardState, HEADER_LEN};
use lemnos_k230_usbboot_stage::telemetry::Record;

/// The sim's clock as the flow's timer: the waits run in virtual time.
struct SimTimer(SimClock);

impl Timer for SimTimer {
    fn now_s(&self) -> f64 {
        self.0.now_us() as f64 / 1e6
    }
    fn sleep(&mut self, d: Duration) {
        self.0.advance_us(d.as_micros() as u64);
    }
}

/// Board B's sim: the download IDs, the ROM deaf 90 s after its reset while enumerated.
fn board_b() -> SimBoard {
    let mut board = SimBoard::new(&SimConfig::default());
    board.set_identity(0x29f1, 0x0230);
    board.set_deaf_after(Some(90_000_000));
    board
}

/// The records a bootable card's report holds, as the image sends them.
fn report_records() -> Vec<[u8; PACKET]> {
    // A firmware header built by the rule: "K230", the length, the type, the hash of the payload, the payload.
    let payload = [0x42u8; 300];
    let mut copy = [0u8; HEADER_LEN];
    copy[0..4].copy_from_slice(b"K230");
    copy[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    let hash: [u8; 32] = sha2::Sha256::digest(payload).into();
    copy[12..44].copy_from_slice(&hash);
    copy[528..528 + payload.len()].copy_from_slice(&payload);
    let areas = report::judge_blocks([copy, [0u8; HEADER_LEN]], 1024 * 512);
    let mut pads = [0u32; PAD_COUNT];
    for (i, p) in pads.iter_mut().enumerate() {
        *p = 0x40 + i as u32;
    }
    let cid = [0x5c; 16];
    let r = report::build(3, &pads, &cid, 1024, CardState::Read(&areas));
    r.packets[..r.count].to_vec()
}

fn options() -> Options {
    Options {
        image: sim_report_image(),
        sd_controller: Some(1),
        identify_wait: Duration::from_secs(10),
        return_wait: Duration::from_secs(30),
    }
}

#[test]
fn the_flow_identifies_first_and_gets_the_records_and_the_return_after_a_keep_alive_gap() {
    let mut board = board_b();
    let clock = board.clock();
    // The keep-alive was stopped 3 s before the flow: the ROM is still answering, inside its window.
    clock.advance_us(3_000_000);
    board.set_report_records(report_records());
    let mut timer = SimTimer(clock.clone());
    let rec: RunRecord = run(&mut board, &mut timer, &options()).expect("the flow runs");

    // The first action was the identify, and it said K230.
    assert!(rec.entries[0].text.starts_with("image bytes"), "{:?}", rec.entries[0]);
    assert!(rec.entries.iter().any(|e| e.text.contains("identify: \"K230\"")), "{:?}", rec.entries);
    // The records: boot, eight pads, the card report, and the dumps of a bootable card.
    let boots = rec.records.iter().filter(|(_, r)| matches!(r, Record::Boot { ctl: 3 })).count();
    assert_eq!(boots, 1);
    let pads: usize = rec.records.iter().filter(|(_, r)| matches!(r, Record::Pad { .. })).count();
    assert_eq!(pads, 8);
    let card = rec.records.iter().find_map(|(_, r)| match r {
        Record::CardReport { verdict, flags, blocks, .. } => Some((*verdict, *flags, *blocks)),
        _ => None,
    });
    assert_eq!(card, Some((1, 0b0000_1001, 1024)), "bootable (hash verified): magic at 2048, blank copy at 3072");
    // The return is counted from the flow's start, which is 3 s after the sim's power-on (the keep-alive gap),
    // so the watchdog's 5.59 s (2^27 cycles at 24 MHz, from the arming) sits in 3 s + 5.59 s, plus the flow's own
    // waits, which are 200 ms steps, before the image starts.
    let returned = rec.returned_s.expect("the watchdog returned the board");
    assert!(returned > 8.0 && returned < 12.0, "returned at {returned} s");
    // The run record prints the commands, the records and the console.
    let text = rec.render(&[]);
    assert!(text.contains("returned: K230"), "{text}");
    assert!(text.contains("CardReport"), "{text}");
}

#[test]
fn a_late_host_gets_the_named_deaf_error_and_no_image_is_sent() {
    let mut board = board_b();
    let clock = board.clock();
    // The window (90 s) has passed with nobody identifying.
    clock.advance_us(100_000_000);
    let mut timer = SimTimer(clock.clone());
    let error = run(&mut board, &mut timer, &options()).unwrap_err();
    assert_eq!(error, UsbError::Deaf, "{error}");
    assert!(error.to_string().contains("EPROTO"), "{error}");
}

#[test]
fn the_wait_for_a_return_that_never_comes_is_recorded_not_retried_blindly() {
    // The image answers no record and its watchdog is not armed: the flow records no return and stops.
    let mut board = board_b();
    let clock = board.clock();
    board.set_report_records(Vec::new());
    let mut timer = SimTimer(clock.clone());
    let rec = run(&mut board, &mut timer, &options()).expect("the flow ends");
    assert!(rec.records.is_empty());
    let _ = rec.render(&[]);
}
