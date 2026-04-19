use crate::SpiDriver;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InteractionRequest, InteractionResponse, SpiBitOrder,
    SpiConfiguration, SpiMode, SpiRequest, SpiResponse, StandardRequest, StandardResponse, Value,
};
use lemnos_driver_sdk::{Driver, DriverBindContext};
use lemnos_mock::{MockHardware, MockSpiDevice};

fn mock_device() -> (MockHardware, DeviceDescriptor) {
    let hardware = MockHardware::builder()
        .with_spi_device(
            MockSpiDevice::new(0, 1)
                .with_display_name("display-controller")
                .with_configuration(SpiConfiguration {
                    mode: SpiMode::Mode0,
                    max_frequency_hz: Some(2_000_000),
                    bits_per_word: Some(8),
                    bit_order: SpiBitOrder::MsbFirst,
                })
                .with_transfer_response([0x9F], [0x12, 0x34, 0x56]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::SpiDevice)
        .expect("spi device");
    (hardware, device)
}

#[test]
fn binds_and_handles_spi_requests() {
    let (hardware, device) = mock_device();
    let mut bound = SpiDriver
        .bind(&device, &DriverBindContext::default().with_spi(&hardware))
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Spi(
            SpiRequest::Transfer { write: vec![0x9F] },
        )))
        .expect("transfer");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Spi(SpiResponse::Bytes(vec![
            0x12, 0x34, 0x56,
        ])))
    );

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Spi(
            SpiRequest::Write {
                bytes: vec![0xAA, 0x55],
            },
        )))
        .expect("write");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Spi(SpiResponse::Applied))
    );
}

#[test]
fn state_reports_configuration_and_transfer_statistics() {
    let (hardware, device) = mock_device();
    let mut bound = SpiDriver
        .bind(&device, &DriverBindContext::default().with_spi(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Spi(
            SpiRequest::Transfer { write: vec![0x9F] },
        )))
        .expect("transfer");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Spi(
            SpiRequest::Write {
                bytes: vec![0xAA, 0x55],
            },
        )))
        .expect("write");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(state.realized_config.get("bus"), Some(&Value::from(0_u64)));
    assert_eq!(
        state.realized_config.get("chip_select"),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.realized_config.get("mode"),
        Some(&Value::from("mode0"))
    );
    assert_eq!(
        state.telemetry.get("transfer_ops"),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get("bytes_written"),
        Some(&Value::from(3_u64))
    );
    assert!(state.last_operation.is_some());
}
