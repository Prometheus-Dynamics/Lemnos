use lemnos_contract_camera_frames::{CameraFrames, Counters, FrameFormat, MAX_STREAM_NAME};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn CameraFrames as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: CameraFrames> Caller for Direct<T> {
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

fn copy_name(name: &str, into: &mut [u8]) -> Result<usize> {
    into.get_mut(..name.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(name.as_bytes());
    Ok(name.len())
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
    assert_eq!(INFO.tag.as_str(), "camera.frames/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("format_count", 1),
            ("format", 2),
            ("select_format", 3),
            ("selected", 4),
            ("frame_stream", 5),
            ("start", 6),
            ("stop", 7),
            ("counters", 8),
        ]
    );
    assert!(INFO.methods[4].has_out_buffer);
    assert_eq!(INFO.methods[0].returns, Shape::U16);
}

#[test]
fn the_types_round_trip() {
    round_trip(FrameFormat {
        fourcc: u32::from_le_bytes(*b"MJPG"),
        width: 1920,
        height: 1080,
        fps_milli: 30_000,
        max_frame_bytes: u32::MAX,
    });
    round_trip(Counters {
        frames: 1,
        dropped: u64::MAX,
        errors: 3,
    });
}

const FORMATS: [FrameFormat; 2] = [
    FrameFormat {
        fourcc: u32::from_le_bytes(*b"MJPG"),
        width: 1280,
        height: 720,
        fps_milli: 30_000,
        max_frame_bytes: 900_000,
    },
    FrameFormat {
        fourcc: u32::from_le_bytes(*b"YUYV"),
        width: 640,
        height: 480,
        fps_milli: 60_000,
        max_frame_bytes: 614_400,
    },
];

#[derive(Default)]
struct Mock {
    selected: Option<u16>,
    streaming: bool,
}

impl CameraFrames for Mock {
    fn format_count(&mut self) -> Result<u16> {
        Ok(FORMATS.len() as u16)
    }

    fn format(&mut self, index: u16) -> Result<FrameFormat> {
        FORMATS
            .get(usize::from(index))
            .copied()
            .ok_or(Status::NOT_FOUND)
    }

    fn select_format(&mut self, index: u16) -> Result<()> {
        if self.streaming {
            return Err(Status::BUSY);
        }
        if usize::from(index) >= FORMATS.len() {
            return Err(Status::NOT_FOUND);
        }
        self.selected = Some(index);
        Ok(())
    }

    fn selected(&mut self) -> Result<Option<u16>> {
        Ok(self.selected)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        copy_name("frames", into)
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

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters {
            frames: 7,
            ..Counters::default()
        })
    }
}

fn client() -> Bound<dyn CameraFrames, Direct<Mock>> {
    Bound::new(Direct {
        driver: Mock::default(),
        args: [0; LOCAL_ARGS],
    })
}

#[test]
fn a_caller_picks_a_format_and_finds_the_stream() {
    let mut camera = client();
    assert_eq!(camera.format_count(), Ok(2));
    assert_eq!(camera.format(1).unwrap().fps_milli, 60_000);
    assert_eq!(camera.format(2), Err(Status::NOT_FOUND));
    assert_eq!(camera.start(), Err(Status::INVALID_ARGUMENT));
    assert_eq!(camera.select_format(1), Ok(()));
    assert_eq!(camera.selected(), Ok(Some(1)));
    assert_eq!(camera.start(), Ok(()));
    assert_eq!(camera.select_format(0), Err(Status::BUSY));

    let mut name = [0; MAX_STREAM_NAME];
    let len = camera.frame_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"frames");
    assert_eq!(camera.counters().unwrap().frames, 7);
}

#[test]
fn a_name_buffer_that_is_too_small_is_refused() {
    let mut camera = client();
    assert_eq!(
        camera.frame_stream(&mut [0; 2]),
        Err(Status::BUFFER_TOO_SMALL)
    );
}
