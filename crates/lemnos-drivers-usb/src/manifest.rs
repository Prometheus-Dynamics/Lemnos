use lemnos_driver_sdk::usb;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.usb.generic";
    summary: "Generic USB driver bundle";
    interface: lemnos_core::InterfaceKind::Usb;
    kinds: &[lemnos_core::DeviceKind::UsbDevice, lemnos_core::DeviceKind::UsbInterface];
    interactions: &[
        (
            usb::CONTROL_TRANSFER_INTERACTION,
            "Perform a USB control transfer",
        ),
        (
            usb::BULK_READ_INTERACTION,
            "Read bytes from a USB bulk endpoint",
        ),
        (
            usb::BULK_WRITE_INTERACTION,
            "Write bytes to a USB bulk endpoint",
        ),
        (
            usb::INTERRUPT_READ_INTERACTION,
            "Read bytes from a USB interrupt endpoint",
        ),
        (
            usb::INTERRUPT_WRITE_INTERACTION,
            "Write bytes to a USB interrupt endpoint",
        ),
        (usb::CLAIM_INTERFACE_INTERACTION, "Claim a USB interface"),
        (usb::RELEASE_INTERFACE_INTERACTION, "Release a USB interface"),
    ];
}
