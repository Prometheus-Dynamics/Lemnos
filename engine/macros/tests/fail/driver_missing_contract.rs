use lemnos_core::{contract, driver, Config, Lifecycle, Result};
#[contract("x", version = 1)]
pub trait T { #[method(1)] fn a(&mut self) -> Result<u8>; }
#[driver("x", provides = [T])]
pub struct D {}
impl Lifecycle for D {
    type Needs = DNeeds;
    fn start(_n: DNeeds, _c: &Config<'_>) -> Result<Self> { Ok(Self {}) }
}
fn main() {}
