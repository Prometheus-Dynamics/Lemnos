//! The register access the ISP's registers are read and written through.

/// Register access for the statistics reads ([`crate::stats::read_raw`]) and for a parameter
/// builder's writes: the device's ioctls (`crate::device`), or a fake register file in a
/// test. Written, untested on board.
pub trait RegisterAccess {
    /// What an access can fail with.
    type Error;

    /// The register at `addr`.
    fn read(&mut self, addr: u32) -> Result<u32, Self::Error>;

    /// Writes `value` to the register at `addr`.
    fn write(&mut self, addr: u32, value: u32) -> Result<(), Self::Error>;
}
