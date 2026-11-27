//! The chain proof's clock file (`lemnos-sim-clock`, `--clock-file`) refuses two things, each by
//! name: a file shorter than the clock's 16 bytes, and a clock that goes backwards. The stand-in
//! clock must refuse both where the real clock file would be refused, or a chain could be timed
//! against a clock no board has. The first case runs the proof as a process, as a board run does;
//! the second reads the clock object the proof installs (`FileClock`) as the proof reads it.

use std::process::Command;

use lemnos_sim_clock::{Clock, FileClock};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

/// A scratch directory under TMPDIR, removed when the guard drops, pass or panic. It derefs to
/// `path`, a file or directory inside `dir`, so the guard must stay bound while `path` is used.
struct Scratch {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl Scratch {
    fn of_dir(dir: std::path::PathBuf) -> Self {
        Self {
            path: dir.clone(),
            dir,
        }
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(_name: &str) -> Scratch {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    Scratch::of_dir(dir)
}

/// The clock file's layout: the sequence word at 0, the time at 8 (`sim-clock`'s `write_raw`).
fn clock_bytes(seq: u64, ns: u64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&seq.to_le_bytes());
    bytes[8..].copy_from_slice(&ns.to_le_bytes());
    bytes
}

#[test]
fn a_short_clock_file_refuses_the_chain_proof_by_name() {
    let dir = scratch("short");
    let clock = dir.join("sim-clock");
    std::fs::write(&clock, [0u8; 8]).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
        .arg("--clock-file")
        .arg(&clock)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a run with a short clock file must fail:\n{text}"
    );
    assert!(
        text.contains("is 8 bytes, short of 16"),
        "the refusal must name the short file:\n{text}"
    );
    assert!(
        !text.contains("clock: sim clock file"),
        "the run must not get as far as installing the clock:\n{text}"
    );
}

#[test]
fn a_clock_that_goes_backwards_is_refused_when_the_proof_reads_it() {
    let dir = scratch("backwards");
    let clock = dir.join("sim-clock");
    std::fs::write(&clock, clock_bytes(2, 1_000)).unwrap();
    let reader = FileClock::open(&clock).unwrap();
    assert_eq!(reader.now_ns().unwrap(), 1_000);
    // A later, complete write with an earlier time: the sequence is even again, so the read is
    // settled, and only the monotonic rule can refuse it.
    std::fs::write(&clock, clock_bytes(4, 500)).unwrap();
    let err = reader.now_ns().unwrap_err();
    assert!(
        err.contains("sim clock went backwards: 1000 -> 500"),
        "the refusal must name both times: {err}"
    );
}

/// A chain run with its clock file moved backwards while the proof runs: the proof reads the file
/// as a frame clock, and must refuse the backwards time by name, not pass the run.
#[test]
fn a_clock_moved_backwards_mid_run_refuses_the_chain_proof() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
    use lemnosd::{Config, Service};

    assert!(lemnos_camera_sim::pisp::install());
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let dir = scratch("midrun");
    let clock = dir.join("sim-clock");
    // The sim's writer moves the clock on as frames start, and the proof reads it from the file.
    lemnos_camera_sim::time::install_file(&clock).unwrap();
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
    let service = Service::start_with_buffers(
        Config {
            socket: socket.clone(),
            name: board.board.id.clone(),
            board,
            drivers: drivers(),
            backend: Box::new(provider()),
            options: lemnos_test_options::options(),
        },
        Arc::new(SimBuffers::probe()),
    )
    .unwrap();
    let reference = dir.join("reference.raw10");
    std::fs::write(&reference, scene::reference_raw10()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lemnos-chain-proof"))
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--sensor",
            "cam0-sensor",
        ])
        .args(["--frames", "120", "--frame-length", "3664", "--fps", "30"])
        .args(["--pattern", "1", "--reference"])
        .arg(&reference)
        .args(["--allow-skip", "latch"])
        .arg("--clock-file")
        .arg(&clock)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Move the clock back to a small time, over and over, while the proof is still running. The
    // sim's writer keeps moving it forward too, so the proof sees the earlier time, after a later
    // one, at least once while it is still reading.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the proof did not finish; it was not refused"
        );
        // Written in place, not truncated: the sim's writer and the proof share this file.
        use std::os::unix::fs::FileExt;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&clock)
            .unwrap()
            .write_all_at(&clock_bytes(2, 1), 0)
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    service.stop();
    let _ = std::fs::remove_dir_all(&*dir);
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stdout(),
            format_args!("chain proof:\n{text}\n"),
        );
    }
    assert!(
        !output.status.success(),
        "a clock that went backwards passed the proof:\n{text}"
    );
    // The proof fails by the clock's refusal and nothing else: its failure line is the clock's own
    // words, as FileClock gives them, with no other failed check after it.
    let failed = text
        .lines()
        .find_map(|line| line.strip_prefix("chain proof: FAILED: "))
        .unwrap_or_else(|| panic!("the proof must print a FAILED line:\n{text}"));
    assert!(
        failed.starts_with("clock read failed: sim clock went backwards: ")
            && !failed.contains("; "),
        "the proof must fail by the clock's refusal alone, by name:\n{text}"
    );
}
