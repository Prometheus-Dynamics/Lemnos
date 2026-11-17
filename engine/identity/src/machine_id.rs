//! The machine id a slot's boot derives from the board serial (`docs/slots.md`, "The machine id").
//! slot-init binds it over `/etc/machine-id`; the `system-identity` driver recomputes it from the
//! same serial to tell its own bind from systemd's transient id. One derivation, so the two agree.

use sha2::{Digest, Sha256};

/// The domain separator of the derivation: the same serial gives another id under another domain.
pub const DOMAIN: &str = "lemnos-machine-id:";

/// The id of a serial: 32 lowercase hex digits and a newline (the file's bytes).
pub fn derive(serial: &str) -> String {
    let digest = Sha256::digest(format!("{DOMAIN}{serial}").as_bytes());
    let mut first = [0u8; 16];
    first.copy_from_slice(&digest[..16]);
    from_bytes(first)
}

/// The id text of 16 bytes. An all-zero id is not a machine id (systemd reads it as none), so its
/// last bit is flipped.
pub fn from_bytes(mut bytes: [u8; 16]) -> String {
    if bytes.iter().all(|&byte| byte == 0) {
        bytes[15] ^= 1;
    }
    let mut text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The known vector: SHA-256 of `lemnos-machine-id:0123456789abcdef` is
    /// `2956f9f7d4ab48e9a2ad950f54b3f7b159e8e7e0b064e88b2ed8838654d899ef` (computed with
    /// `printf 'lemnos-machine-id:%s' 0123456789abcdef | sha256sum`); its first 16 bytes are the id.
    #[test]
    fn the_known_vector_is_the_first_sixteen_bytes_of_the_digest() {
        assert_eq!(
            derive("0123456789abcdef"),
            "2956f9f7d4ab48e9a2ad950f54b3f7b1\n"
        );
    }

    #[test]
    fn the_same_serial_gives_the_same_id_twice() {
        assert_eq!(derive("0123456789abcdef"), derive("0123456789abcdef"));
    }

    #[test]
    fn different_serials_give_different_ids() {
        // The digests computed with sha256sum: their first 16 bytes differ.
        assert_eq!(
            derive("0000000000000001"),
            "4f8d14d1fdc4f07c6c5424c6a0cd3145\n"
        );
        assert_eq!(
            derive("0000000000000002"),
            "2acfacded229ed3a1f126541e2e4055a\n"
        );
    }

    #[test]
    fn an_all_zero_id_has_its_last_bit_flipped() {
        assert_eq!(from_bytes([0; 16]), "00000000000000000000000000000001\n");
        let mut almost = [0u8; 16];
        almost[15] = 0x80;
        assert_eq!(from_bytes(almost), "00000000000000000000000000000080\n");
    }
}
