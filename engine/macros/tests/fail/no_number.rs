use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T { fn a(&mut self) -> Result<u8>; }
fn main() {}
