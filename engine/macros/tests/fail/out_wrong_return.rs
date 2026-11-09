use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&mut self, #[out] x: &mut [u8]) -> Result<u8>; }
fn main() {}
