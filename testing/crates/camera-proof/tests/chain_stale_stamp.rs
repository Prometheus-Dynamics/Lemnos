//! The camera chain proof, run end to end on this machine: the real service, the real sensor,
//! power switch, front end and back end drivers, and a simulated sensor bus, receiver and PiSP
//! blocks under them (`lemnos-camera-sim`). The proof is the program a board run uses, started as
//! a process on the service's socket; it must pass with every check made.
//!
//! What only silicon can show is not here: the real block's timing and DMA, the kernel module's
//! claims and ioctls, the real receiver's counters while the front end holds its node, a heap's
//! CMA limits and the lens's scene. The list is in `testing/board-runs/F2g1.md`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use std::sync::atomic::Ordering;

use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

fn scratch() -> PathBuf {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Removes the test's scratch directory when the test ends, passed or panicked.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_stale_frame_start_fails_the_chain_proof_on_its_age_check() {
    assert!(lemnos_camera_sim::pisp::install());
    // The negative case: every frame is stamped 2 s before its start, which the proof must catch.
    lemnos_camera_sim::pisp::STALE_NS.store(2_000_000_000, Ordering::SeqCst);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    // The chain's clock is the sim's: frames move it, and the proof reads it from this file.
    let clock_dir = scratch();
    let _remove_clock = RemoveOnDrop(clock_dir.clone());
    let clock = clock_dir.join("sim-clock");
    lemnos_camera_sim::time::install_file(&clock).unwrap();
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = scratch();
    let _remove = RemoveOnDrop(dir.clone());
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
    let buffers = SimBuffers::probe();
    let kind = buffers.kind();
    let service = Service::start_with_buffers(
        Config {
            socket: socket.clone(),
            name: board.board.id.clone(),
            board,
            drivers: drivers(),
            backend: Box::new(provider()),
            options: Options::default(),
        },
        Arc::new(buffers),
    )
    .unwrap();
    let reference = dir.join("reference.raw10");
    std::fs::write(&reference, scene::reference_raw10()).unwrap();
    let frames = std::env::var("CHAIN_FRAMES").unwrap_or_else(|_| "120".into());
    // 60 fps is 1832 lines of the sensor's mode 0 (80 MHz, 9.1 us a line); a debug build of the
    // proof and the simulated blocks cannot keep that up, so a test run asks for half the rate
    // (3664 lines) unless CHAIN_FPS=60 says the build is fast enough.
    let (length, fps) = if std::env::var("CHAIN_FPS").is_ok_and(|v| v == "60") {
        ("1832", "60")
    } else {
        ("3664", "30")
    };
    // The latch is checked in process on the sim clock (`chain_latch_sim.rs`), not here.
    let output = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--sensor",
            "cam0-sensor",
        ])
        .args(["--frames", &frames, "--frame-length", length, "--fps", fps])
        .args(["--pattern", "1", "--reference"])
        .arg(&reference)
        .arg("--clock-file")
        .arg(&clock)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    // `CHAIN_SHOW=1 cargo test ... --nocapture` prints the proof's whole output.
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = writeln!(
            std::io::stdout(),
            "stream buffers: {kind}\nchain proof:\n{text}"
        );
    }
    service.stop();
    assert!(!output.status.success(), "a stale stamp passed:\n{text}");
    assert!(
        text.contains("frames taken more than 1 s after their start: front end ")
            && !text.contains("front end 0 of"),
        "the age check did not name the stale frames:\n{text}"
    );
}
