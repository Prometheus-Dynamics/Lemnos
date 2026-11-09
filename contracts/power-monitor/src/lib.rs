//! The `power-monitor/1` contract: what a rail is delivering.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// One reading of a monitored rail.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Default)]
pub struct PowerReading {
    /// Volts on the bus.
    pub bus_v: f32,
    /// Volts across the shunt.
    pub shunt_v: f32,
    /// Amps. Negative when the current runs the other way.
    pub current_a: f32,
    /// Watts.
    pub power_w: f32,
}

#[contract("power-monitor", version = 1)]
pub trait PowerMonitor {
    #[method(1)]
    fn read(&mut self) -> Result<PowerReading>;
}
