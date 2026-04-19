#![forbid(unsafe_code)]

mod bound;
mod driver;
mod manifest;
mod stats;
mod values;

pub use driver::SpiDriver;
pub use manifest::manifest;

#[cfg(test)]
mod tests;
