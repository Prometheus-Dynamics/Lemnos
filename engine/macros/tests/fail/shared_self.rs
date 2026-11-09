use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&self) -> Result<u8>; }
fn main() {}
