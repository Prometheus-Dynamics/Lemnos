//! Whether a job on the block can be confined to its own buffers, and the check a run must pass.
//!
//! A program is the caller's bytes and can name any address the block can reach, so the only
//! sound guarantee is the block's address translation: while a job runs, it must map nothing but
//! that job's own buffers. Two checks stand for that here:
//!
//! - [`Facts`] and [`Facts::gaps`]: what the kernel driver serving the node does, read from its
//!   source, and every way it lets a job reach past its buffers whatever this crate sends.
//! - [`check_reach`]: given what the block's translation maps at the moment a job would start,
//!   and the job's own buffers, every mapped range must lie inside one of them.
//!
//! # The vendor's `vipcore` (`drivers/npu/aw_nna_vip/vip2/`, Radxa `allwinner-bsp` at
//! `a842bb8dc74608ba13975c97c31b9518f1976c01`)
//!
//! - No system IOMMU: the block's node (`npu@3600000` in `configs/linux-6.6/sun60iw2p1.dtsi`)
//!   carries no `iommus`. With the Kconfig option `NPU_USER_IOMMU` set, the Kbuild turns the
//!   block's own MMU off (`vpmdENABLE_MMU` 0), and device addresses are then physical.
//! - One page table for the device: `inc/vip_lite_config.h` builds `vpmdENABLE_MMU` 1, which is
//!   `vpmdMMU_SINGLE_PAGE`; `adjust_mmu_id` in `memory/vip_drv_mmu.c` turns every process's id
//!   into 0. Every opener's allocations and wraps go into the same table and stay until freed.
//! - Mapped at start-up and for as long as the module is loaded (`memory/vip_drv_mmu.c`): the
//!   video memory heap every opener allocates from (`mmu_map_memory_heap`, 16 MiB by
//!   `VIDEO_MEMORY_HEAP_SIZE`), the reserved physical range (`mmu_map_reserve_physical`, by
//!   module parameter), and the on-chip SRAMs (`mmu_map_vip_sram`, `mmu_map_axi_sram`). A freed
//!   heap allocation stays mapped, with whatever it held.
//! - Any opener may read or write any register offset (`ReadReg`, `WriteReg`: `writel` at the
//!   window plus the caller's offset, unchecked), and map any physical page
//!   (`WrapUserPhysical`: page alignment checked only). The node does not hold opens off:
//!   `drv_open` refuses a second open only without a task queue, and this build has one
//!   (`vpmdENABLE_MULTIPLE_TASK` 3).
//! - The job is a command buffer the caller fills; the kernel links to it and runs it without
//!   reading it (`task/vip_drv_task_schedule.c`).
//!
//! So a job reaches, at least, everything in the shared heap and SRAMs and every other opener's
//! mappings, and a second opener can move the block anywhere. None of that can be closed from
//! user space: [`VIPCORE`] has gaps, and the driver refuses to run on it.
//!
//! The vendor's other driver for the same compatible string, `galcore`
//! (`drivers/npu/aw_nna_galcore/`), flat-maps physical ranges into its MMU when it builds it
//! (`gckMMU_FillFlatMapping` callers in `kernel/gc_hal_kernel_mmu.c`: the contiguous pool, the
//! external and exclusive pools, and a range given by module parameter) and lets user space read
//! registers (`gcdREGISTER_READ_FROM_USER` 1 in `kernel/inc/gc_hal_options.h`). Its submission
//! path was not audited; [`GALCORE`] has gaps.
//!
//! # What closes it: the block in `lemnos.ko`
//!
//! With the block named in the module's `vip9000` parameter, the module drives it
//! (`kernel/lemnos_vip.c`): (1) one root table, whose entries while a job runs lead to the module's
//! copy of the job's program (read-only) and to the job's slots, each from a dma-buf registered on
//! the stream, and are cleared when the job ends; (2) the register window, interrupt and MMU
//! stay in the kernel, and a program is checked at load to hold no command that reaches the
//! block's control registers, the MMU's set-up or the command fetcher; (3) one stream on the
//! block at a time. [`LEMNOS`] has no gap. Slots sit at fixed device addresses
//! ([`slot_address`]), so a caller's tooling can emit a program that names its slots without
//! Lemnos reading the program, which the vendor's runtime otherwise does by parsing it.

/// What the block's address translation is shared by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    /// No translation: device addresses are physical.
    None,
    /// One table for every opener of the node.
    Device,
    /// One table per opening process.
    Process,
    /// One table per job, holding that job's buffers only.
    Job,
}

/// What a kernel driver does, as read from its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// What one table is shared by.
    pub table: Table,
    /// The kernel maps memory of its own or of other users into the table before any job, and
    /// keeps it there.
    pub mapped_at_start: bool,
    /// A user of the node can read or write the block's registers.
    pub register_access: bool,
    /// A user of the node can map physical addresses it names.
    pub raw_physical: bool,
    /// The node lets a second opener in while one holds it.
    pub shared_open: bool,
    /// Every part of the submission path was read.
    pub audited: bool,
}

/// The vendor's `vipcore`, as built for the target chip (see the module docs).
pub const VIPCORE: Facts = Facts {
    table: Table::Device,
    mapped_at_start: true,
    register_access: true,
    raw_physical: true,
    shared_open: true,
    audited: true,
};

/// The vendor's `galcore`: flat maps at start-up and user register reads were read; the rest was
/// not audited.
pub const GALCORE: Facts = Facts {
    table: Table::Device,
    mapped_at_start: true,
    register_access: true,
    raw_physical: false,
    shared_open: true,
    audited: false,
};

/// `lemnos.ko` with the block named in its `vip9000` parameter (see the module docs). The one
/// thing mapped between jobs is the window of the module's command buffer, which holds only the
/// copy of the job's own program while a job runs, read-only: it is the job's command buffer, not
/// memory of anyone else's.
pub const LEMNOS: Facts = Facts {
    table: Table::Job,
    mapped_at_start: false,
    register_access: false,
    raw_physical: false,
    shared_open: false,
    audited: true,
};

/// Ways a job reaches past its own buffers. A set of bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gaps(u32);

impl Gaps {
    /// The table is shared beyond one job.
    pub const SHARED_TABLE: Self = Self(1 << 0);
    /// No translation at all.
    pub const NO_TRANSLATION: Self = Self(1 << 1);
    /// Memory is mapped before any job and stays.
    pub const MAPPED_AT_START: Self = Self(1 << 2);
    /// Registers are open to users of the node.
    pub const REGISTER_ACCESS: Self = Self(1 << 3);
    /// Physical addresses are open to users of the node.
    pub const RAW_PHYSICAL: Self = Self(1 << 4);
    /// The node takes a second opener.
    pub const SHARED_OPEN: Self = Self(1 << 5);
    /// The submission path was not read.
    pub const NOT_AUDITED: Self = Self(1 << 6);
    /// The kernel driver is not one this crate knows.
    pub const UNKNOWN_KERNEL: Self = Self(1 << 7);
    /// The table, read at the time, maps a range outside the job's buffers.
    pub const REACH: Self = Self(1 << 8);

    /// No gap.
    pub const NONE: Self = Self(0);

    /// Whether no gap is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every gap of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The bits.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl Facts {
    /// Every way these facts let a job reach past its buffers.
    pub const fn gaps(&self) -> Gaps {
        let mut gaps = Gaps::NONE;
        match self.table {
            Table::None => gaps = gaps.with(Gaps::NO_TRANSLATION),
            Table::Device | Table::Process => gaps = gaps.with(Gaps::SHARED_TABLE),
            Table::Job => {}
        }
        if self.mapped_at_start {
            gaps = gaps.with(Gaps::MAPPED_AT_START);
        }
        if self.register_access {
            gaps = gaps.with(Gaps::REGISTER_ACCESS);
        }
        if self.raw_physical {
            gaps = gaps.with(Gaps::RAW_PHYSICAL);
        }
        if self.shared_open {
            gaps = gaps.with(Gaps::SHARED_OPEN);
        }
        if !self.audited {
            gaps = gaps.with(Gaps::NOT_AUDITED);
        }
        gaps
    }
}

/// A range of device addresses, `start` and `len` bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub len: u64,
}

impl Range {
    /// One past the last address, or `None` when the range wraps the address space.
    pub const fn end(&self) -> Option<u64> {
        self.start.checked_add(self.len)
    }

    /// Whether `inner` lies wholly inside this range. An empty `inner` lies nowhere.
    pub const fn holds(&self, inner: &Range) -> bool {
        let (Some(end), Some(inner_end)) = (self.end(), inner.end()) else {
            return false;
        };
        inner.len > 0 && inner.start >= self.start && inner_end <= end
    }
}

/// Checks that every range in `mapped` (what the block's table maps when the job would start)
/// lies inside one of `own` (the job's attached buffers and its command buffer, at the addresses
/// they were mapped at). The first range that does not is returned.
pub fn check_reach(mapped: &[Range], own: &[Range]) -> Result<(), Range> {
    for range in mapped {
        if !own.iter().any(|buffer| buffer.holds(range)) {
            return Err(*range);
        }
    }
    Ok(())
}

/// Slots a job may name (`LEMNOS_VIP_SLOTS`).
pub const SLOTS: u16 = 13;

/// Device address of slot 0 in a job's table (`LEMNOS_VIP_SLOT_ADDR(0)`).
pub const SLOT_BASE: u64 = 1 << 28;

/// Device address span of each slot in a job's table (`LEMNOS_VIP_SLOT_SPAN`): a slot's buffer
/// is mapped at [`slot_address`] and may be at most this long.
pub const SLOT_SPAN: u64 = 1 << 28;

/// Where the block sees the job's program (`LEMNOS_VIP_PROGRAM_ADDR`).
pub const PROGRAM_ADDRESS: u64 = 0x0010_0000;

/// The device address slot `slot` is mapped at in a job's table, for a buffer of `bytes`. `None`
/// for a slot past [`SLOTS`], an empty buffer or one longer than [`SLOT_SPAN`].
pub const fn slot_address(slot: u16, bytes: u64) -> Option<Range> {
    if slot >= SLOTS || bytes == 0 || bytes > SLOT_SPAN {
        return None;
    }
    Some(Range {
        start: SLOT_BASE + slot as u64 * SLOT_SPAN,
        len: bytes,
    })
}
