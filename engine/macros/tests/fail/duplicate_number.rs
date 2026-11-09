use lemnos_core::{contract, Result};
#[contract("x", version = 1)]
pub trait T {
    #[method(1)] fn a(&mut self) -> Result<u8>;
    #[method(1)] fn b(&mut self) -> Result<u8>;
}
fn main() {}
