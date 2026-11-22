#[lemnos_core::contract("x", version = 1)]
pub trait T { #[method(1, job, stall = 60)] fn a(&mut self) -> lemnos_core::Result<lemnos_core::JobId>; }
fn main() {}
