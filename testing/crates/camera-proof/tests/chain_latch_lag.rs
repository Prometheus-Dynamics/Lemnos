//! The latch's negative case, in process on the simulated clock. The stand-in sensor shows each
//! exposure one frame early (`World::latch_frames` 1, where the sensor's is 2). The latch must
//! report every post early and fail, as it does on a board whose sensor latches a frame early.

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

#[test]
fn a_sensor_that_latches_a_frame_early_fails_the_latch() {
    assert!(lemnos_camera_sim::pisp::install());
    lemnos_camera_sim::pisp::set_paced(true);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    lemnos_camera_sim::world(|w| w.latch_frames = 1);
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = std::env::temp_dir().join(format!("chain-latch-lag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("lemnosd.sock");
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
    let client = Client::connect(&socket, "chain-latch-lag").unwrap();
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
    let _ = std::fs::remove_dir_all(&dir);
    if std::env::var_os("CHAIN_SHOW").is_some() {
        let _ = writeln!(std::io::stdout(), "{}", lines.join("\n"));
    }
    let report = result.unwrap_or_else(|e| panic!("the latch failed to run: {e}\n{lines:#?}"));
    assert!(
        !report.passed,
        "an early sensor passed the latch:\n{lines:#?}"
    );
    for counts in &report.offsets {
        assert_eq!((counts.predicted, counts.early), (0, 2), "{lines:#?}");
    }
    assert_eq!(report.deadline_us, None);
}
