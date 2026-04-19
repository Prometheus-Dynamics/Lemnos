#![forbid(unsafe_code)]

mod bound;
mod driver;
mod manifest;
mod stats;

pub use driver::I2cDriver;
pub use manifest::manifest;

#[cfg(test)]
mod tests;
