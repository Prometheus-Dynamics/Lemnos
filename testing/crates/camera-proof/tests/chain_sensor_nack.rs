//! The sensor bus acknowledges nothing at the sensor's address, as a real I2C bus with the sensor
//! unplugged would not: the transfer is a Nack (`ErrorKind::Nack`, which `backends/linux` gives
//! for ENXIO or EREMOTEIO, see `engine/hal`'s `from_errno`). The chain must refuse the sensor by
//! name, not run on a sensor that answered nothing. A stand-in bus that acknowledges everything
//! would let the chain run, so the proof must fail here, and the failure must name the Nack.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

#[test]
fn a_sensor_that_nacks_its_bus_refuses_the_chain_by_name() {
    assert!(lemnos_camera_sim::pisp::install());
    let mut world = new_world(true, Model::B1);
    // The reference board's sensor is at 0x60 on i2c-10. Remove it from the bus: nothing answers.
    world
        .buses
        .get_mut("i2c-10")
        .expect("the reference board has i2c-10")
        .targets
        .remove(&0x60);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(world);
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir: PathBuf = lemnos_socket_dir::scratch_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
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
    let reference = dir.join("reference.raw10");
    std::fs::write(&reference, scene::reference_raw10()).unwrap();
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
        .args(["--allow-skip", "latch"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    // The sensor's own refusal, as the service records it: the ID read got a Nack, and the driver
    // names that as nothing answering at the address (drivers/ov9782 bind).
    let client = lemnos_client::Client::connect(&socket, "chain-sensor-nack").unwrap();
    let entries = client.list().unwrap();
    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !output.status.success(),
        "a chain with no sensor on its bus passed:\n{text}"
    );
    let sensor = entries
        .iter()
        .find(|e| e.name == "cam0-sensor")
        .expect("the sensor is listed");
    assert!(
        sensor
            .reason
            .contains("no device at 0x60: nothing answers at its address"),
        "the sensor must refuse by name for the Nack, got reason {:?}:\n{text}",
        sensor.reason
    );
}

/// The sim's Nack is the form `backends/linux` gives for an I2C NACK: ENXIO or EREMOTEIO, which
/// `ErrorKind::from_errno` maps to `Nack`. A stand-in that refused with another form would be
/// handled by a driver that cannot see the real failure.
#[test]
fn the_sim_nack_is_the_form_the_linux_backend_maps() {
    use lemnos_hal::ErrorKind;
    assert_eq!(ErrorKind::from_errno(6), ErrorKind::Nack); // ENXIO
    assert_eq!(ErrorKind::from_errno(121), ErrorKind::Nack); // EREMOTEIO
}
