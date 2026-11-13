//! The module's way to the ISP ([`ModuleHardware`]) against a frames session that records what
//! it was asked and answers as `lemnos.ko` would, stepped through the calls the driver makes;
//! and the board file's choice of way at bind. The CSI device is a plain file standing in for the
//! node, so it opens; nothing is asked of it here. Every expected value is written out by hand.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lemnos_core::{Config, ConfigValue, Resources, Status};
use lemnos_driver_k230_capture_sys::memory::{Plane, Planes};
use lemnos_driver_k230_capture_sys::regs::{
    ISP_CTRL, ISP_CTRL_ENABLE, ISP_CTRL_GEN_CFG_UPD, MIV2_CTRL, MP_CB_BASE_AD_INIT,
    MP_CR_BASE_AD_INIT, MP_RAW_BASE_AD_INIT, MP_Y_BASE_AD_INIT,
};
use lemnos_driver_k230_capture_sys::{MiEvent, Output};
use lemnos_driver_regjob_sys::{Addr, Done, Job, STATUS_UNLATCHED, Write};
use lemnos_hal::mock::{MockDma, MockStream};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;

use crate::module::FRAME_TIMEOUT_MS;
use crate::{FrameSession, Hardware, K230Capture, ModuleHardware, Settings};

/// A job as the session was given it: its addresses and start value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ran {
    addrs: Vec<Addr>,
    start: u32,
    timeout_ms: u32,
}

#[derive(Default)]
struct Log {
    opens: u32,
    /// Bytes each memory of the ring holds.
    memory: [u64; 3],
    /// Memories replaced (allocated again).
    allocations: u32,
    imports: Vec<usize>,
    writes: Vec<Write>,
    jobs: Vec<Ran>,
    /// Kept while set: the steady-state check runs with it off.
    record: bool,
    running: bool,
    /// What collects answer with, in order; a job runs while it is empty.
    ends: VecDeque<io::Result<Done>>,
    /// The errno the next write answers with.
    write_answer: Option<i32>,
}

#[derive(Clone, Default)]
struct Played(Arc<Mutex<Log>>);

impl Played {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap()
    }

    fn end(&self, status: u32, timestamp_ns: u64) {
        self.log().ends.push_back(Ok(Done {
            status,
            timestamp_ns,
        }));
    }

    /// The memory each job named, by its Y address's buffer index.
    fn slots(&self) -> Vec<u32> {
        self.log().jobs.iter().map(|j| j.addrs[0].buffer).collect()
    }
}

struct Session(Played);

impl FrameSession for Session {
    fn open(&mut self) -> io::Result<()> {
        let mut log = self.0.log();
        log.opens += 1;
        log.running = false;
        log.imports.clear();
        Ok(())
    }

    fn provide(&mut self, slot: usize, bytes: u64) -> io::Result<()> {
        let mut log = self.0.log();
        if log.memory[slot] < bytes {
            log.memory[slot] = bytes;
            log.allocations += 1;
        }
        Ok(())
    }

    fn import(&mut self, slot: usize) -> io::Result<u32> {
        let mut log = self.0.log();
        log.imports.push(slot);
        // The session numbers the memories 10, 11, 12: not their slots, so a mix-up shows.
        Ok(10 + slot as u32)
    }

    fn write(&mut self, writes: &[Write]) -> io::Result<()> {
        let mut log = self.0.log();
        if let Some(errno) = log.write_answer.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        log.writes.extend_from_slice(writes);
        Ok(())
    }

    fn start(&mut self, job: &Job<'_>) -> io::Result<()> {
        let mut log = self.0.log();
        assert!(!log.running, "a job started over a running one");
        assert!(job.writes.is_empty() && job.reads.is_empty());
        if log.record {
            log.jobs.push(Ran {
                addrs: job.addrs.to_vec(),
                start: job.start,
                timeout_ms: job.timeout_ms,
            });
        }
        log.running = true;
        Ok(())
    }

    fn collect(&mut self) -> io::Result<Option<Done>> {
        let mut log = self.0.log();
        assert!(log.running, "a collect with no job started");
        let Some(end) = log.ends.pop_front() else {
            return Ok(None);
        };
        log.running = false;
        end.map(Some)
    }

    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> io::Result<()> {
        // Each memory reads as its slot, and the offset's low byte.
        for (i, b) in out.iter_mut().enumerate() {
            *b = (slot as u8) << 4 | (offset as u8).wrapping_add(i as u8) & 0xf;
        }
        Ok(())
    }
}

/// A plain file standing in for the CSI node, removed when dropped.
struct FakeMipi(PathBuf);

impl FakeMipi {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lemnos-k230-capture-module-{name}-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"").unwrap();
        Self(path)
    }
}

impl Drop for FakeMipi {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const FRAME: u32 = 1;
/// Linux's `ETIMEDOUT`, as the module answers a frame that never came.
const ETIMEDOUT: i32 = 110;

/// NV12 at 64 by 32: Y 2048 bytes, then CbCr 1024.
fn nv12() -> Planes {
    Planes::new(
        Plane {
            offset: 0,
            size: 2048,
        },
        Some(Plane {
            offset: 2048,
            size: 1024,
        }),
        None,
    )
    .unwrap()
}

fn hardware(name: &str) -> (ModuleHardware, Played, FakeMipi) {
    let mipi = FakeMipi::new(name);
    let settings = Settings::new(None, Some(mipi.0.to_str().unwrap())).unwrap();
    let played = Played::default();
    played.log().record = true;
    let hw = ModuleHardware::new(&settings, Box::new(Session(played.clone())));
    (hw, played, mipi)
}

/// Opened, provided and started, as the driver's `begin` does.
fn started(name: &str, output: Output) -> (ModuleHardware, Played, FakeMipi) {
    let (mut hw, played, mipi) = hardware(name);
    hw.open().unwrap();
    hw.provide(4096).unwrap();
    hw.start(output, nv12()).unwrap();
    (hw, played, mipi)
}

fn addrs(index: u32) -> Vec<Addr> {
    let addr = |reg, offset, bytes| Addr {
        reg,
        buffer: index,
        offset,
        bytes,
    };
    vec![
        addr(MP_Y_BASE_AD_INIT, 0, 2048),
        addr(MP_CB_BASE_AD_INIT, 2048, 1024),
        // No Cr plane: its register names the Y plane, as the raw output's does.
        addr(MP_CR_BASE_AD_INIT, 0, 2048),
        addr(MP_RAW_BASE_AD_INIT, 0, 2048),
    ]
}

#[test]
fn start_imports_the_ring_and_points_the_idle_path_at_memory_0() {
    let (mut hw, played, _mipi) = started("start", Output::YCbCr);
    {
        let log = played.log();
        assert_eq!(log.opens, 1);
        assert_eq!(log.memory, [4096; 3]);
        assert_eq!(log.imports, [0, 1, 2]);
        assert_eq!(
            log.jobs,
            [Ran {
                addrs: addrs(10),
                start: 1,
                timeout_ms: FRAME_TIMEOUT_MS,
            }]
        );
    }
    // The idle block took memory 0 at once.
    assert_eq!(hw.active().unwrap(), Some(0));
    assert_eq!(hw.start(Output::YCbCr, nv12()).unwrap_err(), Status::BUSY);
    assert_eq!(hw.provide(4096).unwrap_err(), Status::BUSY);
}

#[test]
fn the_raw_output_starts_its_own_path() {
    let (_hw, played, _mipi) = started("raw", Output::Raw);
    assert_eq!(played.log().jobs[0].start, 2);
}

#[test]
fn planes_past_the_memory_are_refused_before_anything_is_imported() {
    let (mut hw, played, _mipi) = hardware("past");
    hw.open().unwrap();
    hw.provide(3000).unwrap();
    assert_eq!(
        hw.start(Output::YCbCr, nv12()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert!(played.log().imports.is_empty() && played.log().jobs.is_empty());
}

/// The ring as the driver moves it: each frame end it takes `active`, rearms the free memory and
/// reads the one the block left.
#[test]
fn the_ring_moves_as_the_driver_expects_one_frame_behind_the_block() {
    let (mut hw, played, _mipi) = started("ring", Output::YCbCr);
    assert_eq!(hw.next_event().unwrap(), None, "no frame end yet");
    // The first frame end: memory 0 still, so nothing new finished; memory 1 goes to the module.
    played.end(FRAME, 100);
    assert_eq!(
        hw.next_event().unwrap(),
        Some(MiEvent {
            status: FRAME,
            timestamp_ns: 100
        })
    );
    assert_eq!(hw.active().unwrap(), Some(0));
    assert_eq!(played.slots(), [10, 11]);
    // The second: the block took memory 1. The driver rearms memory 2 and reads memory 0.
    played.end(FRAME, 200);
    assert_eq!(hw.next_event().unwrap().unwrap().timestamp_ns, 200);
    assert_eq!(hw.active().unwrap(), Some(1));
    hw.rearm(2).unwrap();
    assert_eq!(played.slots(), [10, 11, 12]);
    hw.take(0).unwrap();
    let mut out = [0u8; 4];
    hw.copy_out(0, 2, &mut out).unwrap();
    assert_eq!(out, [0x02, 0x03, 0x04, 0x05]);
    // And on round the ring.
    for (n, (now, free)) in [(2, 0), (0, 1), (1, 2)].into_iter().enumerate() {
        played.end(FRAME, 300 + n as u64);
        hw.next_event().unwrap().unwrap();
        assert_eq!(hw.active().unwrap(), Some(now));
        hw.rearm(free).unwrap();
    }
    assert_eq!(played.slots(), [10, 11, 12, 10, 11, 12]);
    assert!(played.log().jobs.iter().all(|j| j.addrs.len() == 4));
}

#[test]
fn a_switch_the_block_did_not_take_is_asked_for_again() {
    let (mut hw, played, _mipi) = started("unlatched", Output::YCbCr);
    played.end(FRAME, 1);
    hw.next_event().unwrap();
    played.end(FRAME | STATUS_UNLATCHED, 2);
    // The frame end is reported without the module's bit, and the block is still on memory 0.
    assert_eq!(hw.next_event().unwrap().unwrap().status, FRAME);
    assert_eq!(hw.active().unwrap(), Some(0));
    assert_eq!(played.slots(), [10, 11, 11]);
    played.end(FRAME, 3);
    hw.next_event().unwrap();
    assert_eq!(hw.active().unwrap(), Some(1));
}

#[test]
fn a_rearm_while_a_job_is_out_waits_for_it() {
    let (mut hw, played, _mipi) = started("wait", Output::YCbCr);
    // Memory 0's job is out: the driver's rearm is held, then replaces the one queued.
    hw.rearm(2).unwrap();
    assert_eq!(played.slots(), [10]);
    played.end(FRAME, 1);
    hw.next_event().unwrap();
    assert_eq!(played.slots(), [10, 12]);
    assert_eq!(hw.rearm(3).unwrap_err(), Status::BUSY, "past the ring");
}

#[test]
fn a_frame_that_never_comes_is_a_device_error() {
    let (mut hw, played, _mipi) = started("timeout", Output::YCbCr);
    played
        .log()
        .ends
        .push_back(Err(io::Error::from_raw_os_error(ETIMEDOUT)));
    assert_eq!(hw.next_event().unwrap_err(), Status::IO);
}

#[test]
fn writes_go_through_the_table_then_the_module() {
    let (mut hw, played, _mipi) = hardware("writes");
    assert_eq!(hw.write(ISP_CTRL, 1).unwrap_err(), Status::IO, "not open");
    hw.open().unwrap();
    for (addr, value) in [
        (MIV2_CTRL, 1),
        (MP_Y_BASE_AD_INIT, 0x1000),
        (ISP_CTRL, 1 << 11),
        (0x9000, 0),
    ] {
        assert_eq!(
            hw.write(addr, value).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{addr:#x}"
        );
    }
    assert!(played.log().writes.is_empty());
    hw.write(ISP_CTRL, ISP_CTRL_ENABLE | ISP_CTRL_GEN_CFG_UPD)
        .unwrap();
    assert_eq!(
        played.log().writes,
        [Write {
            offset: ISP_CTRL,
            value: ISP_CTRL_ENABLE | ISP_CTRL_GEN_CFG_UPD
        }]
    );
    played.log().write_answer = Some(1);
    assert_eq!(hw.write(ISP_CTRL, 0).unwrap_err(), Status::IO);
}

#[test]
fn stop_opens_a_running_session_again_and_keeps_an_idle_one() {
    let (mut hw, played, _mipi) = started("stop", Output::YCbCr);
    hw.stop().unwrap();
    // Opened again: the module reset the block before letting the ring go.
    assert_eq!(played.log().opens, 2);
    assert_eq!(hw.active().unwrap(), None);
    hw.stop().unwrap();
    assert_eq!(played.log().opens, 2, "nothing running: nothing to reset");
    // The memory stays; a larger frame replaces it at once.
    hw.provide(4096).unwrap();
    assert_eq!(played.log().allocations, 3);
    hw.provide(8192).unwrap();
    assert_eq!(played.log().allocations, 6);
    hw.start(Output::YCbCr, nv12()).unwrap();
}

#[test]
fn the_steady_state_allocates_nothing() {
    let (mut hw, played, _mipi) = started("alloc", Output::YCbCr);
    played.log().ends.reserve(64);
    played.log().record = false;
    played.end(FRAME, 1);
    hw.next_event().unwrap();
    let mut out = [0u8; 64];
    let mut free = 2;
    let mut writing = 0;
    for n in 0..30u64 {
        played.end(FRAME, 2 + n);
        let ((), count) = allocations(|| {
            hw.next_event().unwrap();
            let now = hw.active().unwrap().unwrap();
            hw.rearm(free).unwrap();
            hw.take(writing).unwrap();
            hw.copy_out(writing, 0, &mut out).unwrap();
            free = writing;
            writing = now;
        });
        assert_eq!(count, 0, "frame {n}");
    }
}

// ---- The board file's choice.

fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> lemnos_core::Result<()> {
    let mut resources = Resources::new(board, None);
    <K230Capture as lemnos_core::Driver>::bind(&mut resources, &Config::new(entries)).map(|_| ())
}

fn streams(dma: bool) -> SimBoard {
    let board = SimBoard::new();
    board.add_stream_of("frames", MockStream::with_bytes(4, 8192));
    if dma {
        board.add_dma_of("dma", MockDma::at(0x4000_0000));
    }
    board
}

#[test]
fn block_takes_the_modules_way_and_dma_the_vendors() {
    assert!(
        bind(
            &mut streams(false),
            &[("block", ConfigValue::Str("90000000.isp"))]
        )
        .is_ok()
    );
    assert!(bind(&mut streams(true), &[]).is_ok());
}

#[test]
fn both_ways_or_half_of_one_is_refused_at_bind() {
    for (dma, entries) in [
        (true, &[("block", ConfigValue::Str("90000000.isp"))][..]),
        (
            false,
            &[
                ("block", ConfigValue::Str("90000000.isp")),
                ("isp", ConfigValue::Str("/dev/vvcam-isp.0")),
            ][..],
        ),
        (false, &[("block", ConfigValue::Str(""))][..]),
        (false, &[][..]),
    ] {
        assert_eq!(
            bind(&mut streams(dma), entries),
            Err(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
}
