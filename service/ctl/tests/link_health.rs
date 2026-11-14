//! A healthy camera chain as the tool shows it. The chain is the simulated one (`lemnos-camera-sim`)
//! under the real service: the real sensor driver (`ov9782`) with its board `link` to the receiver,
//! and a simulated receiver whose `source` answers NOT_SUPPORTED, as the kernel's does where it does
//! not describe its input (the reference camera board, run c1). The stand-in differs from the real receiver in
//! that it is not the kernel module and it answers in-process, not through the device's wire.
//! `describe` prints no health (the description does not carry it), so the check there is that it
//! answers for the sensor with its contract.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider};
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../testing/boards/reference.toml");

fn ctl(socket: &Path, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

/// The row of `name` in a list or chain: the first word is the name.
fn row<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines()
        .find(|row| row.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no row for {name} in:\n{text}"))
}

#[test]
fn a_healthy_chain_with_stated_wiring_reads_ok_in_list_chain_and_describe() {
    assert!(lemnos_camera_sim::pisp::install());
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = std::env::temp_dir().join(format!("link-health-{}", std::process::id()));
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

    // The sensor binds on the service's own ticks: wait until it runs, a bounded wait.
    let deadline = Instant::now() + Duration::from_secs(30);
    let list = loop {
        let text = ctl(&socket, &["list"]).unwrap();
        if row(&text, "cam0-sensor").contains("running") {
            break text;
        }
        assert!(Instant::now() < deadline, "the sensor never ran:\n{text}");
        std::thread::sleep(Duration::from_millis(100));
    };

    // list: running, ok, no reason after the row.
    let listed = row(&list, "cam0-sensor");
    assert!(listed.contains("running  ok  camera.sensor/1"), "{listed}");
    assert!(
        !listed.contains('('),
        "no reason is printed for ok: {listed}"
    );

    // chain: the sensor's row is ok with no reason; the chain goes on to the receiver.
    let chain = ctl(&socket, &["chain", "cam0-sensor"]).unwrap();
    let first = chain.lines().next().unwrap();
    assert!(first.starts_with("cam0-sensor "), "{chain}");
    assert!(first.contains("running  ok"), "{first}");
    assert!(!first.contains('('), "no reason is printed for ok: {first}");
    assert!(
        chain.lines().any(|r| r.starts_with("cam0-receiver ")),
        "{chain}"
    );

    // describe: the sensor, its driver, and its camera.sensor/1 contract.
    let described = ctl(&socket, &["describe", "cam0-sensor"]).unwrap();
    assert!(described.starts_with("cam0-sensor (ov9782)"), "{described}");
    assert!(described.contains("camera.sensor/1"), "{described}");
    assert!(!described.contains("cannot say"), "{described}");

    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
