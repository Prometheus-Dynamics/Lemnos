lemnos_driver_sdk::define_generic_session_driver! {
    pub struct UsbDriver;
    id: "lemnos.usb.generic";
    interface: lemnos_core::InterfaceKind::Usb;
    manifest: crate::manifest::manifest;
    kinds: &[lemnos_core::DeviceKind::UsbDevice, lemnos_core::DeviceKind::UsbInterface];
    expected: "usb-device or usb-interface";
    open: open_usb;
    access: ExclusiveController;
    bound: crate::bound::UsbBoundDevice;
    stats: crate::stats::UsbStats;
}
