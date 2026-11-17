//! A medium in a byte slice: the stage's tests and the simulator write to it. Erasing sets bytes
//! to 0xff, as a flash does.

use crate::medium::{Geometry, Medium, MediumError};

/// A medium that is a borrowed byte slice.
pub struct RamMedium<'a> {
    bytes: &'a mut [u8],
    geometry: Geometry,
}

impl<'a> RamMedium<'a> {
    /// A medium over `bytes` with the given block and erase sizes. The capacity is the slice length.
    pub fn new(bytes: &'a mut [u8], block: u64, erase: u64) -> Self {
        let geometry = Geometry {
            capacity: bytes.len() as u64,
            block,
            erase,
        };
        Self { bytes, geometry }
    }

    /// The bytes, for a test to look at.
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    fn span(&self, offset: u64, len: usize) -> Result<core::ops::Range<usize>, MediumError> {
        let end = offset
            .checked_add(len as u64)
            .filter(|&end| end <= self.geometry.capacity)
            .ok_or(MediumError("past the end of the medium"))?;
        Ok(offset as usize..end as usize)
    }
}

impl Medium for RamMedium<'_> {
    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError> {
        let span = self.span(offset, out.len())?;
        out.copy_from_slice(&self.bytes[span]);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError> {
        let span = self.span(offset, data.len())?;
        self.bytes[span].copy_from_slice(data);
        Ok(())
    }

    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError> {
        let span = self.span(offset, len as usize)?;
        self.bytes[span].fill(0xff);
        Ok(())
    }
}
