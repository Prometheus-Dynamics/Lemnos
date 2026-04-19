extern crate self as lemnos;

#[path = "../support/lemnos_shim.rs"]
mod lemnos_shim;

pub use lemnos_shim::{core, discovery, driver};

#[lemnos_macros::driver(
    id = "example.bad.driver",
    summary = "Bad driver",
    interface = I2c,
    kind = GpioLine
)]
struct BadDriver;

fn main() {}
