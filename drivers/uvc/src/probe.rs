//! The video probe and commit controls: the block both ends negotiate over.

use crate::descriptors::Mode;

/// `bRequestType` for a class request to an interface, host to device.
pub const REQUEST_OUT: u8 = 0x21;
/// `bRequestType` for a class request to an interface, device to host.
pub const REQUEST_IN: u8 = 0xa1;
pub const SET_CUR: u8 = 0x01;
pub const GET_CUR: u8 = 0x81;
pub const VS_PROBE_CONTROL: u8 = 0x01;
pub const VS_COMMIT_CONTROL: u8 = 0x02;

/// The largest control block of any UVC version.
pub const CONTROL_MAX: usize = 48;
/// The bytes needed to read every field this driver reads, which is all a UVC 1.0 device sends.
pub const CONTROL_MIN: usize = 26;

/// The block's length for a device of `bcdUVC` `version`.
pub fn control_len(version: u16) -> usize {
    if version >= 0x0150 {
        48
    } else if version >= 0x0110 {
        34
    } else {
        CONTROL_MIN
    }
}

/// A probe asking for `mode`, with the frame interval fixed.
pub fn request(mode: &Mode) -> [u8; CONTROL_MAX] {
    let mut block = [0u8; CONTROL_MAX];
    block[..2].copy_from_slice(&1u16.to_le_bytes());
    block[2] = mode.format_index;
    block[3] = mode.frame_index;
    block[4..8].copy_from_slice(&mode.interval.to_le_bytes());
    block
}

/// What a device answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    pub format_index: u8,
    pub frame_index: u8,
    /// `dwMaxPayloadTransferSize`: the bandwidth to reserve, in bytes per (micro)frame.
    pub max_payload: u32,
}

/// Reads a device's answer, or `None` if it is too short to hold what's needed.
pub fn decode(reply: &[u8]) -> Option<Negotiated> {
    let max_payload = reply.get(22..26)?;
    Some(Negotiated {
        format_index: *reply.get(2)?,
        frame_index: *reply.get(3)?,
        max_payload: u32::from_le_bytes([
            max_payload[0],
            max_payload[1],
            max_payload[2],
            max_payload[3],
        ]),
    })
}
