use lemnos_contract_camera_sensor::{
    AppliedLanding, BayerOrder, CameraSensor, Controls, EmbeddedData, FrameStarts, FrameValues,
    GainLaw, GainRange, InForce, Landing, LinkConfig, MAX_IDENTITY_NAME, Mode, Rect, SensorInfo,
    landing_of, launch_frame,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn CameraSensor as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: CameraSensor> Caller for Direct<T> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let mut writer = Reply::new(reply);
        let status = self
            .driver
            .__serve(method, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

/// The most bytes a shape can take on the wire, or `None` for a variable-length one.
fn max_len(shape: &Shape) -> Option<usize> {
    Some(match shape {
        Shape::Unit => 0,
        Shape::Bool | Shape::U8 | Shape::I8 => 1,
        Shape::U16 | Shape::I16 => 2,
        Shape::U32 | Shape::I32 | Shape::F32 => 4,
        Shape::U64 | Shape::I64 | Shape::F64 => 8,
        Shape::Str | Shape::Bytes => return None,
        Shape::Option(inner) => 1 + max_len(inner)?,
        Shape::Array(inner, count) => max_len(inner)? * *count as usize,
        Shape::Record(fields) => {
            let mut total = 4;
            for field in *fields {
                total += max_len(&field.shape)?;
            }
            total
        }
    })
}

#[test]
fn every_call_fits_a_local_caller_and_an_inline_reply() {
    for method in INFO.methods {
        let mut args = if method.has_out_buffer { 4 } else { 0 };
        for param in method.params {
            args += max_len(&param.shape).expect("a fixed-size parameter");
        }
        assert!(args <= LOCAL_ARGS, "{} takes {args} bytes", method.name);
        if !method.has_out_buffer {
            let reply = max_len(&method.returns).expect("a fixed-size reply");
            assert!(
                reply <= REPLY_INLINE,
                "{} returns {reply} bytes",
                method.name
            );
        }
    }
}

fn round_trip<T: Encode + for<'a> Decode<'a> + PartialEq + core::fmt::Debug>(value: T) {
    let mut buffer = [0; REPLY_INLINE];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(used, max_len(&T::SHAPE).unwrap());
    let mut reader = Reader::new(&buffer[..used]);
    assert_eq!(T::decode(&mut reader), Ok(value));
    assert_eq!(reader.remaining(), 0);
    // One byte short is refused, as is every shorter prefix.
    for short in 0..used {
        assert_eq!(
            T::decode(&mut Reader::new(&buffer[..short])).err(),
            Some(Status::INVALID_ARGUMENT)
        );
    }
}

#[test]
fn the_tag_and_numbers_are_stable() {
    assert_eq!(INFO.tag.as_str(), "camera.sensor/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("mode", 2),
            ("select_mode", 3),
            ("selected", 4),
            ("start", 5),
            ("stop", 6),
            ("set_controls", 7),
            ("post_values", 8),
            ("applied", 9),
            ("set_flips", 10),
            ("set_test_pattern", 11),
            ("identity", 12),
            ("post_values_at", 13),
            ("applied_landing", 14),
            ("in_force", 15),
            ("set_exposure", 16),
            ("set_analogue_gain", 17),
            ("set_frame_length", 18),
        ]
    );
    assert!(!INFO.methods[12].has_out_buffer);
    assert!(matches!(INFO.methods[12].returns, Shape::Record(_)));
    assert!(matches!(INFO.methods[13].returns, Shape::Option(_)));
    assert_eq!(INFO.methods[9].returns, Shape::U8);
    assert!(INFO.methods[..11].iter().all(|m| !m.has_out_buffer));
    assert!(INFO.methods[11].has_out_buffer);
    assert_eq!(INFO.methods[11].returns, Shape::Bytes);
}

#[test]
fn a_mode_says_what_a_chip_agnostic_caller_needs() {
    // Named field by field, so a rename breaks here on purpose.
    let Shape::Record(fields) = Mode::SHAPE else {
        panic!("a mode is a record");
    };
    let names: Vec<_> = fields.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        [
            "link",
            "bayer",
            "max_fps_milli",
            "line_time_ps",
            "line_length",
            "frame_length_min",
            "frame_length_max",
            "exposure_min",
            "exposure_margin",
            "exposure_fraction_bits",
            "analogue_gain",
            "digital_gain",
            "crop",
            "binning_x",
            "binning_y",
            "black_level",
            "drop_frames",
            "exposure_delay",
            "gain_delay",
            "frame_length_delay",
            "readout_ps",
        ]
    );
}

const GAIN: GainRange = GainRange {
    min: 16,
    max: 255,
    unity: 16,
    step: 1,
    law: GainLaw::Linear,
};

fn mode() -> Mode {
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
        max_fps_milli: 120_000,
        line_time_ps: 9_100_000,
        line_length: 1456,
        frame_length_min: 910,
        frame_length_max: 51_000,
        exposure_min: 16,
        exposure_margin: 25,
        exposure_fraction_bits: 4,
        analogue_gain: GAIN,
        digital_gain: GainRange::default(),
        crop: Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        },
        binning_x: 1,
        binning_y: 1,
        black_level: 64,
        drop_frames: 2,
        exposure_delay: 2,
        gain_delay: 2,
        frame_length_delay: 2,
        readout_ps: 7_280_000_000,
    }
}

fn values() -> FrameValues {
    FrameValues {
        identity: 0xdead_beef_0123_4567,
        controls: Controls {
            exposure: Some(8_000),
            analogue_gain: Some(32),
            digital_gain: None,
            frame_length: Some(1000),
        },
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(SensorInfo {
        modes: u16::MAX,
        array_width: 1280,
        array_height: 800,
        test_patterns: 4,
        embedded_data: EmbeddedData::Registers,
    });
    round_trip(mode());
    round_trip(GAIN);
    round_trip(GainRange {
        law: GainLaw::Reciprocal { full: 0x1000 },
        ..GAIN
    });
    round_trip(GainRange {
        law: GainLaw::Decibel {
            milli_db_per_code: 300,
        },
        ..GAIN
    });
    round_trip(Rect {
        x: 1,
        y: 2,
        width: 3,
        height: u32::MAX,
    });
}

#[test]
fn absent_controls_stay_absent() {
    let bytes = {
        let mut buffer = [0; 32];
        let mut writer = Writer::new(&mut buffer);
        values().encode(&mut writer).unwrap();
        let used = writer.len();
        assert_eq!(
            FrameValues::decode(&mut Reader::new(&buffer[..used])),
            Ok(values())
        );
        used
    };
    // The block (4), identity (8), the controls' block (4), four present-flags and the three
    // values present.
    assert_eq!(bytes, 4 + 8 + 4 + 4 + 3 * 4);
    // At the limit, every value present: the most a post can take.
    let full = FrameValues {
        identity: u64::MAX,
        controls: Controls {
            exposure: Some(u32::MAX),
            analogue_gain: Some(u32::MAX),
            digital_gain: Some(u32::MAX),
            frame_length: Some(u32::MAX),
        },
    };
    let mut buffer = [0; 36];
    let mut writer = Writer::new(&mut buffer);
    full.encode(&mut writer).unwrap();
    assert_eq!(writer.len(), max_len(&FrameValues::SHAPE).unwrap());
}

#[test]
fn a_bad_present_flag_or_order_is_refused() {
    // A control's flag must be 0 or 1.
    // The block (length 16), identity (8), the controls' block (length 4), then the flags.
    let mut bytes = [0u8; 4 + 8 + 4 + 4];
    bytes[..4].copy_from_slice(&16u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
    bytes[16] = 7;
    assert_eq!(
        FrameValues::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    let mut buffer = [0; REPLY_INLINE];
    let mut writer = Writer::new(&mut buffer);
    mode().encode(&mut writer).unwrap();
    let used = writer.len();
    // The bayer byte follows Mode's block length and the link's whole block.
    let at = 4 + max_len(&LinkConfig::SHAPE).unwrap();
    buffer[at] = 9;
    assert_eq!(
        Mode::decode(&mut Reader::new(&buffer[..used])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_gain_law_or_embedded_flag_that_is_not_defined_is_refused() {
    // The law's tag is 0 linear, 1 reciprocal, 2 decibel; a linear law has no parameter and the
    // other two need a non-zero one.
    let refused = |tag: u8, param: u32| {
        // A block of 5 body bytes: the tag, then the parameter.
        let mut bytes = [0u8; 9];
        bytes[..4].copy_from_slice(&5u32.to_le_bytes());
        bytes[4] = tag;
        bytes[5..].copy_from_slice(&param.to_le_bytes());
        GainLaw::decode(&mut Reader::new(&bytes)).err()
    };
    assert_eq!(refused(0, 0), None);
    assert_eq!(refused(0, 1), Some(Status::INVALID_ARGUMENT));
    assert_eq!(refused(1, 0), Some(Status::INVALID_ARGUMENT));
    assert_eq!(refused(2, 0), Some(Status::INVALID_ARGUMENT));
    assert_eq!(refused(3, 1), Some(Status::INVALID_ARGUMENT));
    assert_eq!(
        EmbeddedData::decode(&mut Reader::new(&[2])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[derive(Default)]
struct Mock {
    selected: Option<u16>,
    streaming: bool,
    controls: Controls,
    pending: Option<FrameValues>,
    applied: Option<FrameValues>,
    flips: (bool, bool),
}

impl CameraSensor for Mock {
    fn info(&mut self) -> Result<SensorInfo> {
        Ok(SensorInfo {
            modes: 1,
            array_width: 1280,
            array_height: 800,
            test_patterns: 2,
            embedded_data: EmbeddedData::None,
        })
    }

    fn mode(&mut self, index: u16) -> Result<Mode> {
        if index == 0 {
            Ok(mode())
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    fn select_mode(&mut self, index: u16) -> Result<()> {
        if self.streaming {
            return Err(Status::BUSY);
        }
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        self.selected = Some(index);
        Ok(())
    }

    fn selected(&mut self) -> Result<Option<u16>> {
        Ok(self.selected)
    }

    fn start(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.streaming = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.streaming = false;
        Ok(())
    }

    fn set_controls(&mut self, controls: Controls) -> Result<()> {
        if controls
            .analogue_gain
            .is_some_and(|g| !(GAIN.min..=GAIN.max).contains(&g))
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.controls = controls;
        Ok(())
    }

    fn post_values(&mut self, values: FrameValues) -> Result<()> {
        if self.pending.is_some() {
            return Err(Status::BUSY);
        }
        self.pending = Some(values);
        // The next latch happens at once, here.
        self.applied = self.pending.take();
        Ok(())
    }

    fn applied(&mut self) -> Result<Option<FrameValues>> {
        Ok(self.applied)
    }

    fn set_flips(&mut self, horizontal: bool, vertical: bool) -> Result<BayerOrder> {
        self.flips = (horizontal, vertical);
        Ok(if horizontal && vertical {
            BayerOrder::Rggb
        } else {
            BayerOrder::Bggr
        })
    }

    fn set_test_pattern(&mut self, code: u16) -> Result<()> {
        if code > 2 {
            Err(Status::INVALID_ARGUMENT)
        } else {
            Ok(())
        }
    }

    fn identity(&mut self, into: &mut [u8]) -> Result<usize> {
        // Named by the kernel only when a kernel driver binds it; this mock is one.
        let name = b"ov9782 10-0036";
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn post_values_at(&mut self, _values: FrameValues) -> Result<Landing> {
        // No receiver in the mock: nothing to count frame starts with.
        Err(Status::NOT_SUPPORTED)
    }

    fn applied_landing(&mut self) -> Result<Option<AppliedLanding>> {
        Err(Status::NOT_SUPPORTED)
    }
    fn in_force(&mut self) -> Result<InForce> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_exposure(&mut self, _lines: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_analogue_gain(&mut self, _code: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_frame_length(&mut self, _lines: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[test]
fn a_launch_that_straddles_a_frame_start_is_the_later_frame() {
    let at = |count| FrameStarts {
        count,
        latest_ns: 0,
    };
    // No frame start during the writes: the frame in progress, count - 1.
    assert_eq!(launch_frame(at(5), at(5)), 4);
    // A start during the writes: the later frame.
    assert_eq!(launch_frame(at(5), at(6)), 5);
    assert_eq!(launch_frame(at(5), at(7)), 6);
    // Nothing started yet: the frame is not counted, and the helper saturates rather than wraps.
    assert_eq!(launch_frame(at(0), at(0)), 0);
}

#[test]
fn a_landing_is_the_launch_frame_plus_each_controls_delay() {
    let posted = Controls {
        exposure: Some(300),
        analogue_gain: None,
        digital_gain: None,
        frame_length: Some(900),
    };
    let landing = landing_of(10, [3, 1, 2], &posted);
    assert_eq!(
        landing,
        Landing {
            exposure: Some(13),
            analogue_gain: None,
            digital_gain: None,
            frame_length: Some(12),
            latest: 13,
        }
    );
    let gain_only = landing_of(
        10,
        [3, 1, 2],
        &Controls {
            analogue_gain: Some(20),
            ..Controls::default()
        },
    );
    assert_eq!(gain_only.latest, 11);
    assert_eq!(
        landing_of(10, [3, 1, 2], &Controls::default()).latest,
        10,
        "a post naming no control has its launch frame"
    );
}

#[test]
fn landings_and_applied_landings_round_trip() {
    // Absent controls are encoded short, so this checks the decode of what was written, not the
    // fixed length `round_trip` asserts.
    fn exact<T: Encode + for<'a> Decode<'a> + PartialEq + core::fmt::Debug>(value: T) {
        let mut buffer = [0; REPLY_INLINE];
        let mut writer = Writer::new(&mut buffer);
        value.encode(&mut writer).unwrap();
        let used = writer.len();
        let mut reader = Reader::new(&buffer[..used]);
        assert_eq!(T::decode(&mut reader), Ok(value));
        assert_eq!(reader.remaining(), 0);
    }
    exact(Landing {
        exposure: Some(u64::MAX),
        analogue_gain: None,
        digital_gain: None,
        frame_length: Some(0),
        latest: 9,
    });
    exact(AppliedLanding {
        values: values(),
        landing: landing_of(3, [2, 2, 2], &values().controls),
    });
    exact(Some(AppliedLanding::default()));
}

#[test]
fn a_caller_reads_the_name_the_kernel_knows_the_sensor_by() {
    let mut sensor = client();
    let mut name = [0; MAX_IDENTITY_NAME];
    let len = sensor.identity(&mut name).unwrap();
    assert_eq!(&name[..len], b"ov9782 10-0036");
    let mut small = [0; 4];
    assert_eq!(sensor.identity(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

fn client() -> Bound<dyn CameraSensor, Direct<Mock>> {
    Bound::new(Direct {
        driver: Mock::default(),
        args: [0; LOCAL_ARGS],
    })
}

#[test]
fn a_caller_selects_a_mode_and_streams() {
    let mut sensor = client();
    assert_eq!(sensor.start(), Err(Status::INVALID_ARGUMENT));
    assert_eq!(sensor.selected(), Ok(None));
    assert_eq!(sensor.mode(1), Err(Status::NOT_FOUND));
    let chosen = sensor.mode(0).unwrap();
    assert_eq!(chosen.exposure_fraction_bits, 4);
    assert_eq!(chosen.drop_frames, 2);
    assert_eq!(sensor.select_mode(0), Ok(()));
    assert_eq!(sensor.selected(), Ok(Some(0)));
    assert_eq!(sensor.start(), Ok(()));
    assert_eq!(sensor.select_mode(0), Err(Status::BUSY));
}

#[test]
fn posted_values_come_back_with_their_identity() {
    let mut sensor = client();
    assert_eq!(sensor.applied(), Ok(None));
    assert_eq!(sensor.post_values(values()), Ok(()));
    assert_eq!(sensor.applied(), Ok(Some(values())));
}

#[test]
fn controls_flips_and_patterns_reach_the_driver() {
    let mut sensor = client();
    let bad = Controls {
        analogue_gain: Some(300),
        ..Controls::default()
    };
    assert_eq!(sensor.set_controls(bad), Err(Status::INVALID_ARGUMENT));
    assert_eq!(sensor.set_controls(values().controls), Ok(()));
    assert_eq!(sensor.set_flips(true, true), Ok(BayerOrder::Rggb));
    assert_eq!(sensor.set_test_pattern(1), Ok(()));
    assert_eq!(sensor.set_test_pattern(3), Err(Status::INVALID_ARGUMENT));
}
