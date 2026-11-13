use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig,
    MAX_EMBEDDED_STREAM_NAME, MAX_FEED_NAME, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn CameraReceiver as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: CameraReceiver> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "camera.receiver/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("limits", 1),
            ("configure", 2),
            ("link", 3),
            ("frame_stream", 4),
            ("start", 5),
            ("stop", 6),
            ("counters", 7),
            ("feeds", 8),
            ("embedded_stream", 9),
            ("source", 10),
            ("layout", 11),
            ("frame_starts", 12),
        ]
    );
    assert!(!INFO.methods[11].has_out_buffer);
    assert!(matches!(INFO.methods[11].returns, Shape::Option(_)));
    assert!(INFO.methods[3].has_out_buffer);
    assert_eq!(INFO.methods[1].params[0].name, "link");
    assert_eq!(INFO.methods[1].params[1].shape, Shape::U8);
    assert_eq!(INFO.methods[3].returns, Shape::Bytes);
    assert!(INFO.methods[7].has_out_buffer);
    assert_eq!(INFO.methods[7].returns, Shape::Bytes);
    assert!(INFO.methods[8].has_out_buffer);
    assert!(INFO.methods[9].has_out_buffer);
    assert_eq!(INFO.methods[9].returns, Shape::Bytes);
    assert!(matches!(INFO.methods[10].returns, Shape::Option(_)));
}

fn link() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 1,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: true,
        width: 1920,
        height: 1080,
        pixel_rate_hz: 200_000_000,
        link_rate_mbps: 800,
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(link());
    round_trip(Limits {
        max_lanes: 4,
        max_link_rate_mbps: 1500,
        max_width: u32::MAX,
        max_height: 1,
    });
    round_trip(FrameLayout {
        width: 1920,
        height: 1080,
        bit_depth: 10,
        packing: Packing::Unpacked16,
        stride: 3840,
        size: 4_147_200,
    });
    round_trip(Counters {
        frames: u64::MAX,
        dropped: 1,
        errors: 2,
    });
    round_trip(Some(link()));
    round_trip(FrameStarts {
        count: u64::MAX,
        latest_ns: 7,
    });
    round_trip(Some(FrameStarts {
        count: 1,
        latest_ns: u64::MAX,
    }));
}

#[test]
fn an_unknown_order_or_packing_is_refused() {
    assert_eq!(
        BayerOrder::decode(&mut Reader::new(&[4])),
        Ok(BayerOrder::Bggr)
    );
    assert_eq!(
        BayerOrder::decode(&mut Reader::new(&[5])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        Packing::decode(&mut Reader::new(&[1])),
        Ok(Packing::Unpacked16)
    );
    assert_eq!(
        Packing::decode(&mut Reader::new(&[2])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_bad_packing_inside_a_layout_is_refused() {
    // A block: its u32 length (18 body bytes), then the layout's fields.
    let mut bytes = [0u8; 22];
    bytes[..4].copy_from_slice(&18u32.to_le_bytes());
    bytes[13] = 2; // packing, after the block length (4), width (4), height (4) and bit depth (1)
    assert_eq!(
        FrameLayout::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[derive(Default)]
struct Mock {
    link: Option<LinkConfig>,
    packing: Packing,
    streaming: bool,
    layout: Option<FrameLayout>,
    /// What `source` answers: `None` is unknown.
    source: Option<&'static str>,
}

impl CameraReceiver for Mock {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: 4,
            max_link_rate_mbps: 1500,
            max_width: 8192,
            max_height: 8192,
        })
    }

    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.streaming {
            return Err(Status::BUSY);
        }
        if link.lanes > 4 {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.link = Some(link);
        self.packing = packing;
        let layout = FrameLayout {
            width: link.width,
            height: link.height,
            bit_depth: link.bit_depth,
            packing,
            // Lines padded to 64 bytes, as a block with an alignment rule writes them.
            stride: (link.width * 2).div_ceil(64) * 64,
            size: (link.width * 2).div_ceil(64) * 64 * link.height,
        };
        self.layout = Some(layout);
        Ok(layout)
    }

    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(self.layout)
    }

    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Err(Status::NOT_SUPPORTED)
    }

    fn source(&mut self, into: &mut [u8]) -> Result<usize> {
        match self.source {
            Some(name) => copy_name(name, into),
            None => Err(Status::NOT_SUPPORTED),
        }
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.link)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        copy_name("frames", into)
    }

    fn start(&mut self) -> Result<()> {
        self.streaming = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.streaming = false;
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }

    fn feeds(&mut self, into: &mut [u8]) -> Result<usize> {
        // Wired to an ISP inside the hardware once it streams; to memory alone before.
        if self.streaming {
            copy_name("isp-vir0", into)
        } else {
            Ok(0)
        }
    }

    fn embedded_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        copy_name("embedded", into)
    }
}

fn client() -> Bound<dyn CameraReceiver, Direct<Mock>> {
    Bound::new(Direct {
        driver: Mock::default(),
        args: [0; LOCAL_ARGS],
    })
}

fn client_wired_to(source: Option<&'static str>) -> Bound<dyn CameraReceiver, Direct<Mock>> {
    Bound::new(Direct {
        driver: Mock {
            source,
            ..Mock::default()
        },
        args: [0; LOCAL_ARGS],
    })
}

#[test]
fn a_caller_reads_the_layout_it_configured() {
    let mut receiver = client();
    assert_eq!(receiver.layout(), Ok(None), "before configure");
    let set = receiver.configure(link(), Packing::Unpacked16).unwrap();
    assert_eq!(receiver.layout(), Ok(Some(set)));
    assert_eq!(
        set.size,
        set.stride * set.height,
        "the bytes a frame takes in a buffer"
    );
}

#[test]
fn a_padded_stride_is_what_the_layout_reports_and_frames_are_sized_by_it() {
    let mut receiver = client();
    // 1000 pixels at 2 bytes is 2000 bytes a line: padded to 2048.
    let narrow = LinkConfig {
        width: 1000,
        ..link()
    };
    let set = receiver.configure(narrow, Packing::Unpacked16).unwrap();
    assert_eq!(set.stride, 2048, "the line is padded");
    assert_eq!(receiver.layout(), Ok(Some(set)));
    assert_eq!(set.size, set.stride * set.height, "a frame is its lines");
    assert!(set.stride > 1000 * 2, "more than the pixels take");
}

#[test]
fn a_caller_asks_what_is_wired_to_the_input_and_gets_three_answers() {
    let mut name = [0; MAX_FEED_NAME];
    // A name: the sensor on the input.
    let mut wired = client_wired_to(Some("ov9782 10-0036"));
    let len = wired.source(&mut name).unwrap();
    assert_eq!(&name[..len], b"ov9782 10-0036");
    // Nothing wired: a length of 0, which is not unknown.
    let mut unwired = client_wired_to(Some(""));
    assert_eq!(unwired.source(&mut name), Ok(0));
    // Unknown: the receiver's kernel does not describe its input.
    let mut unknown = client_wired_to(None);
    assert_eq!(unknown.source(&mut name), Err(Status::NOT_SUPPORTED));
    let mut small = [0; 3];
    assert_eq!(wired.source(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn a_caller_configures_and_finds_the_stream() {
    let mut receiver = client();
    assert_eq!(receiver.link(), Ok(None));
    let layout = receiver.configure(link(), Packing::Unpacked16).unwrap();
    assert_eq!(layout.stride, 3840);
    assert_eq!(layout.packing, Packing::Unpacked16);
    assert_eq!(receiver.link(), Ok(Some(link())));

    let mut name = [0; MAX_STREAM_NAME];
    let len = receiver.frame_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"frames");
}

#[test]
fn a_caller_asks_which_isp_the_hardware_feeds() {
    let mut receiver = client();
    let mut name = [0xAA; MAX_FEED_NAME];
    assert_eq!(receiver.feeds(&mut name), Ok(0), "to memory alone");
    receiver.start().unwrap();
    let len = receiver.feeds(&mut name).unwrap();
    assert_eq!(&name[..len], b"isp-vir0");
    let mut small = [0; 3];
    assert_eq!(receiver.feeds(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn a_caller_finds_the_embedded_stream() {
    let mut receiver = client();
    let mut name = [0; MAX_EMBEDDED_STREAM_NAME];
    let len = receiver.embedded_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"embedded");
}

#[test]
fn a_caller_sees_the_drivers_refusals() {
    let mut receiver = client();
    receiver.start().unwrap();
    assert_eq!(
        receiver.configure(link(), Packing::Packed),
        Err(Status::BUSY)
    );
    receiver.stop().unwrap();
    let too_wide = LinkConfig { lanes: 5, ..link() };
    assert_eq!(
        receiver.configure(too_wide, Packing::Packed),
        Err(Status::INVALID_ARGUMENT)
    );
    let mut small = [0; 3];
    assert_eq!(
        receiver.frame_stream(&mut small),
        Err(Status::BUFFER_TOO_SMALL)
    );
}
