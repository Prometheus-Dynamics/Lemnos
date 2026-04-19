lemnos_driver_sdk::define_generic_session_driver! {
    pub struct SpiDriver;
    id: "lemnos.spi.generic";
    interface: lemnos_core::InterfaceKind::Spi;
    manifest: crate::manifest::manifest;
    kind: lemnos_core::DeviceKind::SpiDevice;
    expected: "spi-device";
    open: open_spi;
    access: Shared;
    bound: crate::bound::SpiBoundDevice;
    stats: crate::stats::SpiStats;
}
