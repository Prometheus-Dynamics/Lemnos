//! The reset paths, in the simulator: the watchdogs (both), the arming readback that fails, the
//! deadline that catches a reset that never reached the board, and a VERIFY that stalls past a
//! watchdog's period. Each ends in a reset the board takes with no hands. Each has a real-failing
//! counterpart beside it.

mod common;

use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::bootrom::{self, Stage};
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::flash::{self, FlashError, retry_until};
use lemnos_k230_usbboot::sim::{
    BOOT_US, BoardState, SimBoard, SimClock, SimConfig, sim_stage_image,
};
use lemnos_k230_usbboot_stage::format::{BOOT_HEADER_LEN, rom_accepts};
use lemnos_k230_usbboot_stage::wdt::{self, CLOCK_HZ_MAX, Fault, TOP, period_us};

const LOAD: u32 = 0x8036_0000;

fn board(hz: u64) -> (SimBoard, SimClock) {
    let board = SimBoard::new(&SimConfig {
        clock_hz: hz,
        ..SimConfig::default()
    });
    let clock = board.clock();
    (board, clock)
}

/// Waits for the ROM, then loads and starts the stage (the blocking call, which settles the start).
fn enter_stage(board: &mut SimBoard, clock: &SimClock) -> Result<(), UsbError> {
    retry_until(
        60_000,
        || match bootrom::stage(&mut *board) {
            Ok(Stage::BootRom) => Some(()),
            _ => None,
        },
        |ms| clock.advance_us(u64::from(ms) * 1000),
    )
    .map_err(|_| UsbError::Timeout(60_000))?;
    bootrom::load_and_start(&mut *board, &sim_stage_image(), LOAD)
}

#[test]
fn a_healthy_stage_comes_up_and_a_silent_host_is_reset_by_its_watchdog_with_no_hands() {
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    enter_stage(&mut board, &clock).unwrap();
    assert_eq!(board.state(), BoardState::Stage);
    // Real-failing: before the period, the stage is still there; after it, the board has reset.
    let period = period_us(TOP, CLOCK_HZ_MAX);
    clock.advance_us(period - 100_000);
    assert_eq!(board.state(), BoardState::Stage, "reset before its period");
    clock.advance_us(200_000 + 100_000);
    clock.advance_us(BOOT_US);
    assert_eq!(
        board.state(),
        BoardState::Rom,
        "the watchdog did not reset the board"
    );
}

#[test]
fn a_cr_that_does_not_take_on_wdt0_is_caught_by_the_readback_and_the_board_resets_into_download() {
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    board.set_wdt_fault(0, Fault::CrIgnored);
    let error = enter_stage(&mut board, &clock).unwrap_err();
    // The stage never runs: the readback failed, the SoC reset ran, and the ROM answered again.
    assert!(error.to_string().contains("answered again"), "{error}");
    assert_eq!(board.state(), BoardState::Rom);
}

#[test]
fn a_cr_that_does_not_take_on_wdt1_is_caught_too() {
    // The big core's watchdog, which the arming must also check.
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    board.set_wdt_fault(1, Fault::CrIgnored);
    let error = enter_stage(&mut board, &clock).unwrap_err();
    assert!(error.to_string().contains("answered again"), "{error}");
    assert_eq!(board.state(), BoardState::Rom);
}

#[test]
fn a_readback_that_lies_about_a_frozen_counter_is_caught_and_takes_the_soc_reset() {
    // CR reads enabled, so the enable check passes; CCVR never moves. The readback must refuse it.
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    board.set_wdt_fault(0, Fault::CounterStuck);
    let error = enter_stage(&mut board, &clock).unwrap_err();
    assert!(error.to_string().contains("answered again"), "{error}");
    assert_eq!(board.state(), BoardState::Rom);
}

#[test]
fn a_reset_that_never_reaches_the_board_is_caught_by_the_deadline_and_the_soc_reset_follows() {
    // The watchdog fires into nothing. The deadline counts from the last kick, so the board is reset
    // by it, and not before.
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    board.set_reset_lost(true);
    enter_stage(&mut board, &clock).unwrap();
    clock.advance_us(wdt::DEADLINE_US - 2_000_000);
    assert_eq!(board.state(), BoardState::Stage, "the deadline fired early");
    clock.advance_us(3_000_000);
    clock.advance_us(BOOT_US);
    assert_eq!(
        board.state(),
        BoardState::Rom,
        "the deadline did not reset the board"
    );
}

#[test]
fn real_failing_without_the_deadline_a_lost_watchdog_reset_would_hang_past_the_deadline() {
    // The same board, the watchdog's reset lost: well past the watchdog's period, nothing has reset
    // yet. That is the case the deadline exists for.
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    board.set_reset_lost(true);
    enter_stage(&mut board, &clock).unwrap();
    clock.advance_us(period_us(TOP, CLOCK_HZ_MAX) * 4);
    assert_eq!(board.state(), BoardState::Stage);
}

#[test]
fn a_verify_that_stalls_past_a_watchdog_period_ends_in_a_reset_not_a_hang() {
    // At 100 MHz the period is 1.34 s. Each medium read takes 2 s, so the first VERIFY chunk outlasts
    // the watchdog, which resets the board mid-VERIFY.
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    enter_stage(&mut board, &clock).unwrap();
    let image: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();
    let mut burner = Burner::new(&mut board);
    flash::payload(&mut burner, Medium::Sd, &image).unwrap();
    clock.stall_reads(2_000_000);
    let error = flash::commit(&mut burner, &image).unwrap_err();
    assert!(matches!(error, FlashError::Usb(_)), "{error}");
    clock.advance_us(BOOT_US);
    assert_eq!(
        board.state(),
        BoardState::Rom,
        "the stalled VERIFY did not reset the board"
    );
    // No header was written: nothing boots.
    assert!(!rom_accepts(&board.medium()[..BOOT_HEADER_LEN]));
}

#[test]
fn real_failing_the_same_verify_without_the_stall_completes_and_writes_the_header() {
    let (mut board, clock) = board(CLOCK_HZ_MAX);
    enter_stage(&mut board, &clock).unwrap();
    let image: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();
    let mut burner = Burner::new(&mut board);
    flash::payload(&mut burner, Medium::Sd, &image).unwrap();
    flash::commit(&mut burner, &image).unwrap();
    assert!(rom_accepts(&board.medium()[..BOOT_HEADER_LEN]));
}

#[test]
fn the_spin_bounds_are_the_deadline_and_the_watchdog_period_is_inside_it() {
    // The entry's spin reads the core's time against SPIN_DEADLINE_TICKS, at the assumed 1 MHz.
    assert_eq!(wdt::SPIN_DEADLINE_TICKS, wdt::DEADLINE_US);
    assert!(wdt::DEADLINE_US > wdt::longest_period_us());
}
