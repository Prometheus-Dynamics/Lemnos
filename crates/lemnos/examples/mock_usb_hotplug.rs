#![allow(clippy::print_stdout)]

use lemnos::mock::{MockHardware, MockUsbDevice};
use lemnos::prelude::*;

fn usb_vendor_request() -> UsbControlTransfer {
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
    }
}

fn mock_usb_device() -> MockUsbDevice {
    MockUsbDevice::new(1, [2])
        .with_vendor_product(0x1209, 0x0001)
        .with_interface(0)
        .with_control_response(usb_vendor_request(), [0x10, 0x20, 0x30, 0x40])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usb = mock_usb_device();
    let hardware = MockHardware::builder().with_usb_device(usb.clone()).build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()?
        .build();

    lemnos.refresh_with_mock_default(&hardware)?;
    let interface_id = lemnos
        .inventory()
        .first_id_by_kind(lemnos::core::DeviceKind::UsbInterface)
        .expect("mock USB interface should be present");

    let claim = lemnos.claim_usb_interface(interface_id.clone(), 0, Some(1))?;
    println!("claim response: {:?}", claim.interaction);

    hardware.remove_device(&interface_id);
    lemnos.refresh_with_mock_default(&hardware)?;
    println!(
        "after removal inventory contains interface: {}",
        lemnos.inventory().contains(&interface_id)
    );

    hardware.attach_usb_device(usb);
    lemnos.refresh_with_mock_default(&hardware)?;

    let control = lemnos.request_usb(
        interface_id.clone(),
        UsbRequest::Control(usb_vendor_request()),
    )?;
    println!("control response: {:?}", control.interaction);

    Ok(())
}
