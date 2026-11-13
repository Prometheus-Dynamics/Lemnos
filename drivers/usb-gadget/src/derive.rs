//! The NCM addresses a board's identity gives: `function ncm derive` in the `interfaces` text
//! (`docs/boards.md`, `docs/identity.md`). The rule is public so a host-side tool can compute the
//! same two addresses:
//!
//! 1. The input is the ASCII bytes of `<family>:<serial>`, where `family` is the board file's
//!    `[board] id` and `serial` is the board's normalised serial fact: lowercase hex, 8 to 32
//!    digits, leading zeros kept.
//! 2. The digest is SHA-256 of the 15 bytes `lemnos-usb-ncm\0` followed by that input.
//! 3. Take the first 6 bytes, and set the first octet to `(b0 & 0xfc) | 0x02`: unicast and
//!    locally administered.
//! 4. The host's address is those 6 bytes with the lowest bit of the last octet cleared; the
//!    device's is the same with that bit set.
//!
//! The two addresses differ in that one bit, so two boards of one type get two pairs, and no
//! fixed address is ever used. Worked vectors are in `docs/boards.md`.

use sha2::{Digest, Sha256};

/// The tag hashed before the input: `lemnos-usb-ncm` and a NUL byte.
pub const TAG: &[u8] = b"lemnos-usb-ncm\0";
/// The fewest and most digits a serial may have, as `lemnos-identity` normalises them.
pub const SERIAL_DIGITS: core::ops::RangeInclusive<usize> = 8..=32;

/// The host's and the device's address for one board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Macs {
    pub host: [u8; 6],
    pub device: [u8; 6],
}

/// The addresses of the board `family` with the normalised `serial`. `None` when the family is
/// empty or the serial is not in the normalised format: there is no fallback address.
pub fn macs(family: &str, serial: &str) -> Option<Macs> {
    let serial_ok = SERIAL_DIGITS.contains(&serial.len())
        && serial
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if family.is_empty() || !serial_ok {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(TAG);
    hash.update(family.as_bytes());
    hash.update(b":");
    hash.update(serial.as_bytes());
    let digest = hash.finalize();
    let mut base = [0u8; 6];
    base.copy_from_slice(&digest[..6]);
    base[0] = (base[0] & 0xfc) | 0x02;
    let mut host = base;
    host[5] &= !1;
    let mut device = base;
    device[5] |= 1;
    Some(Macs { host, device })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors computed with `hashlib` (see `docs/boards.md`), not with this code.
    #[test]
    fn the_worked_vectors_hold() {
        assert_eq!(
            macs("lemnos-test", "0123456789abcdef"),
            Some(Macs {
                host: [0xaa, 0x22, 0x75, 0x11, 0x2a, 0x72],
                device: [0xaa, 0x22, 0x75, 0x11, 0x2a, 0x73],
            })
        );
        assert_eq!(
            macs("alpha", "00000001"),
            Some(Macs {
                host: [0xb2, 0xb5, 0xba, 0x4c, 0x1e, 0x30],
                device: [0xb2, 0xb5, 0xba, 0x4c, 0x1e, 0x31],
            })
        );
    }

    #[test]
    fn the_pair_differs_in_the_last_bit_and_the_bits_are_right() {
        for (family, serial) in [("alpha", "00000001"), ("lemnos-test", "0123456789abcdef")] {
            let Macs { host, device } = macs(family, serial).unwrap();
            assert_eq!(host[..5], device[..5]);
            assert_eq!(host[5] & 1, 0, "host: last bit clear");
            assert_eq!(device[5], host[5] | 1, "device: the same bit set");
            assert_eq!(host[0] & 0x03, 0x02, "locally administered and unicast");
            assert_eq!(device[0] & 0x03, 0x02, "locally administered and unicast");
        }
    }

    #[test]
    fn a_serial_or_family_outside_the_format_has_no_address() {
        assert_eq!(macs("alpha", ""), None);
        assert_eq!(macs("alpha", "0123456"), None, "7 digits");
        assert_eq!(macs("alpha", &"a".repeat(33)), None, "33 digits");
        assert_eq!(macs("alpha", "0123456G"), None, "not hex");
        assert_eq!(macs("alpha", "0123ABCD"), None, "not lowercase");
        assert_eq!(macs("", "0123456789abcdef"), None, "no family");
    }

    #[test]
    fn a_different_serial_or_family_gives_a_different_pair() {
        let base = macs("alpha", "00000001");
        assert_ne!(base, macs("alpha", "00000002"));
        assert_ne!(base, macs("beta", "00000001"));
    }
}
