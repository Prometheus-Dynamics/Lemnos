//! The driver bound by the host from a board file, its job stream declared there: a consumer
//! finds a stream by the name `job_stream` answers, attaches on the real ring, posts a job, and
//! the host's own ticks complete it. The chip is RK3576, whose only unit is `/dev/rga`: a
//! conversion from RGB has no route on it, so the job completes as failed before any device is
//! looked for, and nothing is written to its output. Without a `soc` the driver doesn't bind.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lemnos_contract_image_convert::{ConvertJob, Counters, ImageRef};
use lemnos_core::{CallerId, Decode, Encode, HostClock, MethodId, Reader, Writer};
use lemnos_driver_rga::Rga;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, entry};
use lemnos_sim::SimBoard;

#[derive(Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl HostClock for Clock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

const RGB3: u32 = u32::from_le_bytes(*b"RGB3");
const AR24: u32 = u32::from_le_bytes(*b"AR24");

fn start(config: &str) -> (Host, Clock, u32) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"rga\"\ndriver = \"rga\"\n{config}\
         [devices.streams.convert]\nbuffers = 4\nbuffer_bytes = 4096\n\
         job_slots = 8\njob_bytes = 64\n"
    );
    let clock = Clock::default();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Rga>()],
        Box::new(SimBoard::new()),
        Arc::new(clock.clone()),
        lemnos_test_options::options(),
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
fn without_a_soc_the_device_is_listed_failed_and_never_made() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"rga\"\ndriver = \"rga\"\n";
    // The board starts; the device is never made, and is listed failed with the key.
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Rga>()],
        Box::new(SimBoard::new()),
        Arc::new(Clock::default()),
        lemnos_test_options::options(),
    )
    .expect("the board starts");
    let state = host.device("rga").unwrap().state;
    assert!(
        matches!(&state, DeviceState::Backoff { reason, .. } if reason.contains("setting \"soc\"")),
        "{state:?}"
    );
}

#[test]
fn a_job_with_no_route_on_the_chip_completes_as_failed_through_the_hosts_ticks() {
    let (host, clock, index) = start("config = { soc = \"rk3576\" }\n");
    assert_eq!(host.device("rga").unwrap().state, DeviceState::Running);
    // Only the stream the board gives is offered.
    let name = job_stream(&host, index, 1).unwrap();
    assert_eq!(name, "convert");
    assert!(job_stream(&host, index, 0).is_err());
    assert!(job_stream(&host, index, 2).is_err());

    let lease = host.stream_attach(index, "convert", CallerId(5)).unwrap();
    let mut consumer = lease.consumer_writable().unwrap();
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
    }
    assert!(consumer.write(0, 0, &[1, 2, 3, 4, 5, 6]));
    assert!(consumer.write(1, 0, &[0x5a; 8]));
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
    assert!(consumer.jobs().post(&bytes[..used]).unwrap());
    let mut done = None;
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
        done = consumer
            .pop_entry()
            .unwrap()
            .and_then(|entry| entry.frame());
        if done.is_some() {
            break;
        }
    }
    let done = done.expect("a completion from the host's ticks");
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 77, 0));
    // Nothing was computed in software.
    let mut out = [0u8; 8];
    assert!(consumer.read(1, 0, &mut out));
    assert_eq!(out, [0x5a; 8]);
    // Counted as an error of the contract.
    let id = MethodId {
        contract: 1,
        method: 5,
    };
    let mut reply = [0u8; 64];
    let n = host.call(index, id, &[], &mut reply).unwrap();
    let counters = Counters::decode(&mut Reader::new(&reply[..n])).unwrap();
    assert_eq!((counters.jobs, counters.errors), (0, 1));
}

/// A result is released and named again in a new job, on the host's own ticks: the job is served
/// as the first was, and the stream is not faulted. A control for the codec job server, which
/// must give the same answer on the same order.
#[test]
fn a_result_released_and_named_again_is_completed_again_through_the_hosts_ticks() {
    let (host, clock, index) = start("config = { soc = \"rk3576\" }\n");
    let lease = host.stream_attach(index, "convert", CallerId(5)).unwrap();
    let mut consumer = lease.consumer_writable().unwrap();
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
    }
    assert!(consumer.write(0, 0, &[1, 2, 3, 4, 5, 6]));
    let job = |identity: u64| ConvertJob {
        identity,
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
    for identity in [77u64, 78] {
        let mut bytes = [0u8; 64];
        let mut writer = Writer::new(&mut bytes);
        job(identity).encode(&mut writer).unwrap();
        let used = writer.len();
        assert!(consumer.jobs().post(&bytes[..used]).unwrap());
        let mut done = None;
        for _ in 0..4 {
            clock.0.fetch_add(1_000_000, Ordering::Relaxed);
            host.tick();
            done = consumer
                .pop_entry()
                .unwrap()
                .and_then(|entry| entry.frame());
            if done.is_some() {
                break;
            }
        }
        let done = done.expect("a completion from the host's ticks");
        assert_eq!((done.buffer, done.identity), (1, identity));
        // Released, so the next job may name the buffer again.
        assert!(consumer.release(done.buffer));
    }
    assert_eq!(host.stream_stats(index, "convert").unwrap().faults, 0);
}

/// How many descriptors this process holds on the memfds with these inodes. Counts only the
/// stream's own memfds, so another test running alongside does not change the answer.
fn held_of(inodes: &[u64]) -> usize {
    use std::os::unix::fs::MetadataExt;
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| std::fs::metadata(entry.unwrap().path()).ok())
        .filter(|meta| inodes.contains(&meta.ino()))
        .count()
}

/// The inode of each descriptor's memfd: its identity, which no other memfd of the process has.
fn inodes_of(fds: &[std::os::fd::OwnedFd]) -> Vec<u64> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    fds.iter()
        .map(|fd| {
            std::fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                .unwrap()
                .ino()
        })
        .collect()
}

/// A control, not a regression test: on this board the job completes as failed before any
/// handle is taken, and the convert stream is plain shared memory, so the driver never holds a
/// dma-buf and the stream holds nothing at origin or here.
#[test]
fn a_finished_job_leaves_the_convert_stream_holding_nothing_once_the_consumer_detaches() {
    let (host, clock, index) = start("config = { soc = \"rk3576\" }\n");
    let lease = host.stream_attach(index, "convert", CallerId(5)).unwrap();
    let inodes = inodes_of(lease.handles.as_deref().unwrap_or(&[]));
    let mut consumer = lease.consumer_writable().unwrap();
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
    }
    assert!(consumer.write(0, 0, &[1, 2, 3, 4, 5, 6]));
    assert!(consumer.write(1, 0, &[0x5a; 8]));
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
    assert!(consumer.jobs().post(&bytes[..used]).unwrap());
    for _ in 0..4 {
        clock.0.fetch_add(1_000_000, Ordering::Relaxed);
        host.tick();
        if consumer.pop_entry().unwrap().is_some() {
            break;
        }
    }
    drop(consumer);
    host.stream_detach(index, "convert", CallerId(5)).unwrap();
    let held = held_of(&inodes);
    assert_eq!(
        held, 0,
        "driver stream convert still holds {held} memfds after its consumer detached"
    );
    let stats = host.stream_stats(index, "convert").unwrap();
    assert_eq!(
        (stats.standby_bytes, stats.held_by_driver),
        (0, 0),
        "{stats:?}"
    );
}
