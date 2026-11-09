use lemnos_core::{contract, Result};
pub struct NotOnTheWire;
#[contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&mut self) -> Result<NotOnTheWire>; }
fn main() {}
