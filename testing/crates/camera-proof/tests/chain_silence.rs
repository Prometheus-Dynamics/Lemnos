//! A sensor that stops sending behind its driver: the real `ov9782` driver's silence watch sees the
//! receiver's frame-start count stand still, and restarts the sensor. The stop is a register write
//! from outside the driver (`lemnos_camera_sim::silence_sensor`); the driver's state is unchanged,
//! so only its watch can act. Runs on the chain's in-process daemon with the real drivers, through
//! the camera proof's `Camera` (the same start order as the board). The watch judges by the host's
//! clock, so the restart is waited for in real time. This is the sim half of the F2i silence
//! check; the board step is `testing/board-runs/SILENCE.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_camera_proof::setup::{Chain, Tuning};
use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_client::Client;
use lemnos_contract_camera_receiver::{CameraReceiver, Packing};
use lemnos_driver_ov9782::SILENCE_MARGIN_NS;
use lemnos_host::Options as HostOptions;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");
/// How long the test allows the watch, in real time, before it calls the restart missing.
const RESTART_WITHIN: Duration = Duration::from_secs(30);

/// The test's scratch directory, removed when it drops.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_sensor_that_stops_behind_its_driver_is_seen_silent_and_restarted() {
    assert!(lemnos_camera_sim::pisp::install());
    lemnos_camera_sim::pisp::set_paced(true);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = std::env::temp_dir().join(format!("chain-silence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Removed when the test ends, passed or panicked; declared before the service so it goes last.
    let _scratch = Scratch(dir.clone());
    let socket = dir.join("lemnosd.sock");
    let service = Service::start_with_buffers(
        Config {
            socket: socket.clone(),
            name: board.board.id.clone(),
            board,
            drivers: drivers(),
            backend: Box::new(provider()),
            options: HostOptions::default(),
        },
        Arc::new(SimBuffers::probe()),
    )
    .unwrap();
    std::fs::write(dir.join("reference.raw10"), scene::reference_raw10()).unwrap();
    let client = Client::connect(&socket, "chain-silence").unwrap();
    client.set_timeout(Duration::from_secs(30));
    let list = client.list().unwrap();
    let mut chain = Chain::open(
        &client,
        &list,
        ["cam0-sensor", "cam0-receiver", "cam0-fe", "cam0-isp"],
        Tuning {
            frame_length: Some(3664),
            pattern: Some(1),
        },
    )
    .unwrap();
    // The receiver is configured with the sensor's mode link and started before the sensor, as the
    // camera's start order requires (the chain's start does not start the receiver).
    chain
        .receiver
        .configure(chain.mode.link, Packing::Packed)
        .unwrap();
    chain.receiver.start().unwrap();
    chain.start().unwrap();

    // The receiver counts frame starts only once it streams, and the watch needs a count that has
    // moved. Each sensor start the test steps is a frame while the sensor streams.
    let until = Instant::now() + Duration::from_secs(30);
    while !lemnos_camera_sim::world(|w| w.streaming) {
        assert!(
            Instant::now() < until,
            "the sensor did not start streaming within 30 s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..3 {
        lemnos_camera_sim::sensor_step();
    }

    // The sensor stops behind its driver: no more sensor starts are stepped, and the sim's sensor
    // sends nothing. Only the driver's watch can restart it.
    lemnos_camera_sim::silence_sensor();
    assert!(
        !lemnos_camera_sim::world(|w| w.streaming),
        "the sensor is silent right after the cut"
    );
    let cut = Instant::now();
    let until = cut + RESTART_WITHIN;
    while !lemnos_camera_sim::world(|w| w.streaming) {
        assert!(
            Instant::now() < until,
            "the silence watch did not restart the silent sensor within {RESTART_WITHIN:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // The count last moved at the last start before the cut, so the watch's bound is met a little
    // after the cut. A restart inside the margin would mean the watch judged nothing.
    let silent_for = cut.elapsed();
    assert!(
        silent_for >= Duration::from_nanos(SILENCE_MARGIN_NS),
        "the sensor restarted {silent_for:?} after the cut, inside the watch margin {SILENCE_MARGIN_NS} ns"
    );

    // After the restart the sensor streams and makes frames again.
    lemnos_camera_sim::sensor_step();
    assert!(lemnos_camera_sim::world(|w| w.streaming));
    drop(chain);
    drop(service);
}
