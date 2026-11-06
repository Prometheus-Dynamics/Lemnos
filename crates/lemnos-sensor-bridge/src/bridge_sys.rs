//! The Lemnos bridge ABI the bridge client uses: the private event type, control ids and the
//! acknowledgement encoding, mirroring `kernel-modules/lemnos-sensor-bridge/lemnos_sensor_bridge.h`.
//! The V4L2 structures and ioctls themselves come from [`lemnos_v4l2::v4l2`] and [`lemnos_v4l2::subdev`].

/// `LEMNOS_BRIDGE_PROTOCOL_VERSION`.
pub const PROTOCOL_VERSION: u32 = 1;
/// `V4L2_EVENT_PRIVATE_START`.
pub const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;
/// `LEMNOS_BRIDGE_EVENT_STREAM`.
pub const EVENT_STREAM: u32 = V4L2_EVENT_PRIVATE_START + 0x5354;
/// `LEMNOS_BRIDGE_ACTION_START`.
pub const ACTION_START: u32 = 1;
/// `LEMNOS_BRIDGE_ACTION_STOP`.
pub const ACTION_STOP: u32 = 2;
/// `LEMNOS_BRIDGE_FLAG_CONTINUOUS_CLOCK`.
pub const FLAG_CONTINUOUS_CLOCK: u32 = 1;

/// `LEMNOS_CID_BASE`.
pub const LEMNOS_CID_BASE: u32 = lemnos_v4l2::v4l2::cid::USER_BASE + 0x1f00;
/// `LEMNOS_CID_STREAM_ACK` (integer64, execute on write).
pub const CID_STREAM_ACK: u32 = LEMNOS_CID_BASE;
/// `LEMNOS_CID_STREAM_STATE` (read-only, volatile).
pub const CID_STREAM_STATE: u32 = LEMNOS_CID_BASE + 1;
/// `LEMNOS_CID_ACK_TIMEOUT_MS`.
pub const CID_ACK_TIMEOUT_MS: u32 = LEMNOS_CID_BASE + 2;
/// `LEMNOS_CID_POWER`.
pub const CID_POWER: u32 = LEMNOS_CID_BASE + 3;
/// `LEMNOS_CID_STREAM_SEQUENCE` (read-only, volatile).
pub const CID_STREAM_SEQUENCE: u32 = LEMNOS_CID_BASE + 4;

/// `LEMNOS_BRIDGE_ACK(seq, status)`.
pub const fn ack_value(sequence: u32, status: u32) -> i64 {
    (((status as u64) << 32) | sequence as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lemnos_ids_match_the_header() {
        assert_eq!(EVENT_STREAM, 0x0800_5354);
        assert_eq!(CID_STREAM_ACK, 0x0098_2800);
        assert_eq!(CID_STREAM_SEQUENCE, 0x0098_2804);
        assert_eq!(ack_value(7, 0), 7);
        assert_eq!(ack_value(7, 5), (5i64 << 32) | 7);
        assert_eq!(ack_value(u32::MAX, u32::MAX), -1);
    }
}
