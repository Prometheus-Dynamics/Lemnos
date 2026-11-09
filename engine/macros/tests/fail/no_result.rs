use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&mut self) -> u8; }
fn main() {}
