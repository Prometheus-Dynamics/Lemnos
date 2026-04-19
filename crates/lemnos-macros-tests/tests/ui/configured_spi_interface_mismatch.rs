extern crate self as lemnos;

#[path = "../support/lemnos_shim.rs"]
mod lemnos_shim;

pub use lemnos_shim::{core, discovery, driver};

#[derive(lemnos_macros::ConfiguredDevice)]
#[lemnos(interface = I2c)]
struct BadConfig {
    #[lemnos(bus(spi))]
    bus: u32,
    #[lemnos(endpoint(spi, name = "flash"))]
    chip_select: u16,
}

fn main() {}
