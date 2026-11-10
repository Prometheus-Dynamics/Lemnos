//! The requests the driver sends, each built only by a checked constructor, and what each field
//! becomes in the kernel's structure. These are the only shapes a [`crate::Session`] is asked
//! to issue; the session in `lemnos-driver-rknpu-sys` checks them again against the objects it
//! made before anything reaches the kernel.
//!
//! # Every field the kernel takes from user space
//!
//! What the kernel does with it (rockchip-linux/kernel `develop-6.1` at
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`, `drivers/rknpu/`), and what the driver sends.
//!
//! `MEM_CREATE` ([`Create`], `rknpu_gem_create_ioctl`, `rknpu_gem_object_create`):
//!
//! - `handle`: an existing handle returns that object and makes nothing. Always 0, which no
//!   object has.
//! - `flags`: see [`Create::flags`]. Never `TRY_ALLOC_SRAM` or `TRY_ALLOC_NBUF` (they map shared
//!   on-chip memory into the domain), never `CACHEABLE` (it would need `MEM_SYNC`), never
//!   `SECURE`; always `ZEROING`, so an object holds nothing of a former owner.
//! - `size`: rounded up to pages; 0 is refused. The driver's own byte count, above 0.
//! - `sram_size`, `core_mask`: read only for on-chip memory. Always 0.
//! - `iommu_domain_id`: the kernel switches the device to that domain (making it if new) and maps
//!   the object there. The program's own domain ([`Domain`]).
//!
//! `MEM_MAP`: the handle, one the session made. `MEM_DESTROY`: the handle; the kernel switches to
//! the object's domain, closes the handle, and frees and unmaps the object once nothing else
//! holds it. The session unmaps its CPU mapping first, which is such a hold.
//!
//! `SUBMIT` ([`Run`], `rknpu_submit`, `rknpu_job_subcore_commit_pc`):
//!
//! - `flags`: `PC` (required), and `PINGPONG` when the program asks. Never `NONBLOCK` (the call
//!   would return with the job running), never `FENCE_IN`/`FENCE_OUT`.
//! - `timeout`: [`RUN_TIMEOUT_MS`]. The kernel waits up to three times this, then aborts the job
//!   and resets the block before the call returns.
//! - `task_obj_addr`: a kernel pointer the kernel dereferences unchecked. Only the session fills
//!   it in, from the address the kernel gave for the body object it made, while it holds that
//!   object. The driver never sees it.
//! - `task_start`, `task_number`, `subcore_task[*]`: indices into the body's task array, read
//!   through its kernel mapping unchecked. Always 0 and the program's task count, which the
//!   framing checked against the body, in every entry the kernel may read.
//! - `core_mask`: the kernel only checks it is not above the chip's mask. Always core 0.
//! - `iommu_domain_id`: the domain the kernel switches to for the job; it waits for jobs in other
//!   domains to end first. The program's domain, the one its objects were made in.
//! - `task_base_addr`: written to `PC_DMA_BASE_ADDR`, an address behind the IOMMU. The body's
//!   address plus the framing's task base.
//! - `priority`, `fence_fd`: not read without the flags above. 0 and -1.
//!
//! Inside the body, the kernel reads the first task's `regcmd_addr` (to `PC_DATA_ADDR`) and
//! `regcfg_amount` (to `PC_DATA_AMOUNT`), and both tasks' `int_mask`, and writes the last task's
//! `int_status`. Those are the program's bytes: addresses behind the IOMMU, a fetch length, and
//! an interrupt mask that at worst makes the job time out.
//!
//! `ACTION` ([`Query`]): only the reads, and setting the domain once at a claim to learn whether
//! the kernel was built with domain switching. Never `ACT_RESET`, the bandwidth and power
//! settings, or `SET_PROC_NICE`.

use crate::uapi::{
    self, Action, DOMAINS, MemCreate, SUBCORE_TASKS, SubcoreTask, Submit, TASK_BYTES, action,
};

/// Milliseconds the kernel waits for a run, three times over.
pub const RUN_TIMEOUT_MS: u32 = 2000;

/// An IOMMU domain id, below [`uapi::DOMAINS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Domain(u8);

impl Domain {
    /// Domain 0, the device's own.
    pub const DEFAULT: Self = Self(0);

    /// Domain `id`, if the kernel has one by that number.
    #[must_use]
    pub const fn new(id: u32) -> Option<Self> {
        if id < DOMAINS {
            Some(Self(id as u8))
        } else {
            None
        }
    }

    /// The id.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.0 as u32
    }
}

/// What an object holds, which decides how it is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A program's body: the kernel reads the task array out of it, so it is mapped for the
    /// kernel too.
    Body,
    /// Anything else the block reads or writes: a slot's copy, scratch.
    Data,
}

/// A `MEM_CREATE` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Create {
    domain: Domain,
    bytes: u32,
    kind: Kind,
}

impl Create {
    /// An object of `bytes` (above 0) in `domain`.
    #[must_use]
    pub const fn new(domain: Domain, bytes: u32, kind: Kind) -> Option<Self> {
        if bytes == 0 {
            None
        } else {
            Some(Self {
                domain,
                bytes,
                kind,
            })
        }
    }

    /// The domain.
    #[must_use]
    pub const fn domain(&self) -> Domain {
        self.domain
    }

    /// Bytes asked for.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }

    /// The kind.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Pages through the IOMMU, write-combined, zeroed; kernel-mapped for a body.
    #[must_use]
    pub const fn flags(&self) -> u32 {
        let flags = uapi::mem::NON_CONTIGUOUS
            | uapi::mem::IOMMU
            | uapi::mem::WRITE_COMBINE
            | uapi::mem::ZEROING;
        match self.kind {
            Kind::Body => flags | uapi::mem::KERNEL_MAPPING,
            Kind::Data => flags,
        }
    }

    /// The kernel's structure.
    #[must_use]
    pub const fn to_raw(&self) -> MemCreate {
        MemCreate {
            handle: 0,
            flags: self.flags(),
            size: self.bytes as u64,
            obj_addr: 0,
            dma_addr: 0,
            sram_size: 0,
            iommu_domain_id: self.domain.0 as i32,
            core_mask: 0,
        }
    }
}

/// An object a session made and holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Object {
    /// The DRM handle.
    pub handle: u32,
    /// Its address behind the IOMMU (`dma_addr`).
    pub iova: u64,
    /// Bytes asked for.
    pub bytes: u32,
    /// Bytes made, at least `bytes`: what the session maps and the domain holds.
    pub size: u64,
    /// The domain it was made in.
    pub domain: Domain,
    /// What it holds.
    pub kind: Kind,
}

/// Why a run was not built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunError {
    /// The object is not a body.
    NotBody,
    /// No task, or more tasks than the body holds.
    Tasks,
    /// The task base is not inside the body.
    TaskBase,
}

/// A `SUBMIT` request: the tasks of one body, in the body's domain, on core 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    body: u32,
    tasks: u32,
    task_base: u64,
    domain: Domain,
    pingpong: bool,
}

impl Run {
    /// Runs the first `tasks` entries of `body`'s task array, with the block's base address at
    /// `task_base` bytes into the body.
    pub fn new(
        body: &Object,
        tasks: u32,
        task_base: u32,
        pingpong: bool,
    ) -> Result<Self, RunError> {
        if body.kind != Kind::Body {
            return Err(RunError::NotBody);
        }
        if tasks == 0 || u64::from(tasks) * u64::from(TASK_BYTES) > u64::from(body.bytes) {
            return Err(RunError::Tasks);
        }
        if task_base >= body.bytes {
            return Err(RunError::TaskBase);
        }
        let task_base = body
            .iova
            .checked_add(u64::from(task_base))
            .ok_or(RunError::TaskBase)?;
        Ok(Self {
            body: body.handle,
            tasks,
            task_base,
            domain: body.domain,
            pingpong,
        })
    }

    /// The body's handle.
    #[must_use]
    pub const fn body(&self) -> u32 {
        self.body
    }

    /// Tasks run.
    #[must_use]
    pub const fn tasks(&self) -> u32 {
        self.tasks
    }

    /// The domain.
    #[must_use]
    pub const fn domain(&self) -> Domain {
        self.domain
    }

    /// The address written to `PC_DMA_BASE_ADDR`: inside the body.
    #[must_use]
    pub const fn task_base(&self) -> u64 {
        self.task_base
    }

    /// The kernel's structure, with `obj_addr` the kernel's address of the body object, which
    /// only the session that made the object knows.
    #[must_use]
    pub const fn to_raw(&self, obj_addr: u64) -> Submit {
        let range = SubcoreTask {
            task_start: 0,
            task_number: self.tasks,
        };
        Submit {
            flags: uapi::job::PC
                | if self.pingpong {
                    uapi::job::PINGPONG
                } else {
                    0
                },
            timeout: RUN_TIMEOUT_MS,
            task_start: 0,
            task_number: self.tasks,
            task_counter: 0,
            priority: 0,
            task_obj_addr: obj_addr,
            iommu_domain_id: self.domain.0 as u32,
            reserved: 0,
            task_base_addr: self.task_base,
            hw_elapse_time: 0,
            core_mask: uapi::core::CORE0,
            fence_fd: -1,
            subcore_task: [range; SUBCORE_TASKS],
        }
    }
}

/// An `ACTION` request: the only ones sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    /// `RKNPU_GET_HW_VERSION`.
    HardwareVersion,
    /// `RKNPU_GET_DRV_VERSION`.
    DriverVersion,
    /// `RKNPU_GET_IOMMU_EN`: 1 when the block is behind an IOMMU.
    IommuEnabled,
    /// `RKNPU_GET_IOMMU_DOMAIN_ID`: the domain the device is in now.
    Domain,
    /// `RKNPU_SET_IOMMU_DOMAIN_ID`: switches the device to a domain, making it if new. A kernel
    /// built without domain switching answers 0 and does nothing.
    SetDomain(Domain),
}

impl Query {
    /// The kernel's structure.
    #[must_use]
    pub const fn to_raw(self) -> Action {
        let (flags, value) = match self {
            Self::HardwareVersion => (action::GET_HW_VERSION, 0),
            Self::DriverVersion => (action::GET_DRV_VERSION, 0),
            Self::IommuEnabled => (action::GET_IOMMU_EN, 0),
            Self::Domain => (action::GET_IOMMU_DOMAIN_ID, 0),
            Self::SetDomain(domain) => (action::SET_IOMMU_DOMAIN_ID, domain.0 as u32),
        };
        Action { flags, value }
    }
}
