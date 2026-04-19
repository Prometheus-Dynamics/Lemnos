use crate::UsbDriver;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InteractionRequest, InteractionResponse, StandardRequest,
    StandardResponse, UsbControlSetup, UsbControlTransfer, UsbDirection, UsbRecipient, UsbRequest,
    UsbRequestType, UsbResponse, Value,
};
use lemnos_driver_sdk::{
    Driver, DriverBindContext, MAX_RETAINED_OUTPUT_BYTES, OUTPUT_PREVIEW, TELEMETRY_BULK_READ_OPS,
    TELEMETRY_CLAIMED_INTERFACE_COUNT, TELEMETRY_INTERRUPT_WRITE_OPS,
};
use lemnos_mock::{MockHardware, MockUsbDevice};

fn mock_device() -> (MockHardware, DeviceDescriptor, DeviceDescriptor) {
    let hardware = MockHardware::builder()
        .with_usb_device(
            MockUsbDevice::new(1, [2])
                .with_vendor_product(0x1209, 0x0001)
                .with_interface(0)
                .with_control_response(
                    UsbControlTransfer {
                        setup: UsbControlSetup {
                            direction: UsbDirection::In,
                            request_type: UsbRequestType::Vendor,
                            recipient: UsbRecipient::Interface,
                            request: 0x01,
                            value: 0,
                            index: 0,
                        },
                        data: vec![0; 4],
                        timeout_ms: Some(100),
                    },
                    [0xDE, 0xAD, 0xBE, 0xEF],
                )
                .with_bulk_in_response(0x81, [0x10, 0x20, 0x30])
                .with_interrupt_in_response(0x82, [0x44, 0x55]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::UsbDevice)
        .expect("usb device");
    let interface = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::UsbInterface)
        .expect("usb interface");
    (hardware, device, interface)
}

#[test]
fn binds_and_handles_usb_requests() {
    let (hardware, _, interface) = mock_device();
    let mut bound = UsbDriver
        .bind(
            &interface,
            &DriverBindContext::default().with_usb(&hardware),
        )
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            },
        )))
        .expect("claim");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::InterfaceClaimed {
            interface_number: 0,
            alternate_setting: Some(1),
        }))
    );

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::Control(UsbControlTransfer {
                setup: UsbControlSetup {
                    direction: UsbDirection::In,
                    request_type: UsbRequestType::Vendor,
                    recipient: UsbRecipient::Interface,
                    request: 0x01,
                    value: 0,
                    index: 0,
                },
                data: vec![0; 4],
                timeout_ms: Some(100),
            }),
        )))
        .expect("control");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(vec![
            0xDE, 0xAD, 0xBE, 0xEF,
        ])))
    );
}

#[test]
fn state_reports_configuration_and_transfer_statistics() {
    let (hardware, _, interface) = mock_device();
    let mut bound = UsbDriver
        .bind(
            &interface,
            &DriverBindContext::default().with_usb(&hardware),
        )
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: None,
            },
        )))
        .expect("claim");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::BulkRead {
                endpoint: 0x81,
                length: 3,
                timeout_ms: Some(100),
            },
        )))
        .expect("bulk read");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::InterruptWrite(lemnos_core::UsbInterruptTransfer {
                endpoint: 0x02,
                bytes: vec![0x99, 0x88],
                timeout_ms: Some(100),
            }),
        )))
        .expect("interrupt write");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(state.realized_config.get("bus"), Some(&Value::from(1_u64)));
    assert_eq!(
        state.realized_config.get("interface_number"),
        Some(&Value::from(0_u64))
    );
    assert_eq!(state.realized_config.get("ports"), Some(&Value::from("2")));
    assert_eq!(
        state.realized_config.get("vendor_id"),
        Some(&Value::from("1209"))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_BULK_READ_OPS),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_INTERRUPT_WRITE_OPS),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state.telemetry.get(TELEMETRY_CLAIMED_INTERFACE_COUNT),
        Some(&Value::from(1_u64))
    );
    assert!(state.last_operation.is_some());
}

#[test]
fn state_truncates_large_usb_read_payloads_in_last_operation() {
    let hardware = MockHardware::builder()
        .with_usb_device(
            MockUsbDevice::new(1, [2])
                .with_interface(0)
                .with_bulk_in_response(0x81, vec![0x7E; MAX_RETAINED_OUTPUT_BYTES + 6]),
        )
        .build();
    let interface = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::UsbInterface)
        .expect("usb interface");
    let mut bound = UsbDriver
        .bind(
            &interface,
            &DriverBindContext::default().with_usb(&hardware),
        )
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Usb(
            UsbRequest::BulkRead {
                endpoint: 0x81,
                length: (MAX_RETAINED_OUTPUT_BYTES + 6) as u32,
                timeout_ms: Some(100),
            },
        )))
        .expect("bulk read");

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
