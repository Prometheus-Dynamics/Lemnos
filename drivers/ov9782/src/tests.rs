//! Register sequences against a mocked sensor bus: every expected value here is written out by
//! hand from the chip's facts, not computed from the tables under test.

use super::*;
use chip::{CHIP_ID, DEFAULT_ADDRESS, OTP_PROGRAM};
use lemnos_contract_camera_receiver::CameraReceiver;
use lemnos_contract_camera_sensor::{
    BayerOrder, EmbeddedData, GainLaw, GainRange, LinkConfig, Rect,
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
        if self.down.load(Ordering::SeqCst) {
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
    s.start().unwrap();

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
    s.start().unwrap();

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
    s.start().unwrap();
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
        s.start().unwrap();
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
    assert_eq!(s.start(), Err(Status::NO_DEVICE));
    assert_eq!(writes(&bus), vec![]);
}

#[test]
fn start_accepts_both_chip_ids() {
    for id in [0x9782, 0x9281] {
        let bus = bus_with_id(id);
        let mut s = sensor_on(&bus, &[]);
        s.select_mode(0).unwrap();
        assert_eq!(s.start(), Ok(()), "id {id:#x}");
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
        s.start().unwrap();
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
            max_fps_milli: 114_917,
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
    assert_eq!(m.max_fps_milli, 120_758); // 80 MHz / (728 * 910)
}

#[test]
fn group_hold_brackets_a_full_post_with_vertical_blanking_launch() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    s.start().unwrap();
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
    s.start().unwrap();
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
    s.start().unwrap();
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
        Health::Degraded("bus error while streaming: restarting the sensor")
    );
}

#[test]
fn recovery_retries_with_backoff_capped_at_8s_never_gives_up_and_clears_on_success() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    s.start().unwrap();
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
        Health::Degraded("bus error while streaming: restarting the sensor")
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
    s.start().unwrap();
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
    // Below one line: refused, nothing written.
    assert_eq!(
        write_exposure(&mut s, &bus, 15),
        (Err(Status::INVALID_ARGUMENT), vec![])
    );
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
    // One sixteenth over: refused.
    assert_eq!(
        write_exposure(&mut s, &bus, max + 1),
        (Err(Status::INVALID_ARGUMENT), vec![])
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
fn limits_are_refused_before_any_write() {
    let (mut s, bus) = sensor();
    let bad = [
        // No mode selected: no limits.
        Controls {
            exposure: Some(100),
            ..Controls::default()
        },
    ];
    for controls in bad {
        assert_eq!(s.set_controls(controls), Err(Status::INVALID_ARGUMENT));
    }
    assert_eq!(writes(&bus), vec![]);

    s.select_mode(0).unwrap();
    let check = |s: &mut Ov9782, controls: Controls| {
        assert_eq!(
            s.set_controls(controls),
            Err(Status::INVALID_ARGUMENT),
            "{controls:?}"
        );
    };
    // Gain out of range.
    check(
        &mut s,
        Controls {
            analogue_gain: Some(0x0f),
            ..Controls::default()
        },
    );
    check(
        &mut s,
        Controls {
            analogue_gain: Some(0x100),
            ..Controls::default()
        },
    );
    // The sensor has no digital gain reachable through the contract.
    check(
        &mut s,
        Controls {
            digital_gain: Some(0x10),
            ..Controls::default()
        },
    );
    // Frame length out of range for 1280x800.
    check(
        &mut s,
        Controls {
            frame_length: Some(909),
            ..Controls::default()
        },
    );
    check(
        &mut s,
        Controls {
            frame_length: Some(52_341),
            ..Controls::default()
        },
    );
    // Shortening the frame past a held exposure is refused too: 1000 lines held needs a frame
    // of at least 1025 lines.
    s.set_controls(Controls {
        exposure: Some(1000 << 4),
        ..Controls::default()
    })
    .unwrap();
    clear(&bus);
    check(
        &mut s,
        Controls {
            frame_length: Some(1024),
            ..Controls::default()
        },
    );
    // An exposure over the new frame's limit is refused, and one line under it is accepted.
    check(
        &mut s,
        Controls {
            exposure: Some(((1025 - 25) << 4) + 1),
            frame_length: Some(1025),
            ..Controls::default()
        },
    );
    assert_eq!(writes(&bus), vec![]);
    s.set_controls(Controls {
        exposure: Some((1025 - 25) << 4),
        frame_length: Some(1025),
        ..Controls::default()
    })
    .unwrap();

    // Not streaming: post_values is refused.
    assert_eq!(
        s.post_values(FrameValues::default()),
        Err(Status::INVALID_ARGUMENT)
    );
    // Validation on post_values too.
    s.start().unwrap();
    clear(&bus);
    assert_eq!(
        s.post_values(FrameValues {
            identity: 0,
            controls: Controls {
                analogue_gain: Some(5),
                ..Controls::default()
            },
        }),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(writes(&bus), vec![]);
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
    s.start().unwrap();
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
    s.start().unwrap();
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
    s.start().unwrap();
    assert!(writes(&bus).contains(&(0x5e00, 0x80)));
}

#[test]
fn first_frame_masking_is_programmed_before_streaming_starts() {
    let (mut s, bus) = sensor();
    s.select_mode(0).unwrap();
    assert_eq!(s.mode(0).unwrap().drop_frames, 0);
    s.start().unwrap();
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
