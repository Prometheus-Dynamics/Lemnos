use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, Counters, DATA_OFFSET, EVENT_BYTES, EndpointInfo, Event,
    EventKind, FunctionInfo, JOB_BYTES, Job, JobKind, JobStatus, Request, State, UsbFunction,
};
use lemnos_core::{Contract, Decode, Encode, Reader, Status, Writer};

const INFO: lemnos_core::ContractInfo = <dyn UsbFunction as Contract>::INFO;

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
    assert_eq!(INFO.tag.as_str(), "usb.function/1");
    let methods: Vec<(&str, u16)> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        methods,
        [
            ("info", 1),
            ("endpoint", 2),
            ("data_stream", 3),
            ("state", 4),
            ("control_in", 5),
            ("control_out", 6),
            ("control_stall", 7),
            ("set_halt", 8),
            ("cancel", 9),
            ("counters", 10),
            ("ifname", 11),
        ]
    );
}

#[test]
fn a_job_and_an_event_are_the_sizes_the_contract_names() {
    let job = Job {
        identity: 0x0102_0304_0506_0708,
        kind: JobKind::Receive,
        endpoint: 0x02,
        buffer: 7,
        length: 512,
    };
    assert_eq!(round_trip(job), JOB_BYTES);
    assert_eq!(
        encoded(&job),
        [
            18, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 2, 0x02, 7, 0, 0, 0, 0, 2, 0, 0
        ]
    );
    let event = Event {
        kind: EventKind::Setup,
        request: Request {
            request_type: 0xc0,
            request: 9,
            value: 0x1234,
            index: 2,
            length: 64,
        },
    };
    assert_eq!(round_trip(event), EVENT_BYTES);
    assert_eq!(
        encoded(&event),
        [13, 0, 0, 0, 5, 8, 0, 0, 0, 0xc0, 9, 0x34, 0x12, 2, 0, 64, 0]
    );
    round_trip(FunctionInfo::default());
    round_trip(EndpointInfo::default());
    round_trip(State::default());
    round_trip(Counters::default());
}

#[test]
fn a_kind_no_version_knows_is_refused() {
    for bytes in [[0u8], [4], [0xff]] {
        assert_eq!(
            JobKind::decode(&mut Reader::new(&bytes)),
            Err(Status::INVALID_ARGUMENT)
        );
    }
    assert_eq!(
        EventKind::decode(&mut Reader::new(&[6])),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_completion_record_fits_before_the_data() {
    let done = Completion {
        status: JobStatus::Done,
        count: 0x0102_0304,
    };
    assert_eq!(round_trip(done), COMPLETION_BYTES);
    assert_eq!(encoded(&done), [5, 0, 0, 0, 0, 4, 3, 2, 1]);
    const { assert!(COMPLETION_BYTES <= DATA_OFFSET) };
    for (status, byte) in [
        (JobStatus::Refused, 1),
        (JobStatus::NotConfigured, 2),
        (JobStatus::Cancelled, 3),
        (JobStatus::Failed, 4),
    ] {
        assert_eq!(encoded(&Completion { status, count: 0 })[4], byte);
    }
    assert_eq!(
        Completion::decode(&mut Reader::new(&[5, 0, 0, 0, 5, 0, 0, 0, 0])),
        Err(Status::INVALID_ARGUMENT)
    );
}
