//! The read-only queries of a burner, against a scripted device: its loader version, the probe, the
//! medium's info, reads, and the replies it may send back wrong (stale packets, a refusal, a short
//! packet).

mod common;

use common::*;
use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::packet::{cmd, words};
use lemnos_k230_usbboot::sim::{Answer, ScriptedIo, Transfer};

const VALID: u64 = 1 << 47;

/// The info words of a medium with 512-byte blocks and 4 KiB erase blocks, writable.
fn info_words(flags: u64) -> Vec<u8> {
    words(&[1 << 30, 512, 4096, flags])
}

#[test]
fn probe_and_info_read_the_medium_and_send_only_queries() {
    let mut script = sync(1);
    script.extend(exchange(cmd::PROBE, 1, &words(&[1024, 512])));
    script.extend(exchange(cmd::GET_INFO, 1, &info_words(VALID)));
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    assert_eq!(burner.loader_version(), 1);
    let info = burner.info().unwrap();
    assert_eq!(info.capacity, 1 << 30);
    assert_eq!((info.block_size, info.erase_size), (512, 4096));
    assert!(info.valid && !info.write_protected);
    // NOP, PROBE, GET_INFO: nothing that changes the medium.
    let commands: Vec<u8> = io
        .records()
        .iter()
        .filter_map(|record| match record {
            Transfer::BulkOut(data) => Some(data[0]),
            _ => None,
        })
        .collect();
    assert_eq!(
        commands,
        vec![cmd::NOP as u8, cmd::PROBE as u8, cmd::GET_INFO as u8]
    );
}

#[test]
fn a_read_is_cut_to_the_asked_range_from_whole_blocks() {
    // Block 1 of the medium holds these bytes.
    let block: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
    let mut script = sync(2);
    script.extend(exchange(cmd::PROBE, 1, &words(&[4096, 512])));
    script.extend(exchange(cmd::GET_INFO, 1, &info_words(VALID)));
    // Offset 0x210, 0x100 bytes: the whole block at 0x200 holds them.
    script.extend(exchange(cmd::READ, 1, &words(&[0x200, 512])));
    script.push(Answer::Data(reply_of_any_length(
        cmd::READ_CHUNK,
        1,
        &block,
    )));
    script.push(Answer::Data(reply(cmd::READ_CHUNK, 1, &[])));
    let mut io = ScriptedIo::new(script);
    let mut burner = Burner::new(&mut io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    let read = burner.read(0x210, 0x100).unwrap();
    assert_eq!(read, block[0x10..0x110].to_vec());
}

#[test]
fn a_stale_reply_is_skipped_and_the_awaited_one_is_used() {
    let mut io = ScriptedIo::new([
        Answer::Ok,
        // A reply to another command first, then the one awaited.
        Answer::Data(reply(cmd::PROBE, 1, &words(&[1, 2]))),
        Answer::Data(reply(cmd::GET_INFO, 1, &info_words(VALID))),
    ]);
    let info = Burner::new(&mut io).info().unwrap();
    assert_eq!(info.capacity, 1 << 30);
}

#[test]
fn a_refusal_names_the_command_and_its_message_without_the_junk_after_it() {
    let mut io = ScriptedIo::new([
        Answer::Ok,
        Answer::Data(reply(cmd::GET_INFO, 0xff, b"no medium\0junk")),
    ]);
    let error = Burner::new(&mut io).info().unwrap_err();
    let text = error.to_string();
    assert!(text.contains("0x11"), "{text}");
    assert!(text.contains("no medium"), "{text}");
    assert!(!text.contains("junk"), "{text}");
    assert!(matches!(error, UsbError::Protocol(_)));
}

#[test]
fn a_reply_that_is_not_a_whole_packet_is_an_error() {
    let mut short = reply(cmd::GET_INFO, 1, &[]);
    short.truncate(40);
    let mut io = ScriptedIo::new([Answer::Ok, Answer::Data(short)]);
    let error = Burner::new(&mut io).info().unwrap_err();
    assert!(error.to_string().contains("40-byte"), "{error}");
}

#[test]
fn a_stalled_query_is_reported_as_the_stall() {
    let mut io = ScriptedIo::new([Answer::Stall]);
    let error = Burner::new(&mut io).info().unwrap_err();
    assert_eq!(error, UsbError::Stall);
}

#[test]
fn a_probe_with_chunk_sizes_the_vendor_tool_refuses_is_an_error() {
    for (chunks, fine) in [
        (words(&[1024, 512]), true),
        (words(&[0, 512]), false),
        (words(&[0x8000_0000, 512]), false),
        (words(&[1024, 0]), false),
        (words(&[1024, 65536]), false),
    ] {
        let mut script = sync(1);
        script.extend(exchange(cmd::PROBE, 1, &chunks));
        let mut io = ScriptedIo::new(script);
        let result = Burner::new(&mut io).probe(Medium::Sd);
        assert_eq!(result.is_ok(), fine, "{chunks:02x?}: {result:?}");
    }
}

#[test]
fn medium_codes_are_the_vendor_enum() {
    // kburn.h:39-44: EMMC 1, SD 2, SPI_NAND 3, SPI_NOR 4, OTP 5.
    assert_eq!(
        [
            Medium::Emmc,
            Medium::Sd,
            Medium::SpiNand,
            Medium::SpiNor,
            Medium::Otp
        ]
        .map(|m| m as u8),
        [1, 2, 3, 4, 5]
    );
}
