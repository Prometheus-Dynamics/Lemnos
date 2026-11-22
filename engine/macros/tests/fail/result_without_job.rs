#[lemnos_core::contract("x", version = 1)]
pub trait T { #[method(1, result = text)] fn a(&mut self) -> lemnos_core::Result<u32>; }
fn main() {}
