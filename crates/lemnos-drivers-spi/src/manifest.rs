use lemnos_driver_sdk::spi;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.spi.generic";
    summary: "Generic SPI driver bundle";
    interface: lemnos_core::InterfaceKind::Spi;
    kind: lemnos_core::DeviceKind::SpiDevice;
    interactions: &[
        (
            spi::TRANSFER_INTERACTION,
            "Transfer bytes and receive response",
        ),
        (spi::WRITE_INTERACTION, "Write bytes to SPI device"),
        (spi::CONFIGURE_INTERACTION, "Configure SPI mode and timings"),
        (
            spi::GET_CONFIGURATION_INTERACTION,
            "Read active SPI configuration",
        ),
    ];
}
