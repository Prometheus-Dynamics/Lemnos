//! The `fan-control/1` contract: a controller that turns temperatures into a fan duty. Duty is
//! per-mille, 0 to 1000.

#![no_std]

use lemnos_core::{Decode, Encode, Reader, Result, Writer, contract};

/// Most points in one curve.
pub const MAX_POINTS: usize = 8;
/// Bytes one [`Point`] takes in a curve.
pub const POINT_BYTES: usize = 4 + 4 + 4;

/// One point of a curve: at `value`, in the input's unit, the input asks for `duty`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub value: i32,
    pub duty: i32,
}

impl Point {
    /// Encodes `points` the way [`FanControl::set_curve`] takes them.
    pub fn write_all(points: &[Point], out: &mut [u8]) -> Result<usize> {
        let mut writer = Writer::new(out);
        for point in points {
            point.encode(&mut writer)?;
        }
        Ok(writer.len())
    }

    /// The points in `bytes`, which must be a whole number of them.
    pub fn read_all(bytes: &[u8]) -> Result<impl Iterator<Item = Point> + '_> {
        // Checked first, so the iterator below can't meet a point it cannot read.
        let mut check = Reader::new(bytes);
        while check.remaining() > 0 {
            Point::decode(&mut check)?;
        }
        let mut from = Reader::new(bytes);
        Ok(core::iter::from_fn(move || {
            (from.remaining() > 0)
                .then(|| Point::decode(&mut from).ok())
                .flatten()
        }))
    }
}

/// The tuning.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// The lowest duty while the controller runs.
    pub floor: i32,
    /// How far an input must fall below its peak before its curve lets go.
    pub hysteresis: i32,
    /// The most the duty falls per second. Rising isn't limited.
    pub slew_per_s: i32,
    /// From a stopped fan, full duty for this long first. 0 disables it.
    pub kick_ms: u32,
}

/// What the controller decided on its last step.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// The curves' demand, before the floor and the slew limit.
    pub demand: i32,
    /// The duty commanded.
    pub duty: i32,
    /// A required input is missing or stale, so the duty is full.
    pub degraded: bool,
    /// The input whose curve asked for the most, if any did.
    pub source: Option<u8>,
    /// A timed hold is in force.
    pub held: bool,
}

#[contract("fan-control", version = 1)]
pub trait FanControl {
    #[method(1)]
    fn state(&mut self) -> Result<State>;
    #[method(2)]
    fn settings(&mut self) -> Result<Settings>;
    /// Replaces the tuning until the driver restarts, when the board config applies again.
    #[method(3)]
    fn set_settings(&mut self, settings: Settings) -> Result<()>;
    /// Writes one input's curve into `into`, in the form [`Point::read_all`] reads.
    #[method(4)]
    fn curve(&mut self, input: u8, #[out] into: &mut [u8]) -> Result<usize>;
    /// Replaces one input's curve until the driver restarts. `points` comes from
    /// [`Point::write_all`].
    #[method(5)]
    fn set_curve(&mut self, input: u8, points: &[u8]) -> Result<()>;
    /// Holds this duty for `ms`, overriding the curves.
    #[method(6)]
    fn hold(&mut self, duty: i32, ms: u32) -> Result<()>;
    /// Ends a hold early.
    #[method(7)]
    fn release_hold(&mut self) -> Result<()>;
    /// `false` stops the controller and releases its fan, which goes to its safe state (full duty).
    #[method(8)]
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
}
