//! A C program on the surface, linked against the static library and against
//! the shared one, run against the real service.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_capi::Client;
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

fn service() -> (Service, PathBuf, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-capi-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"c\"\n\
        [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "c".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    (service, socket, dir)
}

fn deps_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[cfg(feature = "shared")]
fn shared_library() -> PathBuf {
    Path::new(env!("LEMNOS_CAPI_SHARED_DIR")).join("liblemnos_capi.so.1")
}

#[cfg(not(feature = "shared"))]
fn shared_library() -> PathBuf {
    deps_dir().join("liblemnos_capi.so")
}

fn have_cc() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn build(out: &Path, link: &[&str]) {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(format!("{manifest}/../../include"))
        .arg(format!("{manifest}/tests/c/client.c"))
        .arg("-o")
        .arg(out)
        .args(link)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_and_check(program: &Path, socket: &Path, env: &[(&str, &Path)]) {
    let mut command = Command::new(program);
    command.arg(socket);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in [
        "devices=1",
        "first=thing state=0",
        "got=1",
        "streamed 3",
        "item=count method=1 of example.counter/1",
        "off status=",
        "closed",
    ] {
        assert!(stdout.contains(expected), "{expected:?} in {stdout}");
    }
}

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
    let (service, socket, dir) = service();
    run_and_check(&program, &socket, &[]);
    service.stop();
    let _ = std::fs::remove_file(&program);
    let _ = std::fs::remove_dir_all(dir);
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
    let (service, socket, dir) = service();
    // The loader looks for the library by its SONAME.
    std::os::unix::fs::symlink(&lib, dir.join("liblemnos_capi.so.1")).unwrap();
    run_and_check(&program, &socket, &[("LD_LIBRARY_PATH", &dir)]);
    service.stop();
    let _ = std::fs::remove_file(&program);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_rust_wrapper_works_through_the_surface() {
    let (service, socket, dir) = service();
    let client = Client::connect(socket.to_str().unwrap(), "w", Duration::from_secs(2)).unwrap();
    assert_eq!(client.device_count().unwrap(), 1);
    assert_eq!(client.device_name(0).unwrap(), "thing");
    let opened = client.open("thing", "example.counter/1").unwrap();
    let mut reply = [0; 4];
    assert_eq!(client.call(opened, 1, &[], &mut reply).unwrap(), 4);
    assert_eq!(u32::from_le_bytes(reply), 1);
    let id = client.subscribe(opened, 1, 1000).unwrap();
    let value = client
        .next_value(&mut reply, Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(value.subscription, id);
    drop(client);
    service.stop();
    let _ = std::fs::remove_dir_all(dir);
}
