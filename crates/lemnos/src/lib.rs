#![forbid(unsafe_code)]

extern crate self as lemnos;

#[cfg(feature = "builtin-drivers")]
mod builtin;
mod facade;
pub mod prelude;

#[cfg(feature = "builtin-drivers")]
pub use builtin::BuiltInDriverBundle;
#[cfg(feature = "tokio")]
pub use facade::AsyncLemnos;
pub use facade::{Lemnos, LemnosBuilder};
#[cfg(feature = "macros")]
pub use lemnos_macros as macros;
pub use lemnos_runtime::DriverId;

pub use lemnos_bus as bus;
pub use lemnos_core as core;
pub use lemnos_discovery as discovery;
#[cfg(feature = "linux-backend")]
pub use lemnos_linux as linux;
#[cfg(feature = "mock")]
pub use lemnos_mock as mock;

pub mod driver {
    pub use lemnos_bus::{
        GpioEdgeEvent, GpioEdgeStreamSession, StreamSession, UartReadChunk, UartStreamSession,
        UsbInterruptPacket, UsbInterruptStreamSession,
    };
    pub use lemnos_driver_manifest::{
        DriverManifest, DriverPriority, DriverVersion, InteractionKind, InteractionManifest,
        ManifestError, ManifestMatch, ManifestResult, MatchCondition, MatchRule,
    };
    pub use lemnos_driver_sdk::{
        BoundDevice, CONFIG_ACTIVE_LOW, CONFIG_ADDRESS, CONFIG_ADDRESS_HEX, CONFIG_BAUD_RATE,
        CONFIG_BIAS, CONFIG_BIT_ORDER, CONFIG_BITS_PER_WORD, CONFIG_BUS, CONFIG_CHANNEL,
        CONFIG_CHIP_NAME, CONFIG_CHIP_SELECT, CONFIG_DATA_BITS, CONFIG_DEBOUNCE_US,
        CONFIG_DIRECTION, CONFIG_DRIVE, CONFIG_DUTY_CYCLE_NS, CONFIG_EDGE, CONFIG_ENABLED,
        CONFIG_FLOW_CONTROL, CONFIG_INITIAL_LEVEL, CONFIG_INTERFACE_NUMBER, CONFIG_LEVEL,
        CONFIG_MAX_FREQUENCY_HZ, CONFIG_MODE, CONFIG_PARITY, CONFIG_PERIOD_NS, CONFIG_POLARITY,
        CONFIG_PORT, CONFIG_PORTS, CONFIG_PRODUCT_ID, CONFIG_STOP_BITS, CONFIG_VENDOR_ID,
        ConformanceError, ConformanceResult, CustomInteraction, Driver, DriverBindContext,
        DriverConformanceHarness, DriverError, DriverMatch, DriverMatchLevel, DriverResult,
        GpioDeviceIo, I2cControllerIo, I2cControllerSession, I2cControllerTarget, I2cDeviceIo,
        I2cSession, LinuxClassDeviceIo, NoopBoundDevice, OUTPUT_BYTES_PREVIEW_KIND, OUTPUT_KIND,
        OUTPUT_LEN, OUTPUT_PREVIEW, OUTPUT_RETAINED_LEN, OUTPUT_TRUNCATED, PwmDeviceIo,
        SessionAccess, SpiDeviceIo, TELEMETRY_BULK_READ_OPS, TELEMETRY_BULK_WRITE_OPS,
        TELEMETRY_BYTES_READ, TELEMETRY_BYTES_WRITTEN, TELEMETRY_CLAIM_OPS,
        TELEMETRY_CLAIMED_INTERFACE_COUNT, TELEMETRY_CONFIGURE_OPS, TELEMETRY_CONTROL_OPS,
        TELEMETRY_DUTY_CYCLE_PERCENT, TELEMETRY_DUTY_CYCLE_RATIO, TELEMETRY_ENABLE_OPS,
        TELEMETRY_FLUSH_OPS, TELEMETRY_INTERRUPT_READ_OPS, TELEMETRY_INTERRUPT_WRITE_OPS,
        TELEMETRY_READ_OPS, TELEMETRY_RELEASE_OPS, TELEMETRY_SET_DUTY_CYCLE_OPS,
        TELEMETRY_SET_PERIOD_OPS, TELEMETRY_TRANSACTION_OPS, TELEMETRY_TRANSFER_OPS,
        TELEMETRY_WRITE_OPS, TELEMETRY_WRITE_READ_OPS, UartDeviceIo, UsbDeviceIo,
        bind_session_for_kind, bind_session_for_kinds, cached_manifest, gpio, i2c,
        interaction_name, pwm, spi, uart, usb,
    };
}

#[cfg(feature = "builtin-drivers")]
pub mod drivers {
    pub use lemnos_drivers_gpio as gpio;
    pub use lemnos_drivers_i2c as i2c;
    pub use lemnos_drivers_pwm as pwm;
    pub use lemnos_drivers_spi as spi;
    pub use lemnos_drivers_uart as uart;
    pub use lemnos_drivers_usb as usb;
}

#[cfg(all(
    test,
    feature = "builtin-drivers",
    feature = "macros",
    feature = "mock"
))]
mod tests;
