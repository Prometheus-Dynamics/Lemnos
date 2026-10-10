//! I2C operations as PIO TX words, and the RX words decoded back into read
//! bytes and ACK checks. The encoding is the one described in [`super::program`].

use super::program::{MAX_WRITES_PER_PUSH, read_word, start_words, stop_words, write_word};

/// One step on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// START, or a repeated START.
    Start,
    /// STOP.
    Stop,
    /// One byte written to the target (or the 7-bit address byte).
    Write(u8),
    /// One byte read from the target. `ack` is true for every byte except the
    /// last, which is NACKed.
    Read { ack: bool },
}

/// A decoded ACK failure: the op index of the WRITE that was NACKed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nack {
    /// Index into the op list.
    pub op: usize,
    /// The WRITE's byte (the address byte for the first write of a START).
    pub byte: u8,
}

/// Why a decode failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The RX stream had fewer or more words than the ops imply.
    Length { expected: usize, actual: usize },
    /// A WRITE was NACKed.
    Nack(Nack),
}

/// The 7-bit address and transfer direction as the address byte.
pub fn address_byte(address: u8, read: bool) -> u8 {
    (address << 1) | u8::from(read)
}

/// The ops of one `write_read`: START, the address write, the write bytes,
/// then (if reading) a repeated START, the address read and the read bytes,
/// then STOP. A pure read skips the write phase.
pub fn write_read_ops(address: u8, write: &[u8], read_len: usize) -> Vec<Op> {
    let mut ops = Vec::with_capacity(write.len() + read_len + 5);
    if !write.is_empty() || read_len == 0 {
        ops.push(Op::Start);
        ops.push(Op::Write(address_byte(address, false)));
        ops.extend(write.iter().copied().map(Op::Write));
    }
    if read_len > 0 {
        ops.push(Op::Start);
        ops.push(Op::Write(address_byte(address, true)));
        ops.extend((0..read_len).map(|i| Op::Read {
            ack: i + 1 < read_len,
        }));
    }
    ops.push(Op::Stop);
    ops
}

/// Whether a list of ops stays within the ACK accumulator: at most
/// [`MAX_WRITES_PER_PUSH`] WRITEs between consecutive START/STOP pushes.
pub fn check_ack_window(ops: &[Op]) -> Result<(), usize> {
    let mut writes = 0usize;
    for op in ops {
        match op {
            Op::Write(_) => {
                writes += 1;
                if writes > MAX_WRITES_PER_PUSH {
                    return Err(writes);
                }
            }
            Op::Start | Op::Stop | Op::Read { .. } => writes = 0,
        }
    }
    Ok(())
}

/// TX words for `ops` (entries relative to program base `base`, SCL pin `scl`).
/// The bus starts idle; a START from idle releases the lines before SDA falls, a
/// repeated START from the data loop first lowers SCL. A STOP returns to idle.
pub fn encode(base: u16, scl: u16, ops: &[Op]) -> Vec<u32> {
    let mut words = Vec::new();
    let mut idle = true;
    for op in ops {
        match *op {
            Op::Start => {
                words.extend(start_words(base, scl, idle));
                idle = false;
            }
            Op::Stop => {
                words.extend(stop_words(base, scl));
                idle = true;
            }
            Op::Write(byte) => words.push(write_word(base, byte)),
            Op::Read { ack } => words.push(read_word(base, ack)),
        }
    }
    words
}

/// RX words the ops produce: one per START/STOP and one per READ.
pub fn rx_word_count(ops: &[Op]) -> usize {
    ops.iter().filter(|op| !matches!(op, Op::Write(_))).count()
}

/// Decodes the RX words of `ops` into the bytes read (in order), checking every
/// WRITE's ACK. The first START/STOP word carries whatever the ISR held before
/// the transaction; it is not an ACK and is skipped.
pub fn decode(ops: &[Op], rx: &[u32]) -> Result<Vec<u8>, DecodeError> {
    let expected = rx_word_count(ops);
    if rx.len() != expected {
        return Err(DecodeError::Length {
            expected,
            actual: rx.len(),
        });
    }
    let mut words = rx.iter().copied();
    let mut data = Vec::new();
    // Indices of the WRITE ops since the last push, oldest first.
    let mut pending: Vec<usize> = Vec::new();
    let mut first = true;
    for (index, op) in ops.iter().enumerate() {
        match *op {
            Op::Write(_) => pending.push(index),
            Op::Start | Op::Stop => {
                let word = words.next().unwrap_or(0);
                if !first {
                    check_acks(ops, &pending, word, 0)?;
                }
                first = false;
                pending.clear();
            }
            Op::Read { .. } => {
                let word = words.next().unwrap_or(0);
                // The first READ after a push also carries the ACK of the
                // address write before it, one bit above the byte.
                check_acks(ops, &pending, word, 8)?;
                data.push(word as u8);
                pending.clear();
            }
        }
    }
    Ok(data)
}

/// Checks the ACK bits of `pending` WRITEs in `word`, the newest at bit
/// `offset`. A set bit is a NACK (SDA stayed high during the ACK slot).
fn check_acks(ops: &[Op], pending: &[usize], word: u32, offset: u32) -> Result<(), DecodeError> {
    let count = pending.len() as u32;
    for (k, &op_index) in pending.iter().enumerate() {
        let bit = offset + (count - 1 - k as u32);
        if word >> bit & 1 != 0 {
            let byte = match ops[op_index] {
                Op::Write(byte) => byte,
                _ => 0,
            };
            return Err(DecodeError::Nack(Nack { op: op_index, byte }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_is_start_addr_bytes_restart_addr_reads_stop() {
        let ops = write_read_ops(0x18, &[0x00], 2);
        assert_eq!(
            ops,
            vec![
                Op::Start,
                Op::Write(0x30),
                Op::Write(0x00),
                Op::Start,
                Op::Write(0x31),
                Op::Read { ack: true },
                Op::Read { ack: false },
                Op::Stop,
            ]
        );
    }

    #[test]
    fn pure_read_and_pure_write_skip_the_other_phase() {
        assert_eq!(
            write_read_ops(0x68, &[], 1),
            vec![
                Op::Start,
                Op::Write(0xd1),
                Op::Read { ack: false },
                Op::Stop
            ]
        );
        assert_eq!(
            write_read_ops(0x68, &[1, 2], 0),
            vec![
                Op::Start,
                Op::Write(0xd0),
                Op::Write(1),
                Op::Write(2),
                Op::Stop
            ]
        );
        // An address probe: START, address, STOP.
        assert_eq!(
            write_read_ops(0x18, &[], 0),
            vec![Op::Start, Op::Write(0x30), Op::Stop]
        );
    }

    #[test]
    fn rx_count_is_one_per_start_stop_and_read() {
        let ops = write_read_ops(0x18, &[0x00], 6);
        assert_eq!(rx_word_count(&ops), 3 + 6);
    }

    #[test]
    fn ack_window_limits_writes_between_pushes() {
        let mut ops = vec![Op::Start];
        ops.extend((0..MAX_WRITES_PER_PUSH).map(|_| Op::Write(0)));
        assert!(check_ack_window(&ops).is_ok());
        ops.push(Op::Write(0));
        assert_eq!(check_ack_window(&ops), Err(MAX_WRITES_PER_PUSH + 1));
        // A STOP between the 32nd and 33rd write opens a new window.
        ops.insert(MAX_WRITES_PER_PUSH + 1, Op::Stop);
        assert!(check_ack_window(&ops).is_ok());
    }

    #[test]
    fn decode_reads_bytes_and_ignores_the_leading_word() {
        let ops = write_read_ops(0x18, &[0x00], 2);
        let rx = [
            0xdead_beef, // first START: whatever the ISR held
            0,           // second START: address and register ACKs (both 0)
            0xab,        // READ 1: byte 0xab; the address-read ACK is bit 8 (0)
            0xcd,        // READ 2: byte 0xcd
            0,           // STOP: no writes pending
        ];
        assert_eq!(decode(&ops, &rx), Ok(vec![0xab, 0xcd]));
    }

    #[test]
    fn decode_reports_the_nacked_write() {
        // Ops: Start, W addr, W reg, Start, W addr|R, Read, Stop -> RX words:
        // START (junk), START (ACKs of addr and reg, bit 1 and bit 0), READ, STOP.
        let ops = write_read_ops(0x18, &[0x00], 1);
        assert_eq!(
            decode(&ops, &[0, 0b10, 0x00, 0]),
            Err(DecodeError::Nack(Nack { op: 1, byte: 0x30 }))
        );
        assert_eq!(
            decode(&ops, &[0, 0b01, 0x00, 0]),
            Err(DecodeError::Nack(Nack { op: 2, byte: 0x00 }))
        );
    }

    #[test]
    fn decode_checks_the_length() {
        let ops = write_read_ops(0x18, &[], 1);
        assert_eq!(
            decode(&ops, &[0]),
            Err(DecodeError::Length {
                expected: 3,
                actual: 1
            })
        );
    }

    #[test]
    fn address_byte_appends_the_direction() {
        assert_eq!(address_byte(0x18, false), 0x30);
        assert_eq!(address_byte(0x68, true), 0xd1);
    }
}
