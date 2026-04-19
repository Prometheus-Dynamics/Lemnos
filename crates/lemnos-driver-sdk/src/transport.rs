use crate::DriverError;
use lemnos_core::DeviceId;

pub(crate) fn transport_error(
    driver_id: &str,
    device_id: &DeviceId,
    source: lemnos_bus::BusError,
) -> DriverError {
    DriverError::Transport {
        driver_id: driver_id.to_string(),
        device_id: device_id.clone(),
        source,
    }
}
