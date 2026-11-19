//! `lemnos-ctl files` against a real service with the `named-files` driver, over temporary
//! directories: the index, a file's bytes, a file larger than one read, a file made new, and a name
//! the node does not serve.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_driver_named_files::NamedFilesDriver;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
    dir: PathBuf,
}

/// A service with one `named-files` device, reading from a fresh temporary directory tree.
fn start() -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-files-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::create_dir_all(dir.join("boot")).unwrap();
    std::fs::create_dir_all(dir.join("gen")).unwrap();
    std::fs::write(dir.join("boot/trial-reason"), "power lost\n").unwrap();
    let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("gen/overlay.dts"), &big).unwrap();
    let socket = dir.join("sock");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"files\"\ndriver = \"named-files\"\n\
         [devices.config]\ndata = \"{}\"\nboot = \"{}\"\nsys_block = \"{}\"\nmodule = \"{}\"\n\
         tree_overlay = \"{}\"\n",
        dir.join("data").display(),
        dir.join("boot").display(),
        dir.join("none-block").display(),
        dir.join("none-module").display(),
        dir.join("gen/overlay.dts").display(),
    );
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<NamedFilesDriver>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    Running {
        _service: service,
        socket,
        dir,
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Runs the tool, asking again while the device is busy.
fn ctl(running: &Running, words: &[&str]) -> Result<Vec<u8>, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            Err(error) if error.starts_with("refused (-16)") && Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(out),
        }
    }
}

#[test]
fn the_index_lists_what_the_node_serves() {
    let running = start();
    let text = String::from_utf8(ctl(&running, &["files", "files"]).unwrap()).unwrap();
    assert!(text.starts_with("orion-index 1\n"), "{text}");
    assert!(text.contains("lemnos/trial-reason "), "{text}");
    assert!(text.contains("lemnos/tree/overlay.dts "), "{text}");
    assert!(
        !text.contains("lemnos/tree/boot.txt"),
        "unset keys are not listed: {text}"
    );
}

#[test]
fn a_small_file_goes_to_stdout_as_it_is_on_the_node() {
    let running = start();
    let bytes = ctl(&running, &["files", "files", "lemnos/trial-reason"]).unwrap();
    assert_eq!(bytes, b"power lost\n");
}

#[test]
fn a_file_larger_than_one_read_comes_back_whole_and_exact() {
    let running = start();
    let bytes = ctl(&running, &["files", "files", "lemnos/tree/overlay.dts"]).unwrap();
    let expected: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(bytes.len(), expected.len());
    assert!(bytes == expected, "the bytes come back in order");
}

#[test]
fn a_file_is_written_to_a_new_path_and_never_over_an_old_one() {
    let running = start();
    let target = running.dir.join("out.txt");
    let target = target.display().to_string();
    assert!(
        ctl(
            &running,
            &["files", "files", "lemnos/trial-reason", &target]
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"power lost\n");
    let again = ctl(
        &running,
        &["files", "files", "lemnos/trial-reason", &target],
    );
    assert!(again.is_err(), "an existing file is not overwritten");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"power lost\n",
        "and it is unchanged"
    );
}

#[test]
fn a_name_the_node_does_not_serve_is_refused_by_name() {
    let running = start();
    let error = ctl(&running, &["files", "files", "lemnos/nope"]).unwrap_err();
    assert!(error.contains("refused"), "{error}");
}
