lemnos_driver_sdk::define_generic_session_driver! {
    pub struct I2cDriver;
    id: "lemnos.i2c.generic";
    interface: lemnos_core::InterfaceKind::I2c;
    manifest: crate::manifest::manifest;
    kind: lemnos_core::DeviceKind::I2cDevice;
    expected: "i2c-device";
    open: open_i2c;
    access: Shared;
    bound: crate::bound::I2cBoundDevice;
    stats: crate::stats::I2cStats;
}
