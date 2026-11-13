//! The driver on the module's way to the block, bound on a simulated board: the ISP's register
//! window, its interrupt, clocks and reset lines are the board's mocks. Every expected value is
//! written out by hand.

use lemnos_contract_camera_isp::{CameraIsp, IspJob, NO_BUFFER};
use lemnos_core::{Config, ConfigValue, Encode, Lifecycle, Provider, Resources, Status, Writer};
use lemnos_driver_k230isp_sys::regs::{
    AE_H_SIZE, AE_V_SIZE, CTRL_GEN_CFG_UPD, ISP_AWB_GAIN_RB, ISP_CTRL, ISP_ICR, ISP_IMSC, ISP_MIS,
    MI_BASE, MIS_FRAME_IN, MIS_FRAME_OUT, WINDOW_BYTES,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::mock::{MockClocks, MockInterrupt, MockRegisters, MockReset, MockStream};
use lemnos_sim::SimBoard;

use crate::hw::{Hardware, ModuleHardware};
use crate::stats::registers;
use crate::{CONFIG_BYTES, K230Isp, LAYOUT_WRITES, STATISTICS_BYTES};

const STRIDE: u64 = 1_000_000;

/// The board's parts, as the test holds them.
struct Parts {
    board: SimBoard,
    regs: MockRegisters,
    irq: MockInterrupt,
    clocks: MockClocks,
    resets: MockReset,
    config: MockStream,
    statistics: MockStream,
}

/// A board with the ISP's window, interrupt, two clocks, one reset line and both streams, each
/// under its need's name.
fn board() -> Parts {
    let board = SimBoard::new();
    Parts {
        regs: board.add_registers("regs", WINDOW_BYTES as usize),
        irq: board.add_interrupt("irq"),
        clocks: board.add_clocks_of("clocks", MockClocks::counted(2)),
        resets: board.add_reset_of("resets", MockReset::counted(1)),
        config: board.add_stream_of("config", MockStream::with_bytes(4, CONFIG_BYTES)),
        statistics: board.add_stream_of("statistics", MockStream::with_bytes(4, STATISTICS_BYTES)),
        board,
    }
}

fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> lemnos_core::Result<K230Isp> {
    let mut resources = Resources::new(board, None);
    <K230Isp as lemnos_core::Driver>::bind(&mut resources, &Config::new(entries))
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

/// The ISP raising its interrupt with `status` in `ISP_MIS`.
fn interrupt(parts: &Parts, status: u32, at: u64) {
    parts.regs.set(ISP_MIS as usize, status);
    parts.irq.fire(at);
}

#[test]
fn naming_the_window_and_interrupt_takes_the_modules_way_and_open_powers_the_block() {
    let mut parts = board();
    parts.regs.set(ISP_MIS as usize, 0x44);
    parts.regs.set(ISP_IMSC as usize, 0x100);
    let mut d = bind(&mut parts.board, &[]).unwrap();
    assert!(parts.regs.writes().is_empty(), "binding touches nothing");
    assert!(!parts.irq.enabled());
    d.start().unwrap();
    assert!(parts.clocks.enabled(0) && parts.clocks.enabled(1));
    assert_eq!(parts.resets.history(), [(0, true), (0, false)]);
    assert_eq!(
        parts.regs.writes(),
        [
            // What was pending from before is cleared, then frames leaving may raise it.
            (ISP_ICR as usize, 0x44),
            (ISP_IMSC as usize, 0x100 | MIS_FRAME_OUT),
        ]
    );
    assert!(parts.irq.enabled());
    d.stop().unwrap();
    assert!(!parts.irq.enabled(), "a stopped driver keeps no interrupt");
    assert!(
        parts.clocks.enabled(0),
        "clocks stay on: turning them off is the host's"
    );
}

#[test]
fn a_job_on_the_modules_way_writes_through_the_table_and_reads_the_second_frames_statistics() {
    let mut parts = board();
    let mut d = bind(&mut parts.board, &[]).unwrap();
    d.start().unwrap();
    d.tick(0);
    let opened = parts.regs.writes().len();
    let writes = [
        (AE_H_SIZE, 0x80),
        (AE_V_SIZE, 0x40),
        (ISP_AWB_GAIN_RB, 0x0123_0145),
    ];
    parts.config.fill(2, &list(&writes));
    parts.regs.set(ISP_CTRL as usize, 0x1);
    post(&parts.config, 77, 2);
    d.tick(STRIDE);
    let after: Vec<_> = parts.regs.writes()[opened..].to_vec();
    assert_eq!(
        after,
        [
            (AE_H_SIZE as usize, 0x80),
            (AE_V_SIZE as usize, 0x40),
            (ISP_AWB_GAIN_RB as usize, 0x0123_0145),
            // A register with a mask is read, merged and written back: the enable stays.
            (ISP_CTRL as usize, 0x1 | CTRL_GEN_CFG_UPD),
        ]
    );
    // The first frame out may have started before the update; an interrupt that is not a frame
    // leaving doesn't count.
    interrupt(&parts, MIS_FRAME_OUT, 10);
    d.tick(2 * STRIDE);
    interrupt(&parts, MIS_FRAME_IN, 15);
    d.tick(3 * STRIDE);
    assert!(parts.statistics.take_frame().is_none());
    let mut expect = Vec::new();
    for (i, addr) in registers().enumerate() {
        let value = 0x0909_0909 ^ (i as u32 * 0x1357);
        parts.regs.set(addr as usize, value);
        expect.extend_from_slice(&value.to_le_bytes());
    }
    interrupt(&parts, MIS_FRAME_OUT, 20);
    d.tick(4 * STRIDE);
    let f = parts.statistics.take_frame().unwrap();
    assert_eq!(
        (f.identity, f.bytes, f.sequence, f.timestamp_ns),
        (77, 172, 2, 20)
    );
    assert_eq!(parts.statistics.contents(f.buffer), expect);
    // Each interrupt's cause was cleared, and the interrupt enabled again for the next.
    let clears: Vec<_> = parts.regs.writes()[opened..]
        .iter()
        .filter(|(addr, _)| *addr == ISP_ICR as usize)
        .map(|(_, value)| *value)
        .collect();
    assert_eq!(clears, [MIS_FRAME_OUT, MIS_FRAME_IN, MIS_FRAME_OUT]);
    assert!(parts.irq.enabled());
    let c = d.counters().unwrap();
    assert_eq!((c.jobs, c.dropped, c.errors), (1, 0, 0));
}

#[test]
fn a_register_outside_the_table_never_reaches_the_window() {
    let mut parts = board();
    let mut hw = ModuleHardware::new(
        parts.board.registers("regs").unwrap(),
        parts.board.interrupt("irq").unwrap(),
        None,
        None,
    );
    assert_eq!(
        hw.write(AE_V_SIZE, 1).unwrap_err(),
        Status::IO,
        "not open yet"
    );
    hw.open().unwrap();
    let opened = parts.regs.writes().len();
    for (addr, value) in [
        (MI_BASE, 1),
        (WINDOW_BYTES, 0),
        (u32::MAX - 3, 0),
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
    assert_eq!(parts.regs.writes().len(), opened);
    hw.write(AE_V_SIZE, 3).unwrap();
    assert_eq!(parts.regs.writes()[opened..], [(AE_V_SIZE as usize, 3)]);
}

#[test]
fn a_window_smaller_than_the_isps_is_refused_at_open() {
    let mut parts = board();
    let small = parts
        .board
        .add_registers("small", WINDOW_BYTES as usize - 4);
    let mut hw = ModuleHardware::new(
        parts.board.registers("small").unwrap(),
        parts.board.interrupt("irq").unwrap(),
        None,
        None,
    );
    assert_eq!(hw.open().unwrap_err(), Status::INVALID_ARGUMENT);
    assert!(small.writes().is_empty());
    assert!(!parts.irq.enabled());
}

#[test]
fn half_of_a_way_or_both_ways_at_once_is_refused_at_bind_and_nothing_is_tried() {
    // The window without its interrupt.
    let board = SimBoard::new();
    let regs = board.add_registers("regs", WINDOW_BYTES as usize);
    board.add_stream_of("config", MockStream::with_bytes(4, CONFIG_BYTES));
    board.add_stream_of("statistics", MockStream::with_bytes(4, STATISTICS_BYTES));
    let mut only_regs = board.clone();
    assert_eq!(
        bind(&mut only_regs, &[]).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    // The interrupt, clocks or resets without the window.
    for name in ["irq", "clocks", "resets"] {
        let board = SimBoard::new();
        board.add_stream_of("config", MockStream::with_bytes(4, CONFIG_BYTES));
        board.add_stream_of("statistics", MockStream::with_bytes(4, STATISTICS_BYTES));
        match name {
            "irq" => {
                board.add_interrupt(name);
            }
            "clocks" => {
                board.add_clocks(name);
            }
            _ => {
                board.add_reset(name);
            }
        }
        let mut b = board.clone();
        assert_eq!(
            bind(&mut b, &[]).err(),
            Some(Status::INVALID_ARGUMENT),
            "{name}"
        );
    }
    // The module's way and the vendor device both named.
    let mut parts = self::board();
    assert_eq!(
        bind(
            &mut parts.board,
            &[("device", ConfigValue::Str("/dev/vvcam-isp.0"))]
        )
        .err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert!(regs.writes().is_empty() && parts.regs.writes().is_empty());
}

#[test]
fn a_window_the_host_cannot_give_fails_the_bind_with_its_reason() {
    let mut parts = board();
    // Someone else holds the window: the host says so, and no other way is tried.
    let held = parts.board.registers("regs").unwrap();
    assert_eq!(bind(&mut parts.board, &[]).err(), Some(Status::BUSY));
    drop(held);
    assert!(
        bind(&mut parts.board, &[]).is_ok(),
        "a later bind tries again"
    );
}

#[test]
fn with_nothing_of_the_modules_named_the_vendor_device_is_the_way() {
    let board = SimBoard::new();
    board.add_stream_of("config", MockStream::with_bytes(4, CONFIG_BYTES));
    board.add_stream_of("statistics", MockStream::with_bytes(4, STATISTICS_BYTES));
    let mut b = board.clone();
    let mut d = bind(
        &mut b,
        &[(
            "device",
            ConfigValue::Str("/nonexistent/lemnos-test/vvcam-isp"),
        )],
    )
    .unwrap();
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
}

#[test]
fn a_failing_interrupt_starts_recovery_which_opens_the_block_again() {
    let mut parts = board();
    let mut d = bind(&mut parts.board, &[]).unwrap();
    d.start().unwrap();
    d.tick(0);
    parts
        .board
        .faults("irq")
        .unwrap()
        .fail_next(ErrorKind::Failed);
    // The failure is seen, recovery is scheduled at the first backoff, and runs when due.
    let due = d.tick(STRIDE).unwrap();
    assert!(!parts.irq.enabled(), "closed while recovering");
    assert_eq!(parts.resets.history().len(), 2);
    assert_eq!(d.tick(due), Some(due + STRIDE));
    assert!(parts.irq.enabled());
    assert_eq!(
        parts.resets.history(),
        [(0, true), (0, false), (0, true), (0, false)],
        "the block starts again from its reset state"
    );
}
