//! C programs on the surface, linked against the static library and against
//! the shared one, run against the real service on the simulated board.

mod common;

use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Producing, package_service, service, stream_service};
use lemnos_capi::Client;
use lemnos_linux_sys::dma_buf;

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

fn build(out: &Path, source: &str, link: &[&str]) {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(format!("{manifest}/../../include"))
        .arg(format!("{manifest}/tests/c/{source}"))
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

fn run(program: &Path, args: &[&Path], env: &[(&str, &Path)], expected: &[&str]) {
    let mut command = Command::new(program);
    command.args(args);
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
    for expected in expected {
        assert!(stdout.contains(expected), "{expected:?} in {stdout}");
    }
}

const CLIENT: &[&str] = &[
    "devices=1",
    "first=thing state=0",
    "got=1",
    "streamed 3",
    "item=count method=1 of example.counter/1",
    "off status=",
    "closed",
];

const STREAM: &[&str] = &[
    "frames buffers=4 bytes=5000 handles=1 writable=0 open=1",
    "frames 6 in place, by copy and by handle",
    "refused write=-13 past=-22 none=-22",
    "jobs 6 written, posted and made",
    "plain handle status=-95",
    "taken again after a close holding a frame",
    "packages=2 mode=1 note=one key file line was not a key",
    "package pkgs/fan.lpkg loaded=1 driver=fan reason=\n",
    "package pkgs/odd.lpkg loaded=0 driver= reason=not signed by a trusted key\n",
    "done",
];

fn run_client(program: &Path, env: &[(&str, &Path)]) {
    let (service, socket, dir) = service();
    run(program, &[&socket], env, CLIENT);
    service.stop();
    let _ = std::fs::remove_dir_all(dir);
}

fn run_stream(program: &Path, env: &[(&str, &Path)]) {
    let streams = stream_service();
    let packages = package_service();
    let producing = Producing::start(&streams);
    run(program, &[&streams.socket, &packages.socket], env, STREAM);
    producing.stop();
}

fn static_link(deps: &Path) -> Vec<String> {
    let lib = deps.join("liblemnos_capi.a");
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    [lib.to_str().unwrap(), "-lpthread", "-ldl", "-lm"]
        .map(String::from)
        .to_vec()
}

fn shared_link(lib: &Path) -> Vec<String> {
    assert!(lib.exists(), "cargo builds {} for the tests", lib.display());
    vec![
        "-L".to_string(),
        lib.parent().unwrap().to_str().unwrap().to_string(),
        format!("-l:{}", lib.file_name().unwrap().to_str().unwrap()),
        "-lm".to_string(),
    ]
}

/// Builds `source` against the static library and runs it with `run_it`.
#[cfg(not(feature = "shared"))]
fn on_static(source: &str, run_it: fn(&Path, &[(&str, &Path)])) {
    let deps = deps_dir();
    if !have_cc() {
        return;
    }
    let link = static_link(&deps);
    let link: Vec<&str> = link.iter().map(String::as_str).collect();
    let program = deps.join(format!(
        "lemnos-capi-c-static-{source}-{}",
        std::process::id()
    ));
    build(&program, source, &link);
    run_it(&program, &[]);
    let _ = std::fs::remove_file(&program);
}

/// Builds `source` against the shared library and runs it with `run_it`, the
/// loader finding the library by its SONAME.
fn on_shared(source: &str, run_it: fn(&Path, &[(&str, &Path)])) {
    let deps = deps_dir();
    if !have_cc() {
        return;
    }
    let lib = shared_library();
    let link = shared_link(&lib);
    let link: Vec<&str> = link.iter().map(String::as_str).collect();
    let program = deps.join(format!(
        "lemnos-capi-c-shared-{source}-{}",
        std::process::id()
    ));
    build(&program, source, &link);
    let dir = deps.join(format!(
        "lemnos-capi-soname-{source}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let soname = dir.join("liblemnos_capi.so.1");
    let _ = std::fs::remove_file(&soname);
    std::os::unix::fs::symlink(&lib, &soname).unwrap();
    run_it(&program, &[("LD_LIBRARY_PATH", &dir)]);
    let _ = std::fs::remove_file(&program);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(not(feature = "shared"))]
#[test]
fn a_c_program_runs_on_the_static_library() {
    on_static("client.c", run_client);
}

#[test]
fn a_c_program_runs_on_the_shared_library() {
    on_shared("client.c", run_client);
}

#[cfg(not(feature = "shared"))]
#[test]
fn a_c_program_takes_streams_and_the_package_list_on_the_static_library() {
    on_static("stream.c", run_stream);
}

#[test]
fn a_c_program_takes_streams_and_the_package_list_on_the_shared_library() {
    on_shared("stream.c", run_stream);
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

/// A stream the wrapper's handle drops without a close is closed by that drop, so the next open of
/// the same stream on the same client succeeds.
#[test]
fn a_stream_dropped_without_a_close_is_closed_for_the_next_open() {
    let streams = stream_service();
    let producing = Producing::start(&streams);
    let client = Client::connect(
        streams.socket.to_str().unwrap(),
        "w",
        Duration::from_secs(2),
    )
    .unwrap();
    drop(client.open_stream(0, "frames", false).unwrap());
    let again = client
        .open_stream(0, "frames", false)
        .expect("the dropped stream was closed");
    assert!(again.info().is_ok());
    drop(again);
    producing.stop();
}

#[test]
fn the_rust_wrapper_takes_streams_and_the_package_list_through_the_surface() {
    let streams = stream_service();
    let producing = Producing::start(&streams);
    let client = Client::connect(
        streams.socket.to_str().unwrap(),
        "w",
        Duration::from_secs(2),
    )
    .unwrap();
    let mut stream = client.open_stream(0, "frames", false).unwrap();
    let info = stream.info().unwrap();
    assert_eq!((info.buffers, info.buffer_bytes), (4, 5000));
    for _ in 0..6 {
        let frame = stream
            .next_frame(Duration::from_secs(5))
            .unwrap()
            .expect("a frame");
        let buffer = frame.buffer;
        assert!(common::frame_is_right(frame.bytes, frame.sequence));
        let handle = stream.buffer_handle(buffer).unwrap();
        let theirs = producing.identity(buffer);
        assert_eq!(dma_buf::identity(handle.as_fd()).unwrap(), theirs);
        let mut copy = [0; 8];
        stream.read(buffer, 0, &mut copy).unwrap();
        assert_eq!(copy[..], frame.bytes[..8]);
        stream.release(buffer).unwrap();
    }
    stream.close().unwrap();

    let mut input = client.open_stream(0, "input", true).unwrap();
    let image: Vec<u8> = (0..=255).collect();
    input.write(1, 0, &image).unwrap();
    assert!(input.post_job(&1u32.to_le_bytes()).unwrap());
    let frame = input
        .next_frame(Duration::from_secs(5))
        .unwrap()
        .expect("the job's frame");
    assert_eq!(frame.buffer, 1);
    assert!(
        frame
            .bytes
            .iter()
            .zip(&image)
            .all(|(made, was)| *made == !was)
    );
    input.release(1).unwrap();
    drop(input);
    producing.stop();

    let packages = package_service();
    let client = Client::connect(
        packages.socket.to_str().unwrap(),
        "w",
        Duration::from_secs(2),
    )
    .unwrap();
    let list = client.packages().unwrap();
    assert_eq!(list, common::package_list());
}

#[test]
fn a_stream_in_planes_gives_each_plane_and_its_descriptor_through_the_surface() {
    use lemnos_hal::StreamEndpoint;
    use lemnos_hal::blocks::StreamFrame;

    let streams = stream_service();
    let client = Client::connect(
        streams.socket.to_str().unwrap(),
        "w",
        Duration::from_secs(2),
    )
    .unwrap();
    let mut stream = client.open_stream(0, "planar", false).unwrap();
    let info = stream.info().unwrap();
    assert_eq!((info.buffers, info.buffer_bytes, info.planes), (2, 300, 2));
    let mut producer = streams
        .service()
        .host()
        .stream_producer(0, "planar")
        .unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    // Each plane's dma-buf as the producer holds it, before it is published.
    let theirs: Vec<(u64, u64)> = (0..2)
        .map(|plane| {
            let handle = producer
                .plane_handle(buffer, plane)
                .unwrap()
                .expect("memory behind it");
            dma_buf::identity(handle.fd()).unwrap()
        })
        .collect();
    let pattern: Vec<u8> = (0..300).map(|at| at as u8).collect();
    assert_eq!(producer.write(buffer, 0, &pattern), Ok(true));
    producer
        .publish(StreamFrame {
            buffer,
            bytes: 250,
            sequence: 1,
            identity: 0,
            timestamp_ns: 0,
        })
        .unwrap();
    let frame = stream
        .next_frame(Duration::from_secs(5))
        .unwrap()
        .expect("a frame");
    // A frame in planes is not one piece of memory: no bytes in place.
    assert!(frame.bytes.is_empty());
    assert_eq!(frame.buffer, buffer);
    // The frame is 250 bytes: all of the first plane, half of the second.
    assert_eq!(stream.plane(buffer, 0).unwrap(), &pattern[..200]);
    assert_eq!(stream.plane(buffer, 1).unwrap(), &pattern[200..250]);
    assert!(stream.plane(buffer, 2).is_err(), "no third plane");
    assert!(stream.plane(1 - buffer, 0).is_err(), "a buffer not held");
    // Each plane's own dma-buf, the producer's.
    for plane in 0..2u32 {
        let ours = stream.plane_handle(buffer, plane).unwrap();
        assert_eq!(
            dma_buf::identity(ours.as_fd()).unwrap(),
            theirs[plane as usize]
        );
    }
    assert_ne!(theirs[0], theirs[1], "each plane is its own memory");
    assert!(stream.plane_handle(buffer, 2).is_err());
    // By copy, across the seam.
    let mut seam = [0; 4];
    stream.read(buffer, 198, &mut seam).unwrap();
    assert_eq!(seam[..], pattern[198..202]);
    stream.release(buffer).unwrap();
    stream.close().unwrap();
}
