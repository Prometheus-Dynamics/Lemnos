use crate::I2cDriver;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, I2cRequest, I2cResponse, InteractionRequest, InteractionResponse,
    StandardRequest, StandardResponse, Value,
};
use lemnos_driver_sdk::{
    Driver, DriverBindContext, MAX_RETAINED_OUTPUT_BYTES, OUTPUT_PREVIEW, TELEMETRY_BYTES_WRITTEN,
    TELEMETRY_READ_OPS, TELEMETRY_WRITE_OPS,
};
use lemnos_mock::{MockHardware, MockI2cDevice};

fn mock_device() -> (MockHardware, DeviceDescriptor) {
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(1, 0x48)
                .with_bytes(0x10, [0xAA, 0xBB, 0xCC])
                .with_display_name("temperature-sensor"),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::I2cDevice)
        .expect("i2c device");
    (hardware, device)
}

#[test]
fn binds_and_handles_i2c_write_read_requests() {
    let (hardware, device) = mock_device();
    let mut bound = I2cDriver
        .bind(&device, &DriverBindContext::default().with_i2c(&hardware))
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::I2c(
            I2cRequest::WriteRead {
                write: vec![0x10],
                read_length: 2,
            },
        )))
        .expect("write_read");

    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::I2c(I2cResponse::Bytes(vec![0xAA, 0xBB,])))
    );
}

#[test]
fn state_reports_address_and_transfer_statistics() {
    let (hardware, device) = mock_device();
    let mut bound = I2cDriver
        .bind(&device, &DriverBindContext::default().with_i2c(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::I2c(
            I2cRequest::Read { length: 3 },
        )))
        .expect("read");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::I2c(
            I2cRequest::Write {
                bytes: vec![0x20, 0x01, 0x02],
            },
        )))
        .expect("write");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(state.realized_config.get("bus"), Some(&Value::from(1_u64)));
    assert_eq!(
        state.realized_config.get("address"),
        Some(&Value::from(0x48_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_READ_OPS),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_WRITE_OPS),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_BYTES_WRITTEN),
        Some(&Value::from(3_u64))
    );
    assert!(state.last_operation.is_some());
}

#[test]
fn state_truncates_large_i2c_read_payloads_in_last_operation() {
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(1, 0x48).with_bytes(0x00, vec![0x5A; MAX_RETAINED_OUTPUT_BYTES + 4]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::I2cDevice)
        .expect("i2c device");
    let mut bound = I2cDriver
        .bind(&device, &DriverBindContext::default().with_i2c(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::I2c(
            I2cRequest::Read {
                length: (MAX_RETAINED_OUTPUT_BYTES + 4) as u32,
            },
        )))
        .expect("read");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");
    let output = state
        .last_operation
        .as_ref()
        .and_then(|record| record.output.as_ref())
        .expect("last operation output");
    let preview = output
        .as_map()
        .and_then(|map| map.get(OUTPUT_PREVIEW))
        .and_then(Value::as_bytes)
        .expect("preview bytes");
    assert_eq!(preview.len(), MAX_RETAINED_OUTPUT_BYTES);
}
