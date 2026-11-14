//! The chip: its registers, the mode table and the one path that writes a register.
//!
//! Provenance: the register addresses, values, modes and control arithmetic here are interface
//! facts about the sensor. They were transcribed from the reference driver data on the `dev` branch
//! (`kernel-modules/lemnos-ov9782`, a table of facts only) and checked against the chip's register
//! map. The code is this crate's own. Where this file differs from the `dev` tables on purpose it
//! says so at the row concerned.

use lemnos_contract_camera_sensor::{
    BayerOrder, EmbeddedData, GainLaw, GainRange, LinkConfig, Mode, Rect, SensorInfo, fps_milli_of,
};
use lemnos_core::{Result, Status, status_of};
use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::I2c;

/// The 7-bit I2C address of the sensor at its default SCCB id.
pub const DEFAULT_ADDRESS: u8 = 0x60;

/// Starts programming the OTP memory, which cannot be undone. Never written by this driver.
pub const OTP_PROGRAM: u16 = 0x3d80;

/// Chip id: two bytes, most significant first.
pub const CHIP_ID: u16 = 0x300a;
/// The chip ids accepted. The first is the part's own id; the second is the id the reference
/// driver also accepts for this part, and the one the register map lists as the reset value.
pub const CHIP_IDS: [u16; 2] = [0x9782, 0x9281];

pub const MODE_SELECT: u16 = 0x0100;
pub const STANDBY: u8 = 0x00;
pub const STREAMING: u8 = 0x01;

pub const PLL2_MULTIPLIER: u16 = 0x030d;
pub const ANA_CORE_2: u16 = 0x3662;

/// Exposure: 20 bits across three registers, in sixteenths of a line.
pub const EXPOSURE: u16 = 0x3500;
pub const ANALOGUE_GAIN: u16 = 0x3509;

/// The group access register. Writes between `GROUP_HOLD_START` and `GROUP_HOLD_END` are captured,
/// and take effect together on `GROUP_LAUNCH`.
pub const GROUP_ACCESS: u16 = 0x3208;
/// Group 0: start capturing.
pub const GROUP_HOLD_START: u8 = 0x00;
/// Group 0: stop capturing.
pub const GROUP_HOLD_END: u8 = 0x10;
/// Group 0: apply the group at the next vertical blanking.
pub const GROUP_LAUNCH: u8 = 0xa0;

/// Line length register, in two pixel clocks per step.
pub const LINE_LENGTH: u16 = 0x380c;
/// Frame length register, in lines.
pub const FRAME_LENGTH: u16 = 0x380e;

pub const FORMAT1: u16 = 0x3820;
pub const FORMAT2: u16 = 0x3821;
/// The flip (vertical) and mirror (horizontal) bit in `FORMAT1` and `FORMAT2`.
pub const FLIP_BIT: u8 = 0x04;

/// Digital binning control.
pub const DIGITAL_BINNING: u16 = 0x3837;

/// Number of frames the frame-control block masks after streaming starts.
pub const FRAME_OFF_COUNT: u16 = 0x4242;
/// Frames masked at the sensor on each start.
pub const FIRST_FRAMES_MASKED: u8 = 1;

pub const MIPI_CTRL00: u16 = 0x4800;
/// Gate the clock lane between lines.
pub const GATED_CLOCK: u8 = 0x20;

/// Test pattern control.
pub const TEST_PATTERN: u16 = 0x5e00;
/// `TEST_PATTERN` values; code `n` is `TEST_PATTERNS[n - 1]`. Bit 7 enables, bits 3..2 are the bar
/// style and bits 1..0 the kind.
pub const TEST_PATTERNS: [u8; 7] = [
    0x80, // Colour bars.
    0x84, // Colour bars darkening top to bottom.
    0x88, // Colour bars darkening right to left.
    0x8c, // Colour bars darkening bottom to top.
    0x81, // Random data.
    0x82, // Squares.
    0x83, // Black.
];

/// Lines of the frame an exposure may not reach.
pub const EXPOSURE_MARGIN: u32 = 25;
/// Bits of fraction in an exposure.
pub const EXPOSURE_FRACTION_BITS: u8 = 4;
/// Shortest exposure: one line.
pub const EXPOSURE_MIN: u32 = 1 << EXPOSURE_FRACTION_BITS;
/// Analogue gain codes, with the low four bits the fraction: `0x10` is a gain of one, so the code
/// is sixteenths of a gain and the law is linear: gain = code / 0x10. Register `0x3509` takes one
/// byte, so the codes run to `0xff` (just under sixteen). Written in the register's own units; the
/// same table gives the reference settings' gains.
pub const ANALOGUE_GAIN_RANGE: GainRange = GainRange {
    min: 0x10,
    max: 0xff,
    unity: 0x10,
    step: 1,
    law: GainLaw::Linear,
};

/// The OV9782 has no embedded data lines in its mode table: no register in this file turns them
/// on, and no frame layout is described. A frame carries no register values.
pub const EMBEDDED_DATA: EmbeddedData = EmbeddedData::None;
/// Exposure in lines after `select_mode`, from the reference settings.
pub const DEFAULT_EXPOSURE_LINES: u32 = 642;
/// Longest frame beyond the height, in lines.
const VBLANK_MAX: u32 = 51_540;
/// Frame length beyond the height after `select_mode`, in lines.
pub const DEFAULT_VBLANK: u32 = 1022;

/// The pixel array: active pixels plus eight dummy rows and columns on each edge.
pub const ARRAY_WIDTH: u32 = 1296;
pub const ARRAY_HEIGHT: u32 = 816;

const LANES: u8 = 2;
const LINK_RATE_MBPS: u32 = 800;
/// Line length register values: the register's unit is one system clock.
const LINE_LENGTH_CONTINUOUS: u32 = 728;
const LINE_LENGTH_GATED: u32 = 765;

/// The colour order at the top-left of every mode's window. With no flips this is the
/// colour-filter layout at the (8, 8) window origin. It is taken to hold under mirror and flip
/// too, because `0x5e10` bits 5:4 default to on (first pixel kept in its unflipped position);
/// untested on board.
pub const BAYER: BayerOrder = BayerOrder::Bggr;

/// Everything `select_mode` chooses that depends on bit depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Depth {
    pub bit_depth: u8,
    /// CSI-2 data type code.
    data_type: u8,
    /// PLL2 multiplier: the system clock is `24 MHz / 3 * multiplier / 8`.
    pub pll2_multiplier: u8,
    pub ana_core_2: u8,
    sys_clk_hz: u64,
    /// The link's pixel rate: lane rate × lanes / bits per pixel.
    pixel_rate_hz: u64,
    black_level: u16,
}

/// 10-bit: 80 MHz system clock.
const RAW10: Depth = Depth {
    bit_depth: 10,
    data_type: 0x2b,
    pll2_multiplier: 0x50,
    ana_core_2: 0x05,
    sys_clk_hz: 80_000_000,
    pixel_rate_hz: 160_000_000,
    black_level: 64,
};

/// 8-bit: 96 MHz system clock.
const RAW8: Depth = Depth {
    bit_depth: 8,
    data_type: 0x2a,
    pll2_multiplier: 0x60,
    ana_core_2: 0x07,
    sys_clk_hz: 96_000_000,
    pixel_rate_hz: 200_000_000,
    black_level: 16,
};

/// A read window of the array, and the registers that make it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub width: u16,
    pub height: u16,
    /// Array pixels combined into one output pixel across and down: 1 or 2.
    pub binning: u8,
    /// Array start and end column and row (inclusive), including the dummy rows and columns.
    pub array: [u16; 4],
    /// Window offset into the array read, in output pixels.
    pub offset: (u16, u16),
    pub x_inc: u8,
    pub y_inc: u8,
    /// `FORMAT1` with its flip bit clear.
    pub format1: u8,
    /// `FORMAT2` with its mirror bit clear.
    pub format2: u8,
    /// `DIGITAL_BINNING`.
    pub digital_binning: u8,
    /// Analogue control register 0x3778.
    pub analog: u8,
    pub blc: Blc,
    /// Vertical blanking the mode needs, in lines.
    pub vblank_min: u32,
}

/// Black-level calibration register values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blc {
    pub target: u8,
    pub up: (u8, u8),
    pub down: (u8, u8),
    pub read_4507: u8,
    pub read_4509: u8,
}

/// Registers written the same way in every mode, in the order the reference settings write them.
pub const COMMON: &[(u16, u8)] = &[
    (0x0302, 0x32),
    (0x030e, 0x02),
    (0x3001, 0x00),
    (0x3004, 0x00),
    (0x3005, 0x00),
    (0x3006, 0x04),
    (0x3011, 0x0a),
    (0x3013, 0x18),
    (0x301c, 0xf0),
    (0x3022, 0x01),
    (0x3030, 0x10),
    (0x3039, 0x32),
    (0x303a, 0x00),
    (0x3503, 0x08),
    (0x3505, 0x8c),
    (0x3507, 0x03),
    (0x3508, 0x00),
    (0x3610, 0x80),
    (0x3611, 0xa0),
    (0x3620, 0x6e),
    (0x3632, 0x56),
    (0x3633, 0x78),
    (0x3666, 0x00),
    (0x366f, 0x5a),
    (0x3680, 0x84),
    (0x3712, 0x80),
    (0x372d, 0x22),
    (0x3731, 0x80),
    (0x3732, 0x30),
    (0x377d, 0x22),
    (0x3788, 0x02),
    (0x3789, 0xa4),
    (0x378a, 0x00),
    (0x378b, 0x4a),
    (0x3799, 0x20),
    (0x3881, 0x42),
    (0x38a8, 0x02),
    (0x38a9, 0x80),
    (0x38b1, 0x00),
    (0x38c4, 0x00),
    (0x38c5, 0xc0),
    (0x38c6, 0x04),
    (0x38c7, 0x80),
    (0x3920, 0xff),
    (0x4010, 0x40),
    (0x4043, 0x40),
    (0x4307, 0x30),
    (0x4317, 0x00),
    (0x4501, 0x00),
    (0x450a, 0x08),
    (0x4601, 0x04),
    (0x470f, 0x00),
    (0x4f07, 0x00),
    (0x5000, 0x9f),
    (0x5001, 0x00),
    (0x5d00, 0x07),
    (0x5d01, 0x00),
    (0x0101, 0x01),
    (0x1000, 0x03),
    (0x5a08, 0x84),
];

/// 1280x800: the whole active array.
const WINDOW_800: Window = Window {
    width: 1280,
    height: 800,
    binning: 1,
    array: [0x000, 0x000, 0x50f, 0x32f],
    offset: (8, 8),
    x_inc: 0x11,
    y_inc: 0x11,
    format1: 0x40,
    format2: 0x00,
    digital_binning: 0x00,
    analog: 0x00,
    blc: Blc {
        target: 0x40,
        up: (0x04, 0x0b),
        down: (0x00, 0x07),
        read_4507: 0x00,
        read_4509: 0x00,
    },
    vblank_min: 110,
};

/// 1280x720: the array crop is the top 720 lines of the active array, not a centred crop.
const WINDOW_720: Window = Window {
    width: 1280,
    height: 720,
    binning: 1,
    array: [0x000, 0x000, 0x50f, 0x2df],
    offset: (8, 8),
    x_inc: 0x11,
    y_inc: 0x11,
    // The reference table has the flip and mirror bits set here (0x3c, 0x84). They are controlled
    // through `set_flips`, so the table keeps them clear.
    format1: 0x38,
    format2: 0x80,
    digital_binning: 0x00,
    analog: 0x00,
    blc: Blc {
        target: 0x40,
        up: (0x02, 0x05),
        down: (0x00, 0x03),
        read_4507: 0x00,
        read_4509: 0x80,
    },
    vblank_min: 41,
};

/// 640x400: 2x2 analogue binning of the whole active array.
///
/// Differs from the reference 640x400 table on purpose, which horizontally binned and skipped
/// lines with an increment that keeps only same-colour rows on a colour filter: the row increment
/// is `0x31` (not `0x22`), vertical binning is on (`format1` bit 0) and the digital binning
/// control is set. It also writes the black-level target, which the reference table left at reset.
const WINDOW_400: Window = Window {
    width: 640,
    height: 400,
    binning: 2,
    array: [0x000, 0x000, 0x50f, 0x32f],
    offset: (4, 4),
    x_inc: 0x31,
    y_inc: 0x31,
    format1: 0x61,
    format2: 0x01,
    digital_binning: 0x0c,
    analog: 0x10,
    blc: Blc {
        target: 0x40,
        up: (0x02, 0x05),
        down: (0x00, 0x03),
        read_4507: 0x03,
        read_4509: 0x80,
    },
    vblank_min: 22,
};

/// One way the sensor can run: a window at a bit depth. Mode `n` is `MODES[n]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeEntry {
    pub window: Window,
    pub depth: Depth,
}

/// The modes, in the order `select_mode` numbers them.
pub const MODES: [ModeEntry; 6] = [
    ModeEntry {
        window: WINDOW_800,
        depth: RAW10,
    },
    ModeEntry {
        window: WINDOW_800,
        depth: RAW8,
    },
    ModeEntry {
        window: WINDOW_720,
        depth: RAW10,
    },
    ModeEntry {
        window: WINDOW_720,
        depth: RAW8,
    },
    ModeEntry {
        window: WINDOW_400,
        depth: RAW10,
    },
    ModeEntry {
        window: WINDOW_400,
        depth: RAW8,
    },
];

/// The global line, `G`, in system clocks: the registers 0x382C/D (`hts_global_tx`) at their reset value
/// 0x05B0. The sensor spends one line of this length in every frame: its frame period is `(VTS - 1) x HTS
/// + G` system clocks, not `VTS x HTS`. The driver never writes 0x382C/D (no table or control touches them),
/// so `G` is this reset default; a driver that programmed another value would pass it to
/// [`ModeEntry::describe_with_global_line`]. Measured, not read from the datasheet: the frame period at
/// seven frame lengths on a board, and a write of 0x382C/D from 1456 to 1536 that moved the period by 80
/// system clocks (testing/board-runs/F2h3.md). The sensor uses it whatever bit 2 of 0x382E reads (0 on that
/// board), so nothing here depends on that bit. In the continuous-clock mode HTS is 728 and `G` is twice it:
/// the frame is `VTS + 1` lines of HTS.
pub const GLOBAL_LINE_DEFAULT: u64 = 1456;

/// The line length register for this clock mode.
pub const fn line_length(continuous_clock: bool) -> u32 {
    if continuous_clock {
        LINE_LENGTH_CONTINUOUS
    } else {
        LINE_LENGTH_GATED
    }
}

impl ModeEntry {
    /// Shortest frame in lines.
    pub const fn frame_length_min(&self) -> u32 {
        self.window.height as u32 + self.window.vblank_min
    }

    /// Longest frame in lines.
    pub const fn frame_length_max(&self) -> u32 {
        self.window.height as u32 + VBLANK_MAX
    }

    /// Frame length after `select_mode`.
    pub const fn frame_length_default(&self) -> u32 {
        self.window.height as u32 + DEFAULT_VBLANK
    }

    /// The part of the array this mode reads.
    pub const fn crop(&self) -> Rect {
        let w = self.window;
        let binning = w.binning as u32;
        Rect {
            x: w.array[0] as u32 + w.offset.0 as u32 * binning,
            y: w.array[1] as u32 + w.offset.1 as u32 * binning,
            width: w.width as u32 * binning,
            height: w.height as u32 * binning,
        }
    }

    /// The whole description of the mode, in the contract's units, with the global line at its reset
    /// value ([`GLOBAL_LINE_DEFAULT`]).
    pub fn describe(&self, continuous_clock: bool) -> Mode {
        self.describe_with_global_line(continuous_clock, GLOBAL_LINE_DEFAULT)
    }

    /// The description of the mode with the global line `global_line` system clocks long (what is in
    /// registers 0x382C/D). The frame period is `(N - 1) x HTS + G` system clocks, which the contract
    /// states as `N x line_time_ps + frame_offset_ps` with the offset `G - HTS` system clocks in
    /// picoseconds; `max_fps_milli` is the rate at the shortest frame, on that same period.
    pub fn describe_with_global_line(&self, continuous_clock: bool, global_line: u64) -> Mode {
        let line_length_reg = u64::from(line_length(continuous_clock));
        let line_time_ps = (line_length_reg * 1_000_000_000_000 / self.depth.sys_clk_hz) as u32;
        // Exact: the line length times the active lines, over the clock, before any rounding.
        let readout_ps = line_length_reg * u64::from(self.window.height) * 1_000_000_000_000
            / self.depth.sys_clk_hz;
        let frame_length_min = self.frame_length_min();
        // `G` less one line of HTS, in picoseconds (negative when `G` is shorter than a line: the
        // contract's offset is unsigned, so such a frame is reported with no offset).
        let frame_offset_ps =
            global_line.saturating_sub(line_length_reg) * 1_000_000_000_000 / self.depth.sys_clk_hz;
        Mode {
            link: LinkConfig {
                lanes: LANES,
                virtual_channel: 0,
                data_type: self.depth.data_type,
                bit_depth: self.depth.bit_depth,
                continuous_clock,
                width: self.window.width.into(),
                height: self.window.height.into(),
                pixel_rate_hz: self.depth.pixel_rate_hz,
                link_rate_mbps: LINK_RATE_MBPS,
            },
            bayer: BAYER,
            max_fps_milli: fps_milli_of(line_time_ps, frame_offset_ps, frame_length_min)
                .unwrap_or(0),
            line_time_ps,
            line_length: (line_length_reg * 2) as u32,
            frame_length_min,
            frame_length_max: self.frame_length_max(),
            exposure_min: EXPOSURE_MIN,
            exposure_margin: EXPOSURE_MARGIN,
            exposure_fraction_bits: EXPOSURE_FRACTION_BITS,
            analogue_gain: ANALOGUE_GAIN_RANGE,
            digital_gain: GainRange::default(),
            crop: self.crop(),
            binning_x: self.window.binning,
            binning_y: self.window.binning,
            black_level: self.depth.black_level,
            drop_frames: 0,
            // One group carries all three, so they land on one frame: a group whose launch completes
            // in frame F is latched at the start of F + 1 and shows on F + 2 (testing/board-runs/D10.md).
            exposure_delay: 2,
            gain_delay: 2,
            frame_length_delay: 2,
            readout_ps,
            frame_offset_ps,
        }
    }
}

/// The sensor as a whole.
pub fn sensor_info() -> SensorInfo {
    SensorInfo {
        modes: MODES.len() as u16,
        array_width: ARRAY_WIDTH,
        array_height: ARRAY_HEIGHT,
        test_patterns: TEST_PATTERNS.len() as u16,
        embedded_data: EMBEDDED_DATA,
    }
}

/// The largest exposure `frame_length` allows.
pub const fn exposure_max(frame_length: u32) -> u32 {
    (frame_length.saturating_sub(EXPOSURE_MARGIN)) << EXPOSURE_FRACTION_BITS
}

/// The one way to write a register: 16-bit address, one data byte, one transaction. Refuses
/// [`OTP_PROGRAM`] so no sequence, table or future caller can reach it.
pub fn write<B: I2c<Error = ErrorKind>>(
    bus: &mut B,
    address: u8,
    register: u16,
    value: u8,
) -> Result<()> {
    if register == OTP_PROGRAM {
        return Err(Status::NOT_PERMITTED);
    }
    let [high, low] = register.to_be_bytes();
    bus.write(address, &[high, low, value]).map_err(status_of)
}

/// Reads two consecutive registers, most significant first.
pub fn read_u16<B: I2c<Error = ErrorKind>>(bus: &mut B, address: u8, register: u16) -> Result<u16> {
    let mut value = [0; 2];
    bus.write_read(address, &register.to_be_bytes(), &mut value)
        .map_err(status_of)?;
    Ok(u16::from_be_bytes(value))
}
