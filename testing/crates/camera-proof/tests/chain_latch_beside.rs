//! The sensor latch cannot run beside the chain, here as on a board.
//!
//! The kernel module refuses the receiver's raw `frames` stream while the front end streams
//! (`kernel/rs/rp1_csi2.rs`, `start`: `EBUSY`), because in front-end mode the CSI-2
//! channel feeds the front end instead of memory. `lemnos-sensor-proof latch` takes that stream,
//! so `lemnos-chain-proof --latch-trials` beside a running chain fails on a board. The simulated
//! receiver makes the same refusal (`lemnos_camera_sim::EXCLUSIVE`), so this is a test with a
//! stand-in that fails where the real one does: a chain proof with `--latch-trials` and no skip
//! must FAIL (`chain_end_to_end.rs` runs the same chain with the latch on its own and
//! `--allow-skip latch`, and it passes).
//!
//! What the simulated receiver does not share with the module: only the one rule above (a start
//! refused with `EBUSY`); it has no claim on a node, no DMA and no interrupt.
//!
//! One test per process: the simulated PiSP blocks are installed once per process.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

#[test]
fn the_latch_beside_the_chain_fails_as_on_a_board() {
    assert!(lemnos_camera_sim::pisp::install());
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir: PathBuf =
        std::env::temp_dir().join(format!("chain-latch-beside-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("lemnosd.sock");
    let service = Service::start_with_buffers(
        Config {
            log: None,
            socket: socket.clone(),
            name: board.board.id.clone(),
            board,
            drivers: drivers(),
            backend: Box::new(provider()),
            options: Options::default(),
        },
        Arc::new(SimBuffers::probe()),
    )
    .unwrap();
    let reference = dir.join("reference.raw10");
    std::fs::write(&reference, scene::reference_raw10()).unwrap();
    let run = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
            .args([
                "--socket",
                socket.to_str().unwrap(),
                "--sensor",
                "cam0-sensor",
            ])
            .args(["--frames", "30", "--frame-length", "3664", "--fps", "30"])
            .args(["--pattern", "1", "--reference"])
            .arg(&reference)
            .args(extra)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };
    let (ok, text) = run(&["--latch-trials", "2"]);
    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "a latch beside the chain must fail the proof:\n{text}");
    // The latch's raw start is the receiver's refusal while the front end holds the block: the
    // proof's failure names the latch, and the latch's own error names the start that was refused
    // and why (BUSY, the kernel's EBUSY for a second stream on the node).
    assert!(text.contains("sensor latch failed"), "{text}");
    let refused = format!("receiver.start failed: {:?}", lemnos_core::Status::BUSY);
    assert!(
        text.contains(&refused),
        "the latch's refusal must be `{refused}` (BUSY from the receiver's start):\n{text}"
    );
}
