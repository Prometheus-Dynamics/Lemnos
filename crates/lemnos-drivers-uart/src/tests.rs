use crate::UartDriver;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InteractionRequest, InteractionResponse, StandardRequest,
    StandardResponse, UartConfiguration, UartDataBits, UartFlowControl, UartParity, UartRequest,
    UartResponse, UartStopBits, Value,
};
use lemnos_driver_sdk::{
    Driver, DriverBindContext, MAX_RETAINED_OUTPUT_BYTES, OUTPUT_PREVIEW, TELEMETRY_BYTES_WRITTEN,
    TELEMETRY_READ_OPS,
};
use lemnos_mock::{MockHardware, MockUartPort};

fn mock_port() -> (MockHardware, DeviceDescriptor) {
    let hardware = MockHardware::builder()
        .with_uart_port(
            MockUartPort::new("ttyUSB0")
                .with_display_name("debug-console")
                .with_configuration(UartConfiguration {
                    baud_rate: 115_200,
                    data_bits: UartDataBits::Eight,
                    parity: UartParity::None,
                    stop_bits: UartStopBits::One,
                    flow_control: UartFlowControl::None,
                })
                .with_rx_bytes([0x48, 0x69]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::UartPort)
        .expect("uart port");
    (hardware, device)
}

#[test]
fn binds_and_handles_uart_requests() {
    let (hardware, device) = mock_port();
    let mut bound = UartDriver
        .bind(&device, &DriverBindContext::default().with_uart(&hardware))
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Read { max_bytes: 2 },
        )))
        .expect("read");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Uart(UartResponse::Bytes(vec![
            0x48, 0x69,
        ])))
    );

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Write {
                bytes: vec![0xAA, 0x55],
            },
        )))
        .expect("write");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Uart(UartResponse::Applied))
    );
}

#[test]
fn state_reports_configuration_and_transfer_statistics() {
    let (hardware, device) = mock_port();
    let mut bound = UartDriver
        .bind(&device, &DriverBindContext::default().with_uart(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Read { max_bytes: 2 },
        )))
        .expect("read");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Write {
                bytes: vec![0xAA, 0x55],
            },
        )))
        .expect("write");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Flush,
        )))
        .expect("flush");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(
        state.realized_config.get("baud_rate"),
        Some(&Value::from(115_200_u64))
    );
    assert_eq!(
        state.realized_config.get("port"),
        Some(&Value::from("ttyUSB0"))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_READ_OPS),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_BYTES_WRITTEN),
        Some(&Value::from(2_u64))
    );
    assert!(state.last_operation.is_some());
}

#[test]
fn state_truncates_large_uart_read_payloads_in_last_operation() {
    let hardware = MockHardware::builder()
        .with_uart_port(MockUartPort::new("ttyUSB0").with_rx_bytes(vec![
            0x33;
            MAX_RETAINED_OUTPUT_BYTES
                + 5
        ]))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::UartPort)
        .expect("uart port");
    let mut bound = UartDriver
        .bind(&device, &DriverBindContext::default().with_uart(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Uart(
            UartRequest::Read {
                max_bytes: (MAX_RETAINED_OUTPUT_BYTES + 5) as u32,
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
