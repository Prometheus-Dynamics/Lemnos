use lemnos_core::{Config, Lifecycle, Result, contract, driver};

#[contract("example.minimal", version = 1)]
pub trait Minimal {
    #[method(1)]
    fn value(&mut self) -> Result<u8>;
}

#[driver("minimal", provides = [Minimal])]
pub struct Plain {}

impl Lifecycle for Plain {
    type Needs = PlainNeeds;
    fn start(_needs: PlainNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

impl Minimal for Plain {
    fn value(&mut self) -> Result<u8> {
        Ok(1)
    }
}

fn main() {}
