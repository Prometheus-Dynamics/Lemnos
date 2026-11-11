//! The driver as a host runs it: bound from a board file, its methods called through the host
//! by callers, its runs posted by a consumer on the host's own frame stream and completed there,
//! against the played kernel. No device node is opened.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_npu::{Counters, NpuInfo, RunJob, RunResult, RunStatus};
use lemnos_core::{
    CallerId, Decode, Driver, Encode, Health, MethodId, Reader, Result, Status, Writer,
};
use lemnos_host::board::parse;
use lemnos_host::stream::Lease;
use lemnos_host::{Binder, DeviceState, DriverEntry, Host, Instance, Options, entry};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::Consumer;

use super::{Device, Framed, Kernel, Node, handles_in, job};
use crate::driver::{BoxedSession, Closed, Opener, POLL_NS};
use crate::{MAX_BUFFERS, Rknpu, RknpuNeeds};

const BUFFERS: u32 = 10;
const BUFFER_BYTES: u32 = 4096;
const ME: CallerId = CallerId(1);
const OTHER: CallerId = CallerId(2);

/// Builds the driver over the played kernel, as `Rknpu::start` builds it over the node.
struct Played(Arc<Mutex<Kernel>>);

fn opener(kernel: &Arc<Mutex<Kernel>>) -> Opener {
    let mut node = Node {
        kernel: Arc::clone(kernel),
    };
    Box::new(move || node.open().map(|s| Box::new(s) as BoxedSession))
}

impl Binder for Played {
    fn bind(
        &self,
        res: &mut lemnos_core::Resources<'_>,
        _cfg: &lemnos_core::Config<'_>,
    ) -> Result<Box<dyn Instance>> {
        let jobs = res.stream("jobs")?;
        Ok(Box::new(Rknpu::with_opener(
            RknpuNeeds { jobs },
            opener(&self.0),
        )?))
    }
}

fn played_entry(kernel: &Arc<Mutex<Kernel>>) -> DriverEntry {
    let binder: &'static Played = Box::leak(Box::new(Played(Arc::clone(kernel))));
    DriverEntry::bound_by(
        <Rknpu as Driver>::MANIFEST,
        <Rknpu as Driver>::TICKS,
        <Rknpu as Driver>::TRACKS_CALLERS,
        binder,
    )
}

fn board(config: &str, buffers: u32) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"npu\"\ndriver = \"rknpu\"\nconfig = {{ {config} }}\n\
         [devices.streams.jobs]\nbuffers = {buffers}\nbuffer_bytes = {BUFFER_BYTES}\n"
    )
}

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn start(text: &str, drivers: &[DriverEntry]) -> Self {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start(
            &parse(text).unwrap(),
            drivers,
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        Self { host, clock }
    }

    fn played(kernel: &Arc<Mutex<Kernel>>) -> Self {
        Self::start(&board("", BUFFERS), &[played_entry(kernel)])
    }

    fn call(
        &self,
        caller: CallerId,
        method: u16,
        args: &[u8],
    ) -> core::result::Result<Vec<u8>, Status> {
        let mut reply = [0u8; 256];
        let n = self.host.call_as(
            caller,
            0,
            MethodId {
                contract: 0,
                method,
            },
            args,
            &mut reply,
        )?;
        Ok(reply[..n].to_vec())
    }

    fn info(&self) -> core::result::Result<NpuInfo, Status> {
        self.call(ME, 1, &[])
            .map(|bytes| NpuInfo::decode(&mut Reader::new(&bytes)).unwrap())
    }

    fn load(&self, caller: CallerId, buffer: u32, bytes: u32) -> core::result::Result<u32, Status> {
        self.call(caller, 3, &[enc(&buffer), enc(&bytes)].concat())
            .map(|bytes| u32::decode(&mut Reader::new(&bytes)).unwrap())
    }

    fn attach(
        &self,
        caller: CallerId,
        program: u32,
        slot: u16,
        buffer: u32,
        bytes: u32,
    ) -> core::result::Result<(), Status> {
        self.call(
            caller,
            5,
            &[enc(&program), enc(&slot), enc(&buffer), enc(&bytes)].concat(),
        )
        .map(drop)
    }

    fn unload(&self, caller: CallerId, program: u32) -> core::result::Result<(), Status> {
        self.call(caller, 4, &enc(&program)).map(drop)
    }

    fn counters(&self) -> Counters {
        Counters::decode(&mut Reader::new(&self.call(ME, 7, &[]).unwrap())).unwrap()
    }

    fn health(&self) -> Health {
        self.host.device("npu").unwrap().health
    }

    fn after(&self, by: Duration) {
        self.clock.advance(by);
        self.host.tick();
    }
}

fn enc<T: Encode + ?Sized>(value: &T) -> Vec<u8> {
    let mut buffer = [0u8; 64];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    buffer[..used].to_vec()
}

fn write(lease: &Lease, buffer: u32, data: &[u8]) {
    let at = lease.layout.buffer_offset(buffer).unwrap();
    assert!(lease.buffers.write(at, data));
}

fn read(lease: &Lease, buffer: u32, len: usize) -> Vec<u8> {
    let at = lease.layout.buffer_offset(buffer).unwrap();
    let mut out = vec![0; len];
    assert!(lease.buffers.read(at, &mut out));
    out
}

fn post(consumer: &mut Consumer, run: RunJob) {
    assert!(consumer.push_job(&enc(&run)).unwrap());
}

/// The next completion's identity and status; its buffer goes back.
fn completion(consumer: &mut Consumer, lease: &Lease) -> (u64, RunStatus) {
    let frame = consumer.pop_frame().unwrap().expect("a completion");
    assert_eq!(frame.bytes, 1);
    let status = RunResult::decode(&mut Reader::new(&read(lease, frame.buffer, 1)))
        .unwrap()
        .status;
    assert!(consumer.release(frame.buffer, frame.fence).unwrap());
    (frame.identity, status)
}

/// Loads the played copy program from buffer 0 as `caller`, with slot 0 on buffer 1 and slot 1
/// on buffer 2, four bytes each.
fn load_copy(rig: &Rig, lease: &Lease, caller: CallerId) -> u32 {
    let bytes = Framed::copy(4).bytes();
    write(lease, 0, &bytes);
    let program = rig.load(caller, 0, bytes.len() as u32).unwrap();
    rig.attach(caller, program, 0, 1, 4).unwrap();
    rig.attach(caller, program, 1, 2, 4).unwrap();
    program
}

#[test]
fn a_board_file_binds_it_and_a_caller_runs_a_program_through_the_host() {
    let kernel = Kernel::new(true);
    let rig = Rig::played(&kernel);
    assert_eq!(rig.host.device("npu").unwrap().state, DeviceState::Running);
    // The first tick opens the node.
    rig.host.tick();
    assert_eq!(rig.health(), Health::Ok);
    let info = rig.info().unwrap();
    assert_eq!(
        (info.max_programs, info.max_attached),
        (15, crate::program::MAX_SLOTS)
    );
    let name = rig.call(ME, 2, &32u32.to_le_bytes()).unwrap();
    assert!(name.ends_with(b"jobs"), "{name:?}");

    let lease = rig.host.stream_attach(0, "jobs", ME).unwrap();
    let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
    let program = load_copy(&rig, &lease, ME);
    assert_eq!(program, 1);
    write(&lease, 1, &[10, 20, 30, 40]);
    post(&mut consumer, job(0xabc, program, 3));
    rig.after(Duration::from_nanos(POLL_NS));
    assert_eq!(completion(&mut consumer, &lease), (0xabc, RunStatus::Done));
    assert_eq!(read(&lease, 2, 4), [11, 21, 31, 41]);
    assert_eq!(rig.counters(), Counters { jobs: 1, errors: 0 });
    // The run named the program's domain, and could reach its body and two slots, no more.
    let seen = kernel.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].domain, seen[0].reachable.len(), seen[0].foreign),
        (1, 3, 0)
    );

    // A run naming no program is refused on its completion; nothing reaches the block.
    post(&mut consumer, job(0xdef, 9, 3));
    rig.after(Duration::from_nanos(POLL_NS));
    assert_eq!(
        completion(&mut consumer, &lease),
        (0xdef, RunStatus::Refused)
    );
    assert_eq!(kernel.lock().unwrap().submits, 1);
}

#[test]
fn a_program_is_its_callers_and_goes_when_its_caller_goes() {
    let kernel = Kernel::new(true);
    let rig = Rig::played(&kernel);
    rig.host.tick();
    let lease = rig.host.stream_attach(0, "jobs", ME).unwrap();
    let program = load_copy(&rig, &lease, ME);
    assert_eq!(handles_in(&kernel, 1).len(), 3);
    // Another caller can't change it.
    assert_eq!(rig.unload(OTHER, program), Err(Status::NOT_FOUND));
    assert_eq!(rig.attach(OTHER, program, 0, 4, 4), Err(Status::NOT_FOUND));
    assert_eq!(handles_in(&kernel, 1).len(), 3);
    // Its caller goes: the program is unloaded and its domain holds nothing.
    rig.host.caller_gone(ME);
    assert!(handles_in(&kernel, 1).is_empty());
    assert_eq!(rig.unload(ME, program), Err(Status::NOT_FOUND));
}

#[test]
fn a_failed_open_backs_off_to_eight_seconds_and_never_gives_up() {
    let kernel = Kernel::new(true);
    kernel.lock().unwrap().fail_opens = 7;
    let rig = Rig::played(&kernel);
    rig.host.tick();
    assert_eq!(
        rig.health(),
        Health::Degraded("the node failed to open: trying it again")
    );
    assert_eq!(rig.info(), Err(Status::NO_DEVICE));
    // 250 ms doubling, capped at 8 s: every open is tried when due, never before.
    let attempts = |k: &Arc<Mutex<Kernel>>| {
        let k = k.lock().unwrap();
        (k.fail_opens, k.files)
    };
    let mut waits = Vec::new();
    for _ in 0..7 {
        let before = attempts(&kernel);
        let mut waited = 0;
        while attempts(&kernel) == before {
            rig.after(Duration::from_millis(250));
            waited += 250;
            assert!(waited <= 8000, "no open within 8 s");
        }
        waits.push(waited);
    }
    assert_eq!(waits, [250, 500, 1000, 2000, 4000, 8000, 8000]);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.info().unwrap().max_programs, 15);
}

#[test]
fn a_block_without_an_iommu_is_refused_and_tried_again() {
    let kernel = Kernel::new(true);
    kernel.lock().unwrap().iommu = false;
    let rig = Rig::played(&kernel);
    rig.host.tick();
    assert!(
        matches!(rig.health(), Health::Failed(_)),
        "{:?}",
        rig.health()
    );
    assert_eq!(rig.info(), Err(Status::NO_DEVICE));
    // Still the driver's: it is not turned off, and it takes the node once it is confined.
    assert_eq!(rig.host.device("npu").unwrap().state, DeviceState::Running);
    kernel.lock().unwrap().iommu = true;
    rig.after(Duration::from_secs(8));
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn a_node_that_goes_away_is_opened_again_and_its_programs_are_gone() {
    let kernel = Kernel::new(true);
    let rig = Rig::played(&kernel);
    rig.host.tick();
    let lease = rig.host.stream_attach(0, "jobs", ME).unwrap();
    let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
    let program = load_copy(&rig, &lease, ME);
    kernel.lock().unwrap().gone = true;
    post(&mut consumer, job(1, program, 3));
    rig.after(Duration::from_nanos(POLL_NS));
    assert_eq!(completion(&mut consumer, &lease), (1, RunStatus::Failed));
    assert_eq!(
        rig.health(),
        Health::Degraded("the node went away: opening it again")
    );
    // Back: the node is opened again, and the old program is gone.
    kernel.lock().unwrap().gone = false;
    rig.after(Duration::from_secs(8));
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.unload(ME, program), Err(Status::NOT_FOUND));
    let again = load_copy(&rig, &lease, ME);
    write(&lease, 1, &[1, 2, 3, 4]);
    post(&mut consumer, job(2, again, 3));
    rig.after(Duration::from_nanos(POLL_NS));
    assert_eq!(completion(&mut consumer, &lease), (2, RunStatus::Done));
}

#[test]
fn the_real_entry_binds_from_the_board_file_and_opens_nothing_it_was_not_given() {
    // A node path that doesn't exist: the device runs, and the node fails to open.
    let rig = Rig::start(
        &board("node = \"/nonexistent/renderD199\"", BUFFERS),
        &[entry::<Rknpu>()],
    );
    rig.host.tick();
    assert_eq!(rig.host.device("npu").unwrap().state, DeviceState::Running);
    assert_eq!(
        rig.health(),
        Health::Degraded("the node failed to open: trying it again")
    );
    assert_eq!(rig.info(), Err(Status::NO_DEVICE));
    // An empty path, and a stream with more buffers than the driver holds, fail to bind.
    for text in [board("node = \"\"", BUFFERS), board("", MAX_BUFFERS + 1)] {
        let rig = Rig::start(&text, &[entry::<Rknpu>()]);
        assert!(
            matches!(
                rig.host.device("npu").unwrap().state,
                DeviceState::Backoff { .. }
            ),
            "{text}"
        );
    }
}

#[test]
fn ticks_with_a_claim_held_allocate_nothing() {
    let kernel = Kernel::new(true);
    // The test plays the host's part: the driver on a host's stream, ticked directly.
    let rig = Rig::start(&board("", BUFFERS), &[]);
    let jobs: lemnos_core::StreamHandle =
        lemnos_core::Held::boxed(Box::new(rig.host.stream_producer(0, "jobs").unwrap()));
    let mut driver = Rknpu::with_opener(RknpuNeeds { jobs }, opener(&kernel)).unwrap();
    assert_eq!(Driver::tick(&mut driver, 0), Some(POLL_NS));
    assert!(driver.claim().is_some() && driver.closed().is_none());
    let lease = rig.host.stream_attach(0, "jobs", ME).unwrap();
    let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
    let bytes = Framed::copy(4).bytes();
    write(&lease, 0, &bytes);
    Driver::caller(&mut driver, ME);
    let program = lemnos_contract_npu::Npu::load(&mut driver, 0, bytes.len() as u32).unwrap();
    lemnos_contract_npu::Npu::attach(&mut driver, program, 0, 1, 4).unwrap();
    lemnos_contract_npu::Npu::attach(&mut driver, program, 1, 2, 4).unwrap();
    kernel.lock().unwrap().record = false;
    let mut encoded = [0u8; 16];
    let mut count = 0;
    let mut now = POLL_NS;
    for n in 0..200u64 {
        let mut writer = Writer::new(&mut encoded);
        job(n, program, 3).encode(&mut writer).unwrap();
        let used = writer.len();
        assert!(consumer.push_job(&encoded[..used]).unwrap());
        let (_, allocated) = allocations(|| Driver::tick(&mut driver, now));
        now += POLL_NS;
        let frame = consumer.pop_frame().unwrap().expect("a completion");
        assert!(consumer.release(frame.buffer, frame.fence).unwrap());
        if n >= 10 {
            count += allocated;
        }
    }
    assert_eq!(count, 0, "allocations in 190 ticks that each ran a job");
    assert_eq!(driver.claim().unwrap().counters().jobs, 200);
    assert_eq!(driver.closed(), None::<Closed>);
}
