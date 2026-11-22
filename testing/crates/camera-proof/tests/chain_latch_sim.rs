//! The sensor latch, run in process on the simulated clock: the same `latch::run` the board's
//! `lemnos-sensor-proof latch` runs, with the simulator's time. Its numbers are exact, whatever the
//! host's load: each of the eleven offsets lands every post on the predicted frame, the latch's
//! two-post check shows the long exposure, and the period is the sensor's.

mod sim_timing;

use std::io::Write;

use std::sync::Arc;
use std::time::Duration;

use lemnos_camera_proof::SocketBus;
use lemnos_camera_proof::latch::{self, OFFSETS_US, Settings};
use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_client::Client;
use lemnos_host::Options;
use lemnosd::{Config, Service};
use sim_timing::SimTiming;

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

/// Starts the chain's service on a scratch socket, with the simulated sensor, receiver and blocks.
fn service(dir: &std::path::Path) -> (Service, std::path::PathBuf) {
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let socket = lemnos_socket_dir::socket(dir, "lemnosd.sock").unwrap();
    let service = Service::start_with_buffers(
        Config {
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
    std::fs::write(dir.join("reference.raw10"), scene::reference_raw10()).unwrap();
    (service, socket)
}

/// Removes the test's scratch directory when the test ends, passed or panicked.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_latch_on_its_own_lands_every_post_on_the_predicted_frame() {
    assert!(lemnos_camera_sim::pisp::install());
    lemnos_camera_sim::pisp::set_paced(true);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let _remove = RemoveOnDrop(dir.clone());
    let (service, socket) = service(&dir);
    let client = Client::connect(&socket, "chain-latch-sim").unwrap();
    client.set_timeout(Duration::from_secs(30));
    let bus = SocketBus::new(client);
    let mut lines = Vec::new();
    let result = latch::run(
        &bus,
        &Settings {
            sensor: Some("cam0-sensor".into()),
            trials: 2,
            offsets_us: &OFFSETS_US,
            frame_wait: Duration::from_secs(30),
        },
        &SimTiming,
        &mut |line| lines.push(line),
    );
    service.stop();
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = writeln!(std::io::stdout(), "{}", lines.join("\n"));
    }
    let report = result.unwrap_or_else(|e| panic!("the latch failed: {e}\n{lines:#?}"));
    assert_eq!(
        report.period_ns,
        lemnos_camera_sim::period_ns(),
        "{lines:#?}"
    );
    assert_eq!(report.offsets.len(), OFFSETS_US.len());
    for (counts, offset) in report.offsets.iter().zip(OFFSETS_US) {
        assert_eq!(
            (
                counts.offset_us,
                counts.trials,
                counts.predicted,
                counts.early,
                counts.late,
                counts.lost
            ),
            (offset, 2, 2, 0, 0, 0),
            "{lines:#?}"
        );
    }
    assert_eq!(report.deadline_us, Some(16500));
    assert_eq!(
        (report.second_long, report.first_middle),
        (2, 0),
        "{lines:#?}"
    );
    assert!(report.brightness_long - report.brightness_short >= 8.0);
    assert!(report.passed, "{lines:#?}");
}
