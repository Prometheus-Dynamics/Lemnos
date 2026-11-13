//! Builds the fixture package with cargo, and the pieces every test here starts from.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use ed25519_dalek::{Signer, SigningKey};
use lemnos_abi::package::{PACKAGE_MAGIC, PACKAGE_VERSION};
use lemnos_core::MethodId;
use lemnos_host::board::parse;
use lemnos_host::{DriverEntry, Host, Options};
use lemnos_package::TrustedKeys;
use lemnos_sim::{SimBoard, SimClock};

/// The fixture's two builds.
pub struct Fixtures {
    /// The package as `export_package!` makes it.
    pub good: PathBuf,
    /// The same crate built so that its registration panics.
    pub panics_registering: PathBuf,
}

/// `target/`, from where cargo put this test binary: `target/<profile>/deps/<binary>`.
fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.ancestors().nth(3).unwrap().to_owned()
}

/// Builds the package crate in `tests/<krate>` with cargo and returns the file it made, kept
/// under the name `keep_as`.
fn build(krate: &str, library: &str, features: &[&str], keep_as: &str) -> PathBuf {
    // One cargo at a time on the shared target directory, whichever test binary gets there.
    static BUILDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(krate)
        .join("Cargo.toml");
    let target = target_dir().join("package-fixture");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target);
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    // The package is its own build: nothing of the outer one's flags applies to it.
    for outer in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_TARGET_DIR"] {
        command.env_remove(outer);
    }
    let output = command.output().expect("cargo runs");
    assert!(
        output.status.success(),
        "the package {krate} did not build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let built = target.join(format!("debug/lib{library}.so"));
    let kept = target.join(keep_as);
    std::fs::copy(&built, &kept).unwrap();
    kept
}

/// Builds the fixture once per test binary.
pub fn fixtures() -> &'static Fixtures {
    static BUILT: OnceLock<Fixtures> = OnceLock::new();
    BUILT.get_or_init(|| Fixtures {
        good: build("fixture", "lemnos_package_fixture", &[], "good.so"),
        panics_registering: build(
            "fixture",
            "lemnos_package_fixture",
            &["panics-registering"],
            "panics-registering.so",
        ),
    })
}

/// The merged `ws2812` driver crate, built as a package by the wrapper crate in
/// `tests/ws2812-package`.
pub fn ws2812_package() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| build("ws2812-package", "lemnos_package_ws2812", &[], "ws2812.so"))
}

/// An empty directory of this test's own.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnos-package-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The good fixture's bytes.
pub fn good_bytes() -> Vec<u8> {
    std::fs::read(&fixtures().good).unwrap()
}

/// Where the package descriptor's first fields are in the fixture's file: the magic, then the
/// version it was built against.
pub fn descriptor_at(bytes: &[u8]) -> usize {
    let mut head = Vec::new();
    head.extend_from_slice(&PACKAGE_MAGIC.to_ne_bytes());
    head.extend_from_slice(&PACKAGE_VERSION.struct_size.to_ne_bytes());
    head.extend_from_slice(&PACKAGE_VERSION.major.to_ne_bytes());
    head.extend_from_slice(&PACKAGE_VERSION.minor.to_ne_bytes());
    head.extend_from_slice(&PACKAGE_VERSION.layout.to_ne_bytes());
    let found: Vec<usize> = bytes
        .windows(head.len())
        .enumerate()
        .filter_map(|(at, window)| (window == head).then_some(at))
        .collect();
    assert_eq!(found.len(), 1, "one descriptor in the file");
    found[0]
}

/// The good fixture with `change` applied to its bytes, written to `path`.
pub fn write_changed(path: &Path, change: impl FnOnce(&mut Vec<u8>)) {
    let mut bytes = good_bytes();
    change(&mut bytes);
    std::fs::write(path, bytes).unwrap();
}

/// A new signing key, from the kernel's random bytes.
pub fn new_key() -> SigningKey {
    let mut seed = [0; 32];
    let mut random = std::fs::File::open("/dev/urandom").unwrap();
    std::io::Read::read_exact(&mut random, &mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

pub fn trusted(keys: &[&SigningKey]) -> TrustedKeys {
    let public: Vec<[u8; 32]> = keys
        .iter()
        .map(|key| key.verifying_key().to_bytes())
        .collect();
    TrustedKeys::from_bytes(&public).unwrap()
}

/// Writes `<path>.sig`: `key`'s signature of the file at `path` as it is now.
pub fn sign(path: &Path, key: &SigningKey) {
    let signature = key.sign(&std::fs::read(path).unwrap());
    std::fs::write(signature_path(path), signature.to_bytes()).unwrap();
}

pub fn signature_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

pub fn host_on(
    sim: SimBoard,
    devices: &str,
    drivers: &[DriverEntry],
    options: Options,
) -> (Host, SimClock) {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    let board = parse(&text).unwrap();
    let clock = sim.clock();
    let host = Host::start(
        &board,
        drivers,
        Box::new(sim),
        Arc::new(clock.clone()),
        options,
    )
    .unwrap();
    (host, clock)
}

pub fn host(devices: &str, drivers: &[DriverEntry]) -> (Host, SimClock) {
    host_on(SimBoard::new(), devices, drivers, Options::default())
}

pub fn method(id: u16) -> MethodId {
    MethodId {
        contract: 0,
        method: id,
    }
}

pub fn index(host: &Host, id: &str) -> u32 {
    host.device(id).unwrap().index
}

/// The fixture's `count`.
pub fn count(host: &Host, device: u32) -> lemnos_core::Result<u32> {
    let mut reply = [0; 4];
    host.call(device, method(1), &[], &mut reply)?;
    Ok(u32::from_le_bytes(reply))
}

/// Two devices on the fixture's driver.
pub const TWO: &str = r#"
[[devices]]
id = "bad"
driver = "package-fixture"

[[devices]]
id = "good"
driver = "package-fixture"
"#;

/// The kinds of resource the fixture's `exercise` takes, as the fixture numbers them.
pub mod kind {
    pub const SPI: u8 = 2;
    pub const LINE: u8 = 3;
    pub const PWM: u8 = 4;
    pub const UART: u8 = 5;
    pub const USB_DEVICE: u8 = 6;
    pub const USB_FUNCTION: u8 = 7;
    pub const REGISTERS: u8 = 8;
    pub const INTERRUPT: u8 = 9;
    pub const DMA: u8 = 10;
    pub const CLOCKS: u8 = 11;
    pub const RESET: u8 = 12;
    pub const STREAM: u8 = 13;
    pub const RAW_SPI: u8 = 14;
}

/// The most the fixture's `exercise` is given room to answer with.
pub const EXERCISE_ROOM: usize = 256;

/// The arguments of the fixture's `exercise`, written into `args`: their length.
pub fn exercise_args(args: &mut [u8], kind: u8, step: u8, data: &[u8]) -> usize {
    args[0] = kind;
    args[1] = step;
    args[2..6].copy_from_slice(&(data.len() as u32).to_le_bytes());
    args[6..6 + data.len()].copy_from_slice(data);
    let end = 6 + data.len();
    args[end..end + 4].copy_from_slice(&(EXERCISE_ROOM as u32).to_le_bytes());
    end + 4
}

/// The fixture's `exercise`: one step with the resource of one kind, and what came back.
pub fn exercise(
    host: &Host,
    device: u32,
    kind: u8,
    step: u8,
    data: &[u8],
) -> lemnos_core::Result<Vec<u8>> {
    let mut args = vec![0; data.len() + 10];
    let len = exercise_args(&mut args, kind, step, data);
    let mut reply = [0; EXERCISE_ROOM];
    host.call(device, method(10), &args[..len], &mut reply)
        .map(|used| reply[..used].to_vec())
}

/// A file of `bytes` zero bytes, as a stream buffer's handle.
pub fn buffer_file(dir: &Path, name: &str, bytes: u64) -> std::os::fd::OwnedFd {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dir.join(name))
        .unwrap();
    file.set_len(bytes).unwrap();
    file.into()
}

/// A USB descriptor set a function can be enabled with: one interface with a bulk IN endpoint
/// (0x81) and a bulk OUT endpoint (0x02).
pub fn usb_descriptors() -> Vec<u8> {
    let mut set = vec![
        18, 1, 0x00, 0x02, 0xff, 0, 0, 64, 0x09, 0x12, 0x34, 0x12, 0x02, 0x01, 0, 0, 0, 1,
    ];
    let mut configuration = vec![9, 2, 0, 0, 1, 1, 0, 0xc0, 50];
    configuration.extend([9, 4, 0, 0, 2, 0xff, 0, 0, 0]);
    configuration.extend([7, 5, 0x81, 2, 64, 0, 0]);
    configuration.extend([7, 5, 0x02, 2, 64, 0, 0]);
    let total = (configuration.len() as u16).to_le_bytes();
    configuration[2..4].copy_from_slice(&total);
    set.extend(configuration);
    set
}

/// Builds the package written in C (`tests/c-package/package.c`) with the C compiler, with
/// `flags` added, and returns the file it made in `dir`.
pub fn c_package(dir: &Path, name: &str, flags: &[&str]) -> PathBuf {
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../include");
    c_package_with(dir, name, &include, flags)
}

/// [`c_package`] against the boundary as it was before the extras: `tests/c-package/base`, a
/// copy of `include/lemnos` from before them.
pub fn c_package_base(dir: &Path, name: &str, flags: &[&str]) -> PathBuf {
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/c-package/base");
    c_package_with(dir, name, &include, flags)
}

fn c_package_with(dir: &Path, name: &str, include: &Path, flags: &[&str]) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = dir.join(name);
    let output = Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC",
        ])
        .arg("-I")
        .arg(include)
        .arg(manifest.join("tests/c-package/package.c"))
        .args(flags)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("the C compiler runs");
    assert!(
        output.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    out
}
