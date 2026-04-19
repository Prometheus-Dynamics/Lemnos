use lemnos_driver_sdk::gpio;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.gpio.generic";
    summary: "Generic GPIO driver bundle";
    interface: lemnos_core::InterfaceKind::Gpio;
    kind: lemnos_core::DeviceKind::GpioLine;
    interactions: &[
        (gpio::READ_INTERACTION, "Read line level"),
        (gpio::WRITE_INTERACTION, "Write line level"),
        (gpio::CONFIGURE_INTERACTION, "Configure line"),
        (
            gpio::GET_CONFIGURATION_INTERACTION,
            "Read active line configuration",
        ),
    ];
}
