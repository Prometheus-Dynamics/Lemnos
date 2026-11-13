use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput,
    MAX_OUTPUTS, MAX_STREAM_NAME, NO_BUFFER, OutputLayout,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn CameraIsp as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: CameraIsp> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "camera.isp/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("config_layout", 2),
            ("set_input", 3),
            ("set_output", 4),
            ("clear_output", 5),
            ("config_stream", 6),
            ("statistics_stream", 7),
            ("input_stream", 8),
            ("output_stream", 9),
            ("start", 10),
            ("stop", 11),
            ("counters", 12),
            ("input_layout", 13),
            ("output_layout", 14),
        ]
    );
    for i in [5, 6, 7, 8] {
        assert!(INFO.methods[i].has_out_buffer, "{}", INFO.methods[i].name);
    }
}

fn job() -> IspJob {
    IspJob {
        identity: u64::MAX,
        layout: 7,
        config_buffer: 1,
        input_buffer: NO_BUFFER,
        outputs: [2, NO_BUFFER, 3, 4],
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(job());
    round_trip(IspInfo {
        max_outputs: MAX_OUTPUTS as u8,
        max_width: 8192,
        max_height: 8192,
        config_layouts: 2,
        statistics_layout: 9,
        statistics_bytes: 16_000,
    });
    round_trip(ConfigLayout {
        layout: 4,
        bytes: 16_000,
    });
    round_trip(IspInput {
        width: 1920,
        height: 1080,
        bit_depth: 12,
        bayer: BayerOrder::Gbrg,
    });
    round_trip(IspOutput {
        width: 1920,
        height: 1080,
        fourcc: u32::from_le_bytes(*b"NV12"),
        pad_bytes: 256,
    });
    round_trip(OutputLayout {
        stride: 1920,
        size: 3_110_400,
    });
    round_trip(Counters {
        jobs: 1,
        dropped: 2,
        errors: u64::MAX,
    });
}

#[test]
fn a_job_is_a_fixed_small_record() {
    assert_eq!(
        max_len(&IspJob::SHAPE),
        Some(8 + 4 + 4 + 4 + 4 * MAX_OUTPUTS)
    );
}

#[test]
fn a_bad_bayer_in_an_input_is_refused() {
    let mut bytes = [0u8; 10];
    bytes[9] = 200;
    assert_eq!(
        IspInput::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[derive(Default)]
struct Mock {
    input: Option<IspInput>,
    outputs: [Option<IspOutput>; MAX_OUTPUTS],
    streaming: bool,
}

impl CameraIsp for Mock {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: 2,
            config_layouts: 1,
            ..IspInfo::default()
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index == 0 {
            Ok(ConfigLayout {
                layout: 5,
                bytes: 16_000,
            })
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        if self.streaming {
            return Err(Status::BUSY);
        }
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        if output >= 2 {
            return Err(Status::NOT_FOUND);
        }
        self.outputs[usize::from(output)] = Some(image);
        Ok(OutputLayout {
            stride: image.width,
            size: image.width * image.height + image.pad_bytes,
        })
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        let slot = self
            .outputs
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)?;
        *slot = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        copy_name("config", into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        copy_name("statistics", into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        match output {
            0 => copy_name("output0", into),
            1 => copy_name("output1", into),
            _ => Err(Status::NOT_FOUND),
        }
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

    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Ok(self.input.map(|i| OutputLayout {
            stride: i.width * 2,
            size: i.width * 2 * i.height,
        }))
    }

    fn output_layout(&mut self, output: u8) -> Result<Option<OutputLayout>> {
        if output >= 2 {
            return Err(Status::NOT_FOUND);
        }
        Ok(self.outputs[usize::from(output)].map(|o| OutputLayout {
            stride: o.width,
            size: o.width * o.height + o.pad_bytes,
        }))
    }
}

fn client() -> Bound<dyn CameraIsp, Direct<Mock>> {
    Bound::new(Direct {
        driver: Mock::default(),
        args: [0; LOCAL_ARGS],
    })
}

#[test]
fn a_caller_sets_up_and_finds_every_stream() {
    let mut isp = client();
    assert_eq!(isp.config_layout(0).unwrap().layout, 5);
    assert_eq!(isp.config_layout(1), Err(Status::NOT_FOUND));
    let input = IspInput {
        width: 1920,
        height: 1080,
        bit_depth: 10,
        bayer: BayerOrder::Rggb,
    };
    assert_eq!(isp.set_input(input), Ok(()));
    let output = IspOutput {
        width: 100,
        height: 10,
        fourcc: 1,
        pad_bytes: 24,
    };
    assert_eq!(isp.set_output(1, output).unwrap().size, 1024);
    assert_eq!(isp.set_output(2, output), Err(Status::NOT_FOUND));

    let mut name = [0; MAX_STREAM_NAME];
    let len = isp.config_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"config");
    let len = isp.statistics_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"statistics");
    let len = isp.output_stream(1, &mut name).unwrap();
    assert_eq!(&name[..len], b"output1");
    assert_eq!(isp.output_stream(9, &mut name), Err(Status::NOT_FOUND));
    assert_eq!(isp.input_stream(&mut name), Err(Status::NOT_SUPPORTED));
}

#[test]
fn a_name_longer_than_the_buffer_is_refused() {
    let mut isp = client();
    let mut small = [0; 4];
    assert_eq!(
        isp.statistics_stream(&mut small),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

#[test]
fn input_cannot_change_while_streaming() {
    let mut isp = client();
    isp.start().unwrap();
    assert_eq!(isp.set_input(IspInput::default()), Err(Status::BUSY));
    assert_eq!(isp.stop(), Ok(()));
    assert_eq!(isp.set_input(IspInput::default()), Ok(()));
}

#[test]
fn layouts_answer_what_was_set_and_none_until_then() {
    let mut c = client();
    assert_eq!(c.input_layout(), Ok(None));
    assert_eq!(c.output_layout(0), Ok(None));
    assert_eq!(c.output_layout(2), Err(Status::NOT_FOUND));
    c.set_input(IspInput {
        width: 640,
        height: 480,
        bit_depth: 10,
        bayer: BayerOrder::default(),
    })
    .unwrap();
    c.set_output(
        1,
        IspOutput {
            width: 640,
            height: 480,
            fourcc: 0,
            pad_bytes: 16,
        },
    )
    .unwrap();
    assert_eq!(
        c.input_layout(),
        Ok(Some(OutputLayout {
            stride: 1280,
            size: 614_400
        }))
    );
    assert_eq!(
        c.output_layout(1),
        Ok(Some(OutputLayout {
            stride: 640,
            size: 307_216
        }))
    );
    assert_eq!(c.output_layout(0), Ok(None));
    c.clear_output(1).unwrap();
    assert_eq!(c.output_layout(1), Ok(None));
}
