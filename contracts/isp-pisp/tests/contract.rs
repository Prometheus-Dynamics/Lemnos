use lemnos_contract_isp_pisp::{
    IspPisp, LAYOUT_BACK_END, LAYOUT_FRONT_END, LAYOUT_STATISTICS, OUTPUT_BACK_END_0,
    OUTPUT_BACK_END_1, OUTPUT_FRONT_END_0, OUTPUT_FRONT_END_1, PispInfo,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn IspPisp as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: IspPisp> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "isp.pisp/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("set_front_end_compression", 2),
            ("enable_temporal_denoise", 3),
            ("reset_temporal_denoise", 4),
            ("set_line_event", 5),
        ]
    );
    assert_eq!(INFO.methods[1].params[1].shape, Shape::Bool);
    assert_eq!(INFO.methods[4].params[0].shape, Shape::U32);
}

#[test]
fn outputs_and_layouts_are_distinct() {
    let outputs = [
        OUTPUT_BACK_END_0,
        OUTPUT_BACK_END_1,
        OUTPUT_FRONT_END_0,
        OUTPUT_FRONT_END_1,
    ];
    assert_eq!(outputs, [0, 1, 2, 3]);
    assert_ne!(LAYOUT_BACK_END, LAYOUT_FRONT_END);
    assert_ne!(LAYOUT_FRONT_END, LAYOUT_STATISTICS);
}

#[test]
fn the_info_round_trips() {
    round_trip(PispInfo {
        hw_version: 0x1234,
        queue_depth: 8,
        front_end_config_bytes: 1,
        back_end_config_bytes: 16_700,
        statistics_bytes: u32::MAX,
        stalled: u64::MAX,
    });
}

#[derive(Default)]
struct Mock {
    compressed: [bool; 2],
    tdn: Option<bool>,
    resets: u32,
    line: Option<u32>,
}

impl IspPisp for Mock {
    fn info(&mut self) -> Result<PispInfo> {
        Ok(PispInfo {
            queue_depth: 8,
            ..PispInfo::default()
        })
    }

    fn set_front_end_compression(&mut self, output: u8, compressed: bool) -> Result<()> {
        let index = match output {
            OUTPUT_FRONT_END_0 => 0,
            OUTPUT_FRONT_END_1 => 1,
            _ => return Err(Status::NOT_FOUND),
        };
        self.compressed[index] = compressed;
        Ok(())
    }

    fn enable_temporal_denoise(&mut self, compressed: bool) -> Result<()> {
        self.tdn = Some(compressed);
        Ok(())
    }

    fn reset_temporal_denoise(&mut self) -> Result<()> {
        self.resets += 1;
        Ok(())
    }

    fn set_line_event(&mut self, line: u32) -> Result<()> {
        self.line = Some(line);
        Ok(())
    }
}

#[test]
fn a_caller_reaches_the_driver() {
    let mut pisp: Bound<dyn IspPisp, Direct<Mock>> = Bound::new(Direct {
        driver: Mock::default(),
        args: [0; LOCAL_ARGS],
    });
    assert_eq!(pisp.info().unwrap().queue_depth, 8);
    assert_eq!(
        pisp.set_front_end_compression(OUTPUT_FRONT_END_1, true),
        Ok(())
    );
    assert!(pisp.caller.driver.compressed[1]);
    assert_eq!(
        pisp.set_front_end_compression(OUTPUT_BACK_END_0, true),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(pisp.enable_temporal_denoise(true), Ok(()));
    assert_eq!(pisp.caller.driver.tdn, Some(true));
    assert_eq!(pisp.reset_temporal_denoise(), Ok(()));
    assert_eq!(pisp.caller.driver.resets, 1);
    assert_eq!(pisp.set_line_event(200), Ok(()));
    assert_eq!(pisp.caller.driver.line, Some(200));
}
