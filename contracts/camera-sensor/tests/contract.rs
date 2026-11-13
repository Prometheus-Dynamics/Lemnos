use lemnos_contract_camera_sensor::{
    BayerOrder, CameraSensor, Controls, EmbeddedData, FrameValues, GainLaw, GainRange, LinkConfig,
    Mode, Rect, SensorInfo,
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
            let mut total = 0;
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
        ]
    );
    assert_eq!(INFO.methods[9].returns, Shape::U8);
    assert!(INFO.methods.iter().all(|m| !m.has_out_buffer));
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
    // Identity (8) and four present-flags plus the three values present.
    assert_eq!(bytes, 8 + 4 + 3 * 4);
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
    let mut buffer = [0; 28];
    let mut writer = Writer::new(&mut buffer);
    full.encode(&mut writer).unwrap();
    assert_eq!(writer.len(), max_len(&FrameValues::SHAPE).unwrap());
}

#[test]
fn a_bad_present_flag_or_order_is_refused() {
    // A control's flag must be 0 or 1.
    let mut bytes = [0u8; 8 + 4];
    bytes[8] = 7;
    assert_eq!(
        FrameValues::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    let mut buffer = [0; REPLY_INLINE];
    let mut writer = Writer::new(&mut buffer);
    mode().encode(&mut writer).unwrap();
    let used = writer.len();
    let at = max_len(&LinkConfig::SHAPE).unwrap();
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
        let mut bytes = [0u8; 5];
        bytes[0] = tag;
        bytes[1..].copy_from_slice(&param.to_le_bytes());
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
