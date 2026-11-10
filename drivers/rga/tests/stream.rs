//! The driver bound by the host from a board file, its three job streams declared there: a
//! consumer finds a stream by the name `job_stream` answers, attaches on the real ring, posts a
//! job, and the host's own ticks run it. No `soc` is configured, so no device is opened and the
//! jobs run on the CPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lemnos_contract_image_convert::{ConvertJob, ImageRef};
use lemnos_core::{CallerId, Encode, HostClock, MethodId, Writer};
use lemnos_driver_rga::Rga;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::SimBoard;
use lemnos_wire::ring::Consumer;

#[derive(Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl HostClock for Clock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

const RGB3: u32 = u32::from_le_bytes(*b"RGB3");
const AR24: u32 = u32::from_le_bytes(*b"AR24");

fn start() -> (Host, Clock, u32) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"rga\"\ndriver = \"rga\"\n\
                [devices.streams.convert]\nbuffers = 4\nbuffer_bytes = 4096\n\
                job_slots = 8\njob_bytes = 64\n";
    let clock = Clock::default();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Rga>()],
        Box::new(SimBoard::new()),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    let index = host.device("rga").unwrap().index;
    (host, clock, index)
}

/// `job_stream` of contract `contract`: the name, or the status.
fn job_stream(host: &Host, index: u32, contract: u8) -> Result<String, lemnos_core::Status> {
    let mut reply = [0u8; 64];
    let id = MethodId {
        contract,
        method: 4,
    };
    // The call carries the capacity of the name's buffer.
    let mut args = [0u8; 4];
    let mut writer = Writer::new(&mut args);
    (reply.len() as u32).encode(&mut writer).unwrap();
    let n = host.call(index, id, &args, &mut reply)?;
    Ok(String::from_utf8(reply[..n].to_vec()).unwrap())
}

#[test]
fn a_job_posted_on_the_stream_the_board_declares_is_run_by_the_hosts_ticks() {
    let (host, clock, index) = start();
    assert_eq!(host.device("rga").unwrap().state, DeviceState::Running);
    // Only the stream the board gives is offered.
    let name = job_stream(&host, index, 1).unwrap();
    assert_eq!(name, "convert");
    assert!(job_stream(&host, index, 0).is_err());
    assert!(job_stream(&host, index, 2).is_err());

    let lease = host.stream_attach(index, "convert", CallerId(5)).unwrap();
    let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
    }
    let offset = lease.layout.buffer_offset(0).unwrap();
    assert!(lease.buffers.write(offset, &[1, 2, 3, 4, 5, 6]));
    let job = ConvertJob {
        identity: 77,
        input: ImageRef {
            buffer: 0,
            width: 2,
            height: 1,
            fourcc: RGB3,
            stride: 6,
        },
        output: ImageRef {
            buffer: 1,
            width: 2,
            height: 1,
            fourcc: AR24,
            stride: 8,
        },
    };
    let mut bytes = [0u8; 64];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    assert!(consumer.push_job(&bytes[..used]).unwrap());
    let mut done = None;
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
        done = consumer.pop_frame().unwrap();
        if done.is_some() {
            break;
        }
    }
    let done = done.expect("a completion from the host's ticks");
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 77, 8));
    let mut out = [0u8; 8];
    let offset = lease.layout.buffer_offset(1).unwrap();
    assert!(lease.buffers.read(offset, &mut out));
    assert_eq!(out, [3, 2, 1, 255, 6, 5, 4, 255]);
}
