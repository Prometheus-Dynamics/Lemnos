//! The flash against the simulated K230: the boot ROM, the stage (the same code the board runs),
//! the watchdog model and the medium. These are the proofs of the write order, the return path and
//! the feed rule, and each one has a real-failing case beside it.

mod common;

use lemnos_k230_usbboot::bootrom::{self, Stage};
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::flash::{
    self, Backoff, FlashError, HEADER_OFFSET, PAYLOAD_OFFSET, Report, flash, retry_until,
};
use lemnos_k230_usbboot::hex;
use lemnos_k230_usbboot::sim::{
    BOOT_US, BoardState, SimBoard, SimClock, SimConfig, sim_stage_image,
};
use lemnos_k230_usbboot_stage::format::{BOOT_HEADER_LEN, HEADER_BLOCK, rom_accepts};
use lemnos_k230_usbboot_stage::wdt::{self, CLOCK_HZ_MAX, CLOCK_HZ_MIN, TOP};
use sha2::{Digest, Sha256};

const LOAD: u32 = 0x8036_0000;

fn image(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 253) as u8).collect()
}

fn board(config: SimConfig) -> (SimBoard, SimClock) {
    let board = SimBoard::new(&config);
    let clock = board.clock();
    (board, clock)
}

/// Brings the board from its download mode into our stage: the ROM answers, the stage is loaded and
/// started, and the stage answers. Waits for the ROM with the flasher's backoff.
fn bring_up(board: &mut SimBoard, clock: &SimClock) -> Result<(), FlashError> {
    retry_until(
        60_000,
        || match bootrom::stage(&mut *board) {
            Ok(Stage::BootRom) => Some(()),
            _ => None,
        },
        |ms| clock.advance_us(u64::from(ms) * 1000),
    )?;
    bootrom::load_and_start(&mut *board, &sim_stage_image(), LOAD)?;
    retry_until(
        60_000,
        || match bootrom::stage(&mut *board) {
            Ok(Stage::Lemnos) => Some(()),
            _ => None,
        },
        |ms| clock.advance_us(u64::from(ms) * 1000),
    )
}

/// One session: bring up, flash, reboot.
fn flash_once(board: &mut SimBoard, clock: &SimClock, image: &[u8]) -> Result<Report, FlashError> {
    bring_up(board, clock)?;
    let mut burner = Burner::new(&mut *board);
    let report = flash(&mut burner, Medium::Sd, image)?;
    burner.reboot()?;
    Ok(report)
}

/// The medium holds a valid header whose payload is exactly `image`.
fn booted_image_is(medium: &[u8], image: &[u8]) -> bool {
    rom_accepts(&medium[..BOOT_HEADER_LEN])
        && medium[PAYLOAD_OFFSET as usize..PAYLOAD_OFFSET as usize + image.len()] == *image
}

#[test]
fn a_blank_board_flashed_once_boots_the_image_it_was_given() {
    let (mut board, clock) = board(SimConfig::default());
    let img = image(10_000);
    let report = flash_once(&mut board, &clock, &img).unwrap();
    assert_eq!(report.pieces, 3);
    assert_eq!(report.image_sha256, hex(&Sha256::digest(&img)));
    assert_eq!(board.state(), BoardState::Booted);
    assert!(booted_image_is(&board.medium(), &img));
}

#[test]
fn a_cut_at_every_operation_and_byte_never_leaves_a_half_bootable_image_and_the_board_recovers() {
    let img = image(10_000);
    // The operations of one whole flash, from a dry run.
    let (mut dry, dry_clock) = board(SimConfig::default());
    flash_once(&mut dry, &dry_clock, &img).unwrap();
    let ops = dry.ops();
    assert!(ops > 20, "the flash should be many operations, got {ops}");

    let (mut booted, mut rom) = (0u32, 0u32);
    for op in 0..ops {
        for bytes in [0u64, 1, 511, 512, 4095, u64::MAX] {
            let (mut board, clock) = board(SimConfig::default());
            board.cut_at(op, bytes);
            let _ = flash_once(&mut board, &clock, &img);
            // The reset: the medium decides what the ROM does.
            clock.advance_us(BOOT_US + 1);
            match board.state() {
                BoardState::Booted => {
                    booted += 1;
                    assert!(
                        booted_image_is(&board.medium(), &img),
                        "a cut at op {op} (bytes {bytes}) booted a half-written image"
                    );
                }
                BoardState::Rom => {
                    rom += 1;
                    assert!(
                        !rom_accepts(&board.medium()[..BOOT_HEADER_LEN]),
                        "a cut at op {op} (bytes {bytes}) left a valid header in download mode"
                    );
                    // Recoverable over USB: a second session from download mode flashes it.
                    flash_once(&mut board, &clock, &img).unwrap_or_else(|error| {
                        panic!("op {op} bytes {bytes}: recovery failed: {error}")
                    });
                    assert_eq!(board.state(), BoardState::Booted, "op {op} bytes {bytes}");
                    assert!(booted_image_is(&board.medium(), &img));
                }
                other => panic!(
                    "op {op} bytes {bytes}: the board is {other:?}, not booted or in download mode"
                ),
            }
        }
    }
    // The sweep reached both sides: cuts before the header commit, and cuts after it.
    assert!(rom > 0, "no cut left the board in download mode");
    assert!(booted > 0, "no cut came after the header commit");
}

#[test]
fn the_return_path_resets_a_host_that_stops_at_every_clock_within_one_period() {
    for hz in [CLOCK_HZ_MIN, 24_000_000, CLOCK_HZ_MAX] {
        let (mut board, clock) = board(SimConfig {
            clock_hz: hz,
            ..SimConfig::default()
        });
        bring_up(&mut board, &clock).unwrap();
        let period = wdt::period_us(TOP, hz);
        // Fed at the arming, then nothing. The readback of the arming takes counter cycles too (a
        // few tens of microseconds at 1 MHz), so the bound is the period, with 1 ms of slack for it.
        clock.advance_us(period - 1_000);
        assert_eq!(board.state(), BoardState::Stage, "{hz} Hz reset early");
        // ...and a reset at it.
        clock.advance_us(1_000);
        assert_eq!(
            board.state(),
            BoardState::Absent,
            "{hz} Hz did not reset at its period"
        );
        clock.advance_us(BOOT_US);
        assert_eq!(
            board.state(),
            BoardState::Rom,
            "{hz} Hz: the ROM is back in download mode"
        );
    }
}

#[test]
fn a_stage_fed_after_each_verified_piece_survives_a_flash_longer_than_one_period() {
    // At 100 MHz one period is 1.34 s. A 4 KiB piece takes 1 s; the flash takes several periods of
    // time in all, and each piece feeds the watchdog once it reads back equal.
    let config = SimConfig {
        clock_hz: CLOCK_HZ_MAX,
        op_us: 0,
        write_us_per_kib: 250_000,
        ..SimConfig::default()
    };
    let (mut board, clock) = board(config);
    let img = image(10_000);
    let start = clock.now_us();
    flash_once(&mut board, &clock, &img).unwrap();
    let elapsed = clock.now_us() - start;
    assert!(
        elapsed > wdt::period_us(TOP, CLOCK_HZ_MAX),
        "the flash took {elapsed} us, under one period"
    );
    assert!(booted_image_is(&board.medium(), &img));
}

#[test]
fn real_failing_a_verified_piece_longer_than_the_period_is_reset_not_fed() {
    // A piece that takes 1.5 s at 100 MHz (period 1.34 s) is cut by the watchdog mid-write.
    let config = SimConfig {
        clock_hz: CLOCK_HZ_MAX,
        op_us: 0,
        write_us_per_kib: 375_000,
        ..SimConfig::default()
    };
    let (mut board, clock) = board(config);
    let img = image(10_000);
    assert!(flash_once(&mut board, &clock, &img).is_err());
    clock.advance_us(BOOT_US + 1);
    assert_eq!(board.state(), BoardState::Rom);
    assert!(!rom_accepts(&board.medium()[..BOOT_HEADER_LEN]));
}

#[test]
fn real_failing_a_medium_that_drops_writes_is_caught_by_the_read_back() {
    let (mut board, clock) = board(SimConfig::default());
    board.drop_writes(true);
    let error = flash_once(&mut board, &clock, &image(10_000)).unwrap_err();
    assert!(matches!(error, FlashError::Verify { .. }), "{error}");
    assert!(
        !rom_accepts(&board.medium()[..BOOT_HEADER_LEN]),
        "no header was written"
    );
}

#[test]
fn real_failing_a_payload_that_changes_after_its_pieces_verify_fails_the_image_hash_and_boots_nothing()
 {
    let (mut board, clock) = board(SimConfig::default());
    let img = image(10_000);
    bring_up(&mut board, &clock).unwrap();
    let medium = board.medium_handle();
    let mut burner = Burner::new(&mut board);
    flash::payload(&mut burner, Medium::Sd, &img).unwrap();
    // A byte decays on the medium after every piece has read back equal.
    medium.poke(PAYLOAD_OFFSET + 5, img[5] ^ 0xff);
    let error = flash::commit(&mut burner, &img).unwrap_err();
    assert!(matches!(error, FlashError::ImageHash { .. }), "{error}");
    assert!(
        !rom_accepts(&board.medium()[..BOOT_HEADER_LEN]),
        "the header was not written"
    );
}

#[test]
fn real_failing_the_stage_refuses_a_header_written_before_its_payload_is_verified() {
    let (mut board, clock) = board(SimConfig::default());
    bring_up(&mut board, &clock).unwrap();
    let mut burner = Burner::new(&mut board);
    burner.probe(Medium::Sd).unwrap();
    burner.info().unwrap();
    burner.erase(HEADER_OFFSET, HEADER_BLOCK).unwrap();
    // A header that names a payload nobody verified.
    let header = lemnos_k230_usbboot_stage::format::BootHeader {
        payload_offset: PAYLOAD_OFFSET,
        payload_size: 16,
        digest: [0x11; 32],
    }
    .encode();
    let mut block = vec![0u8; HEADER_BLOCK as usize];
    block[..BOOT_HEADER_LEN].copy_from_slice(&header);
    let mut source: &[u8] = &block;
    let error = burner
        .write(
            HEADER_OFFSET,
            &mut source,
            HEADER_BLOCK,
            0,
            &mut |_, _| {},
            &|| false,
        )
        .unwrap_err();
    assert!(error.to_string().contains("does not name"), "{error}");
    assert!(!rom_accepts(&board.medium()[..BOOT_HEADER_LEN]));
}

#[test]
fn the_retries_wait_five_seconds_then_ten_and_never_past_the_budget() {
    let mut backoff = Backoff::new();
    let waits: Vec<u32> = (0..6).map(|_| backoff.next_ms()).collect();
    assert_eq!(waits, vec![5_000, 10_000, 10_000, 10_000, 10_000, 10_000]);

    let mut slept = Vec::new();
    let mut tries = 0;
    let result: Result<(), FlashError> = retry_until(
        60_000,
        || {
            tries += 1;
            None
        },
        |ms| slept.push(ms),
    );
    assert_eq!(result, Err(FlashError::NotInStage));
    assert_eq!(
        slept.iter().sum::<u32>(),
        60_000,
        "the budget is spent exactly"
    );
    assert!(slept.iter().all(|&ms| ms <= 10_000));
    assert_eq!(tries as usize, slept.len() + 1);
}

#[test]
fn a_reset_is_waited_out_within_the_budget_and_the_stage_comes_back() {
    // After a reset the ROM is back in BOOT_US; the host's backoff finds it.
    let (mut board, clock) = board(SimConfig::default());
    bring_up(&mut board, &clock).unwrap();
    clock.advance_us(wdt::period_us(TOP, SimConfig::default().clock_hz) + 1);
    assert_eq!(board.state(), BoardState::Absent);
    bring_up(&mut board, &clock).unwrap();
    assert_eq!(board.state(), BoardState::Stage);
}

#[test]
fn an_image_that_does_not_fit_the_medium_or_is_empty_is_refused_before_any_write() {
    let (mut board, clock) = board(SimConfig {
        medium_bytes: 16 * 1024,
        ..SimConfig::default()
    });
    let error = flash_once(&mut board, &clock, &image(20_000)).unwrap_err();
    assert!(matches!(error, FlashError::BadPlan(_)), "{error}");
    let error = flash_once(&mut board, &clock, &[]).unwrap_err();
    assert!(matches!(error, FlashError::BadPlan(_)), "{error}");
    assert!(
        board.medium().iter().all(|&byte| byte == 0xff),
        "nothing was written"
    );
}
