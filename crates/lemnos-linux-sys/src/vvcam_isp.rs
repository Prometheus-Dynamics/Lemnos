//! The K230's `vvcam-isp` misc device (`/dev/vvcam-isp.0`, the VeriSilicon ISP8000 behind the
//! K230D's camera pipeline): its register read and write requests and the register-pair
//! structure they carry. The numbers are the `_IOC` encodings of `vvcam_isp.h` (VeriSilicon,
//! MIT), spelled out and pinned by the tests below:
//!
//! - `VVCAM_ISP_RESET`: `_IOW('v', 0x01, uint32_t)`.
//! - `VVCAM_ISP_READ_REG`: `_IOWR('v', 0x02, vvcam_isp_reg_t)`.
//! - `VVCAM_ISP_WRITE_REG`: `_IOW('v', 0x03, vvcam_isp_reg_t)`.
//!
//! The calls over them are safe ([`crate::Ioctl::call`]); the `lemnos-drivers-k230isp` crate
//! makes them. Untested on board.

use crate::ioctl::{ioctls, pod};

/// `vvcam_isp_reg_t`: one register's address and value (8 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegPair {
    /// The register's offset within the register window the driver maps.
    pub addr: u32,
    /// The value read or written.
    pub value: u32,
}

pod!(RegPair);

ioctls! {
    RESET: u32 = iow(b'v', 0x01);
    READ_REG: RegPair = iowr(b'v', 0x02);
    WRITE_REG: RegPair = iow(b'v', 0x03);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_and_numbers_match_the_driver() {
        assert_eq!(core::mem::size_of::<RegPair>(), 8);
        assert_eq!(RESET.nr(), 0x4004_7601);
        assert_eq!(READ_REG.nr(), 0xC008_7602);
        assert_eq!(WRITE_REG.nr(), 0x4008_7603);
    }
}
