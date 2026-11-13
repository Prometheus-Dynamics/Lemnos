//! The driver on the module's way to the block: the configuration session is played back, and
//! records every request it was given. No node is opened; every expected value is written out
//! by hand.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{CameraIsp, IspJob, NO_BUFFER};
use lemnos_core::{Config, ConfigValue, Encode, Lifecycle, Resources, Status, Writer};
use lemnos_driver_k230isp_sys::regs::{
    AE_H_SIZE, AE_V_SIZE, CTRL_GAMMA_OUT_ENABLE, CTRL_GEN_CFG_UPD, ISP_AWB_GAIN_RB, ISP_CTRL,
    ISP_ICR, ISP_MIS, MI_BASE, MIS_FRAME_IN, MIS_FRAME_OUT, WINDOW_BYTES,
};
use lemnos_driver_regjob_sys::{EVENT_RESET, Event, Write};
use lemnos_hal::mock::MockStream;
use lemnos_sim::SimBoard;

use crate::driver::backoff_ns;
use crate::hw::{ConfigSession, Hardware, ModuleHardware};
use crate::stats::registers;
use crate::{CONFIG_BYTES, K230Isp, K230IspNeeds, LAYOUT_WRITES, STATISTICS_BYTES};

const STRIDE: u64 = 1_000_000;

/// One request as the session was given it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Open,
    Close,
    Write(Vec<Write>, Vec<u32>),
}

#[derive(Default)]
struct Log {
    asked: Vec<Asked>,
    events: VecDeque<Event>,
    /// What each register reads as; 0 when not set.
    regs: Vec<(u32, u32)>,
    /// The errno the next open answers with.
    fail_open: Option<i32>,
    /// The errno the next write answers with.
    fail_write: Option<i32>,
}

#[derive(Clone, Default)]
struct Played(Arc<Mutex<Log>>);

impl Played {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap()
    }

    fn event(&self, status: u32, timestamp_ns: u64) {
        self.log().events.push_back(Event {
            status,
            lost: 0,
            timestamp_ns,
        });
    }

    fn set(&self, addr: u32, value: u32) {
        let mut log = self.log();
        log.regs.retain(|(a, _)| *a != addr);
        log.regs.push((addr, value));
    }

    fn asked(&self) -> Vec<Asked> {
        std::mem::take(&mut self.log().asked)
    }
}

struct Session(Played);

impl ConfigSession for Session {
    fn open(&mut self) -> io::Result<()> {
        let mut log = self.0.log();
        log.asked.push(Asked::Open);
        match log.fail_open.take() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(()),
        }
    }

    fn close(&mut self) {
        self.0.log().asked.push(Asked::Close);
    }

    fn write(&mut self, writes: &[Write], reads: &[u32], values: &mut [u32]) -> io::Result<()> {
        let mut log = self.0.log();
        log.asked
            .push(Asked::Write(writes.to_vec(), reads.to_vec()));
        if let Some(errno) = log.fail_write.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        for (value, addr) in values.iter_mut().zip(reads) {
            *value = log
                .regs
                .iter()
                .find(|(a, _)| a == addr)
                .map_or(0, |(_, v)| *v);
        }
        Ok(())
    }

    fn next_event(&mut self) -> io::Result<Option<Event>> {
        Ok(self.0.log().events.pop_front())
    }
}

fn w(offset: u32, value: u32) -> Write {
    Write { offset, value }
}

/// The streams, as the test holds them.
struct Rig {
    d: K230Isp,
    played: Played,
    config: MockStream,
    statistics: MockStream,
}

fn rig() -> Rig {
    let played = Played::default();
    let config = MockStream::with_bytes(4, CONFIG_BYTES);
    let statistics = MockStream::with_bytes(4, STATISTICS_BYTES);
    let needs = K230IspNeeds {
        config: lemnos_core::Held::boxed(Box::new(config.clone())),
        statistics: lemnos_core::Held::boxed(Box::new(statistics.clone())),
    };
    let hw = ModuleHardware::new(Box::new(Session(played.clone())));
    Rig {
        d: K230Isp::with_hardware(Box::new(hw), needs),
        played,
        config,
        statistics,
    }
}

fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> lemnos_core::Result<K230Isp> {
    let mut resources = Resources::new(board, None);
    <K230Isp as lemnos_core::Driver>::bind(&mut resources, &Config::new(entries))
}

fn streams() -> SimBoard {
    let board = SimBoard::new();
    board.add_stream_of("config", MockStream::with_bytes(4, CONFIG_BYTES));
    board.add_stream_of("statistics", MockStream::with_bytes(4, STATISTICS_BYTES));
    board
}

/// A configuration holding `writes`.
fn list(writes: &[(u32, u32)]) -> Vec<u8> {
    let mut out = vec![0u8; CONFIG_BYTES];
    out[..4].copy_from_slice(&(writes.len() as u32).to_le_bytes());
    for (i, (addr, value)) in writes.iter().enumerate() {
        out[4 + 8 * i..8 + 8 * i].copy_from_slice(&addr.to_le_bytes());
        out[8 + 8 * i..12 + 8 * i].copy_from_slice(&value.to_le_bytes());
    }
    out
}

fn post(stream: &MockStream, identity: u64, config_buffer: u32) {
    let job = IspJob {
        identity,
        layout: LAYOUT_WRITES,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER; 4],
    };
    let mut buf = [0; 128];
    let mut w = Writer::new(&mut buf);
    job.encode(&mut w).unwrap();
    let n = w.len();
    stream.post_job(&buf[..n]);
}

#[test]
fn start_opens_the_configuration_session_and_drops_what_was_queued_before() {
    let mut r = rig();
    r.played.event(MIS_FRAME_OUT, 1);
    r.played.event(MIS_FRAME_OUT, 2);
    r.d.start().unwrap();
    // Whatever was open is closed first.
    assert_eq!(r.played.asked(), [Asked::Close, Asked::Open]);
    assert!(r.played.log().events.is_empty());
    r.d.stop().unwrap();
    assert_eq!(r.played.asked(), [Asked::Close]);
}

#[test]
fn a_job_on_the_modules_way_writes_through_the_session_and_reads_the_second_frames_statistics() {
    let mut r = rig();
    r.d.start().unwrap();
    r.d.tick(0);
    r.played.asked();
    let writes = [
        (AE_H_SIZE, 0x80),
        (AE_V_SIZE, 0x40),
        (ISP_AWB_GAIN_RB, 0x0123_0145),
        (ISP_CTRL, CTRL_GAMMA_OUT_ENABLE),
    ];
    r.config.fill(2, &list(&writes));
    post(&r.config, 77, 2);
    r.d.tick(STRIDE);
    // One request per write: isp_ctrl's other bits are the module's to keep, so the driver
    // sends only its own.
    assert_eq!(
        r.played.asked(),
        [
            Asked::Write(vec![w(AE_H_SIZE, 0x80)], vec![]),
            Asked::Write(vec![w(AE_V_SIZE, 0x40)], vec![]),
            Asked::Write(vec![w(ISP_AWB_GAIN_RB, 0x0123_0145)], vec![]),
            Asked::Write(vec![w(ISP_CTRL, CTRL_GAMMA_OUT_ENABLE)], vec![]),
            Asked::Write(vec![w(ISP_CTRL, CTRL_GEN_CFG_UPD)], vec![]),
        ]
    );
    // The first frame out may have started before the update; an interrupt that is not a frame
    // leaving doesn't count.
    r.played.event(MIS_FRAME_OUT, 10);
    r.d.tick(2 * STRIDE);
    r.played.event(MIS_FRAME_IN, 15);
    r.d.tick(3 * STRIDE);
    assert!(r.statistics.take_frame().is_none());
    let mut expect = Vec::new();
    for (i, addr) in registers().enumerate() {
        let value = 0x0909_0909 ^ (i as u32 * 0x1357);
        r.played.set(addr, value);
        expect.extend_from_slice(&value.to_le_bytes());
    }
    r.played.asked();
    r.played.event(MIS_FRAME_OUT, 20);
    r.d.tick(4 * STRIDE);
    let f = r.statistics.take_frame().unwrap();
    assert_eq!(
        (f.identity, f.bytes, f.sequence, f.timestamp_ns),
        (77, 172, 2, 20)
    );
    assert_eq!(r.statistics.contents(f.buffer), expect);
    // Each statistics register is one read through the session.
    let reads: Vec<u32> = r
        .played
        .asked()
        .into_iter()
        .filter_map(|a| match a {
            Asked::Write(writes, reads) if writes.is_empty() => Some(reads),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(reads, registers().collect::<Vec<_>>());
    let c = r.d.counters().unwrap();
    assert_eq!((c.jobs, c.dropped, c.errors), (1, 0, 0));
}

#[test]
fn a_register_outside_the_table_never_reaches_the_session() {
    let played = Played::default();
    let mut hw = ModuleHardware::new(Box::new(Session(played.clone())));
    assert_eq!(
        hw.write(AE_V_SIZE, 1).unwrap_err(),
        Status::IO,
        "not open yet"
    );
    hw.open().unwrap();
    played.asked();
    for (addr, value) in [
        (MI_BASE, 1),
        (WINDOW_BYTES, 0),
        (u32::MAX - 3, 0),
        // The frames role's bit of isp_ctrl.
        (ISP_CTRL, 1),
        (ISP_ICR, 1),
        (AE_V_SIZE + 1, 0),
    ] {
        assert_eq!(
            hw.write(addr, value).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{addr:#x}"
        );
    }
    for addr in [MI_BASE, WINDOW_BYTES, ISP_MIS] {
        assert_eq!(
            hw.read(addr).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{addr:#x}"
        );
    }
    assert!(played.asked().is_empty());
    hw.write(AE_V_SIZE, 3).unwrap();
    played.set(AE_V_SIZE, 3);
    assert_eq!(hw.read(AE_V_SIZE).unwrap(), 3);
    assert_eq!(
        played.asked(),
        [
            Asked::Write(vec![w(AE_V_SIZE, 3)], vec![]),
            Asked::Write(vec![], vec![AE_V_SIZE]),
        ]
    );
}

#[test]
fn a_write_the_module_refuses_is_a_failed_request() {
    let played = Played::default();
    let mut hw = ModuleHardware::new(Box::new(Session(played.clone())));
    hw.open().unwrap();
    played.log().fail_write = Some(libc::EPERM);
    assert_eq!(hw.write(AE_V_SIZE, 3).unwrap_err(), Status::IO);
}

#[test]
fn a_reset_by_the_frames_role_starts_recovery_which_opens_the_session_again() {
    let mut r = rig();
    r.d.start().unwrap();
    r.d.tick(0);
    r.played.asked();
    r.played.event(EVENT_RESET, 5);
    // The reset is seen, the session closed, recovery scheduled at the first backoff.
    let due = r.d.tick(STRIDE).unwrap();
    assert_eq!(due, STRIDE + backoff_ns(0));
    assert_eq!(r.played.asked(), [Asked::Close]);
    assert_eq!(r.d.tick(due), Some(due + STRIDE));
    assert_eq!(r.played.asked(), [Asked::Close, Asked::Open]);
}

#[test]
fn a_session_held_by_another_driver_fails_the_start() {
    let mut r = rig();
    r.played.log().fail_open = Some(libc::EBUSY);
    assert_eq!(r.d.start().unwrap_err(), Status::IO);
    // `start` fails as a whole; nothing is left open and nothing is retried on its own.
    assert_eq!(r.played.asked(), [Asked::Close, Asked::Open]);
    r.d.start().unwrap();
}

#[test]
fn the_block_setting_takes_the_modules_way_and_nothing_is_opened_at_bind() {
    let mut board = streams();
    let d = bind(&mut board, &[("block", ConfigValue::Str("90000000.isp"))]);
    assert!(d.is_ok());
    let mut board = streams();
    let mut d = bind(
        &mut board,
        &[
            ("block", ConfigValue::Str("90000000.isp")),
            ("node", ConfigValue::Str("/nonexistent/lemnos-test/lemnos")),
        ],
    )
    .unwrap();
    // The node isn't there: the start says so, and the vendor device is not tried.
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
}

#[test]
fn both_ways_at_once_or_an_empty_block_is_refused_at_bind() {
    for entries in [
        &[
            ("block", ConfigValue::Str("90000000.isp")),
            ("device", ConfigValue::Str("/dev/vvcam-isp.0")),
        ][..],
        &[("block", ConfigValue::Str(""))][..],
    ] {
        let mut board = streams();
        assert_eq!(
            bind(&mut board, entries).err(),
            Some(Status::INVALID_ARGUMENT)
        );
    }
}

#[test]
fn with_no_block_the_vendor_device_is_the_way() {
    let mut board = streams();
    let mut d = bind(
        &mut board,
        &[(
            "device",
            ConfigValue::Str("/nonexistent/lemnos-test/vvcam-isp"),
        )],
    )
    .unwrap();
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
}
