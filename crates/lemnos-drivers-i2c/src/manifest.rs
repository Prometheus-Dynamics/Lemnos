use lemnos_driver_sdk::i2c;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.i2c.generic";
    summary: "Generic I2C driver bundle";
    interface: lemnos_core::InterfaceKind::I2c;
    kind: lemnos_core::DeviceKind::I2cDevice;
    interactions: &[
        (i2c::READ_INTERACTION, "Read from device"),
        (i2c::WRITE_INTERACTION, "Write to device"),
        (i2c::WRITE_READ_INTERACTION, "Write then read from device"),
        (i2c::TRANSACTION_INTERACTION, "Execute an I2C transaction"),
    ];
}
