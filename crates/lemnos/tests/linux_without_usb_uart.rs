//! The facade's Linux API without the USB and UART transports, which pull in
//! libusb (C) and serialport. Runs in the `lemnos linux lite` CI matrix entry.
#![cfg(all(
    feature = "linux-backend",
    not(feature = "linux-usb"),
    not(feature = "linux-uart")
))]

use lemnos::bus::{BusError, SessionAccess, UartBusBackend, UsbBusBackend};
use lemnos::core::{DeviceDescriptor, DeviceKind, InterfaceKind};
use lemnos::discovery::DiscoveryContext;
use lemnos::linux::{LinuxBackend, LinuxPaths};
use lemnos::prelude::*;

#[test]
fn facade_linux_api_works_without_usb_and_uart() {
    let root = std::env::temp_dir().join(format!("lemnos-linux-lite-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("temp root");
    let backend = LinuxBackend::with_paths(
        LinuxPaths::new()
            .with_sys_class_root(root.join("sys/class"))
            .with_sys_bus_root(root.join("sys/bus"))
            .with_dev_root(root.join("dev")),
    );
    let mut lemnos = Lemnos::builder().with_linux_backend_ref(&backend).build();
    lemnos
        .refresh_with_linux(&DiscoveryContext::new(), &backend)
        .expect("refresh an empty Linux root");
    assert_eq!(lemnos.inventory_len(), 0);

    let device = DeviceDescriptor::builder_for_kind("usb.test", DeviceKind::UsbDevice)
        .expect("builder")
        .build()
        .expect("descriptor");
    let usb = backend.open_usb(&device, SessionAccess::Exclusive);
    assert!(matches!(
        usb.err(),
        Some(BusError::UnsupportedInterface {
            interface: InterfaceKind::Usb,
            ..
        })
    ));
    let uart = backend.open_uart(&device, SessionAccess::Exclusive);
    assert!(matches!(
        uart.err(),
        Some(BusError::UnsupportedInterface {
            interface: InterfaceKind::Uart,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(root);
}
