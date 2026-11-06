//! A C program on the surface, linked against the static library and against
//! the shared one, run against a service on mock hardware.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lemnos_hal::register::AddressWidth;
use lemnosd::mock::{MockHardware, MockLemnosd};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "capi-c"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 10
"#;

fn service(tag: &str) -> MockLemnosd {
    let root = std::env::temp_dir().join(format!("lemnos-capi-c-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("sys")).unwrap();
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_sampler(0x18, 0x12, Duration::from_millis(10));
    MockLemnosd::start_in(root, BOARD, hardware).unwrap()
}

/// Where cargo put this crate's static and shared libraries: beside the test
/// binary.
fn deps_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The shared library the tests run on: the one cargo built beside the test
/// binary, or with `shared` (where this crate exports nothing) the one its
/// build script found or built, named for its SONAME.
fn shared_library() -> PathBuf {
    #[cfg(feature = "shared")]
    return Path::new(env!("LEMNOS_CAPI_SHARED_DIR")).join("liblemnos_capi.so.1");
    #[cfg(not(feature = "shared"))]
    return deps_dir().join("liblemnos_capi.so");
}

fn have_cc() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn build(out: &Path, link: &[&str]) {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let status = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(format!("{manifest}/include"))
        .arg(format!("{manifest}/tests/c/client.c"))
        .arg("-o")
        .arg(out)
        .args(link)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn run(program: &Path, service: &MockLemnosd, env: &[(&str, &Path)]) -> String {
    let mut command = Command::new(program);
    command.arg(service.socket());
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check(stdout: &str) {
    assert!(stdout.contains("read acceleration.x="), "{stdout}");
    assert!(stdout.contains("status=0"), "{stdout}");
    assert!(stdout.contains("streamed 3"), "{stdout}");
    assert!(stdout.contains("closed"), "{stdout}");
}

/// The static library is this crate's own exports, which `shared` leaves out.
#[cfg(not(feature = "shared"))]
#[test]
fn a_c_program_runs_on_the_static_library() {
    let deps = deps_dir();
    let lib = deps.join("liblemnos_capi.a");
    if !have_cc() {
        return;
    }
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    let program = deps.join(format!("lemnos-capi-c-static-{}", std::process::id()));
    build(
        &program,
        &[lib.to_str().unwrap(), "-lpthread", "-ldl", "-lm"],
    );
    let service = service("static");
    let stdout = run(&program, &service, &[]);
    service.stop();
    let _ = fs::remove_file(&program);
    check(&stdout);
}

#[test]
fn a_c_program_runs_on_the_shared_library() {
    let deps = deps_dir();
    if !have_cc() {
        return;
    }
    let lib = shared_library();
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    let program = deps.join(format!("lemnos-capi-c-shared-{}", std::process::id()));
    build(
        &program,
        &[
            "-L",
            lib.parent().unwrap().to_str().unwrap(),
            &format!("-l:{}", lib.file_name().unwrap().to_str().unwrap()),
            "-lm",
        ],
    );
    // The program records the SONAME, liblemnos_capi.so.1, as what it needs;
    // cargo writes only liblemnos_capi.so, so the loader finds it through a
    // link of that name, as an installed library would have.
    let libdir = std::env::temp_dir().join(format!("lemnos-capi-lib-{}", std::process::id()));
    let _ = fs::remove_dir_all(&libdir);
    fs::create_dir_all(&libdir).unwrap();
    std::os::unix::fs::symlink(&lib, libdir.join("liblemnos_capi.so.1")).unwrap();
    let service = service("shared");
    let stdout = run(&program, &service, &[("LD_LIBRARY_PATH", &libdir)]);
    service.stop();
    let _ = fs::remove_file(&program);
    let _ = fs::remove_dir_all(&libdir);
    check(&stdout);
}

/// The shared library's SONAME carries the surface's major version (where
/// `readelf` is installed).
#[test]
fn the_shared_library_is_named_for_the_surface_major() {
    let lib = shared_library();
    let Ok(out) = Command::new("readelf").arg("-d").arg(&lib).output() else {
        return;
    };
    assert!(out.status.success(), "readelf -d {}", lib.display());
    let dynamic = String::from_utf8_lossy(&out.stdout);
    let major = lemnos_capi::VERSION.major;
    let want = format!("Library soname: [liblemnos_capi.so.{major}]");
    assert!(dynamic.contains(&want), "{want} not in:\n{dynamic}");
}
