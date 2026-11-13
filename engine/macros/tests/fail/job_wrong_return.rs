use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T { #[method(1, job)] fn a(&mut self) -> Result<u32>; }
fn main() {}
