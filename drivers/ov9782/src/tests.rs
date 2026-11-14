//! Register sequences against a mocked sensor bus: every expected value here is written out by
//! hand from the chip's facts, not computed from the tables under test.

use super::*;
use chip::{CHIP_ID, DEFAULT_ADDRESS, OTP_PROGRAM};
use lemnos_contract_camera_receiver::{CameraReceiver, FrameStarts};
use lemnos_contract_camera_sensor::{
    AppliedLanding, BayerOrder, CLAMP_ANALOGUE_GAIN, CLAMP_EXPOSURE, CLAMP_FRAME_LENGTH,
    EmbeddedData, GainLaw, GainRange, LinkConfig, Rect,
};
use lemnos_core::{Config, Contract, Dev, Driver, Health, I2c, Lifecycle, Result, Status};
use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_hal::mock::{MockI2c, MockOp};
use lemnos_hal::register::AddressWidth;

extern crate alloc;
extern crate std;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

mod silence;

const ADDR: u8 = DEFAULT_ADDRESS;

/// Health of a sensor bound with no link: nothing to confirm, so degraded, and healthy otherwise.
pub(crate) const NO_RECEIVER: &str = "the board names no receiver for this sensor";

/// A bus that can fail its Nth transaction from now, and only that one.
#[derive(Clone)]
struct Bus {
    mock: MockI2c,
    /// Counts down on each transaction; a transaction seeing 1 fails. 0 is off.
    fail_at: Arc<AtomicI64>,
    /// Fail every transaction.
    down: Arc<std::sync::atomic::AtomicBool>,
    /// The time the simulated chip is in SCCB, against `answers_from`: every transaction before
    /// that time is refused (the chip is still powering up).
    clock: Arc<std::sync::atomic::AtomicU64>,
    answers_from: Arc<std::sync::atomic::AtomicU64>,
}

impl ErrorType for Bus {
    type Error = ErrorKind;
}

impl HalI2c for Bus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        if self.down.load(Ordering::SeqCst)
            || self.clock.load(Ordering::SeqCst) < self.answers_from.load(Ordering::SeqCst)
        {
            return Err(ErrorKind::Nack);
        }
        let left = self.fail_at.load(Ordering::SeqCst);
        if left > 0 {
            self.fail_at.store(left - 1, Ordering::SeqCst);
            if left == 1 {
                return Err(ErrorKind::Nack);
            }
        }
        self.mock.transaction(address, operations)
    }
}

fn bus_with_id(id: u16) -> Bus {
    Bus {
        mock: MockI2c::new()
            .with_target(ADDR, AddressWidth::Bits16)
            .with_registers(ADDR, CHIP_ID, &id.to_be_bytes()),
        fail_at: Arc::new(AtomicI64::new(0)),
        down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        clock: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        answers_from: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    }
}

/// Starts a sensor and runs its power-up: the start itself touches nothing, the first tick is the
/// power-up time, and the next tick reads the chip and starts it.
pub(crate) fn start_up(s: &mut Ov9782) {
    s.start().unwrap();
    if matches!(s.state, State::PowerUp { .. }) {
        assert_eq!(Lifecycle::tick(s, 0), Some(POWER_UP_NS));
        // A values ring makes the streaming tick return its poll; the state is what matters here.
        Lifecycle::tick(s, POWER_UP_NS);
    }
    assert_eq!(s.state, State::Streaming);
}

#[test]
fn check_config_accepts_a_valid_config_and_names_a_bad_key() {
    let ok = [
        ("address", ConfigValue::Int(0x36)),
        ("continuous_clock", ConfigValue::Bool(true)),
    ];
    assert!(<Ov9782 as Lifecycle>::check_config(&Config::new(&ok)).is_ok());
    let bogus = [("bogus", ConfigValue::Int(1))];
    let err = <Ov9782 as Lifecycle>::check_config(&Config::new(&bogus)).unwrap_err();
    assert_eq!(err.key, "bogus");
    let near = [("addres", ConfigValue::Int(0x36))];
    let err = <Ov9782 as Lifecycle>::check_config(&Config::new(&near)).unwrap_err();
    assert_eq!(err.suggestion, Some("address"));
}

#[test]
fn check_config_refuses_a_wrong_kind_and_an_address_out_of_range() {
    fn check<'a>(
        entries: &'a [(&'a str, ConfigValue<'a>)],
    ) -> core::result::Result<(), ConfigError<'a>> {
        <Ov9782 as Lifecycle>::check_config(&Config::new(entries))
    }
    assert_eq!(
        check(&[("address", ConfigValue::Str("0x36"))])
            .unwrap_err()
            .key,
        "address"
    );
    assert_eq!(
        check(&[("continuous_clock", ConfigValue::Int(1))])
            .unwrap_err()
            .key,
        "continuous_clock"
    );
    // The edges of the 7-bit range that is not reserved pass; one step outside does not.
    for address in [0x08, 0x77] {
        assert!(check(&[("address", ConfigValue::Int(address))]).is_ok());
    }
    for address in [0x07, 0x78] {
        assert_eq!(
            check(&[("address", ConfigValue::Int(address))])
                .unwrap_err()
                .key,
            "address"
        );
    }
}

fn sensor_on(bus: &Bus, entries: &[(&str, ConfigValue<'_>)]) -> Ov9782 {
    <Ov9782 as Lifecycle>::start(
        Ov9782Needs {
            bus: I2c(lemnos_core::Held::boxed(Box::new(bus.clone()))),
            values: None,
            _receiver: None,
        },
        &Config::new(entries),
    )
    .unwrap()
}

fn sensor() -> (Ov9782, Bus) {
    let bus = bus_with_id(0x9782);
    (sensor_on(&bus, &[]), bus)
}

/// Single-register writes the bus has seen, in order. Fails the test on any burst write.
fn writes(bus: &Bus) -> Vec<(u16, u8)> {
    let mut out = Vec::new();
    for transfer in bus.mock.transfers() {
        let is_read = transfer.ops.iter().any(|op| matches!(op, MockOp::Read(_)));
        if is_read {
            continue;
        }
        for op in &transfer.ops {
            let MockOp::Write(bytes) = op else {
                panic!("unexpected op {op:?}");
            };
            assert_eq!(bytes.len(), 3, "every register write is one register");
            out.push((u16::from_be_bytes([bytes[0], bytes[1]]), bytes[2]));
        }
    }
    out
}

fn clear(bus: &Bus) {
    bus.mock.clear_log();
}

/// Common registers, as the sensor must receive them.
const COMMON_EXPECTED: &[(u16, u8)] = &[
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

/// `0x0100=0`, `0x4800`, then the common registers.
fn start_prefix(mipi: u8) -> Vec<(u16, u8)> {
    let mut out = vec![(0x0100, 0x00), (0x4800, mipi)];
    out.extend_from_slice(COMMON_EXPECTED);
    out
}

#[test]
fn full_start_sequence_1280x800_raw10_gated_clock() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);

    let mut expected = start_prefix(0x20);
    expected.extend_from_slice(&[
        // PLL for RAW10.
        (0x030d, 0x50),
        (0x3662, 0x05),
        (0x3778, 0x00),
        (0x3800, 0x00),
        (0x3801, 0x00),
        (0x3802, 0x00),
        (0x3803, 0x00),
        (0x3804, 0x05),
        (0x3805, 0x0f),
        (0x3806, 0x03),
        (0x3807, 0x2f),
        (0x3808, 0x05),
        (0x3809, 0x00),
        (0x380a, 0x03),
        (0x380b, 0x20),
        (0x3810, 0x00),
        (0x3811, 0x08),
        (0x3812, 0x00),
        (0x3813, 0x08),
        (0x3814, 0x11),
        (0x3815, 0x11),
        (0x3820, 0x40),
        (0x3821, 0x00),
        (0x3837, 0x00),
        (0x4003, 0x40),
        (0x4008, 0x04),
        (0x4009, 0x0b),
        (0x400c, 0x00),
        (0x400d, 0x07),
        (0x4507, 0x00),
        (0x4509, 0x00),
        // Line length 765 (gated clock).
        (0x380c, 0x02),
        (0x380d, 0xfd),
        // Frame length 1822 (800 + 1022).
        (0x380e, 0x07),
        (0x380f, 0x1e),
        // Exposure 642 lines, in sixteenths.
        (0x3500, 0x00),
        (0x3501, 0x28),
        (0x3502, 0x20),
        (0x3509, 0x10),
        // First frame masked.
        (0x4242, 0x01),
        // No test pattern.
        (0x5e00, 0x00),
        (0x0100, 0x01),
    ]);
    assert_eq!(writes(&bus), expected);
}

#[test]
fn full_start_sequence_640x400_binned_raw8_continuous_clock() {
    let bus = bus_with_id(0x9782);
    let mut s = sensor_on(&bus, &[("continuous_clock", ConfigValue::Bool(true))]);
    s.select_mode(5).unwrap();
    start_up(&mut s);

    let mut expected = start_prefix(0x00);
    expected.extend_from_slice(&[
        // RAW8: system clock 96 MHz.
        (0x030d, 0x60),
        (0x3662, 0x07),
        (0x3778, 0x10),
        (0x3800, 0x00),
        (0x3801, 0x00),
        (0x3802, 0x00),
        (0x3803, 0x00),
        (0x3804, 0x05),
        (0x3805, 0x0f),
        (0x3806, 0x03),
        (0x3807, 0x2f),
        (0x3808, 0x02),
        (0x3809, 0x80),
        (0x380a, 0x01),
        (0x380b, 0x90),
        (0x3810, 0x00),
        (0x3811, 0x04),
        (0x3812, 0x00),
        (0x3813, 0x04),
        // 2x2 binning: both increments colour-preserving.
        (0x3814, 0x31),
        (0x3815, 0x31),
        // Vertical binning on, horizontal binning on, digital binning control set.
        (0x3820, 0x61),
        (0x3821, 0x01),
        (0x3837, 0x0c),
        (0x4003, 0x40),
        (0x4008, 0x02),
        (0x4009, 0x05),
        (0x400c, 0x00),
        (0x400d, 0x03),
        (0x4507, 0x03),
        (0x4509, 0x80),
        // Line length 728 (continuous clock).
        (0x380c, 0x02),
        (0x380d, 0xd8),
        // Frame length 1422.
        (0x380e, 0x05),
        (0x380f, 0x8e),
        (0x3500, 0x00),
        (0x3501, 0x28),
        (0x3502, 0x20),
        (0x3509, 0x10),
        (0x4242, 0x01),
        (0x5e00, 0x00),
        (0x0100, 0x01),
    ]);
    assert_eq!(writes(&bus), expected);
}

#[test]
fn full_start_sequence_1280x720_raw8_keeps_flip_bits_clear() {
    let (mut s, bus) = sensor();
    s.select_mode(3).unwrap();
    start_up(&mut s);
    let w = writes(&bus);
    // Array crop: rows 0..=735, window offset (8, 8), output 1280x720.
    let tail = [
        (0x030d, 0x60),
        (0x3662, 0x07),
        (0x3778, 0x00),
        (0x3800, 0x00),
        (0x3801, 0x00),
        (0x3802, 0x00),
        (0x3803, 0x00),
        (0x3804, 0x05),
        (0x3805, 0x0f),
        (0x3806, 0x02),
        (0x3807, 0xdf),
        (0x3808, 0x05),
        (0x3809, 0x00),
        (0x380a, 0x02),
        (0x380b, 0xd0),
        (0x3810, 0x00),
        (0x3811, 0x08),
        (0x3812, 0x00),
        (0x3813, 0x08),
        (0x3814, 0x11),
        (0x3815, 0x11),
        (0x3820, 0x38),
        (0x3821, 0x80),
        (0x3837, 0x00),
        (0x4003, 0x40),
        (0x4008, 0x02),
        (0x4009, 0x05),
        (0x400c, 0x00),
        (0x400d, 0x03),
        (0x4507, 0x00),
        (0x4509, 0x80),
    ];
    let at = 2 + COMMON_EXPECTED.len();
    assert_eq!(&w[at..at + tail.len()], &tail);
}

#[test]
fn every_raw8_mode_programs_pll_0x60_and_every_raw10_mode_0x50() {
    for (index, entry) in MODES.iter().enumerate() {
        let (mut s, bus) = sensor();
        s.select_mode(index as u16).unwrap();
        start_up(&mut s);
        let w = writes(&bus);
        let pll = w.iter().find(|(r, _)| *r == 0x030d).unwrap().1;
        let expected = match entry.depth.bit_depth {
            8 => 0x60,
            10 => 0x50,
            other => panic!("unexpected depth {other}"),
        };
        assert_eq!(pll, expected, "mode {index}");
    }
}

#[test]
fn start_refuses_a_wrong_chip_and_writes_nothing() {
    let bus = bus_with_id(0x1234);
    let mut s = sensor_on(&bus, &[]);
    s.select_mode(0).unwrap();
    // The start answers; the chip is read at the first tick after the power-up time.
    assert_eq!(s.start(), Ok(()));
    assert_eq!(writes(&bus), vec![]);
    assert_eq!(Lifecycle::tick(&mut s, 0), Some(POWER_UP_NS));
    assert_eq!(
        Lifecycle::tick(&mut s, POWER_UP_NS),
        Some(POWER_UP_NS + 250_000_000)
    );
    assert!(matches!(s.state, State::Recovering { .. }));
    assert_eq!(writes(&bus), vec![]);
}

#[test]
fn start_accepts_both_chip_ids() {
    for id in [0x9782, 0x9281] {
        let bus = bus_with_id(id);
        let mut s = sensor_on(&bus, &[]);
        s.select_mode(0).unwrap();
        start_up(&mut s);
        assert_eq!(s.state, State::Streaming, "id {id:#x}");
    }
}

#[test]
fn otp_program_register_is_refused_by_the_write_path() {
    let mut bus = bus_with_id(0x9782);
    assert_eq!(
        chip::write(&mut bus, ADDR, OTP_PROGRAM, 0x01),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(
        chip::write(&mut bus, ADDR, OTP_PROGRAM, 0x00),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(writes(&bus), vec![]);

    // Neighbours are not refused: the guard is one register, not a range.
    chip::write(&mut bus, ADDR, 0x3d7f, 0x55).unwrap();
    chip::write(&mut bus, ADDR, 0x3d81, 0xaa).unwrap();
    assert_eq!(writes(&bus), vec![(0x3d7f, 0x55), (0x3d81, 0xaa)]);
}

#[test]
fn no_table_or_sequence_ever_writes_otp_program() {
    assert!(chip::COMMON.iter().all(|(r, _)| *r != OTP_PROGRAM));
    let mut every = Vec::new();
    for index in 0..MODES.len() as u16 {
        let bus = bus_with_id(0x9782);
        let mut s = sensor_on(&bus, &[]);
        s.select_mode(index).unwrap();
        s.set_controls(Controls {
            exposure: Some(32),
            analogue_gain: Some(0x20),
            digital_gain: None,
            frame_length: Some(2000),
        })
        .unwrap();
        s.set_flips(true, true).unwrap();
        s.set_test_pattern(7).unwrap();
        start_up(&mut s);
        s.post_values(FrameValues {
            identity: 1,
            controls: Controls {
                exposure: Some(64),
                analogue_gain: Some(0x30),
                digital_gain: None,
                frame_length: Some(2100),
            },
        })
        .unwrap();
        s.stop().unwrap();
        every.extend(writes(&bus));
    }
    assert!(!every.is_empty());
    assert!(every.iter().all(|(r, _)| *r != OTP_PROGRAM));
}

#[test]
fn mode_table_reports_the_values_the_registers_program() {
    let (mut s, _bus) = sensor();

    // RAW10 800: 765 * 12.5 ns = 9.5625 us line; 80 MHz system clock.
    assert_eq!(
        s.mode(0).unwrap(),
        Mode {
            link: LinkConfig {
                lanes: 2,
                virtual_channel: 0,
                data_type: 0x2b,
                bit_depth: 10,
                continuous_clock: false,
                width: 1280,
                height: 800,
                pixel_rate_hz: 160_000_000,
                link_rate_mbps: 800,
            },
            bayer: BayerOrder::Bggr,
            max_fps_milli: 114_804,
            line_time_ps: 9_562_500,
            line_length: 1530,
            frame_length_min: 910,
            frame_length_max: 52_340,
            exposure_min: 16,
            exposure_margin: 25,
            exposure_fraction_bits: 4,
            analogue_gain: GainRange {
                min: 0x10,
                max: 0xff,
                unity: 0x10,
                step: 1,
                law: GainLaw::Linear,
            },
            digital_gain: GainRange::default(),
            crop: Rect {
                x: 8,
                y: 8,
                width: 1280,
                height: 800,
            },
            binning_x: 1,
            binning_y: 1,
            black_level: 64,
            drop_frames: 0,
            exposure_delay: 2,
            gain_delay: 2,
            frame_length_delay: 2,
            readout_ps: 7_650_000_000,
            // (1456 - 765) system clocks at 80 MHz.
            frame_offset_ps: 8_637_500,
        }
    );

    let raw8_800 = s.mode(1).unwrap();
    assert_eq!(raw8_800.link.bit_depth, 8);
    assert_eq!(raw8_800.link.data_type, 0x2a);
    assert_eq!(raw8_800.link.pixel_rate_hz, 200_000_000);
    assert_eq!(raw8_800.line_time_ps, 7_968_750); // 765 / 96 MHz
    assert_eq!(raw8_800.black_level, 16);

    let binned = s.mode(4).unwrap();
    assert_eq!(binned.link.width, 640);
    assert_eq!(binned.link.height, 400);
    assert_eq!((binned.binning_x, binned.binning_y), (2, 2));
    assert_eq!(binned.frame_length_min, 422);
    // The binned mode reads the whole active array.
    assert_eq!(
        binned.crop,
        Rect {
            x: 8,
            y: 8,
            width: 1280,
            height: 800,
        }
    );

    // The 1280x720 mode crops rows: its array crop is the top 720 active lines.
    let crop720 = s.mode(2).unwrap();
    assert_eq!(
        crop720.crop,
        Rect {
            x: 8,
            y: 8,
            width: 1280,
            height: 720,
        }
    );
    assert_eq!((crop720.binning_x, crop720.binning_y), (1, 1));
    assert_eq!(crop720.frame_length_min, 761);

    assert_eq!(s.mode(6), Err(Status::NOT_FOUND));
    assert_eq!(s.info().unwrap().modes, 6);
    assert_eq!(s.info().unwrap().array_width, 1296);
    assert_eq!(s.info().unwrap().array_height, 816);
    assert_eq!(s.info().unwrap().test_patterns, 7);
}

#[test]
fn continuous_clock_changes_line_length_and_link() {
    let bus = bus_with_id(0x9782);
    let mut s = sensor_on(&bus, &[("continuous_clock", ConfigValue::Bool(true))]);
    let m = s.mode(0).unwrap();
    assert!(m.link.continuous_clock);
    assert_eq!(m.line_length, 1456);
    assert_eq!(m.line_time_ps, 9_100_000); // 728 / 80 MHz
    // 910 lines is 909 lines of 728 and one global line of 1456 system clocks, at 80 MHz: 911 x 9.1 us.
    assert_eq!(m.frame_offset_ps, 9_100_000);
    assert_eq!(m.max_fps_milli, 120_626);
}

#[test]
fn group_hold_brackets_a_full_post_with_vertical_blanking_launch() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    assert_eq!(s.applied(), Ok(None));
    clear(&bus);

    let values = FrameValues {
        identity: 0xfeed,
        controls: Controls {
            // 100.5 lines.
            exposure: Some(1608),
            analogue_gain: Some(0x20),
            digital_gain: None,
            frame_length: Some(1000),
        },
    };
    s.post_values(values).unwrap();
    assert_eq!(
        writes(&bus),
        vec![
            (0x3208, 0x00),
            (0x380e, 0x03),
            (0x380f, 0xe8),
            (0x3500, 0x00),
            (0x3501, 0x06),
            (0x3502, 0x48),
            (0x3509, 0x20),
            (0x3208, 0x10),
            (0x3208, 0xa0),
        ]
    );
    assert_eq!(s.applied(), Ok(Some(values)));
}

#[test]
fn group_hold_writes_only_the_controls_given() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    clear(&bus);
    s.post_values(FrameValues {
        identity: 7,
        controls: Controls {
            exposure: None,
            analogue_gain: Some(0x40),
            digital_gain: None,
            frame_length: None,
        },
    })
    .unwrap();
    assert_eq!(
        writes(&bus),
        vec![
            (0x3208, 0x00),
            (0x3509, 0x40),
            (0x3208, 0x10),
            (0x3208, 0xa0),
        ]
    );
}

#[test]
fn post_values_failure_closes_the_group_and_does_not_launch() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    clear(&bus);
    // Transactions in post_values: hold start, exposure bytes..., so the second fails.
    bus.fail_at.store(2, Ordering::SeqCst);
    let result = s.post_values(FrameValues {
        identity: 1,
        controls: Controls {
            exposure: Some(32),
            analogue_gain: None,
            digital_gain: None,
            frame_length: None,
        },
    });
    assert_eq!(result, Err(Status::IO));
    let w = writes(&bus);
    assert_eq!(w.first(), Some(&(0x3208, 0x00)));
    assert_eq!(w.last(), Some(&(0x3208, 0x10)));
    assert!(!w.iter().any(|(r, v)| *r == 0x3208 && *v == 0xa0));
    assert_eq!(s.applied(), Ok(None));
    assert_eq!(
        Driver::health(&s),
        Health::Failed("bus error while streaming: restarting the sensor")
    );
}

#[test]
fn recovery_retries_with_backoff_capped_at_8s_never_gives_up_and_clears_on_success() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    bus.down.store(true, Ordering::SeqCst);

    assert_eq!(
        s.set_controls(Controls {
            analogue_gain: Some(0x20),
            ..Controls::default()
        }),
        Err(Status::IO)
    );
    assert!(matches!(s.state, State::Recovering { .. }));
    // While recovering, sensor writes are BUSY and the driver names why.
    assert_eq!(s.post_values(FrameValues::default()), Err(Status::BUSY));
    assert_eq!(s.set_flips(true, false), Err(Status::BUSY));
    assert_eq!(s.set_test_pattern(1), Err(Status::BUSY));
    assert_eq!(s.select_mode(1), Err(Status::BUSY));

    // The first tick schedules 250 ms out; each failure doubles to the 8 s cap and stays there.
    let mut now = 1_000_000u64;
    let mut due = Lifecycle::tick(&mut s, now).unwrap();
    assert_eq!(due, now + 250_000_000);
    now = due;
    for expected in [
        500_000_000u64,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        8_000_000_000,
        8_000_000_000,
        8_000_000_000,
        8_000_000_000,
    ] {
        due = Lifecycle::tick(&mut s, now).unwrap();
        assert_eq!(due - now, expected);
        now = due;
    }
    assert!(matches!(s.state, State::Recovering { .. }));
    assert_eq!(
        Driver::health(&s),
        Health::Failed("bus error while streaming: restarting the sensor")
    );

    // The early tick does nothing and keeps the due time.
    assert_eq!(Lifecycle::tick(&mut s, now - 1), Some(due));

    // The bus returns: the next attempt reprograms and restarts, and backoff clears.
    bus.down.store(false, Ordering::SeqCst);
    clear(&bus);
    assert_eq!(Lifecycle::tick(&mut s, now), None);
    assert_eq!(s.state, State::Streaming);
    assert_eq!(Driver::health(&s), Health::Degraded(NO_RECEIVER));
    let w = writes(&bus);
    assert_eq!(w.first(), Some(&(0x0100, 0x00)));
    assert_eq!(w.last(), Some(&(0x0100, 0x01)));

    // The next failure starts from the bottom of the backoff.
    bus.down.store(true, Ordering::SeqCst);
    assert_eq!(s.set_test_pattern(1), Err(Status::IO));
    let t = Lifecycle::tick(&mut s, 5).unwrap();
    assert_eq!(t, 5 + 250_000_000);
}

#[test]
fn stop_during_recovery_is_reported_and_leaves_recovery_to_retry() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    bus.down.store(true, Ordering::SeqCst);
    let _ = s.set_test_pattern(1);
    assert_eq!(s.stop(), Err(Status::IO));
    assert!(matches!(s.state, State::Recovering { .. }));
    bus.down.store(false, Ordering::SeqCst);
    assert_eq!(s.stop(), Ok(()));
    assert_eq!(s.state, State::Stopped);
}

#[test]
fn fractional_exposure_math_at_the_boundaries() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    // Frame length stays 1822 after select.
    let write_exposure = |s: &mut Ov9782, bus: &Bus, e: u32| {
        clear(bus);
        let r = s.set_controls(Controls {
            exposure: Some(e),
            ..Controls::default()
        });
        (r, writes(bus))
    };

    // One line exactly (the minimum): 16 sixteenths.
    assert_eq!(
        write_exposure(&mut s, &bus, 16),
        (Ok(()), vec![(0x3500, 0x00), (0x3501, 0x00), (0x3502, 0x10)])
    );
    // Below one line: clamped to one line, and reported.
    assert_eq!(
        write_exposure(&mut s, &bus, 15),
        (Ok(()), vec![(0x3500, 0x00), (0x3501, 0x00), (0x3502, 0x10)])
    );
    assert_eq!(s.in_force().unwrap().exposure_lines, Some(1));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_EXPOSURE);
    // A fraction of a line: 1 + 5/16 lines.
    assert_eq!(
        write_exposure(&mut s, &bus, 21),
        (Ok(()), vec![(0x3500, 0x00), (0x3501, 0x00), (0x3502, 0x15)])
    );
    // Max: frame 1822 less 25 lines, the fraction may fill the last sixteenth at most.
    let max = (1822 - 25) << 4;
    assert_eq!(
        write_exposure(&mut s, &bus, max),
        (Ok(()), vec![(0x3500, 0x00), (0x3501, 0x70), (0x3502, 0x50)])
    );
    // One sixteenth over: clamped to the longest exposure the frame allows, and reported.
    assert_eq!(
        write_exposure(&mut s, &bus, max + 1),
        (Ok(()), vec![(0x3500, 0x00), (0x3501, 0x70), (0x3502, 0x50)])
    );
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_EXPOSURE);
    assert_eq!(write_exposure(&mut s, &bus, 21).0, Ok(()));
    assert_eq!(
        s.in_force().unwrap().clamped,
        0,
        "a request in range reports nothing"
    );

    // Exposure reaches 20 bits only at the longest frame, and never spills past it.
    let longest = (52_340 - 25) << 4;
    assert_eq!(longest, 0xcc5b0);
    s.set_controls(Controls {
        frame_length: Some(52_340),
        ..Controls::default()
    })
    .unwrap();
    assert_eq!(
        write_exposure(&mut s, &bus, longest),
        (Ok(()), vec![(0x3500, 0x0c), (0x3501, 0xc5), (0x3502, 0xb0)])
    );
}

#[test]
fn limits_are_clamped_to_the_nearest_value_and_reported() {
    let (mut s, bus) = sensor();
    // No mode selected: there are no limits to clamp to, so the request is refused as a state.
    assert_eq!(
        s.set_controls(Controls {
            exposure: Some(100),
            ..Controls::default()
        }),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(writes(&bus), vec![]);

    s.select_mode(0).unwrap();
    // Gain below the range and above the top code: applied at the nearest limit, and reported.
    s.set_controls(Controls {
        analogue_gain: Some(0x0f),
        ..Controls::default()
    })
    .unwrap();
    assert_eq!(s.in_force().unwrap().analogue_gain, Some(0x10));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_ANALOGUE_GAIN);
    clear(&bus);
    s.set_controls(Controls {
        analogue_gain: Some(0x100),
        ..Controls::default()
    })
    .unwrap();
    assert_eq!(writes(&bus), vec![(0x3509, 0xff)]);
    assert_eq!(s.in_force().unwrap().analogue_gain, Some(0xff));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_ANALOGUE_GAIN);

    // Frame length below the mode's minimum (910 for 1280x800): the rate's ceiling. Applied at 910.
    clear(&bus);
    s.set_controls(Controls {
        frame_length: Some(909),
        ..Controls::default()
    })
    .unwrap();
    assert_eq!(writes(&bus), vec![(0x380e, 0x03), (0x380f, 0x8e)]);
    assert_eq!(s.in_force().unwrap().frame_length, Some(910));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_FRAME_LENGTH);
    // Frame length above the maximum (800 + 51540 = 52340): the rate's floor.
    clear(&bus);
    s.set_controls(Controls {
        frame_length: Some(52_341),
        ..Controls::default()
    })
    .unwrap();
    let longest = 52_340u32;
    assert_eq!(
        writes(&bus),
        vec![(0x380e, (longest >> 8) as u8), (0x380f, longest as u8)]
    );
    assert_eq!(s.in_force().unwrap().frame_length, Some(longest));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_FRAME_LENGTH);

    // The sensor has no digital gain reachable through the contract: that is not a range, so it stays
    // refused, and nothing is written.
    clear(&bus);
    assert_eq!(
        s.set_controls(Controls {
            digital_gain: Some(0x10),
            ..Controls::default()
        }),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(writes(&bus), vec![]);

    // A frame shorter than the held exposure allows: 1000 lines held, frame asked for 1000. The exposure
    // is pulled in to the 975 lines that frame allows, and its register write comes before the frame's.
    s.set_controls(Controls {
        exposure: Some(1000 << 4),
        ..Controls::default()
    })
    .unwrap();
    clear(&bus);
    s.set_controls(Controls {
        frame_length: Some(1000),
        ..Controls::default()
    })
    .unwrap();
    assert_eq!(
        writes(&bus),
        vec![
            (0x3500, 0x00),
            (0x3501, 0x3c),
            (0x3502, 0xf0),
            (0x380e, 0x03),
            (0x380f, 0xe8),
        ]
    );
    let in_force = s.in_force().unwrap();
    assert_eq!(in_force.exposure_lines, Some(975));
    assert_eq!(in_force.frame_length, Some(1000));
    // 1000 lines is inside the frame range, so only the exposure was clamped.
    assert_eq!(in_force.clamped, CLAMP_EXPOSURE);

    // Not streaming: post_values is refused as a state, not a range.
    assert_eq!(
        s.post_values(FrameValues::default()),
        Err(Status::INVALID_ARGUMENT)
    );
    // Streaming: a gain of 5 is below the range; it is posted, applied at the minimum, and reported.
    start_up(&mut s);
    clear(&bus);
    assert_eq!(
        s.post_values(FrameValues {
            identity: 0,
            controls: Controls {
                analogue_gain: Some(5),
                ..Controls::default()
            },
        }),
        Ok(())
    );
    assert_eq!(s.in_force().unwrap().analogue_gain, Some(0x10));
    assert_eq!(s.in_force().unwrap().clamped, CLAMP_ANALOGUE_GAIN);
    assert!(writes(&bus).contains(&(0x3509, 0x10)), "{:?}", writes(&bus));
}

#[test]
fn set_controls_is_unheld_and_writes_exposure_first_when_the_frame_shortens() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    // Lengthen the frame first: the frame length register is written before exposure.
    s.set_controls(Controls {
        exposure: Some(32),
        analogue_gain: Some(0x18),
        digital_gain: None,
        frame_length: Some(2000),
    })
    .unwrap();
    assert_eq!(
        writes(&bus),
        vec![
            (0x380e, 0x07),
            (0x380f, 0xd0),
            (0x3500, 0x00),
            (0x3501, 0x00),
            (0x3502, 0x20),
            (0x3509, 0x18),
        ]
    );

    clear(&bus);
    // Shorten: exposure first, so the exposure never exceeds the frame between writes.
    s.set_controls(Controls {
        exposure: Some(48),
        analogue_gain: None,
        digital_gain: None,
        frame_length: Some(1000),
    })
    .unwrap();
    assert_eq!(
        writes(&bus),
        vec![
            (0x3500, 0x00),
            (0x3501, 0x00),
            (0x3502, 0x30),
            (0x380e, 0x03),
            (0x380f, 0xe8),
        ]
    );
}

#[test]
fn select_mode_rules() {
    let (mut s, bus) = sensor();
    assert_eq!(s.selected(), Ok(None));
    assert_eq!(s.select_mode(6), Err(Status::NOT_FOUND));
    assert_eq!(s.start(), Err(Status::INVALID_ARGUMENT));
    s.select_mode(2).unwrap();
    assert_eq!(s.selected(), Ok(Some(2)));
    assert_eq!(writes(&bus), vec![], "select_mode alone touches nothing");
    start_up(&mut s);
    assert_eq!(s.select_mode(0), Err(Status::BUSY));
    assert_eq!(s.start(), Ok(()));
    clear(&bus);
    assert_eq!(s.stop(), Ok(()));
    assert_eq!(writes(&bus), vec![(0x0100, 0x00)]);
    clear(&bus);
    assert_eq!(s.stop(), Ok(()), "stopping when stopped is a no-op");
    assert_eq!(writes(&bus), vec![]);
    s.select_mode(0).unwrap();
    assert_eq!(s.selected(), Ok(Some(0)));
}

#[test]
fn set_flips_sets_the_flip_bits_over_the_mode_base_values() {
    let (mut s, bus) = sensor();
    // Before a mode: stored, nothing written.
    assert_eq!(s.set_flips(true, true), Ok(BayerOrder::Bggr));
    assert_eq!(writes(&bus), vec![]);

    s.select_mode(0).unwrap();
    start_up(&mut s);
    let w = writes(&bus);
    assert!(w.contains(&(0x3820, 0x44)));
    assert!(w.contains(&(0x3821, 0x04)));

    clear(&bus);
    assert_eq!(s.set_flips(false, true), Ok(BayerOrder::Bggr));
    assert_eq!(writes(&bus), vec![(0x3820, 0x44), (0x3821, 0x00)]);

    clear(&bus);
    assert_eq!(s.set_flips(true, false), Ok(BayerOrder::Bggr));
    assert_eq!(writes(&bus), vec![(0x3820, 0x40), (0x3821, 0x04)]);

    clear(&bus);
    assert_eq!(s.set_flips(false, false), Ok(BayerOrder::Bggr));
    assert_eq!(writes(&bus), vec![(0x3820, 0x40), (0x3821, 0x00)]);

    // In a binned mode the binning bits stay set alongside the flip bits.
    let (mut s, bus) = sensor();
    s.select_mode(4).unwrap();
    clear(&bus);
    s.set_flips(true, true).unwrap();
    assert_eq!(writes(&bus), vec![(0x3820, 0x65), (0x3821, 0x05)]);
}

#[test]
fn test_pattern_codes_program_register_0x5e00() {
    let (mut s, bus) = sensor();
    for (code, value) in [
        (0u16, 0x00u8),
        (1, 0x80),
        (2, 0x84),
        (3, 0x88),
        (4, 0x8c),
        (5, 0x81),
        (6, 0x82),
        (7, 0x83),
    ] {
        clear(&bus);
        s.set_test_pattern(code).unwrap();
        assert_eq!(writes(&bus), vec![(0x5e00, value)], "code {code}");
    }
    clear(&bus);
    assert_eq!(s.set_test_pattern(8), Err(Status::INVALID_ARGUMENT));
    assert_eq!(writes(&bus), vec![], "out of range writes nothing");

    // A pattern set before a start is kept through the start sequence.
    s.set_test_pattern(1).unwrap();
    s.select_mode(0).unwrap();
    clear(&bus);
    start_up(&mut s);
    assert!(writes(&bus).contains(&(0x5e00, 0x80)));
}

/// A client that starts the sensor and then goes away (an error, a crash, a closed connection)
/// leaves nothing running: `caller_gone` for the owner stops it; another caller's leaves it alone.
#[test]
fn a_caller_going_away_stops_the_sensor_it_started() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    Lifecycle::caller(&mut s, CallerId(7));
    start_up(&mut s);
    clear(&bus);

    Lifecycle::caller_gone(&mut s, CallerId(8));
    assert!(
        !writes(&bus).contains(&(MODE_SELECT, STANDBY)),
        "a caller that did not start the sensor leaves it running"
    );
    Lifecycle::caller_gone(&mut s, CallerId(7));
    assert!(
        writes(&bus).contains(&(MODE_SELECT, STANDBY)),
        "the owner going away stops the sensor (standby written)"
    );
    Lifecycle::caller_gone(&mut s, CallerId(7));
    assert_eq!(
        writes(&bus)
            .iter()
            .filter(|w| **w == (MODE_SELECT, STANDBY))
            .count(),
        1,
        "a second caller_gone for the same caller stops nothing more"
    );
}

#[test]
fn first_frame_masking_is_programmed_before_streaming_starts() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    assert_eq!(s.mode(0).unwrap().drop_frames, 0);
    start_up(&mut s);
    let w = writes(&bus);
    let mask = w.iter().position(|r| *r == (0x4242, 0x01)).unwrap();
    let run = w.iter().position(|r| *r == (0x0100, 0x01)).unwrap();
    assert!(mask < run);
    assert_eq!(run, w.len() - 1, "streaming starts last");
}

#[test]
fn manifest_names_the_driver_and_its_tag() {
    assert_eq!(Ov9782::MANIFEST.name, "ov9782");
    assert_eq!(Ov9782::MANIFEST.provides.len(), 1);
    assert_eq!(
        <dyn CameraSensor as Contract>::TAG.as_str(),
        "camera.sensor/1"
    );
    assert_eq!(Ov9782::MANIFEST.provides[0].tag.as_str(), "camera.sensor/1");
}

#[test]
fn the_sensor_reports_a_linear_gain_law_and_no_embedded_data() {
    // Gain is sixteenths: code 0x20 is gain two. The sensor carries no register values per frame.
    let range = chip::ANALOGUE_GAIN_RANGE;
    assert_eq!(range.law, GainLaw::Linear);
    assert_eq!(range.unity, 0x10);
    assert_eq!(chip::sensor_info().embedded_data, EmbeddedData::None);
}

/// A receiver whose `source` answers `answer`, as a device the link can reach.
struct Answers(Result<&'static [u8]>);

impl lemnos_core::Router for Answers {
    fn call(
        &self,
        _device: u32,
        _method: lemnos_core::MethodId,
        _args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        let name = self.0?;
        reply
            .get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

/// The receiver the board's link names, answering `answer` to `source`.
fn linked_receiver(answer: Result<&'static [u8]>) -> Dev<dyn CameraReceiver> {
    let router: &'static Answers = Box::leak(Box::new(Answers(answer)));
    lemnos_core::Bound::new(lemnos_core::LocalCaller::new(router, 0, 0))
}

fn start_linked(answer: Option<Result<&'static [u8]>>) -> Result<Ov9782> {
    let bus = bus_with_id(0x9782);
    <Ov9782 as Lifecycle>::start(
        Ov9782Needs {
            bus: I2c(lemnos_core::Held::boxed(Box::new(bus.clone()))),
            values: None,
            _receiver: answer.map(linked_receiver),
        },
        &Config::new(&[]),
    )
}

#[test]
fn a_link_to_a_receiver_with_nothing_wired_fails_the_sensor() {
    // The board says this sensor feeds that receiver, and the receiver says nothing feeds it.
    let sensor = start_linked(Some(Ok(b""))).unwrap();
    assert_eq!(
        Driver::health(&sensor),
        Health::Failed("the linked receiver has nothing wired to its input")
    );
}

#[test]
fn a_receiver_fed_by_another_sensor_fails_the_sensor() {
    let sensor = start_linked(Some(Ok(b"imx219 4-0010"))).unwrap();
    // This driver knows no kernel name for itself, so it cannot tell a match: unknown, never Ok.
    assert_eq!(
        Driver::health(&sensor),
        Health::Degraded("the kernel does not name this sensor")
    );
}

#[test]
fn a_receiver_that_cannot_say_leaves_the_link_unconfirmed() {
    let sensor = start_linked(Some(Err(Status::NOT_SUPPORTED))).unwrap();
    assert_eq!(
        Driver::health(&sensor),
        Health::Degraded("the linked receiver cannot say what is wired to its input")
    );
}

#[test]
fn a_board_without_a_link_has_nothing_to_confirm() {
    let sensor = start_linked(None).unwrap();
    assert_eq!(
        Driver::health(&sensor),
        Health::Degraded("the board names no receiver for this sensor")
    );
}

#[test]
fn a_receiver_that_cannot_be_asked_fails_the_bind_and_is_retried() {
    assert!(matches!(
        start_linked(Some(Err(Status::BUSY))),
        Err(Status::BUSY)
    ));
}

/// A receiver that answers `frame_starts` from a script, one answer a read, and `source` with
/// `source`. Once the script is spent, a read answers an unknown count.
struct Starts {
    script: std::sync::Mutex<std::collections::VecDeque<Result<Option<FrameStarts>>>>,
    source: Result<&'static [u8]>,
}

/// The method number of `frame_starts` in `camera.receiver`.
const FRAME_STARTS_METHOD: u16 = 12;

impl lemnos_core::Router for Starts {
    fn call(
        &self,
        _device: u32,
        method: lemnos_core::MethodId,
        _args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if method.method == FRAME_STARTS_METHOD {
            let answer = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(None))?;
            let mut writer = lemnos_core::Writer::new(reply);
            lemnos_core::Encode::encode(&answer, &mut writer)?;
            return Ok(writer.len());
        }
        let name = self.source?;
        reply
            .get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

/// A started sensor, streaming, linked to `receiver` (or with no link).
fn streaming_sensor(receiver: Option<Dev<dyn CameraReceiver>>) -> (Ov9782, Bus) {
    let bus = bus_with_id(0x9782);
    let mut s = <Ov9782 as Lifecycle>::start(
        Ov9782Needs {
            bus: I2c(lemnos_core::Held::boxed(Box::new(bus.clone()))),
            values: None,
            _receiver: receiver,
        },
        &Config::new(&[]),
    )
    .unwrap();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    clear(&bus);
    (s, bus)
}

/// A receiver linked to a sensor, answering `frame_starts` from `script`.
fn counting_receiver(script: Vec<Result<Option<FrameStarts>>>) -> Dev<dyn CameraReceiver> {
    let router: &'static Starts = Box::leak(Box::new(Starts {
        script: std::sync::Mutex::new(script.into()),
        source: Ok(b"ov9782 10-0036"),
    }));
    lemnos_core::Bound::new(lemnos_core::LocalCaller::new(router, 0, 0))
}

fn starts(count: u64) -> Result<Option<FrameStarts>> {
    Ok(Some(FrameStarts {
        count,
        latest_ns: count * 8_000_000,
    }))
}

fn exposure_post(identity: u64) -> FrameValues {
    FrameValues {
        identity,
        controls: Controls {
            exposure: Some(800),
            analogue_gain: Some(0x20),
            digital_gain: None,
            frame_length: Some(1000),
        },
    }
}

#[test]
fn a_group_launched_in_one_frame_lands_two_frames_after_it() {
    // Before and after the writes the count is 5: the launch is in frame 4, and the delay is 2.
    let (mut s, _bus) = streaming_sensor(Some(counting_receiver(vec![starts(5), starts(5)])));
    let landing = s.post_values_at(exposure_post(1)).unwrap();
    assert_eq!(landing.exposure, Some(6));
    assert_eq!(landing.analogue_gain, Some(6));
    assert_eq!(landing.frame_length, Some(6));
    assert_eq!(landing.latest, 6);
}

#[test]
fn a_group_that_straddles_a_frame_start_lands_one_frame_later() {
    // The count moved from 5 to 6 while the group was written: the launch is the later frame, 5.
    let (mut s, _bus) = streaming_sensor(Some(counting_receiver(vec![starts(5), starts(6)])));
    let landing = s.post_values_at(exposure_post(2)).unwrap();
    assert_eq!(landing.exposure, Some(7));
    assert_eq!(landing.latest, 7);
}

#[test]
fn a_post_with_no_frame_counted_yet_is_refused_before_anything_is_written() {
    let (mut s, bus) = streaming_sensor(Some(counting_receiver(vec![Ok(None)])));
    assert_eq!(s.post_values_at(exposure_post(3)), Err(Status::BUSY));
    assert!(
        writes(&bus).is_empty(),
        "nothing written: {:?}",
        writes(&bus)
    );
    // A refused post is not a post: nothing is recorded for applied_landing.
    assert_eq!(s.applied_landing(), Ok(None));
}

#[test]
fn a_sensor_with_no_link_or_a_receiver_that_cannot_count_refuses_before_writing() {
    let (mut unlinked, bus) = streaming_sensor(None);
    assert_eq!(
        unlinked.post_values_at(exposure_post(4)),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(writes(&bus).is_empty());
    // A plain post still goes through, with no landing to give back.
    unlinked.post_values(exposure_post(4)).unwrap();
    assert_eq!(unlinked.applied_landing(), Err(Status::NOT_SUPPORTED));
    assert_eq!(unlinked.applied().unwrap().unwrap().identity, 4);

    let receiver = counting_receiver(vec![Err(Status::NOT_SUPPORTED)]);
    let (mut cannot, bus) = streaming_sensor(Some(receiver));
    assert_eq!(
        cannot.post_values_at(exposure_post(5)),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(writes(&bus).is_empty());
}

#[test]
fn applied_landing_is_the_last_post_and_where_it_landed() {
    let (mut s, _bus) = streaming_sensor(Some(counting_receiver(vec![
        starts(9),
        starts(9),
        starts(12),
        starts(12),
    ])));
    assert_eq!(s.applied_landing(), Ok(None));
    let landing = s.post_values_at(exposure_post(6)).unwrap();
    assert_eq!(
        s.applied_landing(),
        Ok(Some(AppliedLanding {
            values: exposure_post(6),
            landing,
        }))
    );
    // A later plain post is the last post; its landing is read too.
    s.post_values(exposure_post(7)).unwrap();
    let later = s.applied_landing().unwrap().unwrap();
    assert_eq!(later.values.identity, 7);
    // The count was 12 at the launch: frame 11, and the delay of 2 lands it on 13.
    assert_eq!(later.landing.latest, 13);
}

fn item_named<'a>(
    out: &'a Description,
    name: &str,
    kind: lemnos_core::ItemKind,
) -> &'a lemnos_core::Item {
    out.items()
        .find(|item| item.name == name && item.kind == kind)
        .expect("the item the sensor describes")
}

#[test]
fn before_a_mode_is_selected_the_ranges_of_exposure_and_frame_length_are_unknown() {
    let (mut s, _bus) = sensor();
    let mut out = Description::new();
    Lifecycle::describe(&s, &mut out);
    let exposure = item_named(&out, "exposure", lemnos_core::ItemKind::Control);
    assert_eq!(
        (exposure.min, exposure.max, exposure.step),
        (None, None, None)
    );
    assert_eq!(exposure.unit, Unit::Line);
    assert_eq!(
        exposure.method,
        MethodId {
            contract: 0,
            method: 16
        }
    );
    // The gain's codes are the same in every mode, so its range is known from the start.
    let gain = item_named(&out, "analogue_gain", lemnos_core::ItemKind::Control);
    assert_eq!(
        (gain.min, gain.max, gain.step),
        (Some(0x10), Some(0xff), Some(1))
    );
    assert_eq!(
        gain.method,
        MethodId {
            contract: 0,
            method: 17
        }
    );
    assert_eq!(s.in_force(), Ok(InForce::default()));
}

#[test]
fn with_a_mode_selected_the_items_carry_its_ranges_and_the_values_in_force() {
    let (mut s, _bus) = sensor();
    s.select_mode(0).unwrap();
    let frame = Current::defaults(&MODES[0]).frame_length;
    let mode = MODES[0].describe(false);
    assert_eq!(
        s.in_force(),
        Ok(InForce {
            exposure_lines: Some(DEFAULT_EXPOSURE_LINES),
            analogue_gain: Some(0x10),
            frame_length: Some(frame),
            clamped: 0,
        })
    );
    let mut out = Description::new();
    Lifecycle::describe(&s, &mut out);
    let exposure = item_named(&out, "exposure", lemnos_core::ItemKind::Control);
    // One line up to the frame length less the margin: the most the register takes.
    assert_eq!(
        (exposure.min, exposure.max, exposure.step),
        (
            Some(1),
            Some(i64::from(frame - chip::EXPOSURE_MARGIN)),
            Some(1)
        )
    );
    let value = item_named(&out, "exposure", lemnos_core::ItemKind::Value);
    assert_eq!(value.field, Some("exposure_lines"));
    assert_eq!(
        value.method,
        MethodId {
            contract: 0,
            method: 15
        }
    );
    let length = item_named(&out, "frame_length", lemnos_core::ItemKind::Control);
    assert_eq!(
        (length.min, length.max),
        (
            Some(i64::from(mode.frame_length_min)),
            Some(i64::from(mode.frame_length_max))
        )
    );
}

#[test]
fn a_set_through_an_item_lands_on_the_frame_post_values_at_gives() {
    // Two sensors, each with a receiver that counts frames from the same script: the frame
    // launched in is 4, and exposure's delay is 2, so the exposure lands on frame 6.
    let script = || vec![starts(5), starts(5)];
    let (mut by_item, _bus) = streaming_sensor(Some(counting_receiver(script())));
    let (mut by_post, _bus) = streaming_sensor(Some(counting_receiver(script())));
    let landing = by_item.set_exposure(40).unwrap();
    let posted = by_post
        .post_values_at(FrameValues {
            identity: 0,
            controls: Controls {
                exposure: Some(40 << EXPOSURE_FRACTION_BITS),
                ..Controls::default()
            },
        })
        .unwrap();
    assert_eq!(landing, posted);
    assert_eq!(landing.exposure, Some(6));
    assert_eq!(by_item.in_force().unwrap().exposure_lines, Some(40));
    assert_eq!(
        by_item.applied().unwrap().unwrap().controls.exposure,
        Some(40 << EXPOSURE_FRACTION_BITS)
    );
}

#[test]
fn the_gain_and_frame_length_setters_land_as_their_posts_do() {
    let script = || vec![starts(5), starts(5), starts(9), starts(9)];
    let (mut by_item, _bus) = streaming_sensor(Some(counting_receiver(script())));
    let (mut by_post, _bus) = streaming_sensor(Some(counting_receiver(script())));
    let gain = by_item.set_analogue_gain(0x20).unwrap();
    let gain_post = by_post
        .post_values_at(FrameValues {
            identity: 0,
            controls: Controls {
                analogue_gain: Some(0x20),
                ..Controls::default()
            },
        })
        .unwrap();
    assert_eq!(gain, gain_post);
    assert_eq!(gain.analogue_gain, Some(6));
    let length = by_item.set_frame_length(1000).unwrap();
    // The launch is in frame 8 (the count was 9 at both reads), and the frame length delay is 2.
    assert_eq!(length.frame_length, Some(10));
    assert_eq!(by_item.in_force().unwrap().frame_length, Some(1000));
}

#[test]
fn a_setter_while_stopped_is_refused_and_writes_nothing() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    clear(&bus);
    assert_eq!(s.set_exposure(40), Err(Status::INVALID_ARGUMENT));
    assert_eq!(s.set_analogue_gain(0x20), Err(Status::INVALID_ARGUMENT));
    assert_eq!(s.set_frame_length(1000), Err(Status::INVALID_ARGUMENT));
    assert!(
        writes(&bus).is_empty(),
        "nothing written: {:?}",
        writes(&bus)
    );
}

/// Power-up timing: the chip answers SCCB from `T` after power (here 6 ms, the datasheet's t2 and
/// t3 at their minimums). The start is at power, and the first attempt, `POWER_UP_NS` later, must
/// read the chip and start it. A driver that reads the chip at its start, before `T`, refuses.
#[test]
fn the_first_attempt_after_power_up_succeeds_when_the_chip_answers_within_the_settle() {
    let power = 1_000_000_000u64;
    let bus = bus_with_id(0x9782);
    bus.answers_from.store(power + 6_000_000, Ordering::SeqCst);
    let mut s = sensor_on(&bus, &[]);
    s.select_mode(0).unwrap();
    bus.clock.store(power, Ordering::SeqCst);

    assert_eq!(s.start(), Ok(()));
    assert!(
        bus.mock.transfers().is_empty(),
        "the start does not read the chip"
    );
    assert_eq!(Lifecycle::tick(&mut s, power), Some(power + POWER_UP_NS));

    bus.clock.store(power + POWER_UP_NS, Ordering::SeqCst);
    assert_eq!(Lifecycle::tick(&mut s, power + POWER_UP_NS), None);
    assert_eq!(
        s.state,
        State::Streaming,
        "the first attempt started the sensor"
    );
    assert_eq!(writes(&bus).first(), Some(&(0x0100, 0x00)));
}

/// A chip that still does not answer at the first attempt is recovered with the backoff, as any
/// bus fault is, and starts when it answers. The failure rule is unchanged: no giving up.
#[test]
fn a_chip_that_answers_after_the_first_attempt_is_retried_from_the_bottom_of_the_backoff() {
    let power = 1_000_000_000u64;
    let bus = bus_with_id(0x9782);
    bus.answers_from.store(power + 50_000_000, Ordering::SeqCst);
    let mut s = sensor_on(&bus, &[]);
    s.select_mode(0).unwrap();
    bus.clock.store(power, Ordering::SeqCst);
    assert_eq!(s.start(), Ok(()));

    assert_eq!(Lifecycle::tick(&mut s, power), Some(power + POWER_UP_NS));
    bus.clock.store(power + POWER_UP_NS, Ordering::SeqCst);
    let first_retry = power + POWER_UP_NS + 250_000_000;
    assert_eq!(
        Lifecycle::tick(&mut s, power + POWER_UP_NS),
        Some(first_retry)
    );
    assert!(matches!(s.state, State::Recovering { .. }));

    bus.clock.store(first_retry, Ordering::SeqCst);
    assert_eq!(Lifecycle::tick(&mut s, first_retry), None);
    assert_eq!(s.state, State::Streaming);
}

#[test]
fn a_streaming_sensor_stops_when_its_receiver_goes_away() {
    // The receiver is unbound mid-stream: its frame count answers NO_DEVICE. The watch puts the
    // sensor to standby and frees it, so a new mode can be selected and started.
    let receiver = counting_receiver(vec![Err(Status::NO_DEVICE)]);
    let (mut s, bus) = streaming_sensor(Some(receiver));
    assert_eq!(Lifecycle::tick(&mut s, 0), None);
    assert_eq!(writes(&bus), vec![(MODE_SELECT, STANDBY)]);
    s.select_mode(0).unwrap();
    s.start().unwrap();
    assert!(writes(&bus).contains(&(MODE_SELECT, STREAMING)));
}

#[test]
fn a_stale_receiver_handle_frees_the_sensor_before_the_watch_runs() {
    // The receiver was unbound and bound again between two watches: the old handle is stale. A
    // select_mode that comes first still finds the stream over.
    let receiver = counting_receiver(vec![Err(Status::STALE_HANDLE)]);
    let (mut s, bus) = streaming_sensor(Some(receiver));
    s.select_mode(0).unwrap();
    assert_eq!(writes(&bus), vec![(MODE_SELECT, STANDBY)]);
}

#[test]
fn a_receiver_that_is_still_there_keeps_the_sensor_busy() {
    // A receiver that answers with some other failure is still there: the stream is not over.
    let receiver = counting_receiver(vec![Err(Status::BUSY)]);
    let (mut s, bus) = streaming_sensor(Some(receiver));
    assert_eq!(Lifecycle::tick(&mut s, 0), Some(RECEIVER_WATCH_NS));
    assert!(matches!(s.select_mode(0), Err(Status::BUSY)));
    assert!(writes(&bus).is_empty());
}

#[test]
fn a_streaming_sensor_with_a_link_watches_its_receiver_each_interval() {
    let receiver = counting_receiver(vec![Ok(None), Ok(None)]);
    let (mut s, _bus) = streaming_sensor(Some(receiver));
    assert_eq!(
        Lifecycle::tick(&mut s, 1_000),
        Some(1_000 + RECEIVER_WATCH_NS)
    );
    assert_eq!(
        Lifecycle::tick(&mut s, 1_000 + RECEIVER_WATCH_NS),
        Some(1_000 + 2 * RECEIVER_WATCH_NS)
    );
}

/// The readout span of every mode: its active lines times the line time, exact in picoseconds.
/// Checked against the register arithmetic (line length × height over the clock, before any
/// rounding) and against the rounded line time, which may differ by less than a line's picosecond
/// count times the height.
#[test]
fn every_mode_reports_its_readout_span_exactly() {
    for (index, entry) in chip::MODES.iter().enumerate() {
        for continuous in [false, true] {
            let mode = entry.describe(continuous);
            let line_length = u64::from(mode.line_length / 2);
            // 80 MHz for 10-bit, 96 MHz for 8-bit: the two system clocks in chip.rs.
            let clock = if entry.depth.bit_depth == 10 {
                80_000_000
            } else {
                96_000_000
            };
            let exact = line_length * u64::from(entry.window.height) * 1_000_000_000_000 / clock;
            assert_eq!(
                mode.readout_ps, exact,
                "mode {index} continuous {continuous}"
            );
            let from_line = u64::from(mode.line_time_ps) * u64::from(entry.window.height);
            assert!(
                mode.readout_ps.abs_diff(from_line) < u64::from(entry.window.height),
                "mode {index}: readout {} vs line time × height {from_line}",
                mode.readout_ps
            );
        }
    }
    // Mode 0, gated RAW10: 765 cycles at 80 MHz, 800 lines: 7.65 ms exactly.
    assert_eq!(chip::MODES[0].describe(false).readout_ps, 7_650_000_000);
}

/// The board runs of 0aa30510 (other team's raw runs, 9000 frames each), reproduced from the
/// registers. Per run: the frame length in lines, the rate the runner computed from the frame
/// length and the line time (`backend`, milli-fps), and the shortest frame interval the timestamps
/// show (ns). The shortest interval is the frame as programmed; the mean interval of the same runs
/// is longer, by about one line in two frames (see the last assertion).
#[test]
fn the_registers_reproduce_the_board_rates_and_the_shortest_frames() {
    let rate_line = |depth: u8, continuous: bool| {
        let entry = chip::MODES
            .iter()
            .find(|e| e.depth.bit_depth == depth)
            .expect("a mode at this depth");
        let mode = entry.describe(continuous);
        // The register value (the mode's line length is twice it) over the 96 MHz system clock
        // of the 8-bit mode: exact in picoseconds.
        let reg = u128::from(mode.line_length / 2);
        reg * 1_000_000_000_000 / 96_000_000
    };
    let raw8_gated_ps = rate_line(8, false);
    assert_eq!(raw8_gated_ps, 7_968_750, "765 cycles at 96 MHz");
    let raw8_cont_ps = rate_line(8, true);
    assert_eq!(raw8_cont_ps, 7_583_333, "728 cycles at 96 MHz, truncated");

    let fps_milli = |line_ps: u128, lines: u128| 1_000_000_000_000_000 / (line_ps * lines);
    // (line time ps, lines, backend milli-fps, shortest interval ns).
    let pairs = [
        (raw8_gated_ps, 910u128, 137_901u128, 7_251_648u128),
        (raw8_gated_ps, 1046, 119_972, 8_335_278),
        (raw8_gated_ps, 2092, 59_986, 16_672_111),
        (raw8_cont_ps, 1099, 119_989, 8_334_277),
    ];
    for (line_ps, lines, backend, shortest_ns) in pairs {
        let computed = fps_milli(line_ps, lines);
        assert!(
            computed.abs_diff(backend) * 10_000 <= backend,
            "{lines} lines: computed {computed} vs {backend} milli-fps"
        );
        // Within 120 parts per million of the programmed frame (the 0.01 % of the four runs; the
        // continuous-clock run is 113 ppm, the gated ones 4 to 89 ppm).
        let period_ns = line_ps * lines / 1000;
        assert!(
            shortest_ns.abs_diff(period_ns) * 1_000_000 <= period_ns * 120,
            "{lines} lines: shortest interval {shortest_ns} ns vs {period_ns} ns"
        );
    }
    // The ceiling run (910 lines programmed): the shortest interval is 7 251 648 ns (910 lines), the
    // longest 7 266 000 ns, and the mean 7 258 821 ns. The mean is the midpoint of the two levels,
    // so about half the frames are the longer one, 14.35 us (about 1.8 lines) later. Those frames
    // are not whole lines longer, so the cause is not a frame length in the registers: it is
    // either the timestamps or the readout, and the per-frame log on the board decides it.
    let (shortest, longest, mean) = (7_251_648u128, 7_266_000u128, 7_258_821u128);
    assert!(
        (2 * mean).abs_diff(shortest + longest) <= 20,
        "the mean is the midpoint of the two levels"
    );
    assert_eq!(longest - shortest, 14_352);
}

#[test]
fn a_bus_error_while_streaming_reads_as_a_failure_the_driver_recovers_from_and_clears_on_success() {
    // The host reads `Failed` from a live driver as a stall it recovers from by itself (it does not
    // rebind for it), so a stream fed by this sensor reads retrying until the sensor is back.
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    start_up(&mut s);
    bus.down.store(true, Ordering::SeqCst);
    assert_eq!(
        s.set_controls(Controls {
            analogue_gain: Some(0x20),
            ..Controls::default()
        }),
        Err(Status::IO)
    );
    assert!(matches!(s.state, State::Recovering { .. }));
    assert_eq!(
        Driver::health(&s),
        Health::Failed("bus error while streaming: restarting the sensor")
    );
    bus.down.store(false, Ordering::SeqCst);
    let mut now = 1_000_000u64;
    for _ in 0..4 {
        if !matches!(s.state, State::Recovering { .. }) {
            break;
        }
        now = Lifecycle::tick(&mut s, now).unwrap_or(now);
    }
    assert_eq!(s.state, State::Streaming);
    assert!(!matches!(Driver::health(&s), Health::Failed(_)));
}

/// The measured frame periods of the board run of 2026-11-14 (a board run; serial and figures in testing/board-runs/F2h3.md):
/// mode 1 (1280x800 RAW8, 96 MHz system clock), 5000 frames each, the mean interval being (last - first) / 4999
/// from the timestamps. (lines, mean interval in ns). The crystal's own error is not modelled: every mean sits
/// +7.5 to +8.1 ppm above the period, and the tests allow 10 ppm.
const MEASURED_GATED: [(u32, u128); 7] = [
    (1100, 8_772_889),
    (2082, 16_598_264),
    (2091, 16_669_986),
    (2092, 16_677_956),
    (2093, 16_685_925),
    (2102, 16_757_645),
    (4300, 34_273_081),
];
const MEASURED_CONTINUOUS: [(u32, u128); 5] = [
    (2188, 16_600_050),
    (2197, 16_668_301),
    (2198, 16_675_885),
    (2199, 16_683_468),
    (2208, 16_751_719),
];

/// Parts per million of `measured` that `model` is off by (ceiling).
fn ppm_off(model_ns: u128, measured_ns: u128) -> u128 {
    model_ns.abs_diff(measured_ns) * 1_000_000 / measured_ns
}

/// The one period: `(N - 1) x HTS + G` system clocks. Within 10 ppm of every measured mean, in both clock
/// modes; and the two periods the board ruled out, M0 (`N x HTS`) and A (`(N + 1) x HTS`), are not.
#[test]
fn the_frame_period_reproduces_the_measured_means_and_not_the_ruled_out_models() {
    for (continuous, runs) in [
        (false, &MEASURED_GATED[..]),
        (true, &MEASURED_CONTINUOUS[..]),
    ] {
        let mode = chip::MODES[1].describe(continuous);
        for &(lines, measured) in runs {
            let model = mode.period_ps(lines) / 1000;
            assert!(
                ppm_off(model, measured) <= 10,
                "continuous {continuous}, {lines} lines: period {model} ns against the measured {measured} ns"
            );
            // M0: lines x line time. Always more than 10 ppm away (217 ppm at 4300 lines, 829 at 1100).
            let m0 = u128::from(mode.line_time_ps) * u128::from(lines) / 1000;
            assert!(ppm_off(m0, measured) > 100, "M0 at {lines} lines is {m0}");
            // A: one extra line. In the gated mode it is 15 to 80 ppm away, so a regression to it fails;
            // in the continuous mode the global line is two lines, so A and the period are the same.
            let a = u128::from(mode.line_time_ps) * u128::from(lines + 1) / 1000;
            if continuous {
                assert!(
                    a.abs_diff(model) <= 1,
                    "{lines} lines: A {a}, period {model}"
                );
            } else {
                assert!(ppm_off(a, measured) > 10, "A at {lines} lines is {a}");
            }
        }
    }
}

/// The period at the unit of the frame, an exact figure: 2092 lines gated is 2091 lines of 765 system clocks and
/// one of 1456, at 96 MHz: 16 677 822.9 ns.
#[test]
fn the_frame_period_is_the_lines_less_one_plus_the_global_line_in_system_clocks() {
    let gated = chip::MODES[1].describe(false);
    assert_eq!(gated.line_time_ps, 7_968_750);
    assert_eq!(gated.frame_offset_ps, 7_197_916);
    assert_eq!(gated.period_ps(2092), 16_677_822_916);
    assert_eq!(
        gated.period_ps(2092),
        (2091 * 765 + 1456) * 1_000_000_000_000 / 96_000_000
    );
    let cont = chip::MODES[1].describe(true);
    // G is 2 x HTS: the frame is N + 1 lines of HTS (line time truncated to the picosecond).
    assert_eq!(cont.frame_offset_ps, 7_583_333);
    let exact = (2197 * 728 + 1456) * 1_000_000_000_000u128 / 96_000_000;
    assert!(
        cont.period_ps(2198).abs_diff(exact) < 1_000,
        "truncated line time, under a nanosecond"
    );
    // The shortest frame's rate is on the same period.
    assert_eq!(
        Some(gated.max_fps_milli),
        lemnos_contract_camera_sensor::fps_milli_of(
            gated.line_time_ps,
            gated.frame_offset_ps,
            gated.frame_length_min
        )
    );
}

/// The write of 0x382C/D from 1456 to 1536 (80 system clocks) moved the mean period by +832 ns on the board
/// (16 677 958 to 16 678 790 ns at 2092 lines); the model moves it by 80 / 96 MHz = 833.3 ns.
#[test]
fn a_longer_global_line_lengthens_the_period_by_its_system_clocks() {
    let entry = chip::MODES[1];
    let base = entry.describe(false);
    let longer = entry.describe_with_global_line(false, chip::GLOBAL_LINE_DEFAULT + 80);
    assert_eq!(
        base,
        entry.describe_with_global_line(false, chip::GLOBAL_LINE_DEFAULT)
    );
    let change = longer.period_ps(2092) - base.period_ps(2092);
    assert!(change.abs_diff(833_333) <= 1, "{change} ps");
    let board_change: u128 = 16_678_790 - 16_677_958;
    assert!(
        board_change.abs_diff(change / 1000) <= 2,
        "{board_change} ns on the board"
    );
    // The frame length does not change what the global line adds.
    assert_eq!(
        longer.period_ps(910) - base.period_ps(910),
        longer.period_ps(4000) - base.period_ps(4000)
    );
}

/// The frame length for an asked rate is the nearest on the frame's own period, in both clock modes. 60 fps gated
/// is 2091 lines (59 989 milli-fps; the lines alone gave 2092 and 59 986), continuous 2197 lines.
#[test]
fn the_frame_length_for_an_asked_rate_is_the_nearest_on_the_frame_period() {
    use lemnos_contract_camera_sensor::{fps_milli_of, frame_length_for_fps_milli};
    for continuous in [false, true] {
        let mode = chip::MODES[1].describe(continuous);
        for asked in [60_000u32, 120_000] {
            let (lines, rate) = frame_length_for_fps_milli(
                mode.line_time_ps,
                mode.frame_offset_ps,
                mode.frame_length_min,
                mode.frame_length_max,
                asked,
            )
            .unwrap();
            let miss = rate.abs_diff(asked);
            for other in [lines - 1, lines + 1] {
                let other_rate =
                    fps_milli_of(mode.line_time_ps, mode.frame_offset_ps, other).unwrap();
                assert!(
                    miss <= other_rate.abs_diff(asked),
                    "continuous {continuous}, asked {asked}: {lines} lines ({rate}) is not nearest; {other} gives {other_rate}"
                );
            }
        }
    }
    let gated = chip::MODES[1].describe(false);
    assert_eq!(
        frame_length_for_fps_milli(
            gated.line_time_ps,
            gated.frame_offset_ps,
            gated.frame_length_min,
            gated.frame_length_max,
            60_000
        ),
        Some((2091, 59_989))
    );
}
