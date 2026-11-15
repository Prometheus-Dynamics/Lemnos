//! The medium the stage writes: its geometry and the three operations the stage uses. The
//! board's flash or card driver implements it later; the simulator and the RAM medium implement
//! it now.

/// A medium's geometry, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The medium's size.
    pub capacity: u64,
    /// The smallest unit a write covers (the logical block).
    pub block: u64,
    /// The smallest unit an erase covers.
    pub erase: u64,
}

/// A medium operation that failed, with the reason as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediumError(pub &'static str);

/// The operations the stage needs. Every method is bounded: it does a fixed amount of work for
/// the size it is given, and the watchdog covers any driver that blocks.
pub trait Medium {
    /// The medium's geometry.
    fn geometry(&self) -> Geometry;
    /// Reads `out.len()` bytes at `offset`.
    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError>;
    /// Writes `data` at `offset`.
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError>;
    /// Erases `len` bytes at `offset` (to the erased state, 0xff).
    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError>;
}
