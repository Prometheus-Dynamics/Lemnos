//! The vendor kernel's `/dev/mpp_service` interface, as far as an IEP2 job uses it.
//!
//! Transcribed from rockchip-linux/kernel `develop-6.1` at commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`: the request number, commands and flags from
//! `include/uapi/linux/rk-mpp.h` (`(GPL-2.0+ WITH Linux-syscall-note) OR MIT`), the message
//! layout (`struct mpp_msg_v1`), the device types and the message and offset limits from
//! `drivers/video/rockchip/mpp/mpp_common.c` and `mpp_common.h` (`GPL-2.0+ OR MIT`). The tests pin
//! the sizes and offsets the C compiler gives on LP64. No code is copied.
//!
//! The service is shared by every Rockchip media block; this is the IEP's copy of the part it
//! needs, kept here until the shared layer (`drivers/rk-mpp`) lands.

use std::mem::size_of;

use lemnos_linux_sys::iow;

/// The service's node.
pub const NODE: &str = "/dev/mpp_service";

/// `MPP_IOC_CFG_V1`: one call carries a list of [`Msg`]s, read until the one marked last. The
/// number encodes `unsigned int`, not the message, as the header declares it.
pub const IOC_CFG_V1: u32 = iow::<u32>(b'v', 1);

/// `struct mpp_msg_v1`: one message of a call. `data_ptr` is the address of the message's data in
/// the caller's memory, which only the code that issues the call fills in.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Msg {
    /// One of the `CMD_` numbers.
    pub cmd: u32,
    /// `FLAG_` bits.
    pub flags: u32,
    /// Bytes of data.
    pub size: u32,
    /// Byte offset into the device's register file, for register reads and writes of
    /// register-file blocks; zero for the IEP.
    pub offset: u32,
    /// The data's address; zero until the call is issued.
    pub data_ptr: u64,
}

/// `struct reg_offset_elem`: an offset the kernel adds to the address it gives one register.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffsetElem {
    /// The register's index (its byte offset over four).
    pub index: u32,
    /// Bytes added to the buffer's address.
    pub offset: u32,
}

/// `MPP_CMD_QUERY_HW_SUPPORT`: a bit per device type present.
pub const CMD_QUERY_HW_SUPPORT: u32 = 0x000;
/// `MPP_CMD_INIT_CLIENT_TYPE`: binds the open file to one device type.
pub const CMD_INIT_CLIENT_TYPE: u32 = 0x100;
/// `MPP_CMD_INIT_TRANS_TABLE`: replaces the table of registers holding descriptors. Never sent.
pub const CMD_INIT_TRANS_TABLE: u32 = 0x102;
/// `MPP_CMD_SET_REG_WRITE`: for the IEP, the job's parameters.
pub const CMD_SET_REG_WRITE: u32 = 0x200;
/// `MPP_CMD_SET_REG_READ`: for the IEP, where the job's results go.
pub const CMD_SET_REG_READ: u32 = 0x201;
/// `MPP_CMD_SET_REG_ADDR_OFFSET`: a list of [`OffsetElem`].
pub const CMD_SET_REG_ADDR_OFFSET: u32 = 0x202;
/// `MPP_CMD_POLL_HW_FINISH`: waits for the job and copies its results out.
pub const CMD_POLL_HW_FINISH: u32 = 0x300;
/// `MPP_CMD_RESET_SESSION`: waits for the file's jobs and lets go of every dma-buf it imported.
pub const CMD_RESET_SESSION: u32 = 0x400;
/// `MPP_CMD_RELEASE_FD`: lets go of the listed dma-bufs.
pub const CMD_RELEASE_FD: u32 = 0x402;

/// `MPP_FLAGS_MULTI_MSG`: more messages may follow.
pub const FLAG_MULTI_MSG: u32 = 0x1;
/// `MPP_FLAGS_LAST_MSG`: with `FLAG_MULTI_MSG`, this is the last.
pub const FLAG_LAST_MSG: u32 = 0x2;
/// `MPP_FLAGS_REG_FD_NO_TRANS`: the address words are bus addresses, not descriptors. Never set
/// here: it would hand the block raw addresses.
pub const FLAG_REG_FD_NO_TRANS: u32 = 0x4;
/// `MPP_FLAGS_SCL_FD_NO_TRANS`. Never set.
pub const FLAG_SCL_FD_NO_TRANS: u32 = 0x8;
/// `MPP_FLAGS_REG_NO_OFFSET`: an address word is the whole descriptor, and its offset comes from
/// a `CMD_SET_REG_ADDR_OFFSET` list. Without it, a word holds the descriptor in its low 10 bits
/// and the offset in the high 22.
pub const FLAG_REG_NO_OFFSET: u32 = 0x10;
/// `MPP_FLAGS_SECURE_MODE`. Never set.
pub const FLAG_SECURE_MODE: u32 = 0x1_0000;

/// `MPP_DEVICE_IEP2`: the device type an IEP2 session binds to.
pub const DEVICE_IEP2: u32 = 28;

/// `MPP_MAX_MSG_NUM`: messages one call may carry.
pub const MAX_MSGS: usize = 16;
/// `MPP_MAX_REG_TRANS_NUM`: offsets one job may carry.
pub const MAX_OFFSETS: usize = 80;

/// The bytes of a [`Msg`].
pub const MSG_BYTES: usize = size_of::<Msg>();
