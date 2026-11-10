//! The kernel interface of `/dev/mpp_service`, transcribed. Interface facts only: request numbers,
//! structure layouts, command ids, flags, client types and limits. No code is copied.
//!
//! Sources, all in rockchip-linux/kernel, branch `develop-6.1`, commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//!
//! - `include/uapi/linux/rk-mpp.h` (`(GPL-2.0+ WITH Linux-syscall-note) OR MIT`): the request
//!   numbers, [`Command`], the `MPP_FLAGS_*` flags, `struct mpp_bat_msg` ([`BatMsg`]) and
//!   `MPP_BAT_MSG_DONE`.
//! - `drivers/video/rockchip/mpp/mpp_common.c`: `struct mpp_msg_v1` ([`MsgV1`]), the only argument
//!   layout `mpp_collect_msgs` accepts, and how each command is processed.
//! - `drivers/video/rockchip/mpp/mpp_common.h` (`GPL-2.0+ OR MIT`): `enum MPP_DEVICE_TYPE`
//!   ([`client`]), `struct reg_offset_elem` ([`RegOffsetElem`]), `MPP_MAX_MSG_NUM` and
//!   `MPP_MAX_REG_TRANS_NUM`.
//!
//! The sizes and offsets are pinned by the tests, and were checked against `gcc` (x86_64) and
//! `clang --target=aarch64-linux-gnu` on the same declarations.

use lemnos_linux_sys::iow;

/// `MPP_IOC_MAGIC`.
pub const MAGIC: u8 = b'v';

/// `MPP_IOC_CFG_V1`: one call carries a chain of [`MsgV1`]. The number encodes `unsigned int` as
/// its size, but the kernel reads the chain of 24-byte messages from the argument, one after the
/// other, until a message without [`flags::MULTI_MSG`] or with [`flags::LAST_MSG`].
pub const IOC_CFG_V1: u32 = iow::<u32>(MAGIC, 1);

/// `MPP_IOC_CFG_V2`: declared by the header, refused by `mpp_collect_msgs`; never sent.
pub const IOC_CFG_V2: u32 = iow::<u32>(MAGIC, 2);

/// `enum MPP_DEV_COMMAND_TYPE`: the `cmd` of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Command {
    /// `MPP_CMD_QUERY_HW_SUPPORT`: the kernel writes a `u32` bit set of the client types present.
    QueryHwSupport = 0x000,
    /// `MPP_CMD_QUERY_HW_ID`: the kernel reads a client type and writes that block's id.
    QueryHwId = 0x001,
    /// `MPP_CMD_QUERY_CMD_SUPPORT`: the kernel reads a command base and writes its end.
    QueryCmdSupport = 0x002,
    /// `MPP_CMD_INIT_CLIENT_TYPE`: binds the session to one client type (a `u32`).
    InitClientType = 0x100,
    /// `MPP_CMD_INIT_DRIVER_DATA`: writes a `u32` into a fixed system register. Never sent.
    InitDriverData = 0x101,
    /// `MPP_CMD_INIT_TRANS_TABLE`: the session's own list of `u16` register indices whose values
    /// the kernel takes as a dma-buf descriptor plus offset.
    InitTransTable = 0x102,
    /// `MPP_CMD_SET_REG_WRITE`: register values for the task, at a byte `offset`.
    SetRegWrite = 0x200,
    /// `MPP_CMD_SET_REG_READ`: registers to copy back to the given memory when the task is polled.
    SetRegRead = 0x201,
    /// `MPP_CMD_SET_REG_ADDR_OFFSET`: [`RegOffsetElem`]s, byte offsets added to translated
    /// registers.
    SetRegAddrOffset = 0x202,
    /// `MPP_CMD_SET_RCB_INFO`: places the block's line buffers in on-chip memory by register
    /// index. Never sent.
    SetRcbInfo = 0x203,
    /// `MPP_CMD_SET_SESSION_FD`: switches the rest of the chain to the session of another open
    /// file, by descriptor ([`BatMsg`]). Never sent.
    SetSessionFd = 0x204,
    /// `MPP_CMD_POLL_HW_FINISH`: waits for the session's oldest task and copies its read-back.
    PollHwFinish = 0x300,
    /// `MPP_CMD_POLL_HW_IRQ`. Never sent.
    PollHwIrq = 0x301,
    /// `MPP_CMD_RESET_SESSION`: waits for the session's tasks, then unmaps every buffer it holds.
    ResetSession = 0x400,
    /// `MPP_CMD_TRANS_FD_TO_IOVA`: maps descriptors and writes their device addresses back. Never
    /// sent.
    TransFdToIova = 0x401,
    /// `MPP_CMD_RELEASE_FD`: drops the session's mapping of each `u32` descriptor given.
    ReleaseFd = 0x402,
    /// `MPP_CMD_SEND_CODEC_INFO`: per-block statistics. Never sent.
    SendCodecInfo = 0x403,
}

impl Command {
    /// The value a message carries.
    #[must_use]
    pub const fn code(self) -> u32 {
        self as u32
    }
}

/// The `MPP_FLAGS_*` values of a message.
pub mod flags {
    /// `MPP_FLAGS_MULTI_MSG`: another message follows, unless [`LAST_MSG`] is set too.
    pub const MULTI_MSG: u32 = 0x0000_0001;
    /// `MPP_FLAGS_LAST_MSG`: the last message of a chain.
    pub const LAST_MSG: u32 = 0x0000_0002;
    /// `MPP_FLAGS_REG_FD_NO_TRANS`: no register is translated; their raw values reach the block
    /// as device addresses. Never set.
    pub const REG_FD_NO_TRANS: u32 = 0x0000_0004;
    /// `MPP_FLAGS_SCL_FD_NO_TRANS`. Never set.
    pub const SCL_FD_NO_TRANS: u32 = 0x0000_0008;
    /// `MPP_FLAGS_REG_NO_OFFSET`: a translated register holds a whole descriptor and no offset.
    /// Never set.
    pub const REG_NO_OFFSET: u32 = 0x0000_0010;
    /// `MPP_FLAGS_SECURE_MODE`. Never set.
    pub const SECURE_MODE: u32 = 0x0001_0000;
}

/// `enum MPP_DEVICE_TYPE`: the client types a session binds to, and their bit in
/// [`Command::QueryHwSupport`]'s answer.
pub mod client {
    /// `MPP_DEVICE_VDPU1`.
    pub const VDPU1: u32 = 0;
    /// `MPP_DEVICE_VDPU2`.
    pub const VDPU2: u32 = 1;
    /// `MPP_DEVICE_VDPU1_PP`.
    pub const VDPU1_PP: u32 = 2;
    /// `MPP_DEVICE_VDPU2_PP`.
    pub const VDPU2_PP: u32 = 3;
    /// `MPP_DEVICE_AV1DEC`.
    pub const AV1DEC: u32 = 4;
    /// `MPP_DEVICE_HEVC_DEC`.
    pub const HEVC_DEC: u32 = 8;
    /// `MPP_DEVICE_RKVDEC`.
    pub const RKVDEC: u32 = 9;
    /// `MPP_DEVICE_AVSPLUS_DEC`.
    pub const AVSPLUS_DEC: u32 = 12;
    /// `MPP_DEVICE_RKJPEGD`.
    pub const RKJPEGD: u32 = 13;
    /// `MPP_DEVICE_RKVENC`.
    pub const RKVENC: u32 = 16;
    /// `MPP_DEVICE_VEPU1`.
    pub const VEPU1: u32 = 17;
    /// `MPP_DEVICE_VEPU2`.
    pub const VEPU2: u32 = 18;
    /// `MPP_DEVICE_VEPU2_JPEG`.
    pub const VEPU2_JPEG: u32 = 19;
    /// `MPP_DEVICE_RKJPEGE`.
    pub const RKJPEGE: u32 = 20;
    /// `MPP_DEVICE_VEPU22`.
    pub const VEPU22: u32 = 24;
    /// `MPP_DEVICE_IEP2`.
    pub const IEP2: u32 = 28;
    /// `MPP_DEVICE_VDPP`.
    pub const VDPP: u32 = 29;
    /// `MPP_DEVICE_BUTT`: one past the last.
    pub const END: u32 = 30;
}

/// `MPP_MAX_MSG_NUM`: messages one task may carry.
pub const MAX_MSG_NUM: usize = 16;

/// `MPP_MAX_REG_TRANS_NUM`: entries of a session's translation table, of a task's register
/// offsets, and of a task's mapped regions.
pub const MAX_REG_TRANS_NUM: usize = 80;

/// The bits of a translated register that name the descriptor (the rest is a byte offset, which
/// this crate always leaves zero): `0x3ff` in `mpp_translate_reg_address`.
pub const FD_MASK: u32 = 0x3ff;

/// `MPP_BAT_MSG_DONE`.
pub const BAT_MSG_DONE: u64 = 1;

/// `struct mpp_msg_v1`: one message of an [`IOC_CFG_V1`] chain.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MsgV1 {
    /// A [`Command`].
    pub cmd: u32,
    /// [`flags`].
    pub flags: u32,
    /// Bytes at `data_ptr`.
    pub size: u32,
    /// For register reads and writes, the byte offset of the first register.
    pub offset: u32,
    /// A user pointer the kernel reads or writes `size` bytes at.
    pub data_ptr: u64,
}

/// `struct mpp_bat_msg`: the payload of [`Command::SetSessionFd`]. Transcribed for the layout
/// test only; never sent.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatMsg {
    /// [`BAT_MSG_DONE`] when this switch is to be skipped.
    pub flag: u64,
    /// The descriptor of the session to switch to.
    pub fd: u32,
    /// The kernel's answer.
    pub ret: i32,
}

/// `struct reg_offset_elem`: one entry of [`Command::SetRegAddrOffset`]. After translation, the
/// kernel adds `offset` to register `index` (`mpp_translate_reg_offset_info`), checking neither.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegOffsetElem {
    /// The register's index in the task's register array.
    pub index: u32,
    /// Bytes added to its device address.
    pub offset: u32,
}

impl RegOffsetElem {
    /// The entry as the kernel reads it.
    #[must_use]
    pub fn to_ne_bytes(self) -> [u8; 8] {
        let mut out = [0; 8];
        out[..4].copy_from_slice(&self.index.to_ne_bytes());
        out[4..].copy_from_slice(&self.offset.to_ne_bytes());
        out
    }
}
