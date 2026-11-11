//! A merged driver crate, built as a package with nothing but the export line added, runs on
//! the simulated board exactly as it does built into the host.
//!
//! The driver is `drivers/ws2812`, which uses an SPI device. `tests/ws2812-package` is the
//! wrapper crate: its whole source is `export_package!(lemnos_driver_ws2812::Ws2812)`. The same
//! board and the same calls are run twice, once on the loaded package and once on the driver
//! compiled in, and the bytes on the bus are compared.

mod common;

use common::{host_on, index, method, ws2812_package};
use lemnos_core::{Health, Status};
use lemnos_driver_ws2812::{Ws2812, encode_spi_byte};
use lemnos_host::{DeviceState, DriverEntry, Options, entry};
use lemnos_package::{Loader, Mode};
use lemnos_sim::SimBoard;

const DEVICES: &str = r#"
[[devices]]
id = "strip"
driver = "ws2812"
uses = { spi = "spi0" }
config = { count = 2 }
"#;

/// One RGB LED as the strip takes it: green, red, blue, three SPI bytes each.
fn led(r: u8, g: u8, b: u8) -> Vec<u8> {
    [g, r, b].into_iter().flat_map(encode_spi_byte).collect()
}

/// `pixels/1` `set`: an LED offset, then the bytes.
fn set(offset: u16, pixels: &[u8]) -> Vec<u8> {
    let mut args = offset.to_le_bytes().to_vec();
    args.extend((pixels.len() as u32).to_le_bytes());
    args.extend(pixels);
    args
}

/// Binds the strip from `drivers`, sets and shows two frames with a bus failure between them,
/// and returns everything the bus was sent, one entry per transaction.
fn run(drivers: &[DriverEntry]) -> Vec<Vec<u8>> {
    let sim = SimBoard::new();
    let spi = sim.add_spi("spi0");
    let faults = sim.faults("spi0").unwrap();
    let (host, _) = host_on(sim, DEVICES, drivers, Options::default());
    let strip = index(&host, "strip");
    let device = host.device("strip").unwrap();
    assert_eq!(device.state, DeviceState::Running);
    assert_eq!(device.tags[0].as_str(), "pixels/1");
    assert!(host.describe(strip).unwrap().find("pixels").is_some());

    let mut reply = [0; 8];
    assert_eq!(host.call(strip, method(1), &[], &mut reply), Ok(2));
    assert_eq!(u16::from_le_bytes([reply[0], reply[1]]), 2);
    // Red on the first LED, blue on the second: nothing is sent until `show`.
    let frame = [0xff, 0, 0, 0, 0, 0, 0xff, 0];
    assert_eq!(
        host.call(strip, method(2), &set(0, &frame), &mut reply),
        Ok(0)
    );
    assert_eq!(spi.written().len(), 1, "set must not touch the bus");
    assert_eq!(host.call(strip, method(3), &[], &mut reply), Ok(0));

    // The bus fails once: the call says so, health says so, and the next show sends the frame.
    faults.fail_next(lemnos_core::ErrorKind::Busy);
    assert_eq!(
        host.call(strip, method(3), &[], &mut reply),
        Err(Status::BUSY)
    );
    assert!(matches!(
        host.device("strip").unwrap().health,
        Health::Failed(_)
    ));
    assert_eq!(
        host.call(strip, method(2), &set(1, &[0, 0xff, 0, 0]), &mut reply),
        Ok(0)
    );
    assert_eq!(host.call(strip, method(3), &[], &mut reply), Ok(0));
    assert_eq!(host.device("strip").unwrap().health, Health::Ok);
    host.shutdown();
    spi.written()
}

#[test]
fn a_merged_driver_built_as_a_package_runs_as_it_does_built_in() {
    let mut loader = Loader::new(Mode::Open);
    let package = loader.load(ws2812_package()).unwrap();
    assert_eq!(package.name(), "ws2812");
    assert_eq!(package.manifest().provides[0].tag.as_str(), "pixels/1");
    let need = &package.manifest().needs[0];
    assert_eq!((need.name, need.optional), ("spi", false));

    let packaged = run(&loader.entries());
    let built_in = run(&[entry::<Ws2812>()]);
    assert_eq!(packaged, built_in);

    // And what both sent is the strip's bit stream: off at the start, then the two frames.
    let off: Vec<u8> = [led(0, 0, 0), led(0, 0, 0)].concat();
    let first: Vec<u8> = [led(0xff, 0, 0), led(0, 0, 0xff)].concat();
    let second: Vec<u8> = [led(0xff, 0, 0), led(0, 0xff, 0)].concat();
    assert_eq!(packaged, [off, first, second]);
}

#[test]
fn a_packaged_driver_without_its_bus_waits_for_it_as_a_built_in_one_does() {
    let mut loader = Loader::new(Mode::Open);
    let entries = [loader.load(ws2812_package()).unwrap().entry()];
    let state = |drivers: &[DriverEntry]| {
        let (host, _) = host_on(SimBoard::new(), DEVICES, drivers, Options::default());
        let DeviceState::Backoff {
            attempts, reason, ..
        } = host.device("strip").unwrap().state
        else {
            panic!("{:?}", host.device("strip").unwrap().state);
        };
        (attempts, reason)
    };
    assert_eq!(state(&entries), state(&[entry::<Ws2812>()]));
}
