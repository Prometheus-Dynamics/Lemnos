//! The base hash of every contract in `contracts/`, pinned in one table.
//!
//! A contract's base hash (`lemnos_core::layout::contract_hash`) is what a typed client and a device
//! compare at bind: a client built with another base hash is refused with `ContractMismatch`
//! (`VERSION_MISMATCH`), with the device's hash and its own in the text (`docs/wire.md`, "Contract
//! hashes"). Adding a method or a field with `#[added]` leaves the hash as it is; every other change to
//! a contract moves it.
//!
//! So a change to a contract's base shows here as a diff of one row of `PINNED`, and the diff is the
//! record that a consumer built before it may be refused at bind. Update the row (the failure prints the
//! whole table as it is now) and say in the PR which consumers must be rebuilt.

use lemnos_core::{Contract, ContractInfo, layout};

/// Every contract in `contracts/`, each crate's `#[contract]` trait.
const CONTRACTS: &[ContractInfo] = &[
    <dyn lemnos_contract_accelerometer::Accelerometer as Contract>::INFO,
    <dyn lemnos_contract_camera_frames::CameraFrames as Contract>::INFO,
    <dyn lemnos_contract_camera_isp::CameraIsp as Contract>::INFO,
    <dyn lemnos_contract_camera_receiver::CameraReceiver as Contract>::INFO,
    <dyn lemnos_contract_camera_sensor::CameraSensor as Contract>::INFO,
    <dyn lemnos_contract_codec_block::CodecBlock as Contract>::INFO,
    <dyn lemnos_contract_fan::Fan as Contract>::INFO,
    <dyn lemnos_contract_fan_control::FanControl as Contract>::INFO,
    <dyn lemnos_contract_gpu_lease::GpuLease as Contract>::INFO,
    <dyn lemnos_contract_gyroscope::Gyroscope as Contract>::INFO,
    <dyn lemnos_contract_image_blend::ImageBlend as Contract>::INFO,
    <dyn lemnos_contract_image_convert::ImageConvert as Contract>::INFO,
    <dyn lemnos_contract_image_scaler::ImageScaler as Contract>::INFO,
    <dyn lemnos_contract_indicator::Indicator as Contract>::INFO,
    <dyn lemnos_contract_isp_isp600::IspIsp600 as Contract>::INFO,
    <dyn lemnos_contract_isp_pisp::IspPisp as Contract>::INFO,
    <dyn lemnos_contract_lens::Lens as Contract>::INFO,
    <dyn lemnos_contract_light::Light as Contract>::INFO,
    <dyn lemnos_contract_magnetometer::Magnetometer as Contract>::INFO,
    <dyn lemnos_contract_named_files::NamedFiles as Contract>::INFO,
    <dyn lemnos_contract_npu::Npu as Contract>::INFO,
    <dyn lemnos_contract_orientation::Orientation as Contract>::INFO,
    <dyn lemnos_contract_pixels::Pixels as Contract>::INFO,
    <dyn lemnos_contract_power_monitor::PowerMonitor as Contract>::INFO,
    <dyn lemnos_contract_power_switch::PowerSwitch as Contract>::INFO,
    <dyn lemnos_contract_raw_bus::RawBus as Contract>::INFO,
    <dyn lemnos_contract_security::Security as Contract>::INFO,
    <dyn lemnos_contract_security_precondition::SecurityPrecondition as Contract>::INFO,
    <dyn lemnos_contract_self_test::SelfTest as Contract>::INFO,
    <dyn lemnos_contract_system_flash::Flash as Contract>::INFO,
    <dyn lemnos_contract_system_identity::SystemIdentity as Contract>::INFO,
    <dyn lemnos_contract_system_reboot::Reboot as Contract>::INFO,
    <dyn lemnos_contract_system_slots::Slots as Contract>::INFO,
    <dyn lemnos_contract_temperature::Temperature as Contract>::INFO,
    <dyn lemnos_contract_usb_function::UsbFunction as Contract>::INFO,
];

/// Contract tag and base hash. One row per contract, in tag order.
const PINNED: &[(&str, u64)] = &[
    ("accelerometer/1", 0xfa7617318969b33f),
    ("camera.frames/1", 0x84f3fa025a0951e7),
    ("camera.isp/1", 0x5a7921cba5b34a20),
    ("camera.receiver/1", 0x95aaa7c9d0ce1d60),
    ("camera.sensor/1", 0x4d6f4b4bddf78636),
    ("codec.block/1", 0xd2f2f17fc8dbe36a),
    ("fan-control/1", 0xdc53417ac6ad2dff),
    ("fan/1", 0x5dab136ed5227ce1),
    ("gpu-lease/1", 0xa91d6e77e9cd9ccd),
    ("gyroscope/1", 0x6af97be3acc54eb3),
    ("image.blend/1", 0x7cf5511c981abeaa),
    ("image.convert/1", 0x9dc144130a06746f),
    ("image.scaler/1", 0x35e2e7c622ee88d4),
    ("indicator/1", 0x78978222dd970f79),
    ("isp.isp600/1", 0x5ac20894f91acb06),
    ("isp.pisp/1", 0x613d73681c661b8b),
    ("lemnos.files/1", 0x2481101dc43449a6),
    ("lens/1", 0xfec59427684e7db9),
    ("light/1", 0x9afc706b53f2c641),
    ("magnetometer/1", 0x73b25422f7a40397),
    ("npu/1", 0x9db6798c8169083f),
    ("orientation/1", 0x46613173c6345352),
    ("pixels/1", 0xb320c90394119c89),
    ("power-monitor/1", 0x0790b8ea27f1ec99),
    ("power-switch/1", 0x690283d9620e96aa),
    ("raw-bus/1", 0xee5acf1cc8c01e10),
    ("self-test/1", 0x6aff59ade5c0bef1),
    ("system.flash/1", 0xcab60ed5d1098774),
    ("system.identity/1", 0xbdb6627ca69d645f),
    ("system.reboot/1", 0xface08cdca5b68c0),
    ("system.security-precondition/1", 0x436bdf18d866e921),
    ("system.security/1", 0xe060a86af2d91c75),
    ("system.slots/1", 0xab19a1e7d0b755c6),
    ("temperature/1", 0xc77f0ded2da1235c),
    ("usb.function/1", 0x16930250d8e9aeaf),
];

fn table() -> Vec<(&'static str, u64)> {
    let mut rows: Vec<(&'static str, u64)> = CONTRACTS
        .iter()
        .map(|info| (info.tag.as_str(), layout::contract_hash(info)))
        .collect();
    rows.sort_unstable();
    rows
}

fn printed(rows: &[(&str, u64)]) -> String {
    rows.iter()
        .map(|(tag, hash)| format!("    ({tag:?}, {hash:#018x}),\n"))
        .collect()
}

#[test]
fn every_contracts_base_hash_is_the_pinned_one() {
    let now = table();
    assert!(
        now == PINNED,
        "a contract's base hash moved: a consumer built before this change may be refused at bind \
         (ContractMismatch). If that is meant, replace PINNED with this table and name the contracts \
         in the PR:\n{}",
        printed(&now)
    );
}

#[test]
fn every_contract_under_contracts_is_in_the_table() {
    let in_table: Vec<&str> = CONTRACTS.iter().map(|info| info.tag.as_str()).collect();
    let mut in_sources = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    for entry in std::fs::read_dir(&root).unwrap() {
        let lib = entry.unwrap().path().join("src/lib.rs");
        let Ok(text) = std::fs::read_to_string(&lib) else {
            continue;
        };
        for attribute in text.split("#[contract(").skip(1) {
            let attribute = attribute.split(")]").next().unwrap();
            let tag = attribute.split('"').nth(1).unwrap();
            let version = attribute
                .split("version = ")
                .nth(1)
                .and_then(|v| v.split(|c: char| !c.is_ascii_digit()).next())
                .unwrap_or_else(|| panic!("{}: a contract with no version", lib.display()));
            in_sources.push(format!("{tag}/{version}"));
        }
    }
    in_sources.sort();
    let mut listed: Vec<String> = in_table.iter().map(|tag| tag.to_string()).collect();
    listed.sort();
    assert_eq!(
        in_sources, listed,
        "contracts/ and this table list different contracts"
    );
}

#[test]
fn the_pinned_table_has_each_contract_once() {
    let mut tags: Vec<&str> = PINNED.iter().map(|(tag, _)| *tag).collect();
    let all = tags.len();
    tags.dedup();
    assert_eq!(tags.len(), all, "a contract is pinned twice");
    assert_eq!(all, CONTRACTS.len());
}
