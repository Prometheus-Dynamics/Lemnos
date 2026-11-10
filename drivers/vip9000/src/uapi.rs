//! The vendor kernel's `vipcore` node as it defines it: the request number, the command numbers,
//! the allocation flags, and the structures the requests this crate builds take.
//!
//! Transcribed (interface facts only, no code copied) from the Radxa `allwinner-bsp` tree at
//! commit `a842bb8dc74608ba13975c97c31b9518f1976c01` (branch `cubie-aiot-v1.5.0`), directory
//! `drivers/npu/aw_nna_vip/vip2/` (VeriSilicon, dual MIT and GPL, used under MIT):
//!
//! - `inc/vip_drv_share.h`: `vipdrv_intrface_e` (the commands), `vipdrv_video_mem_alloc_flag_e`,
//!   `vipdrv_task_cmd_type_e`, `vipdrv_task_patch_type_e`, the request structures,
//!   `vipdMAX_CMD_SIZE` and the command sizes that make up `APPEND_COMMAND_SIZE`.
//! - `os/linux/vip_drv_os.h`: `VIPDRV_IOCTL` and `struct ioctl_data`.
//! - `inc/vip_lite_common.h` and `inc/vip_lite_config.h`: the scalar types (`vip_address_t` is 64
//!   bits, `vip_size_t` 32 bits without `vpmdNBG_SIZE_OVER_4G`, `vip_enum` 32 bits) and the build
//!   options the layouts depend on (`vpmdENABLE_PREEMPTION` 1, which adds `priority` to the
//!   submit structure; `vpmdENABLE_MULTIPLE_TASK` 3, parallel synchronous, so not
//!   `vpmdTASK_PARALLEL_ASYNC` and not `vpmdENABLE_POLLING`).
//! - `os/linux/vip_drv_device_driver.c`: `drv_ioctl`, the request's handling: the request number
//!   is a plain number (not an `_IOC` encoding); the kernel copies `bytes` from `buffer`, calls
//!   the command, and copies the same bytes back.
//!
//! The sizes and offsets are pinned in `layout.rs`, generated from the headers with gcc on x86_64
//! by `layout-check/run.sh`; every field has a fixed width and the same alignment on aarch64.

/// The node the kernel creates (`DEVICE_NAME`, a misc device or a character device).
pub const DEVICE: &str = "/dev/vipcore";

/// The only request number the node takes (`VIPDRV_IOCTL`). Its argument is an [`IoctlData`].
pub const VIPDRV_IOCTL: u32 = 30000;

/// The largest command buffer the kernel runs, in bytes (`vipdMAX_CMD_SIZE`, 65535 * 8).
pub const MAX_CMD_SIZE: u32 = 65535 * 8;

/// Bytes the kernel writes after a subtask's commands when it appends its own end of task
/// (`APPEND_COMMAND_SIZE`: `SEMAPHORE_STALL` 24 + `CHIP_ENABLE_SIZE` 8 + `EVENT_SIZE` 8 +
/// `CHIP_ENABLE_SIZE` 8 + `END_SIZE` 8, the build without `vpmdTASK_PARALLEL_ASYNC` and without
/// `vpmdENABLE_POLLING`). `vip_drv_task_schedule.c` writes them at the command buffer's address
/// plus the subtask's `size`, which the kernel does not compare with the buffer.
pub const APPEND_COMMAND_SIZE: u32 = 24 + 8 + 8 + 8 + 8;

/// The commands of [`IoctlData::command`] (`vipdrv_intrface_e`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Command {
    Init = 0,
    Destroy = 1,
    ReadReg = 2,
    WriteReg = 3,
    WaitTask = 4,
    SubmitTask = 5,
    Allocation = 6,
    Deallocation = 7,
    NotUsed = 8,
    QueryAddressInfo = 9,
    QueryDatabase = 10,
    OperateCache = 11,
    WrapUserMemory = 12,
    UnwrapUserMemory = 13,
    NotUsed2 = 14,
    PowerManagement = 15,
    WrapUserPhysical = 16,
    UnwrapUserPhysical = 17,
    WrapUserFd = 18,
    UnwrapUserFd = 19,
    QueryProfiling = 20,
    QueryDriverStatus = 21,
    QueryMmuInfo = 22,
    MapUserLogical = 23,
    UnmapUserLogical = 24,
    CancelTask = 25,
    QueryRegisterDump = 26,
    CreateTask = 27,
    SetTaskProperty = 28,
    DestroyTask = 29,
}

impl Command {
    /// Every command, in number order (`VIPDRV_INTRFACE_MAX` is 30).
    pub const ALL: [Self; 30] = [
        Self::Init,
        Self::Destroy,
        Self::ReadReg,
        Self::WriteReg,
        Self::WaitTask,
        Self::SubmitTask,
        Self::Allocation,
        Self::Deallocation,
        Self::NotUsed,
        Self::QueryAddressInfo,
        Self::QueryDatabase,
        Self::OperateCache,
        Self::WrapUserMemory,
        Self::UnwrapUserMemory,
        Self::NotUsed2,
        Self::PowerManagement,
        Self::WrapUserPhysical,
        Self::UnwrapUserPhysical,
        Self::WrapUserFd,
        Self::UnwrapUserFd,
        Self::QueryProfiling,
        Self::QueryDriverStatus,
        Self::QueryMmuInfo,
        Self::MapUserLogical,
        Self::UnmapUserLogical,
        Self::CancelTask,
        Self::QueryRegisterDump,
        Self::CreateTask,
        Self::SetTaskProperty,
        Self::DestroyTask,
    ];
}

/// `vipdrv_video_mem_alloc_flag_e`.
pub mod alloc_flag {
    pub const NONE: u32 = 0x000;
    pub const CONTIGUOUS_64K: u32 = 0x001;
    pub const CONTIGUOUS_1M: u32 = 0x002;
    pub const CONTIGUOUS_16M: u32 = 0x004;
    pub const CONTIGUOUS: u32 = 0x008;
    pub const ADDR_4GB: u32 = 0x010;
    pub const NPU_READ_ONLY: u32 = 0x040;
    /// Not mapped in the block's page table: the allocation's address is a physical one.
    pub const NO_MMU_PAGE: u32 = 0x080;
    pub const NONE_CACHE: u32 = 0x100;
    pub const MAP_USER: u32 = 0x200;
    pub const MAP_KERNEL: u32 = 0x400;
    pub const MMU_PAGE_MEM: u32 = 0x800;
}

/// `vipdrv_task_cmd_type_e`: which command buffer of a task a property sets.
pub mod task_cmd {
    pub const SUBTASK: u32 = 0;
    pub const INIT: u32 = 1;
    pub const PRELOAD: u32 = 2;
}

/// `vipdrv_task_patch_type_e`.
pub mod patch {
    pub const NONE: u32 = 0;
    pub const SYNC: u32 = 1;
    pub const CHIP: u32 = 2;
    pub const DUP: u32 = 3;
}

/// `vip_status_e`: what a command answers in [`IoctlData::error`].
pub mod status {
    pub const SUCCESS: i32 = 0;
    pub const TIMEOUT: i32 = -1;
    pub const IO: i32 = -2;
    pub const INVALID_ARGUMENTS: i32 = -3;
    pub const NOT_SUPPORTED: i32 = -4;
    pub const OUT_OF_RESOURCE: i32 = -5;
    pub const OUT_OF_MEMORY: i32 = -6;
    pub const FAILURE: i32 = -11;
    pub const POWER_OFF: i32 = -12;
    pub const CANCELED: i32 = -15;
}

/// `struct ioctl_data`: the request's argument. `buffer` is a user address the kernel copies
/// `bytes` from and back to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct IoctlData {
    pub command: u32,
    pub error: i32,
    pub bytes: u32,
    pub buffer: u64,
}

/// `vipdrv_initialize_t` (`Init`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Initialize {
    pub video_mem_size: u32,
    pub version: u32,
}

/// `vipdrv_allocation_t` (`Allocation`). `physical` is in and out: with `specified` set the
/// kernel places the allocation at that address, virtual or (with `NO_MMU_PAGE`) physical.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Allocation {
    pub logical: u64,
    pub mem_id: u32,
    pub physical: u64,
    pub size: u32,
    pub align: u32,
    pub alloc_flag: u32,
    pub device_vis: u64,
    pub specified: u8,
}

/// `vipdrv_deallocation_t` (`Deallocation`), and the argument of the unwrap commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Deallocation {
    pub mem_id: u32,
}

/// `vipdrv_wrap_user_fd_t` (`WrapUserFd`): a dma-buf mapped into the block's page table.
/// `virtual_addr`, `mem_id` and `logical` come back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct WrapUserFd {
    pub fd: u32,
    pub memory_type: u32,
    pub size: u32,
    pub device_vis: u64,
    pub virtual_addr: u64,
    pub mem_id: u32,
    pub logical: u64,
}

/// `vipdrv_reg_t` (`ReadReg`, `WriteReg`). Pinned so the refusal of those commands names a
/// layout; this crate builds none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Reg {
    pub dev_index: u32,
    pub core_index: u32,
    pub reg: u32,
    pub data: u32,
}

/// `TASK_NAME_LENGTH`.
pub const TASK_NAME_LENGTH: usize = 16;

/// `vipdrv_create_task_param_t` (`CreateTask`). `task_id` comes back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CreateTask {
    pub task_id: u32,
    pub task_name: [u8; TASK_NAME_LENGTH],
    pub subtask_count: u32,
}

/// `vipdrv_destroy_task_param_t` (`DestroyTask`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct DestroyTask {
    pub task_id: u32,
}

/// `vipdrv_set_task_property_t` (`SetTaskProperty`). The union `param` is five words; for
/// `patch_type` [`patch::NONE`] it is `subtask_info`: `subtask_id` (the command buffer's
/// `mem_id`), `size` (the commands' bytes), `patch_count`, `core_index`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct SetTaskProperty {
    pub task_id: u32,
    pub kind: u32,
    pub index: u32,
    pub patch_type: u32,
    pub param: [u32; 5],
}

/// `vipdrv_submit_task_param_t` (`SubmitTask`), with `vpmdENABLE_PREEMPTION` set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct SubmitTask {
    pub task_id: u32,
    pub subtask_index: u32,
    pub subtask_cnt: u32,
    pub device_index: u32,
    pub core_index: u32,
    pub core_cnt: u32,
    pub priority: u8,
    pub time_out: u32,
    pub need_schedule: u8,
}

/// `vipdrv_wait_task_param_t` (`WaitTask`), the same shape as `vipdrv_cancel_task_param_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct WaitTask {
    pub task_id: u32,
    pub subtask_index: u32,
}
