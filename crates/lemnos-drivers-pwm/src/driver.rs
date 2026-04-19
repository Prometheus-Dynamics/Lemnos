lemnos_driver_sdk::define_generic_session_driver! {
    pub struct PwmDriver;
    id: "lemnos.pwm.generic";
    interface: lemnos_core::InterfaceKind::Pwm;
    manifest: crate::manifest::manifest;
    kind: lemnos_core::DeviceKind::PwmChannel;
    expected: "pwm-channel device";
    open: open_pwm;
    access: Shared;
    bound: crate::bound::PwmBoundDevice;
    stats: crate::stats::PwmStats;
}
