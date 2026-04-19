#![forbid(unsafe_code)]

mod conformance;
mod context;
mod device;
mod error;
pub mod gpio;
pub mod i2c;
mod linux;
mod matching;
mod operation;
mod output;
pub mod pwm;
pub mod spi;
mod state_helpers;
mod state_keys;
mod stats_helpers;
mod transport;
pub mod uart;
pub mod usb;

pub use conformance::{ConformanceError, ConformanceResult, DriverConformanceHarness};
pub use context::DriverBindContext;
pub use device::{
    BoundDevice, CustomInteraction, Driver, NoopBoundDevice, bind_session_for_kind,
    bind_session_for_kinds, bind_with_session, cached_manifest, close_session, ensure_device_kind,
    ensure_device_kinds, generic_driver_manifest,
    generic_driver_manifest_with_standard_interactions, interaction_name, unsupported_action_error,
    validate_request_for_device,
};
pub use error::{DriverError, DriverResult};
pub use gpio::GpioDeviceIo;
pub use i2c::{I2cControllerIo, I2cControllerTarget, I2cDeviceIo};
pub use lemnos_bus::{I2cControllerSession, I2cSession, SessionAccess};
pub use linux::LinuxClassDeviceIo;
pub use matching::{DriverMatch, DriverMatchLevel};
pub use operation::{succeeded_operation, succeeded_operation_with_output, with_last_operation};
pub use output::{
    MAX_RETAINED_OUTPUT_BYTES, OUTPUT_BYTES_PREVIEW_KIND, OUTPUT_KIND, OUTPUT_LEN, OUTPUT_PREVIEW,
    OUTPUT_RETAINED_LEN, OUTPUT_TRUNCATED, bounded_bytes_output,
};
pub use pwm::PwmDeviceIo;
pub use spi::SpiDeviceIo;
pub use state_helpers::with_byte_telemetry;
pub use state_keys::*;
pub use stats_helpers::{
    record_bytes_read_operation, record_bytes_written_count_operation,
    record_bytes_written_slice_operation, record_operation, record_output_operation,
};
pub use uart::UartDeviceIo;
pub use usb::UsbDeviceIo;

#[cfg(test)]
mod tests;
