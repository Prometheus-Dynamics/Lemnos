//! A block's register region, mapped through the kernel module: [`ModuleRegisters`].

use std::os::fd::OwnedFd;

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::Registers;
use lemnos_linux_sys::mmap::{Mapping, page_size};
use lemnos_linux_sys::module::{self as sys, BlockInfo, INFO_MMIO_MAPPABLE};

use super::Node;
use crate::kind_of;

/// One register region of a block bound to the module, mapped shared and uncached by the module
/// (`regs:<block>/<n>`). Only a block the module says may be mapped is: one whose DMA its IOMMU
/// confines, so the registers can't be used to reach memory outside the block's own.
///
/// Each access is one aligned 32-bit load or store, inside the region. Nothing allocates.
///
/// Once the block is unbound the module removes the mapping's pages, and an access then faults:
/// a driver holding these should hold the block's interrupt too, which reports the unbind.
pub struct ModuleRegisters {
    map: Mapping,
    size: usize,
    _claim: OwnedFd,
}

/// Whether the module will map this block's region `index`: `NotFound` past its regions,
/// `PermissionDenied` when the module says its registers may not be mapped.
pub(crate) fn mappable(info: &BlockInfo, index: u32) -> Result<(), ErrorKind> {
    if index >= info.regions {
        return Err(ErrorKind::NotFound);
    }
    if info.flags & INFO_MMIO_MAPPABLE == 0 {
        return Err(ErrorKind::PermissionDenied);
    }
    Ok(())
}

/// The bytes to map for a region of `size` bytes: whole pages.
fn mapped_len(size: u64, page: usize) -> Result<usize, ErrorKind> {
    let size = usize::try_from(size).map_err(|_| ErrorKind::Failed)?;
    if size == 0 {
        return Err(ErrorKind::Failed);
    }
    size.checked_next_multiple_of(page).ok_or(ErrorKind::Failed)
}

impl ModuleRegisters {
    /// Region `index` of the block bound to the module as `name`, mapped. `Unavailable` when no
    /// such block is bound, `NotFound` past its regions, `PermissionDenied` when the module won't
    /// map them (or this process lacks `CAP_SYS_RAWIO`), `Busy` when someone holds them.
    pub(crate) fn open(node: &Node, name: &str, index: u32) -> Result<Self, ErrorKind> {
        let (block, info) = super::block::find(node, name)?;
        mappable(&info, index)?;
        let claim = node
            .claim(format_args!("regs:{block}/{index}"))
            .map_err(|e| kind_of(&e))?;
        let region = sys::region(node.fd(), block, index).map_err(|e| kind_of(&e))?;
        let len = mapped_len(region.size, page_size())?;
        let map = Mapping::new(node.fd(), len, region.offset, true).map_err(|e| kind_of(&e))?;
        Ok(Self {
            map,
            // Access stops at the region's own end, though the mapping runs to the end of a page.
            size: usize::try_from(region.size).map_err(|_| ErrorKind::Failed)?,
            _claim: claim,
        })
    }
}

impl Registers for ModuleRegisters {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        self.size
    }

    fn read32(&mut self, offset: usize) -> Result<u32, ErrorKind> {
        inside(offset, self.size)?;
        self.map.read32(offset).ok_or(ErrorKind::InvalidInput)
    }

    fn write32(&mut self, offset: usize, value: u32) -> Result<(), ErrorKind> {
        inside(offset, self.size)?;
        self.map
            .write32(offset, value)
            .ok_or(ErrorKind::InvalidInput)
    }
}

/// Whether the word at `offset` lies inside a region of `size` bytes.
fn inside(offset: usize, size: usize) -> Result<(), ErrorKind> {
    match offset.checked_add(4) {
        Some(end) if end <= size => Ok(()),
        _ => Err(ErrorKind::InvalidInput),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(flags: u32, regions: u32) -> BlockInfo {
        BlockInfo {
            abi: sys::ABI,
            flags,
            regions,
            irqs: 0,
            dma_bits: 32,
        }
    }

    #[test]
    fn registers_are_handed_out_only_when_the_module_says_they_may_be_mapped() {
        assert_eq!(mappable(&info(INFO_MMIO_MAPPABLE, 1), 0), Ok(()));
        assert_eq!(mappable(&info(0, 1), 0), Err(ErrorKind::PermissionDenied));
        // Other flags don't stand in for it.
        assert_eq!(
            mappable(&info(!INFO_MMIO_MAPPABLE, 1), 0),
            Err(ErrorKind::PermissionDenied)
        );
        assert_eq!(
            mappable(&info(INFO_MMIO_MAPPABLE, 1), 1),
            Err(ErrorKind::NotFound)
        );
        assert_eq!(
            lemnos_core::status_of(ErrorKind::PermissionDenied),
            lemnos_core::Status::ACCESS
        );
    }

    #[test]
    fn a_region_maps_as_whole_pages() {
        assert_eq!(mapped_len(4096, 4096), Ok(4096));
        assert_eq!(mapped_len(0x100, 4096), Ok(4096));
        assert_eq!(mapped_len(0x1_0001, 4096), Ok(0x1_1000));
        assert_eq!(mapped_len(0, 4096), Err(ErrorKind::Failed));
        assert_eq!(mapped_len(u64::MAX, 4096), Err(ErrorKind::Failed));
    }

    #[test]
    fn a_word_must_lie_inside_the_region() {
        assert_eq!(inside(0, 0x100), Ok(()));
        assert_eq!(inside(0xfc, 0x100), Ok(()));
        assert_eq!(inside(0xfd, 0x100), Err(ErrorKind::InvalidInput));
        assert_eq!(inside(0x100, 0x100), Err(ErrorKind::InvalidInput));
        assert_eq!(inside(usize::MAX - 1, 0x100), Err(ErrorKind::InvalidInput));
    }
}
