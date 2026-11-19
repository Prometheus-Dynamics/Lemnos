//! The PMP read at the card image's entry (FLASH-EXTENT). The image reads `pmpcfg0` and `pmpaddr0` and prints what the
//! core gave, or that the read trapped. The read itself is in the `-sys` crate (`pmp::probe`), behind a trap vector of
//! its own; this module holds the outcome and its line, so the text is tested on the host.
//!
//! The line is one of two:
//! - `pmp: cfg0=0x<8 hex> addr0=0x<8 hex>`: both reads returned;
//! - `pmp: trapped mcause=0x<8 hex> (no PMP)`: a read trapped (an absent PMP CSR is an illegal instruction, mcause 2),
//!   the trap vector recorded the cause and stepped past the instruction, and the reset of the vector was restored.

use crate::stack::put_hex32;

/// What the read gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Both CSRs read: `pmpcfg0`, then `pmpaddr0`.
    Read { cfg: u32, addr: u32 },
    /// A read trapped, with the cause the trap vector recorded.
    Trapped { mcause: u32 },
}

/// Writes the outcome's line into `line` from the start, and returns its length. The line must hold at least
/// [`LINE_LEN`] bytes.
pub fn put_line(outcome: Outcome, line: &mut [u8]) -> usize {
    let mut at = 0;
    let (head, mid): (&[u8], &[u8]) = match outcome {
        Outcome::Read { .. } => (b"pmp: cfg0=0x", b" addr0=0x"),
        Outcome::Trapped { .. } => (b"pmp: trapped mcause=0x", b" (no PMP)"),
    };
    line[at..at + head.len()].copy_from_slice(head);
    at += head.len();
    match outcome {
        Outcome::Read { cfg, addr } => {
            at = put_hex32(line, at, cfg);
            line[at..at + mid.len()].copy_from_slice(mid);
            at += mid.len();
            at = put_hex32(line, at, addr);
        }
        Outcome::Trapped { mcause } => {
            at = put_hex32(line, at, mcause);
            line[at..at + mid.len()].copy_from_slice(mid);
            at += mid.len();
        }
    }
    at
}

/// The longest line [`put_line`] writes.
pub const LINE_LEN: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    fn text(outcome: Outcome) -> std::string::String {
        let mut line = [0u8; LINE_LEN];
        let n = put_line(outcome, &mut line);
        std::string::String::from_utf8(line[..n].to_vec()).unwrap()
    }

    #[test]
    fn a_read_prints_both_csrs_in_eight_hex_digits() {
        assert_eq!(
            text(Outcome::Read {
                cfg: 0x0000_0018,
                addr: 0xffff_ffff
            }),
            "pmp: cfg0=0x00000018 addr0=0xffffffff"
        );
    }

    #[test]
    fn a_trap_prints_its_cause_and_says_there_is_no_pmp() {
        assert_eq!(
            text(Outcome::Trapped { mcause: 2 }),
            "pmp: trapped mcause=0x00000002 (no PMP)"
        );
    }
}
