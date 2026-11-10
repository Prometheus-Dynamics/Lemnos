//! The vendor kernel's `vvcam-mipi` misc device (`/dev/vvcam-mipi.N`): one request, carrying plain
//! numbers, after which the kernel sets up the CSI receiver, its D-PHY and the video input block
//! that feeds the ISP. Nothing in it is an address.
//!
//! Transcribed from `buildroot-overlay/package/vvcam/mipi/vvcam_mipi.h` of the Kendryte K230 Linux
//! SDK (`github.com/kendryte/k230_linux_sdk` at `fc6cc2151d54cb307be47f81815023f7ffb0eae6`;
//! VeriSilicon, dual MIT and GPL, used under MIT). What the kernel reads from the structure is
//! from the same directory's `vvcam_mipi_driver.c` (`convert_to_vi_attr`, `vvcam_mipi_ioctl`):
//!
//! - the lanes are the count of `lane_id` entries that are not `-1`: one, two, or four for any
//!   other count;
//! - `itf_cfg[0].data_type`: [`DATA_RAW8`], [`DATA_RAW10`], [`DATA_RAW12`] (and RAW16 and 8-bit
//!   YUV 4:2:2, not offered here); any other value is logged and the default RAW10 kept, so the
//!   builder refuses them;
//! - `phy_freq`: 800, 1200 or 1600 (Mbit/s per lane); any other value is logged and 800 used, so
//!   the builder refuses them;
//! - nothing else. The image rectangle and the line timings are not read; they are filled in for
//!   the record.
//!
//! Requests the device has and this crate does not issue: `VVCAM_MIPI_RESET` resets the CSI block
//! *and the sensor's reset line* (the sensor is its own driver's), and `VVCAM_MIPI_SET_STREAM`
//! does nothing in this kernel. Their numbers are kept for the layout test.

use core::mem::size_of;

const fn iow(nr: u8, size: usize) -> u32 {
    assert!(size < (1 << 14));
    (1 << 30) | ((size as u32) << 16) | ((MAGIC as u32) << 8) | nr as u32
}

/// `VVCAM_MIPI_IOC_MAGIC`.
pub const MAGIC: u8 = b'v';
/// `MIPI_LANE_NUM_MAX`.
pub const LANES_MAX: usize = 8;
/// `MIPI_OUTPUT_INTERFACES_MAX`.
pub const INTERFACES_MAX: usize = 8;

/// `VVCAM_INPUT_MODE_MIPI`.
pub const MODE_MIPI: u32 = 0;
/// `VVCAM_RAW_8BIT`.
pub const DATA_RAW8: u32 = 0;
/// `VVCAM_RAW_10BIT`.
pub const DATA_RAW10: u32 = 1;
/// `VVCAM_RAW_12BIT`.
pub const DATA_RAW12: u32 = 2;
/// The D-PHY rates the kernel takes, Mbit/s per lane.
pub const PHY_RATES_MBPS: [u32; 3] = [800, 1200, 1600];

/// `vvcam_image_rect_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// `left`.
    pub left: u32,
    /// `top`.
    pub top: u32,
    /// `width`.
    pub width: u32,
    /// `height`.
    pub height: u32,
}

/// `vvcam_itf_horizontal_time_t` and `vvcam_itf_vertical_time_t`: four counts each.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    /// `hsa` / `vsa`.
    pub sync: u32,
    /// `hbp` / `vbp`.
    pub back_porch: u32,
    /// `hfp` / `vfp`.
    pub front_porch: u32,
    /// `hline` / `vactivate_lines`.
    pub total: u32,
}

/// `vvcam_mipi_output_itf_t` (48 bytes): the two `bool`s, two bytes of padding, then the enums
/// (each an `int`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    /// `enable` (a C `bool`: 0 or 1).
    pub enable: u8,
    /// `embedded_data_en` (a C `bool`).
    pub embedded_data: u8,
    /// Padding before `vcid`.
    pub reserved: [u8; 2],
    /// `vcid`: the CSI-2 virtual channel.
    pub vcid: u32,
    /// `bit_mode`: 0 for 48 bits.
    pub bit_mode: u32,
    /// `data_type`: [`DATA_RAW8`], [`DATA_RAW10`] or [`DATA_RAW12`].
    pub data_type: u32,
    /// `h_time`.
    pub h_time: Timing,
    /// `v_time`.
    pub v_time: Timing,
}

/// `vvcam_mipi_dev_attr_t` (404 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MipiAttr {
    /// `itf_cfg`.
    pub interfaces: [Interface; INTERFACES_MAX],
    /// `lane_id`: a lane's id, or -1 for a lane not in use.
    pub lane_id: [i16; LANES_MAX],
    /// `phy_freq`, Mbit/s per lane: one of [`PHY_RATES_MBPS`].
    pub phy_freq: u32,
}

/// `vvcam_input_dev_attr_t` (424 bytes). Its union's other member (`dvp_attr`, 16 bytes) is
/// smaller than [`MipiAttr`] and is not used.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DevAttr {
    /// `mode`: [`MODE_MIPI`].
    pub mode: u32,
    /// `image_rect`.
    pub image_rect: Rect,
    /// `mipi_attr`.
    pub mipi: MipiAttr,
}

/// Why [`DevAttr::csi`] refused a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Lanes other than 1, 2 or 4.
    Lanes,
    /// A virtual channel past 3.
    VirtualChannel,
    /// A bit depth other than 8, 10 or 12.
    Depth,
    /// A rate past 1600 Mbit/s per lane, or 0.
    Rate,
}

impl DevAttr {
    /// The request for a raw CSI-2 link of `lanes` data lanes on `virtual_channel`, at `depth`
    /// bits, `rate_mbps` per lane and `width` x `height`. The D-PHY is set to the lowest rate the
    /// kernel takes that is at least `rate_mbps`.
    pub fn csi(
        lanes: u8,
        virtual_channel: u8,
        depth: u8,
        rate_mbps: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, LinkError> {
        if !matches!(lanes, 1 | 2 | 4) {
            return Err(LinkError::Lanes);
        }
        if virtual_channel > 3 {
            return Err(LinkError::VirtualChannel);
        }
        let data_type = match depth {
            8 => DATA_RAW8,
            10 => DATA_RAW10,
            12 => DATA_RAW12,
            _ => return Err(LinkError::Depth),
        };
        if rate_mbps == 0 {
            return Err(LinkError::Rate);
        }
        let phy_freq = PHY_RATES_MBPS
            .into_iter()
            .find(|&r| r >= rate_mbps)
            .ok_or(LinkError::Rate)?;
        let mut lane_id = [-1i16; LANES_MAX];
        for (id, lane) in (0i16..).zip(lane_id.iter_mut().take(usize::from(lanes))) {
            *lane = id;
        }
        let mut interfaces = [Interface::default(); INTERFACES_MAX];
        interfaces[0] = Interface {
            enable: 1,
            vcid: u32::from(virtual_channel),
            data_type,
            v_time: Timing {
                total: height,
                ..Timing::default()
            },
            ..Interface::default()
        };
        Ok(Self {
            mode: MODE_MIPI,
            image_rect: Rect {
                left: 0,
                top: 0,
                width,
                height,
            },
            mipi: MipiAttr {
                interfaces,
                lane_id,
                phy_freq,
            },
        })
    }
}

/// `VVCAM_MIPI_RESET`: `_IOW('v', 0x01, unsigned int)`. Not issued.
pub const RESET: u32 = iow(0x01, size_of::<u32>());
/// `VVCAM_MIPI_SET_DEV_ATTR`: `_IOW('v', 0x02, vvcam_input_dev_attr_t)`.
pub const SET_DEV_ATTR: u32 = iow(0x02, size_of::<DevAttr>());
/// `VVCAM_MIPI_SET_STREAM`: `_IOW('v', 0x03, unsigned int)`. Not issued.
pub const SET_STREAM: u32 = iow(0x03, size_of::<u32>());
