//! The kernel interface of the NPU's DRM node, transcribed. Interface facts only: request numbers,
//! structure layouts, flags, action ids and limits. No code is copied.
//!
//! Sources, all in rockchip-linux/kernel, branch `develop-6.1`, commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`, directory `drivers/rknpu/`:
//!
//! - `include/rknpu_ioctl.h` (`GPL-2.0`): the request numbers, [`MemCreate`], [`MemMap`],
//!   [`MemDestroy`], [`MemSync`], [`Task`], [`SubcoreTask`], [`Submit`], [`Action`], the memory
//!   flags ([`mem`]), the job flags ([`job`]) and the action ids ([`action`]).
//! - `include/rknpu_job.h`: the core masks ([`core`]) and `RKNPU_MAX_CORES`.
//! - `include/rknpu_drv.h`: `RKNPU_MAX_IOMMU_DOMAIN_NUM` ([`DOMAINS`]).
//! - `rknpu_drv.c`: the platform driver's name ([`DRIVER_NAME`]) and the DRM ioctl table.
//!
//! The sizes and offsets are pinned by the tests, and are generated from the header by
//! `layout-check/` with `gcc` on x86_64 and `aarch64-linux-gnu-gcc`.

use std::mem::offset_of;

use lemnos_linux_sys::iowr;

/// The platform driver's name (`rknpu_driver.driver.name` in `rknpu_drv.c`), as sysfs shows it
/// under the device's `driver` link.
pub const DRIVER_NAME: &str = "RKNPU";

/// `DRM_IOCTL_BASE`.
pub const DRM_MAGIC: u8 = b'd';

/// `DRM_COMMAND_BASE`: driver requests number from here.
pub const DRM_COMMAND_BASE: u8 = 0x40;

/// `DRM_IOCTL_RKNPU_ACTION`.
pub const IOCTL_ACTION: u32 = iowr::<Action>(DRM_MAGIC, DRM_COMMAND_BASE);
/// `DRM_IOCTL_RKNPU_SUBMIT`.
pub const IOCTL_SUBMIT: u32 = iowr::<Submit>(DRM_MAGIC, DRM_COMMAND_BASE + 1);
/// `DRM_IOCTL_RKNPU_MEM_CREATE`.
pub const IOCTL_MEM_CREATE: u32 = iowr::<MemCreate>(DRM_MAGIC, DRM_COMMAND_BASE + 2);
/// `DRM_IOCTL_RKNPU_MEM_MAP`.
pub const IOCTL_MEM_MAP: u32 = iowr::<MemMap>(DRM_MAGIC, DRM_COMMAND_BASE + 3);
/// `DRM_IOCTL_RKNPU_MEM_DESTROY`.
pub const IOCTL_MEM_DESTROY: u32 = iowr::<MemDestroy>(DRM_MAGIC, DRM_COMMAND_BASE + 4);
/// `DRM_IOCTL_RKNPU_MEM_SYNC`. Never sent: it takes a kernel object address and an offset and
/// size the kernel does not check against the object (`rknpu_gem_sync_ioctl`), and the driver
/// makes no cacheable object, which is the only kind it syncs.
pub const IOCTL_MEM_SYNC: u32 = iowr::<MemSync>(DRM_MAGIC, DRM_COMMAND_BASE + 5);

/// `RKNPU_MAX_IOMMU_DOMAIN_NUM`: domain ids run from 0 below this.
pub const DOMAINS: u32 = 16;

/// `RKNPU_MAX_CORES`.
pub const MAX_CORES: usize = 3;

/// Entries of [`Submit::subcore_task`].
pub const SUBCORE_TASKS: usize = 5;

/// `struct rknpu_mem_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemCreate {
    /// In: an existing object's handle returns that object instead of making one (0 is no
    /// handle). Out: the new object's handle.
    pub handle: u32,
    /// [`mem`] flags.
    pub flags: u32,
    /// In: bytes wanted. Out: bytes made, rounded up to pages.
    pub size: u64,
    /// Out: the kernel's own address of the object, which `SUBMIT` and `MEM_SYNC` take back.
    pub obj_addr: u64,
    /// Out: the object's address on the NPU side of the IOMMU.
    pub dma_addr: u64,
    /// In: on-chip memory wanted with [`mem::TRY_ALLOC_SRAM`].
    pub sram_size: u64,
    /// In: the IOMMU domain the object is made in.
    pub iommu_domain_id: i32,
    /// In: read only with [`mem::TRY_ALLOC_NBUF`].
    pub core_mask: u32,
}

/// `struct rknpu_mem_map`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemMap {
    /// The object's handle.
    pub handle: u32,
    /// Padding.
    pub reserved: u32,
    /// Out: the offset to `mmap` the object at, on the same file.
    pub offset: u64,
}

/// `struct rknpu_mem_destroy`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemDestroy {
    /// The handle to close. The object is freed, and unmapped from its domain, once nothing else
    /// holds it (a CPU mapping, a running job).
    pub handle: u32,
    /// Padding.
    pub reserved: u32,
    /// Not read by the DRM build (`rknpu_gem_destroy_ioctl` goes by handle).
    pub obj_addr: u64,
}

/// `struct rknpu_mem_sync`. Never sent (see [`IOCTL_MEM_SYNC`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemSync {
    /// [`sync`] direction.
    pub flags: u32,
    /// Padding.
    pub reserved: u32,
    /// A kernel object address.
    pub obj_addr: u64,
    /// Unchecked by the kernel.
    pub offset: u64,
    /// Unchecked by the kernel.
    pub size: u64,
}

/// `struct rknpu_task`, packed: one entry of the task array the kernel reads out of the body
/// object at submit. The program writes these, not the driver; the layout is here for the
/// offsets the kernel reads.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Task {
    /// Not read by the kernel.
    pub flags: u32,
    /// Not read by the kernel.
    pub op_idx: u32,
    /// Not read by the kernel.
    pub enable_mask: u32,
    /// The last task's is written to `INT_MASK` and is the completion the kernel waits for; the
    /// first task's is written to `INT_CLEAR`.
    pub int_mask: u32,
    /// Not read by the kernel.
    pub int_clear: u32,
    /// Written by the kernel into the last task when the job ends.
    pub int_status: u32,
    /// The first task's sets `PC_DATA_AMOUNT`, how much the block fetches from `regcmd_addr`.
    pub regcfg_amount: u32,
    /// Not read by the kernel.
    pub regcfg_offset: u32,
    /// The first task's is written to `PC_DATA_ADDR`: an address on the NPU side of the IOMMU.
    pub regcmd_addr: u64,
}

/// Bytes of one [`Task`].
pub const TASK_BYTES: u32 = 40;

/// `struct rknpu_subcore_task`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubcoreTask {
    /// First task index for the core.
    pub task_start: u32,
    /// Tasks for the core.
    pub task_number: u32,
}

/// `struct rknpu_submit`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Submit {
    /// [`job`] flags.
    pub flags: u32,
    /// Milliseconds per wait; the kernel waits up to three times.
    pub timeout: u32,
    /// Index of the first task in the body.
    pub task_start: u32,
    /// Tasks to run.
    pub task_number: u32,
    /// Out: tasks completed.
    pub task_counter: u32,
    /// Not read by the kernel.
    pub priority: i32,
    /// A kernel object address from [`MemCreate::obj_addr`], dereferenced as is.
    pub task_obj_addr: u64,
    /// The IOMMU domain the job runs in.
    pub iommu_domain_id: u32,
    /// Padding.
    pub reserved: u32,
    /// Written to `PC_DMA_BASE_ADDR`: an address on the NPU side of the IOMMU.
    pub task_base_addr: u64,
    /// Out: nanoseconds the block ran.
    pub hw_elapse_time: i64,
    /// [`core`] mask.
    pub core_mask: u32,
    /// Read with [`job::FENCE_IN`], written with [`job::FENCE_OUT`].
    pub fence_fd: i32,
    /// Per-core task ranges, used instead of `task_start`/`task_number` on a block with more than
    /// one core.
    pub subcore_task: [SubcoreTask; SUBCORE_TASKS],
}

/// `struct rknpu_action`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Action {
    /// An [`action`] id.
    pub flags: u32,
    /// The value set, or the value read.
    pub value: u32,
}

/// `enum e_rknpu_mem_type`.
pub mod mem {
    /// `RKNPU_MEM_NON_CONTIGUOUS`: pages, mapped through the IOMMU.
    pub const NON_CONTIGUOUS: u32 = 1 << 0;
    /// `RKNPU_MEM_CACHEABLE`. Never set: only cacheable objects need `MEM_SYNC`.
    pub const CACHEABLE: u32 = 1 << 1;
    /// `RKNPU_MEM_WRITE_COMBINE`.
    pub const WRITE_COMBINE: u32 = 1 << 2;
    /// `RKNPU_MEM_KERNEL_MAPPING`: the kernel maps the object for itself. A body needs it: the
    /// kernel reads the task array through that mapping and has none without it.
    pub const KERNEL_MAPPING: u32 = 1 << 3;
    /// `RKNPU_MEM_IOMMU`.
    pub const IOMMU: u32 = 1 << 4;
    /// `RKNPU_MEM_ZEROING`: the object starts zeroed, so it holds nothing of a former owner.
    pub const ZEROING: u32 = 1 << 5;
    /// `RKNPU_MEM_SECURE`. Never set.
    pub const SECURE: u32 = 1 << 6;
    /// `RKNPU_MEM_DMA32`. Never set.
    pub const DMA32: u32 = 1 << 7;
    /// `RKNPU_MEM_TRY_ALLOC_SRAM`. Never set: it maps a chunk of shared on-chip memory into the
    /// domain, which the kernel does not clear between owners.
    pub const TRY_ALLOC_SRAM: u32 = 1 << 8;
    /// `RKNPU_MEM_TRY_ALLOC_NBUF`. Never set: it maps the one shared on-chip buffer into every
    /// object that asks, whichever domain the object is in.
    pub const TRY_ALLOC_NBUF: u32 = 1 << 9;
    /// `RKNPU_MEM_IOMMU_LIMIT_IOVA_ALIGNMENT`. Never set.
    pub const IOMMU_LIMIT_IOVA_ALIGNMENT: u32 = 1 << 10;
}

/// `enum e_rknpu_mem_sync_mode`.
pub mod sync {
    /// `RKNPU_MEM_SYNC_TO_DEVICE`.
    pub const TO_DEVICE: u32 = 1 << 0;
    /// `RKNPU_MEM_SYNC_FROM_DEVICE`.
    pub const FROM_DEVICE: u32 = 1 << 1;
}

/// `enum e_rknpu_job_mode`.
pub mod job {
    /// `RKNPU_JOB_PC`: the block fetches its register commands itself. The kernel refuses a job
    /// without it.
    pub const PC: u32 = 1 << 0;
    /// `RKNPU_JOB_NONBLOCK`. Never set: the call would return before the job ends.
    pub const NONBLOCK: u32 = 1 << 1;
    /// `RKNPU_JOB_PINGPONG`: sets the task control's ping-pong bit.
    pub const PINGPONG: u32 = 1 << 2;
    /// `RKNPU_JOB_FENCE_IN`. Never set.
    pub const FENCE_IN: u32 = 1 << 3;
    /// `RKNPU_JOB_FENCE_OUT`. Never set.
    pub const FENCE_OUT: u32 = 1 << 4;
}

/// `enum e_rknpu_action`: [`Action::flags`].
pub mod action {
    /// `RKNPU_GET_HW_VERSION`.
    pub const GET_HW_VERSION: u32 = 0;
    /// `RKNPU_GET_DRV_VERSION`.
    pub const GET_DRV_VERSION: u32 = 1;
    /// `RKNPU_ACT_RESET`. Never sent.
    pub const ACT_RESET: u32 = 6;
    /// `RKNPU_GET_IOMMU_EN`.
    pub const GET_IOMMU_EN: u32 = 18;
    /// `RKNPU_SET_PROC_NICE`. Never sent: it renices the calling thread.
    pub const SET_PROC_NICE: u32 = 19;
    /// `RKNPU_GET_IOMMU_DOMAIN_ID`.
    pub const GET_IOMMU_DOMAIN_ID: u32 = 24;
    /// `RKNPU_SET_IOMMU_DOMAIN_ID`.
    pub const SET_IOMMU_DOMAIN_ID: u32 = 25;
}

/// Core masks (`rknpu_job.h`).
pub mod core {
    /// `RKNPU_CORE_AUTO_MASK`.
    pub const AUTO: u32 = 0x00;
    /// `RKNPU_CORE0_MASK`.
    pub const CORE0: u32 = 0x01;
    /// `RKNPU_CORE1_MASK`.
    pub const CORE1: u32 = 0x02;
    /// `RKNPU_CORE2_MASK`.
    pub const CORE2: u32 = 0x04;
}

// Layouts, request numbers, flags and ids, generated by `layout-check/run.sh` from
// `rknpu_ioctl.h` (gcc, x86_64). Every structure but `Task` (packed) is the same on aarch64.
// BEGIN GENERATED
const _: () = {
    assert!(size_of::<MemCreate>() == 48);
    assert!(offset_of!(MemCreate, handle) == 0);
    assert!(offset_of!(MemCreate, flags) == 4);
    assert!(offset_of!(MemCreate, size) == 8);
    assert!(offset_of!(MemCreate, obj_addr) == 16);
    assert!(offset_of!(MemCreate, dma_addr) == 24);
    assert!(offset_of!(MemCreate, sram_size) == 32);
    assert!(offset_of!(MemCreate, iommu_domain_id) == 40);
    assert!(offset_of!(MemCreate, core_mask) == 44);
    assert!(size_of::<MemMap>() == 16);
    assert!(offset_of!(MemMap, handle) == 0);
    assert!(offset_of!(MemMap, offset) == 8);
    assert!(size_of::<MemDestroy>() == 16);
    assert!(offset_of!(MemDestroy, handle) == 0);
    assert!(offset_of!(MemDestroy, obj_addr) == 8);
    assert!(size_of::<MemSync>() == 32);
    assert!(offset_of!(MemSync, flags) == 0);
    assert!(offset_of!(MemSync, obj_addr) == 8);
    assert!(offset_of!(MemSync, offset) == 16);
    assert!(offset_of!(MemSync, size) == 24);
    assert!(size_of::<Task>() == 40);
    assert!(offset_of!(Task, flags) == 0);
    assert!(offset_of!(Task, op_idx) == 4);
    assert!(offset_of!(Task, enable_mask) == 8);
    assert!(offset_of!(Task, int_mask) == 12);
    assert!(offset_of!(Task, int_clear) == 16);
    assert!(offset_of!(Task, int_status) == 20);
    assert!(offset_of!(Task, regcfg_amount) == 24);
    assert!(offset_of!(Task, regcfg_offset) == 28);
    assert!(offset_of!(Task, regcmd_addr) == 32);
    assert!(size_of::<SubcoreTask>() == 8);
    assert!(offset_of!(SubcoreTask, task_number) == 4);
    assert!(size_of::<Submit>() == 104);
    assert!(offset_of!(Submit, flags) == 0);
    assert!(offset_of!(Submit, timeout) == 4);
    assert!(offset_of!(Submit, task_start) == 8);
    assert!(offset_of!(Submit, task_number) == 12);
    assert!(offset_of!(Submit, task_counter) == 16);
    assert!(offset_of!(Submit, priority) == 20);
    assert!(offset_of!(Submit, task_obj_addr) == 24);
    assert!(offset_of!(Submit, iommu_domain_id) == 32);
    assert!(offset_of!(Submit, reserved) == 36);
    assert!(offset_of!(Submit, task_base_addr) == 40);
    assert!(offset_of!(Submit, hw_elapse_time) == 48);
    assert!(offset_of!(Submit, core_mask) == 56);
    assert!(offset_of!(Submit, fence_fd) == 60);
    assert!(offset_of!(Submit, subcore_task) == 64);
    assert!(size_of::<Action>() == 8);
    assert!(offset_of!(Action, value) == 4);
    assert!(IOCTL_ACTION == 0xc0086440);
    assert!(IOCTL_SUBMIT == 0xc0686441);
    assert!(IOCTL_MEM_CREATE == 0xc0306442);
    assert!(IOCTL_MEM_MAP == 0xc0106443);
    assert!(IOCTL_MEM_DESTROY == 0xc0106444);
    assert!(IOCTL_MEM_SYNC == 0xc0206445);
    assert!(mem::NON_CONTIGUOUS == 0x1);
    assert!(mem::CACHEABLE == 0x2);
    assert!(mem::WRITE_COMBINE == 0x4);
    assert!(mem::KERNEL_MAPPING == 0x8);
    assert!(mem::IOMMU == 0x10);
    assert!(mem::ZEROING == 0x20);
    assert!(mem::SECURE == 0x40);
    assert!(mem::DMA32 == 0x80);
    assert!(mem::TRY_ALLOC_SRAM == 0x100);
    assert!(mem::TRY_ALLOC_NBUF == 0x200);
    assert!(mem::IOMMU_LIMIT_IOVA_ALIGNMENT == 0x400);
    assert!(sync::TO_DEVICE == 0x1);
    assert!(sync::FROM_DEVICE == 0x2);
    assert!(job::PC == 0x1);
    assert!(job::NONBLOCK == 0x2);
    assert!(job::PINGPONG == 0x4);
    assert!(job::FENCE_IN == 0x8);
    assert!(job::FENCE_OUT == 0x10);
    assert!(action::GET_HW_VERSION == 0);
    assert!(action::GET_DRV_VERSION == 0x1);
    assert!(action::ACT_RESET == 0x6);
    assert!(action::GET_IOMMU_EN == 0x12);
    assert!(action::SET_PROC_NICE == 0x13);
    assert!(action::GET_IOMMU_DOMAIN_ID == 0x18);
    assert!(action::SET_IOMMU_DOMAIN_ID == 0x19);
};
// END GENERATED
