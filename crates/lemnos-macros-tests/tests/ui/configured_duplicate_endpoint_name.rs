extern crate self as lemnos;

#[path = "../support/lemnos_shim.rs"]
mod lemnos_shim;

pub use lemnos_shim::{core, discovery, driver};

#[derive(lemnos_macros::ConfiguredDevice)]
#[lemnos(interface = I2c)]
struct BadConfig {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    accel_address: u16,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    gyro_address: u16,
}

fn main() {}
