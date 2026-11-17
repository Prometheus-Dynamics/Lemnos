//! The burner's failure paths against a scripted device: the stale-reply limit, a stale NOP, the verify
//! reply's exact layout (digest 32, verified bytes 8, elapsed 8 = 48), the messages of a refused verify and a
//! refused erase, and the start of a write. Each case is written against this crate's burner; the
//! behaviour is the vendor burner's (`kburn_parse_resp`, the verify reply, the erase bounds).

mod common;

use common::*;
use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::packet::{cmd, words};
use lemnos_k230_usbboot::sim::{Answer, ScriptedIo};

const VALID: u64 = 1 << 47;

/// The answers to a sync, a probe and an info of a 1 GiB medium with 512-byte blocks and 4 KiB erase blocks.
fn medium(version: u32) -> Vec<Answer> {
    let mut answers = sync(version);
    answers.extend(exchange(cmd::PROBE, 1, &words(&[1024, 512])));
    answers.extend(exchange(
        cmd::GET_INFO,
        1,
        &words(&[1 << 30, 512, 4096, VALID]),
    ));
    answers
}

/// A burner that has probed and read the medium's info, ready for a command.
fn ready(io: &mut ScriptedIo) -> Burner<'_> {
    let mut burner = Burner::new(io);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    burner
}

#[test]
fn eight_stale_replies_are_skipped_and_the_ninth_is_an_error() {
    // A reply to another command is discarded; the burner allows 8 discards (packet.rs STALE_REPLIES).
    let mut script = vec![Answer::Ok];
    script.extend((0..8).map(|_| Answer::Data(reply(cmd::PROBE, 1, &[]))));
    script.push(Answer::Data(reply(
        cmd::GET_INFO,
        1,
        &words(&[1 << 30, 512, 4096, VALID]),
    )));
    let mut io = ScriptedIo::new(script);
    assert_eq!(Burner::new(&mut io).info().unwrap().capacity, 1 << 30);

    let mut script = vec![Answer::Ok];
    script.extend((0..9).map(|_| Answer::Data(reply(cmd::PROBE, 1, &[]))));
    let mut io = ScriptedIo::new(script);
    let error = Burner::new(&mut io).info().unwrap_err();
    assert!(error.to_string().contains("stale"), "{error}");
}

#[test]
fn a_nop_reply_with_a_failing_result_is_stale_and_the_next_one_is_used() {
    // The start-up NOP answers with its own result; a failing NOP reply is a stale reply, not the answer.
    let mut script = sync(2);
    script.insert(2, Answer::Data(reply(0, 2, &[])));
    script.extend(exchange(cmd::PROBE, 1, &words(&[1024, 512])));
    let mut io = ScriptedIo::new(script);
    Burner::new(&mut io).probe(Medium::Sd).unwrap();
}

#[test]
fn a_verify_reply_is_exactly_digest_verified_and_elapsed_or_it_is_not_a_match() {
    // The verify reply is 48 bytes: sha256 (32), verified bytes (8), elapsed ms (8). Any other length, or a
    // verified count other than the one asked for, is refused.
    let digest = [0xabu8; 32];
    let good = |count: u64, extra: usize| {
        let mut data = digest.to_vec();
        data.extend(words(&[count, 5]));
        data.resize(48 + extra, 0);
        data
    };
    let cases = [
        (digest[..31].to_vec(), false),
        (digest.to_vec(), false),
        (good(512, 0)[..47].to_vec(), false),
        (good(512, 0), true),
        (good(512, 6), false),
        (good(511, 0), false),
    ];
    for (data, fine) in cases {
        let mut script = medium(2);
        script.extend(exchange(cmd::VERIFY, 1, &data));
        let mut io = ScriptedIo::new(script);
        let mut burner = ready(&mut io);
        let result = burner.sha256(0, 512);
        assert_eq!(result.is_ok(), fine, "{} bytes: {result:?}", data.len());
        if let Ok(hex) = result {
            assert_eq!(hex, "ab".repeat(32));
        }
    }
}

#[test]
fn a_verify_the_burner_refuses_reports_its_message() {
    let mut script = medium(2);
    script.extend(exchange(cmd::VERIFY, 0xff, b"read error\0junk"));
    let mut io = ScriptedIo::new(script);
    let error = ready(&mut io).sha256(0, 512).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("read error"), "{text}");
    assert!(!text.contains("junk"), "{text}");
    assert!(matches!(error, UsbError::Protocol(_)));
}

#[test]
fn a_failed_erase_names_its_result_and_a_message_reply_gives_its_text() {
    // Result 2 is an error with no message; 0xff carries a message in the data.
    let mut script = medium(2);
    script.extend(exchange(cmd::ERASE, 2, &[]));
    script.extend(exchange(cmd::ERASE, 0xff, b"erase failed at 0x1000\0junk"));
    script.extend(exchange(cmd::ERASE, 1, &[]));
    let mut io = ScriptedIo::new(script);
    let mut burner = ready(&mut io);
    let plain = burner.erase(0, 4096).unwrap_err().to_string();
    assert!(plain.contains("result 2"), "{plain}");
    let message = burner.erase(0, 4096).unwrap_err().to_string();
    assert!(message.contains("erase failed at 0x1000"), "{message}");
    assert!(!message.contains("junk"), "{message}");
    burner.erase(0, 4096).unwrap();
}
