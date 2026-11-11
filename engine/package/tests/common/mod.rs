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

fn build(features: &[&str], keep_as: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/Cargo.toml");
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
    // The fixture is its own build: nothing of the outer one's flags applies to it.
    for outer in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_TARGET_DIR"] {
        command.env_remove(outer);
    }
    let output = command.output().expect("cargo runs");
    assert!(
        output.status.success(),
        "the fixture package did not build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let built = target.join("debug/liblemnos_package_fixture.so");
    let kept = target.join(keep_as);
    std::fs::copy(&built, &kept).unwrap();
    kept
}

/// Builds the fixture once per test binary.
pub fn fixtures() -> &'static Fixtures {
    static BUILT: OnceLock<Fixtures> = OnceLock::new();
    BUILT.get_or_init(|| Fixtures {
        good: build(&[], "good.so"),
        panics_registering: build(&["panics-registering"], "panics-registering.so"),
    })
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
