//! DMA memory for a block, from the kernel module: [`ModuleDma`].
//!
//! # Where an address comes from
//!
//! Every [`DmaBuffer::device_address`] this hands out is the address the module returned from
//! `LEMNOS_IOC_ALLOC` for the block the board file names: the module allocated the memory with
//! the kernel's DMA API on that block's own device, so the address is the block's view of it
//! (behind an IOMMU, the IOMMU's mapping for that block), valid while the memory lives. The memory
//! comes back as a dma-buf, which this holds: the module frees it only when the last reference
//! goes, and the block can't be unbound before then. So an address handed out stays the memory's
//! for as long as the buffer is held here, and [`DmaBuffers::release`] (or dropping the whole
//! handle) is what lets it go.
//!
//! Nothing else is a source of addresses: not `/proc`, not page maps, not a CMA region, not a
//! dma-heap. Without the module, or for a block it doesn't have bound, there is no memory and the
//! request is refused.
//!
//! What the module answered is checked before it is handed on, overflow-safe: at least the bytes
//! asked for, the dma-buf really that large, and the whole range below the block's `dma_bits`.

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{DmaBuffer, DmaBuffers};
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::module as sys;

use super::Node;
use crate::kind_of;

/// The prefix of a board-file name for a block's DMA memory: `dma:<block>`, where `<block>` is the
/// block's device name as the module lists it (the platform device bound to it).
pub(crate) const PREFIX: &str = "dma:";

/// The block a `dma:<block>` name means, if it can be one: not empty, shorter than the module's
/// names, and no NUL or `/`.
pub(crate) fn block_name(need: &str) -> Option<&str> {
    let block = need.strip_prefix(PREFIX)?;
    super::block::valid(block).then_some(block)
}

/// DMA memory for one block bound to the kernel module. Each buffer is the module's allocation
/// for that block, held as a dma-buf and mapped for the CPU; see the module documentation.
///
/// A buffer handed to the block with [`DmaBuffers::sync_for_device`] can't be read or written
/// until [`DmaBuffers::sync_for_cpu`] takes it back (`Busy`). Reading, writing and syncing
/// allocate nothing.
pub struct ModuleDma {
    node: Node,
    block: u32,
    dma_bits: u32,
    buffers: Table,
    next: u32,
}

impl ModuleDma {
    /// The memory of the block bound to the module as `name`. `Unavailable` when no block of
    /// that name is bound, `Unsupported` when the module speaks another interface.
    pub(crate) fn open(node: Node, name: &str) -> Result<Self, ErrorKind> {
        let (block, info) = super::block::find(&node, name)?;
        Ok(Self {
            node,
            block,
            dma_bits: info.dma_bits,
            buffers: Table::default(),
            next: 0,
        })
    }

    /// Maps buffer `id` on its block a second time (`LEMNOS_IOC_IMPORT`, released at once) and
    /// returns the device address the module gave that mapping: a report from the kernel that
    /// doesn't go through the allocation. With no IOMMU between the block and memory it equals
    /// the buffer's `device_address`; behind one it may be a second mapping.
    pub fn import_address(&mut self, id: u32) -> io::Result<u64> {
        let slot = self.buffers.slot(id).ok_or(io::ErrorKind::NotFound)?;
        let imported = sys::import(self.node.fd(), self.block, slot.fd.as_fd())?;
        sys::release(self.node.fd(), self.block, imported.handle)?;
        Ok(imported.addr)
    }

    fn sync(&mut self, id: u32, device: bool) -> Result<(), ErrorKind> {
        let slot = self.buffers.slot_mut(id).ok_or(ErrorKind::NotFound)?;
        let flags = dma_buf::SYNC_READ
            | dma_buf::SYNC_WRITE
            | if device {
                dma_buf::SYNC_END
            } else {
                dma_buf::SYNC_START
            };
        dma_buf::sync(slot.fd.as_fd(), flags).map_err(|e| kind_of(&e))?;
        slot.device = device;
        Ok(())
    }
}

/// What the module answered for an allocation of `asked` bytes, checked: at least that much, the
/// dma-buf (`fd_size`, the kernel's own size of it) as large as the module says, and the whole
/// range below `2^dma_bits`. Overflow-safe.
pub(crate) fn checked(
    asked: u64,
    addr: u64,
    size: u64,
    fd_size: u64,
    dma_bits: u32,
) -> Result<(), ErrorKind> {
    let end = addr.checked_add(size).ok_or(ErrorKind::Failed)?;
    let reach = if dma_bits >= 64 {
        u64::MAX
    } else {
        1u64 << dma_bits
    };
    if size < asked || fd_size < size || end > reach || dma_bits == 0 {
        return Err(ErrorKind::Failed);
    }
    Ok(())
}

impl DmaBuffers for ModuleDma {
    type Error = ErrorKind;

    fn allocate(&mut self, len: u64) -> Result<DmaBuffer, ErrorKind> {
        if len == 0 {
            return Err(ErrorKind::InvalidInput);
        }
        let id = self.next.checked_add(1).ok_or(ErrorKind::Unavailable)?;
        let memory = sys::alloc(self.node.fd(), self.block, len).map_err(|e| kind_of(&e))?;
        let fd_size = dma_buf::size(memory.fd.as_fd()).map_err(|e| kind_of(&e))?;
        checked(len, memory.addr, memory.size, fd_size, self.dma_bits)?;
        let bytes = usize::try_from(memory.size).map_err(|_| ErrorKind::InvalidInput)?;
        let map = Mapping::new(memory.fd.as_fd(), bytes, 0, true).map_err(|e| kind_of(&e))?;
        self.next = id;
        self.buffers.insert(Slot {
            id,
            fd: memory.fd,
            map,
            device: false,
        });
        Ok(DmaBuffer {
            id,
            len: memory.size,
            device_address: memory.addr,
        })
    }

    fn release(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.buffers.remove(id).ok_or(ErrorKind::NotFound).map(drop)
    }

    fn write(&mut self, id: u32, offset: u64, data: &[u8]) -> Result<(), ErrorKind> {
        self.buffers
            .cpu_bytes(id, offset, data.len())?
            .copy_from_slice(data);
        Ok(())
    }

    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Result<(), ErrorKind> {
        out.copy_from_slice(self.buffers.cpu_bytes(id, offset, out.len())?);
        Ok(())
    }

    fn sync_for_device(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.sync(id, true)
    }

    fn sync_for_cpu(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.sync(id, false)
    }
}

/// One buffer: the dma-buf, its CPU mapping and who has it.
pub(crate) struct Slot {
    pub(crate) id: u32,
    pub(crate) fd: OwnedFd,
    pub(crate) map: Mapping,
    /// Handed to the block: the CPU keeps out until it is taken back.
    pub(crate) device: bool,
}

/// The buffers held, by id. Looking one up allocates nothing.
#[derive(Default)]
pub(crate) struct Table(Vec<Slot>);

impl Table {
    pub(crate) fn insert(&mut self, slot: Slot) {
        self.0.push(slot);
    }

    pub(crate) fn slot(&self, id: u32) -> Option<&Slot> {
        self.0.iter().find(|slot| slot.id == id)
    }

    fn slot_mut(&mut self, id: u32) -> Option<&mut Slot> {
        self.0.iter_mut().find(|slot| slot.id == id)
    }

    /// Takes the buffer out; dropping it unmaps it and closes the dma-buf.
    pub(crate) fn remove(&mut self, id: u32) -> Option<Slot> {
        let at = self.0.iter().position(|slot| slot.id == id)?;
        Some(self.0.swap_remove(at))
    }

    /// `len` bytes of buffer `id` from `offset`, for the CPU: `NotFound` for a buffer not held,
    /// `Busy` while the block has it, `InvalidInput` for a range not wholly inside it.
    pub(crate) fn cpu_bytes(
        &mut self,
        id: u32,
        offset: u64,
        len: usize,
    ) -> Result<&mut [u8], ErrorKind> {
        let slot = self.slot_mut(id).ok_or(ErrorKind::NotFound)?;
        if slot.device {
            return Err(ErrorKind::Busy);
        }
        let start = usize::try_from(offset).map_err(|_| ErrorKind::InvalidInput)?;
        let end = start.checked_add(len).ok_or(ErrorKind::InvalidInput)?;
        slot.map
            .as_mut_slice()
            .get_mut(start..end)
            .ok_or(ErrorKind::InvalidInput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_measure::test_alloc::allocations;

    #[test]
    fn names_are_a_block_or_nothing() {
        assert_eq!(block_name("dma:91400000.isp"), Some("91400000.isp"));
        assert_eq!(
            block_name("dma:lemnos-test-block"),
            Some("lemnos-test-block")
        );
        assert_eq!(block_name("dma:"), None);
        assert_eq!(block_name("dma:a/b"), None);
        assert_eq!(block_name("dma:a\0b"), None);
        assert_eq!(block_name(&format!("dma:{}", "x".repeat(64))), None);
        assert_eq!(
            block_name(&format!("dma:{}", "x".repeat(63))).map(str::len),
            Some(63)
        );
        assert_eq!(block_name("i2c-1"), None);
        assert_eq!(block_name("91400000.isp"), None);
    }

    #[test]
    fn what_the_module_answers_is_checked() {
        let page = 4096;
        assert_eq!(checked(5000, 0x1000_0000, 2 * page, 2 * page, 32), Ok(()));
        // Less than asked, or a dma-buf smaller than the module says.
        assert!(checked(5000, 0x1000_0000, page, page, 32).is_err());
        assert!(checked(5000, 0x1000_0000, 2 * page, page, 32).is_err());
        // The last byte must be below 2^dma_bits: ending exactly there is fine, past it isn't.
        assert_eq!(checked(page, (1 << 32) - page, page, page, 32), Ok(()));
        assert!(checked(page, (1 << 32) - page + 16, page, page, 32).is_err());
        // Wrapping, and a block that says it reaches nothing.
        assert!(checked(page, u64::MAX - 10, page, page, 64).is_err());
        assert!(checked(page, 0x1000, page, page, 0).is_err());
        assert_eq!(checked(page, u64::MAX - page, page, page, 64), Ok(()));
    }

    fn memfd_slot(id: u32, len: u64) -> Slot {
        let fd = dma_buf::sealed_memfd(c"lemnos-dma-test", len).unwrap();
        let map = Mapping::new(fd.as_fd(), len as usize, 0, true).unwrap();
        Slot {
            id,
            fd,
            map,
            device: false,
        }
    }

    #[test]
    fn cpu_access_stays_inside_a_buffer_the_cpu_has() {
        let mut table = Table::default();
        table.insert(memfd_slot(1, 4096));
        table.insert(memfd_slot(2, 4096));
        table
            .cpu_bytes(1, 4092, 4)
            .unwrap()
            .copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(table.cpu_bytes(1, 4092, 4).unwrap(), &[1, 2, 3, 4]);
        assert_eq!(table.cpu_bytes(2, 4092, 4).unwrap(), &[0; 4]);
        assert_eq!(table.cpu_bytes(1, 4093, 4), Err(ErrorKind::InvalidInput));
        assert_eq!(
            table.cpu_bytes(1, u64::MAX, 1),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(
            table.cpu_bytes(1, 1, usize::MAX),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(table.cpu_bytes(3, 0, 1), Err(ErrorKind::NotFound));
        table.slot_mut(1).unwrap().device = true;
        assert_eq!(table.cpu_bytes(1, 0, 1), Err(ErrorKind::Busy));
        assert!(table.remove(1).is_some());
        assert!(table.remove(1).is_none());
        assert_eq!(table.cpu_bytes(1, 0, 1), Err(ErrorKind::NotFound));
        assert_eq!(table.cpu_bytes(2, 4092, 4).unwrap(), &[0; 4]);
    }

    #[test]
    fn reading_and_writing_allocate_nothing() {
        let mut table = Table::default();
        for id in 1..=3 {
            table.insert(memfd_slot(id, 8192));
        }
        let mut out = [0u8; 256];
        let ((), count) = allocations(|| {
            for round in 0..100u64 {
                table
                    .cpu_bytes(3, round * 16, out.len())
                    .unwrap()
                    .copy_from_slice(&[round as u8; 256]);
                out.copy_from_slice(table.cpu_bytes(3, round * 16, 256).unwrap());
            }
        });
        assert_eq!(count, 0);
        assert_eq!(out, [99; 256]);
    }
}
