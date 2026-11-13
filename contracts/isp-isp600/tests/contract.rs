use lemnos_contract_isp_isp600::{
    Isp600Info, IspIsp600, LAYOUT_LOAD, LAYOUT_STATISTICS, LOAD_BAYER_TABLE, LOAD_FRONT_END_TABLE,
    LOAD_REGIONS, LOAD_REGISTERS, LOAD_RGB_TABLE, LOAD_YUV_TABLE, OUTPUT_0, Region, STATISTICS_AE,
    STATISTICS_AF_FIR_COUNT, STATISTICS_AF_FIR_SUM, STATISTICS_AF_HIGHLIGHT_COUNT,
    STATISTICS_AF_IIR_COUNT, STATISTICS_AF_IIR_SUM, STATISTICS_AFS, STATISTICS_AWB,
    STATISTICS_DENOISE_K, STATISTICS_HISTOGRAM_0, STATISTICS_HISTOGRAM_1, STATISTICS_REGIONS,
    STATISTICS_REGISTERS, STATISTICS_RES, STATISTICS_TONE_LUMINANCE, STATISTICS_TONE_PKX,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};

const INFO: lemnos_core::ContractInfo = <dyn IspIsp600 as Contract>::INFO;

/// A caller that reaches a mock driver the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: IspIsp600> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "isp.isp600/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [("info", 1), ("load_region", 2), ("statistics_region", 3)]
    );
    assert_eq!(INFO.methods[1].params[0].shape, Shape::U8);
    assert_eq!(INFO.methods[2].params[0].shape, Shape::U8);
}

#[test]
fn layouts_and_region_numbers_are_distinct_and_dense() {
    assert_eq!(OUTPUT_0, 0);
    assert_ne!(LAYOUT_LOAD, LAYOUT_STATISTICS);
    assert_eq!(
        [
            LOAD_REGISTERS,
            LOAD_FRONT_END_TABLE,
            LOAD_BAYER_TABLE,
            LOAD_RGB_TABLE,
            LOAD_YUV_TABLE,
            LOAD_REGIONS,
        ],
        [0, 1, 2, 3, 4, 5]
    );
    assert_eq!(
        [
            STATISTICS_REGISTERS,
            STATISTICS_AE,
            STATISTICS_RES,
            STATISTICS_AF_IIR_SUM,
            STATISTICS_AF_FIR_SUM,
            STATISTICS_AF_IIR_COUNT,
            STATISTICS_AF_FIR_COUNT,
            STATISTICS_AF_HIGHLIGHT_COUNT,
            STATISTICS_AWB,
            STATISTICS_AFS,
            STATISTICS_HISTOGRAM_0,
            STATISTICS_HISTOGRAM_1,
            STATISTICS_TONE_LUMINANCE,
            STATISTICS_TONE_PKX,
            STATISTICS_DENOISE_K,
            STATISTICS_REGIONS,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
}

#[test]
fn the_info_and_a_region_round_trip() {
    round_trip(Isp600Info {
        layout_version: 1,
        load_bytes: 0x7700,
        statistics_bytes: u32::MAX,
        load_regions: 5,
        statistics_regions: 15,
        settle_frames: 2,
    });
    round_trip(Region {
        offset: 0x5a80,
        bytes: u32::MAX,
    });
}

struct Mock;

impl IspIsp600 for Mock {
    fn info(&mut self) -> Result<Isp600Info> {
        Ok(Isp600Info {
            load_regions: LOAD_REGIONS,
            ..Isp600Info::default()
        })
    }

    fn load_region(&mut self, region: u8) -> Result<Region> {
        if region >= LOAD_REGIONS {
            return Err(Status::NOT_FOUND);
        }
        Ok(Region {
            offset: u32::from(region) * 16,
            bytes: 16,
        })
    }

    fn statistics_region(&mut self, region: u8) -> Result<Region> {
        if region >= STATISTICS_REGIONS {
            return Err(Status::NOT_FOUND);
        }
        Ok(Region {
            offset: u32::from(region),
            bytes: 1,
        })
    }
}

#[test]
fn a_caller_reaches_the_driver() {
    let mut isp: Bound<dyn IspIsp600, Direct<Mock>> = Bound::new(Direct {
        driver: Mock,
        args: [0; LOCAL_ARGS],
    });
    assert_eq!(isp.info().unwrap().load_regions, 5);
    assert_eq!(
        isp.load_region(LOAD_YUV_TABLE),
        Ok(Region {
            offset: 64,
            bytes: 16
        })
    );
    assert_eq!(isp.load_region(LOAD_REGIONS), Err(Status::NOT_FOUND));
    assert_eq!(
        isp.statistics_region(STATISTICS_DENOISE_K).unwrap().offset,
        14
    );
    assert_eq!(
        isp.statistics_region(STATISTICS_REGIONS),
        Err(Status::NOT_FOUND)
    );
}
