//! Builders shared by the protocol tests. Replies are whole 60-byte packets as the burner sends
//! them (C:50-59); a scripted device answers them in order.
#![allow(dead_code)]

use lemnos_k230_usbboot::sim::Answer;

/// A 60-byte reply: `cmd` with the reply flag, `result`, then `data`.
pub fn reply(cmd: u16, result: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = reply_of_any_length(cmd, result, data);
    bytes.resize(60, 0);
    bytes
}

/// A reply of any length, for a bulk read of data (kburn_read_chunk).
pub fn reply_of_any_length(cmd: u16, result: u16, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for word in [cmd | 0x8000, result, data.len() as u16] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(data);
    bytes
}

/// The answers to one command: its packet is accepted, then its reply comes back.
pub fn exchange(cmd: u16, result: u16, data: &[u8]) -> [Answer; 2] {
    [Answer::Ok, Answer::Data(reply(cmd, result, data))]
}

/// A 4-byte little-endian integer, the loader version reply.
pub fn le32(value: u32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// The answers to a burner's start-up: its loader version, then the NOP that clears its error
/// state (C:872-897).
pub fn sync(version: u32) -> Vec<Answer> {
    let mut answers = vec![Answer::Data(le32(version))];
    answers.extend(exchange(0, 1, &[]));
    answers
}

/// The 32-byte CPU info of a stage, NUL padded, as the boot ROM answers it.
pub fn cpu_info(text: &str) -> Answer {
    let mut bytes = text.as_bytes().to_vec();
    bytes.resize(32, 0);
    Answer::Data(bytes)
}
