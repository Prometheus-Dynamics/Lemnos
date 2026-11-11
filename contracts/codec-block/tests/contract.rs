use lemnos_contract_codec_block::work::{Builder, Work};
use lemnos_contract_codec_block::{
    BUFFER_REF_BYTES, BlockInfo, BufferRef, COMPLETION_BYTES, CodecBlock, Completion, Counters,
    DATA_OFFSET, ITEM_HEADER_BYTES, ItemHeader, JOB_BYTES, Job, JobStatus, WORK_HEADER_BYTES,
    WorkHeader,
};
use lemnos_core::{Contract, Decode, Encode, Reader, Status, Writer};

const INFO: lemnos_core::ContractInfo = <dyn CodecBlock as Contract>::INFO;

fn encoded<T: Encode>(value: &T) -> Vec<u8> {
    let mut bytes = [0u8; 64];
    let mut writer = Writer::new(&mut bytes);
    value.encode(&mut writer).unwrap();
    let len = writer.len();
    bytes[..len].to_vec()
}

fn round_trip<T: Encode + for<'a> Decode<'a> + PartialEq + core::fmt::Debug>(value: T) -> usize {
    let bytes = encoded(&value);
    let mut reader = Reader::new(&bytes);
    assert_eq!(T::decode(&mut reader).unwrap(), value);
    assert_eq!(reader.remaining(), 0);
    bytes.len()
}

#[test]
fn the_tag_and_the_method_numbers_are_the_contract() {
    assert_eq!(INFO.tag.as_str(), "codec.block/1");
    let methods: Vec<(&str, u16)> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        methods,
        [
            ("info", 1),
            ("block", 2),
            ("job_stream", 3),
            ("counters", 4)
        ]
    );
}

#[test]
fn every_record_is_the_size_the_contract_names() {
    let job = Job {
        identity: 0x0102_0304_0506_0708,
        work: 3,
        work_bytes: 0x200,
        result: 4,
    };
    assert_eq!(round_trip(job), JOB_BYTES);
    assert_eq!(
        encoded(&job),
        [8, 7, 6, 5, 4, 3, 2, 1, 3, 0, 0, 0, 0, 2, 0, 0, 4, 0, 0, 0]
    );
    assert_eq!(
        round_trip(WorkHeader { refs: 2, items: 3 }),
        WORK_HEADER_BYTES
    );
    let reference = BufferRef {
        slot: 1,
        buffer: 2,
        offset: 3,
        length: 4,
    };
    assert_eq!(round_trip(reference), BUFFER_REF_BYTES);
    assert_eq!(
        round_trip(ItemHeader { tag: 9, bytes: 1 }),
        ITEM_HEADER_BYTES
    );
    let done = Completion {
        status: JobStatus::Failed,
        count: 0x0102_0304,
    };
    assert_eq!(round_trip(done), COMPLETION_BYTES);
    assert_eq!(encoded(&done), [2, 4, 3, 2, 1]);
    const { assert!(COMPLETION_BYTES <= DATA_OFFSET) };
    assert_eq!(
        Completion::decode(&mut Reader::new(&[3, 0, 0, 0, 0])),
        Err(Status::INVALID_ARGUMENT)
    );
    round_trip(BlockInfo::default());
    round_trip(Counters::default());
}

fn sample(out: &mut [u8]) -> usize {
    let mut work = Builder::new(out).unwrap();
    work.reference(BufferRef {
        slot: 64,
        buffer: 1,
        offset: 16,
        length: 0,
    })
    .unwrap();
    work.reference(BufferRef {
        slot: 65,
        buffer: 2,
        offset: 0,
        length: 100,
    })
    .unwrap();
    work.item(7, &[1, 2, 3]).unwrap();
    work.item(8, &[]).unwrap();
    work.item(9, &[0xaa; 5]).unwrap();
    work.finish()
}

#[test]
fn work_reads_back_as_it_was_written() {
    let mut bytes = [0u8; 128];
    let len = sample(&mut bytes);
    assert_eq!(
        len,
        WORK_HEADER_BYTES + 2 * BUFFER_REF_BYTES + 3 * ITEM_HEADER_BYTES + 8
    );
    let work = Work::parse(&bytes[..len]).unwrap();
    assert_eq!((work.ref_count(), work.item_count()), (2, 3));
    let refs: Vec<BufferRef> = work.refs().collect();
    assert_eq!(
        (refs[0].slot, refs[0].offset, refs[1].buffer, refs[1].length),
        (64, 16, 2, 100)
    );
    let items: Vec<(u32, &[u8])> = work.items().collect();
    assert_eq!(
        items,
        [(7, &[1u8, 2, 3][..]), (8, &[][..]), (9, &[0xaa; 5][..])]
    );
    // Work with nothing in it is still work.
    let mut empty = [0u8; WORK_HEADER_BYTES];
    let len = Builder::new(&mut empty).unwrap().finish();
    let work = Work::parse(&empty[..len]).unwrap();
    assert_eq!((work.ref_count(), work.item_count()), (0, 0));
}

#[test]
fn bytes_that_are_not_work_are_refused_whole() {
    let mut bytes = [0u8; 128];
    let len = sample(&mut bytes);
    // Cut anywhere, or with a byte left over, it is not work.
    for cut in 0..len {
        assert!(Work::parse(&bytes[..cut]).is_none(), "cut at {cut}");
    }
    assert!(Work::parse(&bytes[..len + 1]).is_none());
    // A count that claims more than is there.
    let mut more = bytes;
    more[0] = 3;
    assert!(Work::parse(&more[..len]).is_none());
    let mut more = bytes;
    more[2] = 4;
    assert!(Work::parse(&more[..len]).is_none());
    // An item length that runs past the end, up to the largest there is.
    let first_item = WORK_HEADER_BYTES + 2 * BUFFER_REF_BYTES;
    for size in [4u32, 100, u32::MAX] {
        let mut long = bytes;
        long[first_item + 4..first_item + 8].copy_from_slice(&size.to_le_bytes());
        assert!(Work::parse(&long[..len]).is_none(), "{size}");
    }
    // The largest counts, with nothing behind them.
    assert!(Work::parse(&[0xff, 0xff, 0xff, 0xff]).is_none());
}

#[test]
fn a_builder_never_writes_past_its_buffer() {
    assert!(Builder::new(&mut [0u8; WORK_HEADER_BYTES - 1]).is_none());
    let mut small = [0u8; WORK_HEADER_BYTES + BUFFER_REF_BYTES + ITEM_HEADER_BYTES + 2];
    let mut work = Builder::new(&mut small).unwrap();
    work.reference(BufferRef::default()).unwrap();
    assert!(work.item(1, &[0; 3]).is_none(), "one byte too many");
    work.item(1, &[0; 2]).unwrap();
    assert!(work.item(2, &[]).is_none(), "no room for a header");
    assert!(
        work.reference(BufferRef::default()).is_none(),
        "references come before items"
    );
    let len = work.finish();
    assert_eq!(len, small.len());
    assert!(Work::parse(&small).is_some());
}
