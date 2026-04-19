lemnos_driver_sdk::define_generic_session_driver! {
    pub struct UartDriver;
    id: "lemnos.uart.generic";
    interface: lemnos_core::InterfaceKind::Uart;
    manifest: crate::manifest::manifest;
    kind: lemnos_core::DeviceKind::UartPort;
    expected: "uart-port";
    open: open_uart;
    access: Shared;
    bound: crate::bound::UartBoundDevice;
    stats: crate::stats::UartStats;
}
