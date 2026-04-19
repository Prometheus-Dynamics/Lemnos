use lemnos_core::DeviceStateSnapshot;

use crate::{TELEMETRY_BYTES_READ, TELEMETRY_BYTES_WRITTEN};

pub fn with_byte_telemetry(
    state: DeviceStateSnapshot,
    bytes_read: u64,
    bytes_written: u64,
) -> DeviceStateSnapshot {
    state
        .with_telemetry(TELEMETRY_BYTES_READ, bytes_read)
        .with_telemetry(TELEMETRY_BYTES_WRITTEN, bytes_written)
}
