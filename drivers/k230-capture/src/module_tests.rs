//! The driver on the module's way to the ISP, on a simulated board: the ISP's register window, its
//! memory-interface interrupt, its DMA memory, clocks and reset lines are the board's mocks. The
//! CSI device is a plain file standing in for the node, so it opens; nothing is asked of it here.
//! Every expected value is written out by hand.

use std::path::PathBuf;

use lemnos_core::{Config, ConfigValue, Provider, Resources, Status};
use lemnos_driver_k230_capture_sys::memory::{Plane, Planes};
use lemnos_driver_k230_capture_sys::regs::{
    MIS_MP_RAW_FRAME_END, MIV2_CTRL, MIV2_ICR, MIV2_IMSC, MIV2_MIS, MP_RAW_BASE_AD_INIT,
    MP_RAW_BASE_AD_SHD, MP_RAW_SIZE_INIT, WINDOW_BYTES,
};
use lemnos_driver_k230_capture_sys::{MiEvent, Output};
use lemnos_hal::mock::{MockClocks, MockDma, MockInterrupt, MockRegisters, MockReset, MockStream};
use lemnos_sim::SimBoard;

use crate::hw::{HeldInterrupt, HeldRegisters};
use crate::{Hardware, K230Capture, ModuleHardware, Settings};

/// The board's parts, as the test holds them.
struct Parts {
    board: SimBoard,
    regs: MockRegisters,
    irq: MockInterrupt,
    dma: MockDma,
    clocks: MockClocks,
    resets: MockReset,
}

/// A board with the ISP's window, its interrupt, DMA memory below 4 GiB, three clocks, two reset
/// lines and the frame stream, each under its need's name.
fn board() -> Parts {
    let board = SimBoard::new();
    board.add_stream_of("frames", MockStream::with_bytes(4, 8192));
    Parts {
        regs: board.add_registers("regs", WINDOW_BYTES as usize),
        irq: board.add_interrupt("irq"),
        dma: board.add_dma_of("dma", MockDma::at(0x4000_0000)),
        clocks: board.add_clocks_of("clocks", MockClocks::counted(3)),
        resets: board.add_reset_of("resets", MockReset::counted(2)),
        board,
    }
}

fn bind(
    board: &mut SimBoard,
    entries: &[(&str, ConfigValue<'_>)],
) -> lemnos_core::Result<K230Capture> {
    let mut resources = Resources::new(board, None);
    <K230Capture as lemnos_core::Driver>::bind(&mut resources, &Config::new(entries))
}

/// A plain file standing in for the CSI node, removed when dropped.
struct FakeMipi(PathBuf);

impl FakeMipi {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("lemnos-k230-capture-{name}-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for FakeMipi {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The module's hardware over the board's parts, with the CSI device at `mipi`.
fn hardware(parts: &mut Parts, mipi: &FakeMipi) -> ModuleHardware {
    let settings = Settings::new(None, Some(mipi.path())).unwrap();
    let Ok(hw) = ModuleHardware::new(
        &settings,
        Box::new(HeldRegisters(parts.board.registers("regs").unwrap())),
        Box::new(HeldInterrupt(parts.board.interrupt("irq").unwrap())),
        Box::new(crate::hw::HeldDma(parts.board.dma("dma").unwrap())),
        Some(parts.board.clocks("clocks").unwrap()),
        Some(parts.board.reset("resets").unwrap()),
    ) else {
        panic!("the ISP's window was refused");
    };
    hw
}

fn writes(regs: &MockRegisters) -> Vec<(u32, u32)> {
    regs.writes()
        .into_iter()
        .map(|(offset, value)| (offset as u32, value))
        .collect()
}

/// A 256-byte raw frame.
fn raw() -> Planes {
    Planes::new(
        Plane {
            offset: 0,
            size: 256,
        },
        None,
        None,
    )
    .unwrap()
}

#[test]
fn open_powers_the_block_turns_the_path_off_resets_it_and_takes_the_interrupt_in_that_order() {
    let mut parts = board();
    let mipi = FakeMipi::new("open");
    let mut hw = hardware(&mut parts, &mipi);
    assert!(parts.regs.writes().is_empty(), "made, nothing touched");
    parts.regs.set(MIV2_MIS as usize, 0x10);
    hw.open().unwrap();
    assert!((0..3).all(|c| parts.clocks.enabled(c)));
    assert_eq!(
        parts.resets.history(),
        [(0, true), (1, true), (0, false), (1, false)]
    );
    let w = writes(&parts.regs);
    // The path off and updated, then what was pending cleared, then frame ends may raise it.
    assert_eq!(w[0], (MIV2_CTRL, 0));
    assert_eq!(w[w.len() - 2..], [(MIV2_ICR, 0x10), (MIV2_IMSC, 0x3)]);
    assert!(parts.irq.enabled());
}

#[test]
fn on_the_modules_way_a_frame_is_written_into_the_blocks_own_memory_and_read_back() {
    let mut parts = board();
    let mipi = FakeMipi::new("frame");
    let mut hw = hardware(&mut parts, &mipi);
    hw.open().unwrap();
    hw.provide(256).unwrap();
    hw.start(Output::Raw, raw()).unwrap();
    let w = writes(&parts.regs);
    // Memories at 0x4000_0000, 0x4000_1000, 0x4000_2000: 0 loaded, 1 armed, then the raw path on.
    let bases: Vec<u32> = w
        .iter()
        .filter(|(addr, _)| *addr == MP_RAW_BASE_AD_INIT)
        .map(|(_, v)| *v)
        .collect();
    assert_eq!(bases, [0x4000_0000, 0x4000_1000]);
    assert!(w.contains(&(MP_RAW_SIZE_INIT, 256)));
    assert_eq!(w.last(), Some(&(MIV2_CTRL, 2)));
    assert_eq!(hw.next_event().unwrap(), None);
    // The block finishes memory 0 and moves to 1.
    parts.dma.device_write(1, 0, &[0x5a; 256]);
    parts.regs.set(MP_RAW_BASE_AD_SHD as usize, 0x4000_1000);
    parts.regs.set(MIV2_MIS as usize, MIS_MP_RAW_FRAME_END);
    parts.irq.fire(1234);
    assert_eq!(
        hw.next_event().unwrap(),
        Some(MiEvent {
            status: MIS_MP_RAW_FRAME_END,
            timestamp_ns: 1234
        })
    );
    assert_eq!(
        writes(&parts.regs).last(),
        Some(&(MIV2_ICR, MIS_MP_RAW_FRAME_END))
    );
    assert!(parts.irq.enabled(), "enabled again for the next frame end");
    assert_eq!(hw.active().unwrap(), Some(1));
    hw.rearm(2).unwrap();
    hw.take(0).unwrap();
    let mut out = [0u8; 256];
    hw.copy_out(0, 0, &mut out).unwrap();
    assert_eq!(out, [0x5a; 256]);
    assert_eq!(hw.copy_out(0, 1, &mut out).unwrap_err(), Status::BUSY);
    hw.stop().unwrap();
    assert_eq!(
        writes(&parts.regs)[writes(&parts.regs).len() - 2],
        (MIV2_CTRL, 0)
    );
}

#[test]
fn naming_the_window_and_interrupt_takes_the_modules_way_without_touching_anything() {
    let mut parts = board();
    let d = bind(&mut parts.board, &[]);
    assert!(d.is_ok());
    assert!(parts.regs.writes().is_empty());
    assert!(!parts.irq.enabled());
    assert_eq!(parts.dma.live_buffers(), 0);
    assert!(!parts.clocks.enabled(0));
}

#[test]
fn half_of_a_way_both_ways_or_a_small_window_is_refused_at_bind() {
    let plain = || {
        let board = SimBoard::new();
        board.add_stream_of("frames", MockStream::with_bytes(4, 8192));
        board.add_dma_of("dma", MockDma::at(0x4000_0000));
        board
    };
    // The window without its interrupt.
    let mut b = plain();
    b.add_registers("regs", WINDOW_BYTES as usize);
    assert_eq!(bind(&mut b, &[]).err(), Some(Status::INVALID_ARGUMENT));
    // The interrupt, clocks or resets without the window.
    let mut b = plain();
    b.add_interrupt("irq");
    assert_eq!(bind(&mut b, &[]).err(), Some(Status::INVALID_ARGUMENT));
    let mut b = plain();
    b.add_clocks("clocks");
    assert_eq!(bind(&mut b, &[]).err(), Some(Status::INVALID_ARGUMENT));
    let mut b = plain();
    b.add_reset("resets");
    assert_eq!(bind(&mut b, &[]).err(), Some(Status::INVALID_ARGUMENT));
    // The module's way and the vendor node both named.
    let mut parts = board();
    assert_eq!(
        bind(
            &mut parts.board,
            &[("isp", ConfigValue::Str("/dev/vvcam-isp.0"))]
        )
        .err(),
        Some(Status::INVALID_ARGUMENT)
    );
    // A window too small to be the ISP's.
    let mut b = plain();
    let small = b.add_registers("regs", WINDOW_BYTES as usize - 4);
    b.add_interrupt("irq");
    assert_eq!(bind(&mut b, &[]).err(), Some(Status::INVALID_ARGUMENT));
    assert!(small.writes().is_empty() && parts.regs.writes().is_empty());
}

#[test]
fn a_resource_the_host_cannot_give_fails_the_bind_with_its_reason() {
    let mut parts = board();
    let held = parts.board.interrupt("irq").unwrap();
    assert_eq!(bind(&mut parts.board, &[]).err(), Some(Status::BUSY));
    drop(held);
    assert!(
        bind(&mut parts.board, &[]).is_ok(),
        "a later bind tries again"
    );
}

#[test]
fn with_nothing_of_the_modules_named_the_vendor_node_is_the_way() {
    let mut b = SimBoard::new();
    b.add_stream_of("frames", MockStream::with_bytes(4, 8192));
    b.add_dma_of("dma", MockDma::at(0x4000_0000));
    assert!(
        bind(
            &mut b,
            &[(
                "isp",
                ConfigValue::Str("/nonexistent/lemnos-test/vvcam-isp")
            )]
        )
        .is_ok()
    );
}
