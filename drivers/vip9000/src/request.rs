//! The requests a run on the block needs, built and checked here, and why every other command of
//! the node is never sent.
//!
//! Each builder takes the numbers a caller would steer and refuses the ones the kernel takes
//! without a check, so a request built here names no memory but a buffer the claim holds. The
//! paths, with what the kernel does with each (`drivers/npu/aw_nna_vip/vip2/` at
//! `a842bb8dc74608ba13975c97c31b9518f1976c01`):
//!
//! - `WrapUserFd` (`os/linux/allocator/vip_drv_mem_allocator_wrap_dma_buf.c`): the kernel maps the
//!   dma-buf behind `fd` and refuses a `size` past the dma-buf's mapped length. [`wrap_fd`] also
//!   refuses a size past the buffer's size measured by the claim, a size of 0, a size a 32-bit
//!   `vip_size_t` cannot carry, and a descriptor that is not positive; `device_vis` is always
//!   device 0 only and `memory_type` 0.
//! - `Allocation` (`memory/vip_drv_video_memory.c`, `vipdrv_mem_allocate_videomemory`): with
//!   `specified` the kernel places the allocation at the caller's `physical`, a virtual address,
//!   or a physical one under `NO_MMU_PAGE`. [`command_buffer`] never sets `specified` or
//!   `NO_MMU_PAGE`, takes no address, and sizes the buffer for the commands plus
//!   [`APPEND_COMMAND_SIZE`].
//! - `SetTaskProperty` (`task/vip_drv_task_descriptor.c`): the kernel stores `subtask_info.size`
//!   and the patch offsets as given; at submit (`task/vip_drv_task_schedule.c`) it writes
//!   [`APPEND_COMMAND_SIZE`] bytes at the command buffer plus `size`, and patch words at the
//!   buffer plus each patch `offset`, comparing neither with the buffer. [`subtask`] takes the
//!   command buffer's real capacity and refuses commands that leave no room for the append, and
//!   always sends no patches (`patch_count` 0, `patch_type` NONE).
//! - `SubmitTask`: the patch loop runs only for `core_cnt` above 1. [`submit`] always submits one
//!   subtask on device 0, core 0, one core.
//!
//! No run is offered (see [`crate::confine`]), so nothing in this crate sends these requests yet;
//! they are the checked half of the run a confining kernel path would take.

use crate::uapi::{
    APPEND_COMMAND_SIZE, Allocation, Command, CreateTask, MAX_CMD_SIZE, SetTaskProperty,
    SubmitTask, TASK_NAME_LENGTH, WaitTask, WrapUserFd, alloc_flag, patch, task_cmd,
};

/// The only device a request names: bit 0 of `device_vis`, device index 0.
const DEVICE_0: u64 = 1;

/// Alignment of every allocation (`vipdMEMORY_ALIGN_SIZE`, 64 bytes).
pub const ALIGN: u32 = 64;

/// Why a request was not built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The descriptor is not positive.
    Descriptor,
    /// No bytes.
    Empty,
    /// Past the buffer the claim holds, or past what the field carries.
    TooLarge,
    /// The commands are not a whole number of 8-byte command words.
    Unaligned,
}

/// A dma-buf the claim holds, `bytes` of it mapped for the block. `held` is its size as the claim
/// measured it.
pub fn wrap_fd(fd: i32, held: u64, bytes: u64) -> Result<WrapUserFd, RequestError> {
    let fd = u32::try_from(fd)
        .ok()
        .filter(|&fd| fd > 0)
        .ok_or(RequestError::Descriptor)?;
    if bytes == 0 {
        return Err(RequestError::Empty);
    }
    if bytes > held {
        return Err(RequestError::TooLarge);
    }
    let size = u32::try_from(bytes).map_err(|_| RequestError::TooLarge)?;
    Ok(WrapUserFd {
        fd,
        memory_type: 0,
        size,
        device_vis: DEVICE_0,
        ..WrapUserFd::default()
    })
}

/// The bytes a command buffer for `commands` bytes of commands needs: the commands and the
/// kernel's append, aligned.
pub fn command_capacity(commands: u32) -> Result<u32, RequestError> {
    if commands == 0 {
        return Err(RequestError::Empty);
    }
    if !commands.is_multiple_of(8) {
        return Err(RequestError::Unaligned);
    }
    let need = commands
        .checked_add(APPEND_COMMAND_SIZE)
        .filter(|&n| n <= MAX_CMD_SIZE)
        .ok_or(RequestError::TooLarge)?;
    need.checked_next_multiple_of(ALIGN)
        .ok_or(RequestError::TooLarge)
}

/// An allocation for a command buffer holding `commands` bytes: mapped in the block's page
/// table and for the caller to fill, at an address the kernel picks.
pub fn command_buffer(commands: u32) -> Result<Allocation, RequestError> {
    Ok(Allocation {
        size: command_capacity(commands)?,
        align: ALIGN,
        alloc_flag: alloc_flag::MAP_USER,
        device_vis: DEVICE_0,
        specified: 0,
        physical: 0,
        ..Allocation::default()
    })
}

/// A task of one subtask.
pub fn create_task(name: &[u8]) -> CreateTask {
    let mut task_name = [0u8; TASK_NAME_LENGTH];
    let n = name.len().min(TASK_NAME_LENGTH - 1);
    task_name[..n].copy_from_slice(&name[..n]);
    CreateTask {
        task_id: 0,
        task_name,
        subtask_count: 1,
    }
}

/// Subtask 0 of `task` runs `commands` bytes of the command buffer `mem_id`, whose capacity the
/// claim allocated as `capacity`.
pub fn subtask(
    task: u32,
    mem_id: u32,
    capacity: u32,
    commands: u32,
) -> Result<SetTaskProperty, RequestError> {
    if command_capacity(commands)? > capacity {
        return Err(RequestError::TooLarge);
    }
    Ok(SetTaskProperty {
        task_id: task,
        kind: task_cmd::SUBTASK,
        index: 0,
        patch_type: patch::NONE,
        // subtask_info: subtask_id, size, patch_count, core_index.
        param: [mem_id, commands, 0, 0, 0],
    })
}

/// Subtask 0 of `task`, on device 0, core 0, one core, waited for up to `time_out_ms`.
pub fn submit(task: u32, time_out_ms: u32) -> SubmitTask {
    SubmitTask {
        task_id: task,
        subtask_index: 0,
        subtask_cnt: 1,
        device_index: 0,
        core_index: 0,
        core_cnt: 1,
        priority: 0,
        time_out: time_out_ms,
        need_schedule: 1,
    }
}

/// The wait for [`submit`]'s subtask.
pub fn wait(task: u32) -> WaitTask {
    WaitTask {
        task_id: task,
        subtask_index: 0,
    }
}

/// Why a command is never sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    /// Reads or writes any offset from the register window: `vipdrv_os_write_reg` is `writel` at
    /// the window's base plus the caller's 32-bit `reg`, checked against nothing.
    RegisterAccess,
    /// Maps physical addresses the caller names into the block's page table; the kernel checks
    /// only their page alignment (`vip_drv_mem_allocator_wrap_user_physical.c`).
    RawPhysical,
    /// Pins and maps any user address range of the calling process. Buffers come as dma-bufs.
    UserMemory,
    /// Exposes the page tables, or maps any allocation by id into the caller.
    PageTables,
    /// Power, frequency, profiling, dumps and agent addresses: not part of a run.
    NotARun,
    /// No command behind the number.
    Unused,
}

/// Whether `command` is one a run sends, and if not, why.
pub const fn withheld(command: Command) -> Option<Withheld> {
    match command {
        Command::Init
        | Command::Destroy
        | Command::WaitTask
        | Command::SubmitTask
        | Command::Allocation
        | Command::Deallocation
        | Command::QueryDatabase
        | Command::OperateCache
        | Command::WrapUserFd
        | Command::UnwrapUserFd
        | Command::CancelTask
        | Command::CreateTask
        | Command::SetTaskProperty
        | Command::DestroyTask => None,
        Command::ReadReg | Command::WriteReg => Some(Withheld::RegisterAccess),
        Command::WrapUserPhysical | Command::UnwrapUserPhysical => Some(Withheld::RawPhysical),
        Command::WrapUserMemory | Command::UnwrapUserMemory => Some(Withheld::UserMemory),
        Command::QueryMmuInfo | Command::MapUserLogical | Command::UnmapUserLogical => {
            Some(Withheld::PageTables)
        }
        Command::PowerManagement
        | Command::QueryProfiling
        | Command::QueryDriverStatus
        | Command::QueryRegisterDump
        | Command::QueryAddressInfo => Some(Withheld::NotARun),
        Command::NotUsed | Command::NotUsed2 => Some(Withheld::Unused),
    }
}
