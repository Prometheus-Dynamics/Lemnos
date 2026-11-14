//! The reference board's camera streams. Every `buffer_bytes` the board file states is recomputed
//! here from the drivers' own constants and layout rules and the sensor's mode, and the board must
//! agree with each one. A board that validates but states another size fails here.

use lemnos_camera_proof::chain::{Stage, resolve};
use lemnos_client::{DeviceEntry, DeviceHealth, DeviceState};
use lemnos_contract_camera_sensor::CameraSensor;
use lemnos_core::Contract;
use lemnos_driver_ov9782::JOB_BYTES;
use lemnos_driver_ov9782::chip::MODES;
use lemnos_driver_pisp::fe::FE_OUTPUT_ALIGN;
use lemnos_driver_pisp::fe_job;
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::BE_OUTPUT_MIN_ALIGN;
use lemnos_driver_pisp::uapi::ImageFormatConfig;
use lemnos_driver_pisp::{BE_INPUT_ALIGN, be_job};
use lemnos_driver_pisp_sys::CONFIG_BYTES as BE_CONFIG_BYTES;
use lemnos_driver_rp1_csi2::packed_frame_bytes;
use lemnos_host::board::{BoardFile, parse};

/// The reference board file, as the other tests of the repository read it.
const REFERENCE: &str = include_str!("../../../boards/reference.toml");

/// The pad after each front-end image, one page: the reference's jobs' `pad_bytes`.
const FE_PAD_BYTES: u64 = 4096;

/// The mode the camera's streams run: the sensor's first mode, width, height and bit depth.
fn mode() -> (u32, u32, u8) {
    let mode = MODES[0];
    (
        u32::from(mode.window.width),
        u32::from(mode.window.height),
        mode.depth.bit_depth,
    )
}

/// Bytes of one `width` x `height` image in `format`, its stride aligned to `align`, plus `pad`.
fn image_bytes(format: u32, width: u32, height: u32, align: u32, pad: u64) -> u64 {
    let mut image = ImageFormatConfig {
        width: u16::try_from(width).expect("a mode's width"),
        height: u16::try_from(height).expect("a mode's height"),
        format,
        ..Default::default()
    };
    compute_stride_align(&mut image, align);
    be_job::image_bytes(&image).expect("a mode's image fits") + pad
}

/// Every camera stream of the reference, as (device, stream, the bytes its driver gives a buffer).
fn expected() -> Vec<(&'static str, &'static str, u64)> {
    let (width, height, depth) = mode();
    let (half_width, half_height) = (width / 2, height / 2);
    let frame = u64::from(packed_frame_bytes(width, height, depth).expect("a mode's frame"));
    vec![
        ("cam0-sensor", "values", JOB_BYTES as u64),
        ("cam0-receiver", "frames", frame),
        ("cam0-fe", "config", fe_job::CONFIG_BYTES as u64),
        ("cam0-fe", "statistics", fe_job::STATS_BYTES as u64),
        (
            "cam0-fe",
            "output0",
            image_bytes(
                formats::BAYER16,
                width,
                height,
                FE_OUTPUT_ALIGN,
                FE_PAD_BYTES,
            ),
        ),
        (
            "cam0-fe",
            "output1",
            image_bytes(
                formats::BAYER16,
                half_width,
                half_height,
                FE_OUTPUT_ALIGN,
                FE_PAD_BYTES,
            ),
        ),
        ("cam0-isp", "config", BE_CONFIG_BYTES as u64),
        (
            "cam0-isp",
            "input",
            image_bytes(formats::BAYER16, width, height, BE_INPUT_ALIGN, 0),
        ),
        (
            "cam0-isp",
            "output0",
            image_bytes(formats::NV12, width, height, BE_OUTPUT_MIN_ALIGN, 0),
        ),
        (
            "cam0-isp",
            "output1",
            image_bytes(
                formats::NV12,
                half_width,
                half_height,
                BE_OUTPUT_MIN_ALIGN,
                0,
            ),
        ),
    ]
}

fn reference() -> BoardFile {
    parse(REFERENCE).expect("the reference board validates")
}

fn buffer_bytes(board: &BoardFile, device: &str, stream: &str) -> Option<u64> {
    let spec = board.devices.iter().find(|d| d.id == device)?;
    spec.streams.get(stream).map(|s| u64::from(s.buffer_bytes))
}

#[test]
fn the_reference_board_validates() {
    let _ = reference();
}

#[test]
fn every_camera_stream_is_sized_as_its_drivers_compute() {
    let board = reference();
    for (device, stream, bytes) in expected() {
        assert_eq!(
            buffer_bytes(&board, device, stream),
            Some(bytes),
            "{device}.{stream} is sized {bytes} by its driver"
        );
    }
}

#[test]
fn the_camera_declares_no_stream_beyond_these() {
    let board = reference();
    let declared: usize = ["cam0-sensor", "cam0-receiver", "cam0-fe", "cam0-isp"]
        .iter()
        .map(|device| {
            board
                .devices
                .iter()
                .find(|d| d.id == *device)
                .map_or(0, |d| d.streams.len())
        })
        .sum();
    assert_eq!(declared, expected().len());
}

/// The device list the daemon would give for `board`: names, drivers and links as declared, the
/// sensor tagged as a camera sensor.
fn listed(board: &BoardFile) -> Vec<DeviceEntry> {
    board
        .devices
        .iter()
        .enumerate()
        .map(|(i, d)| DeviceEntry {
            index: u32::try_from(i).unwrap(),
            name: d.id.clone(),
            driver: d.driver.clone(),
            tags: if d.driver == "ov9782" {
                vec![<dyn CameraSensor as Contract>::TAG.as_str().to_string()]
            } else {
                Vec::new()
            },
            state: DeviceState::Running,
            health: DeviceHealth::Ok,
            reason: String::new(),
            link: d.link.clone(),
            jobs: Vec::new(),
        })
        .collect()
}

/// The chain proof takes its devices from the board's links: the reference board names the back
/// end `cam0-isp`, the chain example `cam0-be`, and the same resolution finds each.
#[test]
fn the_chain_resolves_on_each_board_file_that_declares_one() {
    let stages = [
        Stage {
            label: "receiver",
            flag: "--receiver",
            name: None,
        },
        Stage {
            label: "front end",
            flag: "--front-end",
            name: None,
        },
        Stage {
            label: "back end",
            flag: "--back-end",
            name: None,
        },
    ];
    let chain_file = parse(include_str!("../../../boards/pisp-camera-chain.toml"))
        .expect("the chain example validates");
    for (board, back) in [(reference(), "cam0-isp"), (chain_file, "cam0-be")] {
        let got = resolve(&listed(&board), Some("cam0-sensor"), &stages).unwrap();
        assert_eq!(got.stages, ["cam0-receiver", "cam0-fe", back]);
        let again = resolve(&listed(&board), None, &stages).unwrap();
        assert_eq!(again, got, "the one camera sensor is found by its tag");
    }
}
