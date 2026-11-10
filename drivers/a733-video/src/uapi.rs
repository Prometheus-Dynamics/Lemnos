//! The video engine's character device as the vendor kernel defines it: request numbers, lock
//! bits, modes, the structures its requests take, and the facts about the register window.
//!
//! Transcribed (interface facts only, no code copied) from the Radxa `allwinner-bsp` tree at
//! commit `a842bb8dc74608ba13975c97c31b9518f1976c01` (branch `cubie-aiot-v1.5.0`):
//!
//! - `include/uapi/linux/cedar_ve_uapi.h` (`GPL-2.0-or-later WITH Linux-syscall-note`): the
//!   request numbers (`enum IOCTL_CMD`) and the structures.
//! - `drivers/ve/cedar-ve/cedar_ve.h`: the lock bits (`VE_LOCK_*`) and `enum VE_MODE`.
//! - `drivers/ve/cedar-ve/cedar_ve.c`: what each request does with its argument
//!   (`_compat_cedardev_ioctl`), the device name (`cedar_ve2_quirk`), and `cedardev_mmap`.
//! - `drivers/ve/cedar-ve/platforms/ve_plat_sun60iw2.c`: the register blocks inside the window.
//! - `configs/linux-6.6/sun60iw2p1.dtsi`, node `ve2@1c10000`: the window's size and
//!   `ve_top_reg_offset`.
//!
//! The requests are plain numbers, not `_IOC` encodings. The sizes and offsets below were checked
//! against gcc on x86_64 and clang for aarch64; the tests pin them.

/// The device node of the second engine, the one the brief names.
pub const DEVICE: &str = "/dev/cedar_dev_ve2";

/// `IOCTL_WAIT_VE_DE`: waits for the decoder interrupt. The argument is the timeout in whole
/// seconds, by value; the result is 1 when the interrupt came, 0 on timeout.
pub const WAIT_VE_DE: u32 = 0x102;
/// `IOCTL_WAIT_VE_EN`: as [`WAIT_VE_DE`], for the encoder interrupt.
pub const WAIT_VE_EN: u32 = 0x103;
/// `IOCTL_RESET_VE`: pulses the engine's reset line. No argument.
pub const RESET_VE: u32 = 0x104;
/// `IOCTL_ENGINE_REQ`: counts a user of the engine; the first turns its clocks on. No argument.
pub const ENGINE_REQ: u32 = 0x206;
/// `IOCTL_ENGINE_REL`: the reverse of [`ENGINE_REQ`]; the last turns the clocks off.
pub const ENGINE_REL: u32 = 0x207;
/// `IOCTL_SET_REFCOUNT`: overwrites the user count. Never issued.
pub const SET_REFCOUNT: u32 = 0x20c;
/// `IOCTL_GET_LOCK`: takes the lock named by one [`lock`] bit, by value. Blocks, uninterruptibly,
/// while another opener holds it. Closing the file releases the locks it holds.
pub const GET_LOCK: u32 = 0x310;
/// `IOCTL_RELEASE_LOCK`: releases a lock this file holds, by value.
pub const RELEASE_LOCK: u32 = 0x311;
/// `IOCTL_GET_IOMMU_ADDR`: the older form of [`MAP_DMA_BUF`], taking a [`UserIommuParam`].
/// Never issued.
pub const GET_IOMMU_ADDR: u32 = 0x502;
/// `IOCTL_MAP_DMA_BUF`: attaches a dma-buf to the engine and answers its bus address. Takes a
/// [`DmaBufParam`]: `fd` in, `phy_addr` out.
pub const MAP_DMA_BUF: u32 = 0x504;
/// `IOCTL_UNMAP_DMA_BUF`: detaches the mapping made from the same descriptor number by this
/// process. Takes a [`DmaBufParam`] (`fd` in).
pub const UNMAP_DMA_BUF: u32 = 0x505;
/// `IOCTL_FLUSH_CACHE_RANGE`: cache maintenance over a range of the caller's virtual addresses,
/// taking a [`CacheRange`]. Never issued: the dma-buf sync request does this for a buffer.
pub const FLUSH_CACHE_RANGE: u32 = 0x506;
/// `IOCTL_VE_MODE`: sets which interrupt the engine's handler looks at, a [`mode`] by value. The
/// setting is the engine's, shared by every opener.
pub const VE_MODE: u32 = 0x800;
/// `IOCTL_WAIT_VCU_ENC`: as [`WAIT_VE_EN`], in [`mode::VCU_ENC`].
pub const WAIT_VCU_ENC: u32 = 0x801;
/// `IOCTL_CLEAR_EN_INT_FLAG`: forgets an encoder interrupt not yet waited for. No argument.
pub const CLEAR_EN_INT_FLAG: u32 = 0x803;
/// `IOCTL_GET_VE_TOP_REG_OFFSET`: answers `ve_top_reg_offset` as the call's result.
pub const GET_VE_TOP_REG_OFFSET: u32 = 0x804;
/// `IOCTL_WAIT_VCU_DEC`: as [`WAIT_VE_DE`], in [`mode::VCU_DEC`].
pub const WAIT_VCU_DEC: u32 = 0x805;

/// The lock bits `IOCTL_GET_LOCK` and `IOCTL_RELEASE_LOCK` take (`VE_LOCK_*`).
pub mod lock {
    /// `VE_LOCK_VDEC`.
    pub const VDEC: u32 = 0x01;
    /// `VE_LOCK_VENC`.
    pub const VENC: u32 = 0x02;
    /// `VE_LOCK_JDEC`.
    pub const JDEC: u32 = 0x04;
}

/// `enum VE_MODE`, the values `IOCTL_VE_MODE` accepts.
pub mod mode {
    /// `VE_MODE_ENCPP`.
    pub const ENCPP: u32 = 0;
    /// `VE_MODE_ENC`.
    pub const ENC: u32 = 1;
    /// `VE_MODE_DE`.
    pub const DE: u32 = 2;
    /// `VE_MODE_VCUENC`.
    pub const VCU_ENC: u32 = 3;
    /// `VE_MODE_VCUDEC`.
    pub const VCU_DEC: u32 = 4;
}

/// The register window's size in bytes: the first `reg` entry of the engine's device-tree node.
/// `mmap` of the node maps the window's physical start for as many bytes as asked, whatever the
/// offset, so a mapping longer than this reaches the registers of whatever follows it. A mapping
/// is always exactly this long, at offset 0.
pub const WINDOW_BYTES: usize = 0x1000;

/// `ve_top_reg_offset` of this engine: where its top-level registers start in the window.
pub const TOP_REG_OFFSET: usize = 0x800;

/// The interrupt the encoder raises is read by the kernel at `0xb00 + 0x1c` (`VE_REG_ENC_BASE`),
/// enabled at `0xb00 + 0x14`; the kernel's handler clears the enable bits when it fires.
pub const ENC_INT_STATUS: usize = 0xb00 + 0x1c;

const _: () = assert!(ENC_INT_STATUS + 4 <= WINDOW_BYTES && TOP_REG_OFFSET < WINDOW_BYTES);

/// `struct dma_buf_param`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmaBufParam {
    /// The dma-buf's descriptor, in.
    pub fd: i32,
    /// Its bus address, out: the first scatterlist entry's DMA address, cut to 32 bits.
    pub phy_addr: u32,
}

/// `struct user_iommu_param`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserIommuParam {
    /// The dma-buf's descriptor.
    pub fd: i32,
    /// Its bus address.
    pub iommu_addr: u32,
}

/// `struct cedarv_env_infomation_compat`, what `IOCTL_GET_ENV_INFO` writes (all zero on this
/// kernel).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnvInformation {
    /// `phymem_start`.
    pub phymem_start: u32,
    /// `phymem_total_size`.
    pub phymem_total_size: i32,
    /// `address_macc`.
    pub address_macc: u64,
    /// `from_kernel`.
    pub from_kernel: u32,
}

/// `struct cache_range`: user virtual addresses.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheRange {
    /// First byte.
    pub start: u64,
    /// One past the last.
    pub end: u64,
}

/// `struct ve_case_load_param`, `IOCTL_UPDATE_CASE_LOAD_PARAM`'s frequency hint.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaseLoadParam {
    /// `width`.
    pub width: i32,
    /// `height`.
    pub height: i32,
    /// `frame_rate`.
    pub frame_rate: i32,
    /// `codec_format`.
    pub codec_format: i32,
    /// `is_remove_cur_param`.
    pub is_remove_cur_param: i32,
    /// `thread_channel_id`.
    pub thread_channel_id: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn the_layouts_match_the_c_compiler() {
        assert_eq!(size_of::<DmaBufParam>(), 8);
        assert_eq!(offset_of!(DmaBufParam, phy_addr), 4);
        assert_eq!(size_of::<UserIommuParam>(), 8);
        assert_eq!(offset_of!(UserIommuParam, iommu_addr), 4);
        assert_eq!(size_of::<EnvInformation>(), 24);
        assert_eq!(align_of::<EnvInformation>(), 8);
        assert_eq!(offset_of!(EnvInformation, address_macc), 8);
        assert_eq!(offset_of!(EnvInformation, from_kernel), 16);
        assert_eq!(size_of::<CacheRange>(), 16);
        assert_eq!(offset_of!(CacheRange, end), 8);
        assert_eq!(size_of::<CaseLoadParam>(), 24);
        assert_eq!(offset_of!(CaseLoadParam, thread_channel_id), 20);
    }

    #[test]
    fn the_request_numbers_are_the_kernels() {
        assert_eq!((WAIT_VE_DE, WAIT_VE_EN, RESET_VE), (0x102, 0x103, 0x104));
        assert_eq!(
            (ENGINE_REQ, ENGINE_REL, SET_REFCOUNT),
            (0x206, 0x207, 0x20c)
        );
        assert_eq!((GET_LOCK, RELEASE_LOCK), (0x310, 0x311));
        assert_eq!(GET_IOMMU_ADDR, 0x502);
        assert_eq!(
            (MAP_DMA_BUF, UNMAP_DMA_BUF, FLUSH_CACHE_RANGE),
            (0x504, 0x505, 0x506)
        );
        assert_eq!((VE_MODE, WAIT_VCU_ENC), (0x800, 0x801));
        assert_eq!(
            (CLEAR_EN_INT_FLAG, GET_VE_TOP_REG_OFFSET, WAIT_VCU_DEC),
            (0x803, 0x804, 0x805)
        );
        assert_eq!((lock::VDEC, lock::VENC, lock::JDEC), (1, 2, 4));
        assert_eq!(
            (
                mode::ENCPP,
                mode::ENC,
                mode::DE,
                mode::VCU_ENC,
                mode::VCU_DEC
            ),
            (0, 1, 2, 3, 4)
        );
    }

    #[test]
    fn the_window_holds_every_register_block_the_kernel_reads() {
        assert_eq!(WINDOW_BYTES, 0x1000);
        assert_eq!(TOP_REG_OFFSET, 0x800);
    }
}
