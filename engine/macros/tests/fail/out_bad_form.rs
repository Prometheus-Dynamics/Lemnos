#[lemnos_core::contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&mut self, #[out(json)] into: &mut [u8]) -> lemnos_core::Result<usize>; }
fn main() {}
