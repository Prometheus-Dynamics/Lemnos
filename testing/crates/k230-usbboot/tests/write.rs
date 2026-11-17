//! The burner's write path against a scripted device: a write in chunks with a short last block, its
//! SHA-256 verify, the reboot, and the failures (write protection, a refused start, a reset or stall
//! mid-write, cancel).

mod common;

use common::*;
use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::hex;
use lemnos_k230_usbboot::packet::{REBOOT_MARK, cmd, words};
use lemnos_k230_usbboot::sim::{Answer, ScriptedIo, Transfer};
use sha2::{Digest, Sha256};

const VALID: u64 = 1 << 47;

/// The loader's answers to a sync, a probe (1 KiB out, 512 in) and an info of a 1 GiB medium with
/// 512-byte blocks and 4 KiB erase blocks.
fn medium(version: u32, flags: u64) -> Vec<Answer> {
    let mut answers = sync(version);
    answers.extend(exchange(cmd::PROBE, 1, &words(&[1024, 512])));
    answers.extend(exchange(
        cmd::GET_INFO,
        1,
        &words(&[1 << 30, 512, 4096, flags]),
    ));
    answers
}

fn image(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 13 % 256) as u8).collect()
}

#[test]
fn a_write_goes_in_chunks_with_a_padded_last_block_and_verifies() {
    let data = image(1300);
    let mut script = medium(2, VALID);
    // The start: 1300 bytes round up to 1536 (three 512-byte blocks).
    script.extend(exchange(cmd::WRITE, 1, &[]));
    script.push(Answer::Ok); // 1024 bytes
    script.push(Answer::Ok); // 512 bytes, the last block
    script.push(Answer::Data(reply(cmd::WRITE, 1, &[])));
    script.extend(exchange(cmd::NOP, 1, &[]));
    // The verify: digest, the 1300 bytes it covered, 5 ms.
    let mut digest_reply = Sha256::digest(&data).to_vec();
    digest_reply.extend(words(&[1300, 5]));
    script.extend(exchange(cmd::VERIFY, 1, &digest_reply));
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let mut source: &[u8] = &data;
    let mut progress_calls = Vec::new();
    let mut progress = |done: u64, total: u64| progress_calls.push((done, total));
    let digest = burner
        .write(0x1_0000, &mut source, 1300, 0, &mut progress, &|| false)
        .unwrap();
    assert_eq!(digest, hex(&Sha256::digest(&data)));
    assert_eq!(progress_calls.last(), Some(&(1300, 1300)));
    assert_eq!(burner.sha256(0x1_0000, 1300).unwrap(), digest);

    // The first chunk is 1024 bytes of image; the last is the rest and zeros.
    let bulk: Vec<Vec<u8>> = io
        .records()
        .iter()
        .filter_map(|record| match record {
            Transfer::BulkOut(bytes) if bytes.len() > 60 => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(bulk.len(), 2);
    assert_eq!(bulk[0], data[..1024].to_vec());
    assert_eq!(bulk[1][..276], data[1024..1300]);
    assert!(bulk[1][276..].iter().all(|&byte| byte == 0));
    // The start packet carries offset, padded size, region and flags.
    let start = io
        .records()
        .iter()
        .find_map(|record| match record {
            Transfer::BulkOut(bytes) if bytes[0] == cmd::WRITE as u8 => Some(bytes.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(&start[6..14], &0x1_0000u64.to_le_bytes());
    assert_eq!(&start[14..22], &1536u64.to_le_bytes());
}

#[test]
fn a_verify_that_covers_another_count_is_not_a_match() {
    let mut script = medium(2, VALID);
    let mut digest_reply = vec![0xabu8; 32];
    digest_reply.extend(words(&[1000, 5]));
    script.extend(exchange(cmd::VERIFY, 1, &digest_reply));
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let error = burner.sha256(0, 1300).unwrap_err();
    assert!(error.to_string().contains("1000"), "{error}");
}

#[test]
fn a_loader_without_verify_refuses_the_check_without_a_transfer() {
    let mut io = ScriptedIo::new(medium(1, VALID));
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let before = io.records().len();
    let mut burner = Burner::new(&mut io);
    let error = burner.sha256(0, 16).unwrap_err();
    assert!(error.to_string().contains("verification"), "{error}");
    assert_eq!(io.records().len(), before);
}

#[test]
fn the_reboot_sends_the_mark_and_the_reset_after_it_is_success() {
    // The mark goes out as a plain packet first.
    let mut io = ScriptedIo::new([Answer::Ok]);
    Burner::new(&mut io).reboot().unwrap();
    let Transfer::BulkOut(bytes) = &io.records()[0] else {
        panic!("{:?}", io.records());
    };
    assert_eq!(bytes[0], cmd::REBOOT as u8);
    assert_eq!(&bytes[6..14], &REBOOT_MARK.to_le_bytes());

    // The board resets on it: the disconnect is the reboot, not an error.
    let mut io = ScriptedIo::new([Answer::Disconnect]);
    Burner::new(&mut io).reboot().unwrap();
    assert!(io.is_gone());
}

#[test]
fn a_write_is_refused_before_probe_and_on_a_protected_medium() {
    let mut io = ScriptedIo::new(medium(2, VALID | (1 << 32)));
    let mut burner = Burner::new(&mut io);
    let mut source: &[u8] = &[1, 2, 3];
    let early = burner
        .write(0, &mut source, 3, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert!(early.to_string().contains("before probe"), "{early}");

    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let erase = burner.erase(0, 4096).unwrap_err();
    assert!(erase.to_string().contains("write-protected"), "{erase}");
    let mut source: &[u8] = &[1, 2, 3];
    let protected = burner
        .write(0, &mut source, 3, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert!(
        protected.to_string().contains("write-protected"),
        "{protected}"
    );
}

#[test]
fn a_write_must_start_on_an_erase_block_and_stay_on_the_medium() {
    let mut io = ScriptedIo::new(medium(2, VALID));
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let mut source: &[u8] = &[1u8; 16];
    let unaligned = burner
        .write(0x100, &mut source, 16, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert!(unaligned.to_string().contains("aligned"), "{unaligned}");
    let mut source: &[u8] = &[1u8; 16];
    let past = burner
        .write(1 << 30, &mut source, 16, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert!(past.to_string().contains("past the medium"), "{past}");
    // Sync (3 transfers: the version and the NOP), probe (2) and info (2): nothing after them.
    assert_eq!(io.records().len(), 7, "refused before any write was sent");
}

#[test]
fn a_reset_mid_write_is_reported_and_no_reply_is_read() {
    let mut script = medium(2, VALID);
    script.extend(exchange(cmd::WRITE, 1, &[]));
    script.push(Answer::Disconnect);
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let data = image(1024);
    let mut source: &[u8] = &data;
    let error = burner
        .write(0x1_0000, &mut source, 1024, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert_eq!(error, UsbError::Disconnected);
}

#[test]
fn a_stalled_chunk_reports_the_burner_reason_when_it_gives_one() {
    let mut script = medium(2, VALID);
    script.extend(exchange(cmd::WRITE, 1, &[]));
    script.push(Answer::Stall);
    script.push(Answer::Data(reply(cmd::WRITE, 2, &[])));
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let data = image(1024);
    let mut source: &[u8] = &data;
    let error = burner
        .write(0x1_0000, &mut source, 1024, 0, &mut |_, _| {}, &|| false)
        .unwrap_err();
    assert!(error.to_string().contains("result 2"), "{error}");
}

#[test]
fn a_cancel_stops_the_write_before_the_next_chunk() {
    let mut script = medium(2, VALID);
    script.extend(exchange(cmd::WRITE, 1, &[]));
    script.push(Answer::Ok);
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let data = image(2048);
    let mut source: &[u8] = &data;
    let mut chunks = 0;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let error = burner
        .write(
            0x1_0000,
            &mut source,
            2048,
            0,
            &mut |_, _| {
                chunks += 1;
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            &|| cancel.load(std::sync::atomic::Ordering::SeqCst),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert_eq!(chunks, 1);
}
