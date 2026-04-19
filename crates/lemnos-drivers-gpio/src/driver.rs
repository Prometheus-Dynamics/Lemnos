lemnos_driver_sdk::define_generic_session_driver! {
    pub struct GpioDriver;
    id: "lemnos.gpio.generic";
    interface: lemnos_core::InterfaceKind::Gpio;
    manifest: crate::manifest::manifest;
    kind: lemnos_core::DeviceKind::GpioLine;
    expected: "gpio-line device";
    open: open_gpio;
    access: Shared;
    bound: crate::bound::GpioBoundDevice;
    stats: crate::stats::GpioStats;
}
