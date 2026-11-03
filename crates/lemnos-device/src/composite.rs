//! Composite devices: a device computed from other devices' channels
//! (`docs/composite-devices.md`). The host feeds the inputs, carries out the
//! outputs and owns the schedule; the composite holds only its own state.

use crate::{Device, DeviceError};
use lemnos_hal::ErrorKind;

/// The latest sample of one input: `present` is false when it has never been
/// read or its read failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub value: i32,
    pub age_ms: u32,
    pub present: bool,
}

impl Sample {
    /// No sample.
    pub const NONE: Self = Self {
        value: 0,
        age_ms: 0,
        present: false,
    };
}

/// One input the composite reads: a channel of another device, whose unit
/// the composite's curves are in. The host binds it to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSpec {
    /// Required inputs decide the composite's health: a missing or stale one
    /// makes it degraded. Optional ones are left out while they are missing.
    pub required: bool,
    /// The oldest sample the composite accepts (ms).
    pub max_age_ms: u32,
}

/// One output the composite drives: a control of another device (the host
/// names it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSpec {
    /// The control's value is a fraction of full scale (per-mille).
    pub per_mille: bool,
}

/// The host's side of a composite's outputs: `output` is the index in
/// [`Composite::outputs`].
pub trait Outputs {
    /// Writes `value` to output `output`. The host applies its write policy
    /// and records failures.
    fn set(&mut self, output: usize, value: i32) -> Result<(), ErrorKind>;
    /// Hands output `output` back to its owner (its failsafe).
    fn release(&mut self, output: usize);
}

/// A device computed from other devices' channels. To clients it is an
/// ordinary device: read with [`Sensor::read`](crate::Sensor::read), tuned
/// with [`Control::set`](crate::Control::set). The host calls [`step`](Self::step)
/// every [`period_ms`](Self::period_ms) with the latest sample of each input.
pub trait Composite: Device {
    /// The inputs, in the order `step` takes their samples.
    fn inputs(&self) -> &[InputSpec];
    /// The outputs the composite drives.
    fn outputs(&self) -> &[OutputSpec];
    /// How often the host calls `step` (ms).
    fn period_ms(&self) -> u32;
    /// Computes one step at `now_ms` (a monotonic clock, ms). `inputs[i]` is
    /// input `i`'s latest sample; a failed write is an error, and the host
    /// hands the output back.
    fn step(
        &mut self,
        now_ms: u64,
        inputs: &[Sample],
        out: &mut dyn Outputs,
    ) -> Result<(), DeviceError<Self::Error>>;
    /// The host stops the composite: its outputs go back to their failsafe.
    fn release(&mut self, out: &mut dyn Outputs);
}
