//! Whether a job on the KPU can be confined to its own buffers, and the check a run must pass.
//!
//! A program is the caller's bytes and can name any address the block can reach, so the only
//! sound guarantee is something in front of the block: while a job runs, the block must reach
//! nothing but that job's own buffers. Two checks stand for that here:
//!
//! - [`Facts`] and [`Facts::gaps`]: what the kernel driver serving the node does, read from its
//!   source, and every way it leaves a job's reach open whatever this crate does.
//! - [`check_reach`]: given what the block can reach at the moment a job would start, and the
//!   job's own buffers, every reachable range must lie inside one of them.
//!
//! # The kernel the SDK builds (`ruyisdk/linux-xuantie-kernel` at
//! `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`)
//!
//! - Nothing in front of the block: `arch/riscv/boot/dts/canaan/k230.dtsi` gives the KPU
//!   (`gnne@80400000`) no `iommus`, and describes no IOMMU or bus memory protection on the chip.
//!   The block's addresses are physical, and it reaches all of memory.
//! - No submission path: `drivers/misc/canaan/gnne/k230_gnne.c` has `open` (writes two system
//!   control registers), `release` (writes them back), `poll` (one global interrupt flag, cleared
//!   by whichever poller sees it) and `fasync`. It takes no job, maps no register and checks no
//!   address. Whatever runs a job writes the block's registers itself from user space, through a
//!   mapping of physical memory, addresses included.
//! - Physical memory open to user space: the memory zone node's `mmap` (`mmz.c`, `mmz_map`) maps
//!   any physical range at the mapping's offset (`range_is_allowed` limits it only under
//!   `CONFIG_STRICT_DEVMEM`, which `k230_defconfig` does not set), and `MMZ_ALLOC_MEM` answers
//!   physical addresses. `MMZ_FREE_MEM` frees the addresses and length it is given, checked
//!   against nothing.
//! - Shared: neither node holds a second opener off, and every opener's `release` of the KPU node
//!   writes the bring-down values, whoever else holds it.
//!
//! So a job reaches all physical memory, and no driver in user space can change that: [`GNNE`]
//! has gaps, and the driver refuses to run on it.
//!
//! # What would close it
//!
//! Nothing the sources show. A Lemnos kernel path for the block could take the register window
//! and the memory zone away from user space and hold the block for one claim at a time, which
//! closes [`Gaps::REGISTER_ACCESS`], [`Gaps::RAW_PHYSICAL`] and [`Gaps::SHARED_OPEN`]. But with no
//! translation in front of the block, a program still names physical addresses, and checking them
//! means reading the program, which Lemnos never does. Confinement needs a memory protection unit
//! in front of the KPU that the vendor documents (an IOMMU or a bus firewall), and none is
//! described.

/// What the block's addresses go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    /// No translation and no protection: device addresses are physical.
    None,
    /// One table for every opener of the node.
    Device,
    /// One table per job, holding that job's buffers only.
    Job,
}

/// What a kernel driver does, as read from its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// What the block's addresses go through.
    pub table: Table,
    /// The kernel takes the job (and could check it). Without, user space writes the block's
    /// registers directly.
    pub submits: bool,
    /// A user can reach the block's registers.
    pub register_access: bool,
    /// A user can map physical addresses it names.
    pub raw_physical: bool,
    /// The node lets a second opener in while one holds it.
    pub shared_open: bool,
    /// Every part of the path was read.
    pub audited: bool,
}

/// The kernel the SDK builds, as read (see the module docs).
pub const GNNE: Facts = Facts {
    table: Table::None,
    submits: false,
    register_access: true,
    raw_physical: true,
    shared_open: true,
    audited: true,
};

/// Ways a job reaches past its own buffers. A set of bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gaps(u32);

impl Gaps {
    /// No translation or protection in front of the block.
    pub const NO_TRANSLATION: Self = Self(1 << 0);
    /// One table shared beyond one job.
    pub const SHARED_TABLE: Self = Self(1 << 1);
    /// The kernel takes no job: the block is driven from user space.
    pub const NO_SUBMISSION: Self = Self(1 << 2);
    /// Registers are open to users.
    pub const REGISTER_ACCESS: Self = Self(1 << 3);
    /// Physical addresses are open to users.
    pub const RAW_PHYSICAL: Self = Self(1 << 4);
    /// The node takes a second opener.
    pub const SHARED_OPEN: Self = Self(1 << 5);
    /// The path was not read.
    pub const NOT_AUDITED: Self = Self(1 << 6);
    /// The kernel driver is not one this crate knows.
    pub const UNKNOWN_KERNEL: Self = Self(1 << 7);
    /// What the block can reach, read at the time, includes a range outside the job's buffers.
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
            Table::Device => gaps = gaps.with(Gaps::SHARED_TABLE),
            Table::Job => {}
        }
        if !self.submits {
            gaps = gaps.with(Gaps::NO_SUBMISSION);
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
    /// Everything a block with no translation reaches: the whole address space.
    pub const EVERYTHING: Self = Self {
        start: 0,
        len: u64::MAX,
    };

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

/// Checks that every range in `reach` (what the block can reach when the job would start) lies
/// inside one of `own` (the job's buffers and its program). The first range that does not is
/// returned.
pub fn check_reach(reach: &[Range], own: &[Range]) -> Result<(), Range> {
    for range in reach {
        if !own.iter().any(|buffer| buffer.holds(range)) {
            return Err(*range);
        }
    }
    Ok(())
}
