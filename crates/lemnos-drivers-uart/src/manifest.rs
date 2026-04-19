use lemnos_driver_sdk::uart;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.uart.generic";
    summary: "Generic UART driver bundle";
    interface: lemnos_core::InterfaceKind::Uart;
    kind: lemnos_core::DeviceKind::UartPort;
    interactions: &[
        (uart::READ_INTERACTION, "Read bytes from UART"),
        (uart::WRITE_INTERACTION, "Write bytes to UART"),
        (uart::CONFIGURE_INTERACTION, "Configure UART parameters"),
        (uart::FLUSH_INTERACTION, "Flush UART output"),
        (
            uart::GET_CONFIGURATION_INTERACTION,
            "Read active UART configuration",
        ),
    ];
}
