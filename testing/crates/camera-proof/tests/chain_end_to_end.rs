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
use std::time::{Duration, Instant};

use lemnos_camera_sim::profile;
use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

/// A failing run's text: what failed, then the sim tick profile (where the time went).
fn fail(what: &str) -> ! {
    panic!("{}", profile::failure(what));
}

fn check(ok: bool, what: impl FnOnce() -> String) {
    if !ok {
        fail(&what());
    }
}

fn scratch() -> PathBuf {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_chain_proof_passes_end_to_end_against_the_simulated_chain() {
    assert!(lemnos_camera_sim::pisp::install());
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    // The chain's clock is the sim's: frames move it, and the proof reads it from this file.
    let clock_dir = scratch();
    let clock = clock_dir.join("sim-clock");
    lemnos_camera_sim::time::install_file(&clock).unwrap();
    profile::reset();
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = scratch();
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
    // The latch is not run here: it is timed on the host's clock on a board, and runs in process
    // on the sim clock in `chain_latch_sim.rs`. The proof says so, and does not check it again.
    // The proof's output goes to files, so a proof that stops making progress can be cut off at
    // the deadline (`CHAIN_TIMEOUT_S`, 300 by default) and the failure still shows what it printed.
    let timeout = Duration::from_secs(
        std::env::var("CHAIN_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
    );
    let stdout_path = dir.join("proof.stdout");
    let stderr_path = dir.join("proof.stderr");
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
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
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            // Our own child, stopped at the deadline: the proof is not going to finish.
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let wall = started.elapsed();
    let text = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let errors = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    // `CHAIN_SHOW=1 cargo test ... --nocapture` prints the proof's whole output.
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = writeln!(
            std::io::stdout(),
            "stream buffers: {kind}\nchain proof:\n{text}"
        );
    }
    let Some(status) = status else {
        fail(&format!(
            "the chain proof did not finish within {} s (stopped after {} ms; stream buffers: {kind}):\nstdout:\n{text}\nstderr:\n{errors}",
            timeout.as_secs(),
            wall.as_millis()
        ));
    };
    check(status.success(), || {
        format!(
            "the chain proof failed (stream buffers: {kind}, after {} ms):\n{text}\nstderr:\n{errors}",
            wall.as_millis()
        )
    });
    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
    check(text.contains("chain proof: pass"), || text.clone());
    // Exact counts, whatever the host's load: the sim clock moves only with frame starts, so
    // nothing is late, nothing is dropped and no placeholder is delivered in a steady run.
    for line in [
        "front end: jobs ".to_string(),
        "dropped 0, errors 0".to_string(),
        format!(
            "frames taken more than 1 s after their start: front end 0 of {frames}, back end 0 of {frames}"
        ),
        "placeholder frames: front end 0, back end 0".to_string(),
        "clock: sim clock file".to_string(),
        "sensor latch: checked in-process (sim clock)".to_string(),
    ] {
        check(text.contains(&line), || {
            format!("missing {line:?} in:\n{text}")
        });
    }
    // The back end's luma is made once for the sim's scene: the front end sends the same frame every
    // time, so the conversion runs on the first job and a job whose input is unchanged reuses it.
    // Before the change it ran once a job (`frames` times), which is most of the sim's cost.
    let conversions =
        lemnos_camera_sim::pisp::LUMA_CONVERSIONS.load(std::sync::atomic::Ordering::Relaxed);
    check(conversions == 1, || {
        format!("the back end converted its luma {conversions} times for an unchanged input")
    });
    // What a board run must show before it claims the path: both stages bound, the streams as
    // the host made them, and frames end to end. Which path ran depends on the host's buffers, and
    // the proof says which: a dma-buf host lends each frame; a host without a dma-heap copies them
    // through the back end's input, and proves the lend refused on a memfd.
    let dma = kind.starts_with("dma-bufs");
    let (stats, input, path) = if dma {
        (
            "dma-buf per buffer",
            "dma-buf per buffer, taken writable (not written: the front end's output0 is lent to each job)",
            "front end output0: dma-bufs: the lent path",
        )
    } else {
        (
            "memfd per buffer",
            "memfd per buffer, taken writable (written: the front end's frames are copied in, not lent)",
            "front end output0: memfds: the copy path",
        )
    };
    for line in [
        "stage front end: cam0-fe (pisp-fe) Running Ok, provides camera.isp/1, isp.pisp/1"
            .to_string(),
        "stage back end: cam0-isp (pisp-be) Running Ok, provides camera.isp/1, isp.pisp/1"
            .to_string(),
        "cam0-fe config: 8 buffers of 632 bytes, job-driven, shared memory, taken writable"
            .to_string(),
        format!("cam0-fe statistics: 8 buffers of 23200 bytes, job-driven, {stats}"),
        format!("cam0-isp input: 2 buffers of 2048000 bytes, job-driven, {input}"),
        path.to_string(),
        format!("frames delivered end to end: {frames}"),
    ] {
        check(text.contains(&line), || {
            format!("missing {line:?} in:\n{text}")
        });
    }
    if dma {
        check(text.contains("chain proof: pass (lent path"), || {
            text.clone()
        });
    } else {
        // The refusal is asserted for real: the one lent job on a memfd was refused bad-config, and
        // the run went on over the copy path.
        check(
            text.contains("lend probe: a lent job on a memfd frame (identity")
                && text.contains("was refused: job end bad-config"),
            || format!("the memfd lend was not refused as bad-config:\n{text}"),
        );
        check(text.contains("chain proof: pass (copy path"), || {
            text.clone()
        });
    }
}
