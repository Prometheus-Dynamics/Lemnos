use lemnos_core::{driver, I2c};
#[driver("x", provides = [])]
pub struct D { #[needs(optional)] bus: I2c }
fn main() {}
