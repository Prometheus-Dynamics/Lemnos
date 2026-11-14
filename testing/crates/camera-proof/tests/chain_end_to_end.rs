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

use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chain-end-to-end-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_chain_proof_passes_end_to_end_against_the_simulated_chain() {
    assert!(lemnos_camera_sim::pisp::install());
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = scratch();
    let socket = dir.join("lemnosd.sock");
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
    let output = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--sensor",
            "cam0-sensor",
        ])
        .args(["--frames", &frames, "--frame-length", length, "--fps", fps])
        .args(["--pattern", "1", "--latch-trials", "2", "--reference"])
        .arg(&reference)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    // `CHAIN_SHOW=1 cargo test ... --nocapture` prints the proof's whole output.
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = writeln!(std::io::stdout(), "stream buffers: {kind}\n{text}");
    }
    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "the chain proof failed (stream buffers: {kind}):\n{text}"
    );
    assert!(text.contains("chain proof: pass"), "{text}");
    // What a board run must show before it claims the path: both stages bound, the streams as
    // declared (the back end's input is dma-bufs its caller fills), and frames end to end.
    for line in [
        "stage front end: cam0-fe (pisp-fe) Running Ok",
        "stage back end: cam0-isp (pisp-be) Running Ok",
        "cam0-fe config: 8 buffers of 632 bytes, job-driven, shared memory, taken writable",
        "cam0-fe statistics: 8 buffers of 23200 bytes, job-driven, dma-buf per buffer",
        "cam0-isp input: 2 buffers of 2048000 bytes, job-driven, dma-buf per buffer, taken writable",
        &format!("frames delivered end to end: {frames}"),
    ] {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
}
