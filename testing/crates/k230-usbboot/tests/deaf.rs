//! The boot ROM goes deaf a while after its reset and stays enumerated (bench: about a minute, measured
//! as "possibly less"; the window is a parameter here). A host that loads in time gets the stage; a host
//! that is late gets the named error at its first transfer, and no blind retry reaches a deaf ROM; the
//! probe flow across a reset gets the named error too, not a "no return".

use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::bootrom::{self, Stage};
use lemnos_k230_usbboot::probe::run_probe;
use lemnos_k230_usbboot::sim::{SimBoard, SimConfig, sim_stage_image};
use std::time::Duration;

const LOAD: u32 = 0x8036_0000;

fn board() -> SimBoard {
    SimBoard::new(&SimConfig::default())
}

#[test]
fn a_host_that_loads_inside_the_window_gets_the_stage() {
    let mut board = board();
    board.set_deaf_after(Some(60_000_000));
    let clock = board.clock();
    // The ROM answers at once after the power-on, well inside the window.
    assert_eq!(bootrom::stage(&mut board).unwrap(), Stage::BootRom);
    bootrom::load_and_start(&mut board, &sim_stage_image(), LOAD).unwrap();
    assert_eq!(bootrom::stage(&mut board).unwrap(), Stage::Lemnos);
    let _ = clock;
}

#[test]
fn a_host_that_is_late_gets_the_named_error_at_its_first_transfer_and_sends_no_image() {
    let mut board = board();
    board.set_deaf_after(Some(60_000_000));
    let clock = board.clock();
    // The window passes with nobody loading.
    clock.advance_us(61_000_000);
    let error = bootrom::load_and_start(&mut board, &sim_stage_image(), LOAD).unwrap_err();
    assert_eq!(error, UsbError::Deaf, "{error}");
    assert!(error.to_string().contains("EPROTO"), "{error}");
    // Still enumerated: the device is on the bus, and only the ROM is deaf.
    assert_eq!(board.state(), lemnos_k230_usbboot::sim::BoardState::Rom);
}

#[test]
fn the_probe_flow_across_a_reset_into_a_deaf_rom_is_named_not_a_missing_return() {
    let mut board = board();
    board.set_deaf_after(Some(0));
    let error = run_probe(&mut board, &sim_stage_image(), Duration::from_secs(5))
        .expect_err("a deaf ROM is an error, not an outcome");
    assert_eq!(error, UsbError::Deaf, "{error}");
}
