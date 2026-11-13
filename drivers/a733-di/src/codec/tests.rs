//! The deinterlacer as `codec.block/1`, through a real host's job stream of sealed memfds, on
//! a device that records the requests it was given. No node is opened.

use std::sync::{Arc, Mutex};

use lemnos_contract_codec_block::{BufferRef, JobStatus};
use lemnos_core::Lifecycle;
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_host::board::parse;
use lemnos_host::stream::MemfdAllocator;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_linux_sys::dma_buf;
use lemnos_sim::SimBoard;
use std::os::fd::AsFd;

use super::*;
use crate::Settings;

const NV12: u32 = u32::from_le_bytes(*b"NV12");
/// Each stream buffer: room for a 64 by 64 NV12 frame (6144 bytes) and some.
const BYTES: u32 = 8192;

#[derive(Default)]
struct Log {
    opens: u32,
    fail_open: bool,
    refuse: bool,
    progressive: bool,
    settings: Vec<Settings>,
    requests: Vec<uapi::ProcessFbArg>,
    /// The size of the dma-buf behind each request's first input, as the kernel would see it.
    sizes: Vec<u64>,
}

#[derive(Clone, Default)]
struct Played(Arc<Mutex<Log>>);

impl Device for Played {
    fn version(&self) -> io::Result<uapi::Version> {
        Ok(uapi::Version::default())
    }

    fn reset(&self) -> io::Result<()> {
        Ok(())
    }

    fn configure(&self, settings: &Settings) -> io::Result<()> {
        self.0.lock().unwrap().settings.push(*settings);
        Ok(())
    }

    fn check(&self) -> io::Result<()> {
        Ok(())
    }

    fn process(&self, job: &Job<'_>) -> io::Result<i32> {
        let mut log = self.0.lock().unwrap();
        if log.refuse {
            return Err(io::Error::from_raw_os_error(22));
        }
        let request = job.request();
        // The descriptor is open and names a buffer of the stream's size.
        let fd = request.in_fb0.dma_buf_fd;
        assert!(fd >= 0);
        let size = std::fs::metadata(format!("/proc/self/fd/{fd}")).map_or(0, |m| m.len());
        log.sizes.push(size);
        log.requests.push(request);
        Ok(i32::from(log.progressive))
    }
}

fn board(driver: &str, memory: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"di\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/deinterlace\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 8\nbuffer_bytes = {BYTES}\n\
         job_slots = 8\njob_bytes = 24\n"
    )
}

mod nothing {
    use lemnos_core::{Config, Lifecycle, Result, driver};

    /// A driver that leaves its device's stream to the test.
    #[driver("nothing", provides = [])]
    pub struct Nothing {}

    impl Lifecycle for Nothing {
        type Needs = NothingNeeds;

        fn start(_: NothingNeeds, _: &Config<'_>) -> Result<Self> {
            Ok(Self {})
        }
    }
}

fn host(text: &str, handles: bool) -> Host {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let allocator: Arc<dyn lemnos_host::stream::StreamAllocator> = if handles {
        Arc::new(PerBuffer)
    } else {
        Arc::new(MemfdAllocator)
    };
    Host::start_with_streams(
        &parse(text).unwrap(),
        &[entry::<A733Di>(), entry::<nothing::Nothing>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        allocator,
    )
    .unwrap()
}

struct Rig {
    _host: Host,
    driver: A733Di,
    device: Played,
    caller: Caller,
    now: u64,
}

fn config_item(width: u32, height: u32, tail: [u8; 4]) -> Vec<u8> {
    [width.to_le_bytes(), height.to_le_bytes(), tail].concat()
}

/// A frame item: NV12 by default, with these strides and offsets.
fn frame_item(fourcc: u32, strides: (u32, u32), offsets: (u32, u32, u32)) -> Vec<u8> {
    [
        fourcc, strides.0, strides.1, offsets.0, offsets.1, offsets.2,
    ]
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect()
}

fn usual_frame() -> Vec<u8> {
    frame_item(NV12, (64, 64), (0, 4096, 0))
}

impl Rig {
    fn new() -> Self {
        let host = host(&board("nothing", "memory = \"dma-buf\"\n"), true);
        let device = Played::default();
        let opened = device.clone();
        let open = move || {
            let mut log = opened.0.lock().unwrap();
            if log.fail_open {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            log.opens += 1;
            Ok(opened.clone())
        };
        let jobs = Box::new(host.stream_producer(0, "jobs").unwrap());
        let needs = A733DiNeeds {
            jobs: lemnos_core::Held::boxed(jobs),
        };
        let mut driver = A733Di::on(needs, Box::new(DiBlock::new(open)));
        let caller = Caller::attach(&host, 0, "jobs", 1);
        let now = driver.tick(1).unwrap();
        Self {
            _host: host,
            driver,
            device,
            caller,
            now,
        }
    }

    /// Posts a job (work in buffer 0, result in buffer 1), ticks, and returns how it ended
    /// with its read-back.
    fn run(&mut self, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) -> (JobStatus, Vec<u8>) {
        let items: Vec<(u32, &[u8])> = items.iter().map(|(t, b)| (*t, b.as_slice())).collect();
        self.caller.post(4, 0, 1, refs, &items);
        self.now = self.driver.tick(self.now).unwrap();
        let (identity, record, back) = self.caller.completion().expect("a completion");
        assert_eq!(identity, 4);
        (record.status, back)
    }

    /// A bob of a 64 by 64 frame from buffer 2 into buffer 3, with `input` as the input's
    /// frame item.
    fn bob(&mut self, input: Vec<u8>) -> JobStatus {
        let refs = [reference(0, 2, 0, 0), reference(1, 3, 0, 0)];
        let items = [
            (TAG_CONFIG, config_item(64, 64, [mode::BOB, 0, 0, 0])),
            (TAG_FRAME, input),
            (TAG_FRAME, usual_frame()),
        ];
        self.run(&refs, &items).0
    }

    fn requests(&self) -> usize {
        self.device.0.lock().unwrap().requests.len()
    }
}

#[test]
fn a_job_becomes_one_request_of_the_callers_frames_in_the_streams_buffers() {
    let mut rig = Rig::new();
    let refs = [reference(0, 2, 0, 0), reference(1, 3, 0, 0)];
    let items = [
        (
            TAG_CONFIG,
            config_item(
                64,
                64,
                [mode::BOB, flag::TOP_FIELD_FIRST | flag::BASE_BOTTOM, 0, 0],
            ),
        ),
        (TAG_FRAME, frame_item(NV12, (64, 64), (0, 4096, 0))),
        (TAG_FRAME, frame_item(NV12, (128, 128), (0, 4096, 0))),
    ];
    assert_eq!(rig.run(&refs, &items), (JobStatus::Done, vec![0]));
    {
        let log = rig.device.0.lock().unwrap();
        let request = log.requests[0];
        assert_eq!((request.top_field_first, request.base_field), (1, 1));
        assert_eq!(request.in_fb0.format, NV12);
        assert_eq!(
            (request.in_fb0.buf.y_addr, request.in_fb0.buf.cb_addr),
            (0, 4096)
        );
        assert_eq!(
            (request.in_fb0.buf.ystride, request.in_fb0.buf.cstride),
            (64, 64)
        );
        assert_eq!(request.out_dit_fb0.buf.ystride, 128);
        assert_eq!(
            (request.in_fb0.size.width, request.in_fb0.size.height),
            (64, 64)
        );
        // Two different buffers of the stream, each by a descriptor the driver holds.
        assert_ne!(request.in_fb0.dma_buf_fd, request.out_dit_fb0.dma_buf_fd);
        assert_eq!(log.sizes, [u64::from(BYTES)]);
        assert_eq!(log.settings.len(), 1, "the settings were applied once");
        assert_eq!(log.opens, 1);
    }
    assert_eq!(
        dma_buf::size(rig.caller.handle(2).as_fd()).unwrap(),
        u64::from(BYTES)
    );

    // A job the block's motion detector calls progressive says so in its read-back.
    rig.device.0.lock().unwrap().progressive = true;
    assert_eq!(rig.run(&refs, &items), (JobStatus::Done, vec![1]));

    // Motion-adaptive with noise reduction: two inputs, a deinterlaced frame and a
    // noise-reduced one, the outputs planar.
    let planar = frame_item(u32::from_le_bytes(*b"YU12"), (64, 32), (0, 4096, 5120));
    let refs = [
        reference(0, 2, 0, 0),
        reference(1, 3, 0, 0),
        reference(2, 4, 0, 0),
        reference(3, 5, 0, 0),
    ];
    let items = [
        (
            TAG_CONFIG,
            config_item(64, 64, [mode::MOTION, flag::FILM_DETECT, 2, 1]),
        ),
        (TAG_FRAME, usual_frame()),
        (TAG_FRAME, usual_frame()),
        (TAG_FRAME, planar.clone()),
        (TAG_FRAME, planar),
    ];
    assert_eq!(rig.run(&refs, &items).0, JobStatus::Done);
    let mut name = [0u8; 48];
    let len = CodecBlock::block(&mut rig.driver, &mut name).unwrap();
    assert_eq!(&name[..len], b"sunxi-di");
    assert_eq!(rig.requests(), 3);
}

#[test]
fn a_frame_that_does_not_lie_inside_its_buffer_never_reaches_the_kernel() {
    let mut rig = Rig::new();
    let cases = [
        (
            "a chroma plane a byte past the end",
            frame_item(NV12, (64, 64), (0, BYTES - 2047, 0)),
        ),
        (
            "a luma plane past the end",
            frame_item(NV12, (64, 64), (BYTES - 4095, 4096, 0)),
        ),
        (
            "the largest offsets",
            frame_item(NV12, (64, 64), (u32::MAX, u32::MAX, 0)),
        ),
        (
            "a stride that runs the rows past the end",
            frame_item(NV12, (200, 64), (0, 4096, 0)),
        ),
        (
            "a stride shorter than a row",
            frame_item(NV12, (63, 64), (0, 4096, 0)),
        ),
        (
            "a stride past the block's pitch",
            frame_item(NV12, (0x8000, 64), (0, 4096, 0)),
        ),
        (
            "a chroma offset of 0",
            frame_item(NV12, (64, 64), (0, 0, 0)),
        ),
        (
            "a second chroma plane a two-plane format lacks",
            frame_item(NV12, (64, 64), (0, 4096, 6000)),
        ),
        (
            "a format the block doesn't take",
            frame_item(u32::from_le_bytes(*b"RGB3"), (64, 64), (0, 4096, 0)),
        ),
        ("a frame item cut short", usual_frame()[..20].to_vec()),
    ];
    for (what, input) in cases {
        assert_eq!(rig.bob(input), JobStatus::Refused, "{what}");
    }
    // The last byte of the buffer is inside it.
    assert_eq!(
        rig.bob(frame_item(NV12, (64, 64), (0, BYTES - 2048, 0))),
        JobStatus::Done
    );
    assert_eq!(rig.requests(), 1);
}

#[test]
fn work_of_the_wrong_shape_is_refused() {
    let mut rig = Rig::new();
    let two = [reference(0, 2, 0, 0), reference(1, 3, 0, 0)];
    let bob = |tail: [u8; 4], width: u32| (TAG_CONFIG, config_item(width, 64, tail));
    let frame = || (TAG_FRAME, usual_frame());
    type Shape = (&'static str, Vec<BufferRef>, Vec<(u32, Vec<u8>)>);
    let shapes: [Shape; 12] = [
        ("no config", two.to_vec(), vec![frame(), frame()]),
        (
            "a short config",
            two.to_vec(),
            vec![(TAG_CONFIG, vec![0; 8]), frame(), frame()],
        ),
        (
            "a width that is no multiple of 4",
            two.to_vec(),
            vec![bob([mode::BOB, 0, 0, 0], 66), frame(), frame()],
        ),
        (
            "a width past the block's",
            two.to_vec(),
            vec![bob([mode::BOB, 0, 0, 0], 2052), frame(), frame()],
        ),
        (
            "a mode nobody knows",
            two.to_vec(),
            vec![bob([9, 0, 0, 0], 64), frame(), frame()],
        ),
        (
            "a flag nobody knows",
            two.to_vec(),
            vec![bob([mode::BOB, 8, 0, 0], 64), frame(), frame()],
        ),
        (
            "noise reduction with bob",
            two.to_vec(),
            vec![bob([mode::BOB, 0, 1, 0], 64), frame(), frame()],
        ),
        (
            "a level past the last",
            two.to_vec(),
            vec![bob([mode::MOTION, 0, 1, 3], 64), frame(), frame()],
        ),
        (
            "a frame too few",
            two[..1].to_vec(),
            vec![bob([mode::BOB, 0, 0, 0], 64), frame()],
        ),
        (
            "a frame item too few",
            two.to_vec(),
            vec![bob([mode::BOB, 0, 0, 0], 64), frame()],
        ),
        (
            "slots out of order",
            vec![reference(1, 2, 0, 0), reference(0, 3, 0, 0)],
            vec![bob([mode::BOB, 0, 0, 0], 64), frame(), frame()],
        ),
        (
            "a reference with an offset",
            vec![reference(0, 2, 4, 0), reference(1, 3, 0, 0)],
            vec![bob([mode::BOB, 0, 0, 0], 64), frame(), frame()],
        ),
    ];
    for (what, refs, items) in shapes {
        assert_eq!(rig.run(&refs, &items).0, JobStatus::Refused, "{what}");
    }
    assert_eq!(rig.requests(), 0);
    assert_eq!(
        rig.device.0.lock().unwrap().opens,
        0,
        "nothing was opened for any of it"
    );
}

#[test]
fn a_block_that_fails_fails_the_job_and_is_opened_again_later() {
    let mut rig = Rig::new();
    rig.device.0.lock().unwrap().fail_open = true;
    assert_eq!(rig.bob(usual_frame()), JobStatus::Failed);
    assert_eq!(Lifecycle::health(&rig.driver), Health::Degraded(REOPENING));
    rig.device.0.lock().unwrap().fail_open = false;
    assert_eq!(rig.bob(usual_frame()), JobStatus::Failed, "backing off");
    rig.now += 8_000_000_000;
    assert_eq!(rig.bob(usual_frame()), JobStatus::Done);
    assert_eq!(Lifecycle::health(&rig.driver), Health::Ok);
    // A job the kernel refuses as invalid is the job's: the block stays open and healthy.
    rig.device.0.lock().unwrap().refuse = true;
    assert_eq!(rig.bob(usual_frame()), JobStatus::Refused);
    assert_eq!(Lifecycle::health(&rig.driver), Health::Ok);
}

#[test]
fn a_board_file_binds_it_on_a_stream_of_dma_bufs_only() {
    let up = host(&board("a733-di", "memory = \"dma-buf\"\n"), true);
    up.tick();
    assert_eq!(up.device("di").unwrap().state, DeviceState::Running);
    let plain = host(&board("a733-di", ""), false);
    plain.tick();
    assert_ne!(plain.device("di").unwrap().state, DeviceState::Running);
}

#[test]
fn the_config_check_takes_a_device_path_and_refuses_bad_settings() {
    use lemnos_core::{Config, ConfigValue, Driver};
    let entries = [("device", ConfigValue::Str("/dev/a733-di"))];
    assert_eq!(
        <crate::A733Di as Driver>::check_config(&Config::new(&entries)),
        Ok(())
    );
    let bogus = [("bogus", ConfigValue::Int(1))];
    let error = <crate::A733Di as Driver>::check_config(&Config::new(&bogus)).unwrap_err();
    assert_eq!(error.key, "bogus");
    let near = [("devise", ConfigValue::Str("/dev/a733-di"))];
    let error = <crate::A733Di as Driver>::check_config(&Config::new(&near)).unwrap_err();
    assert_eq!(error.suggestion, Some("device"));
    let wrong = [("device", ConfigValue::Bool(true))];
    let error = <crate::A733Di as Driver>::check_config(&Config::new(&wrong)).unwrap_err();
    assert_eq!(error.key, "device");
    let empty = [("device", ConfigValue::Str(""))];
    let error = <crate::A733Di as Driver>::check_config(&Config::new(&empty)).unwrap_err();
    assert_eq!(error.reason, "must not be empty");
}
