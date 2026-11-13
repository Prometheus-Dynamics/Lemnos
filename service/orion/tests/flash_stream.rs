//! A flash image pushed through a real Orion node, streamed into the flash driver's write job as it
//! arrives: the manifest goes to disk like any file, the image is never written to the bridge's disk,
//! and the job's result is the sender's answer. Temporary files and a fake mount table only.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_driver_system_flash::SystemFlash;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, files, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::files::FileState;
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(60);
/// The board the device stands for. The vector's signature is made for this id.
const BOARD: &str = "flash-test";

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lf-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

/// A ustar member header.
/// The hash tree's size over `blocks` data blocks, as `veritysetup` writes it: a level of
/// `ceil(n / 128)` blocks while the count being hashed is more than 1.
/// An unsigned schema 3 slot bundle for board `t`, its image `data_blocks` blocks of data and
/// room for their tree.
struct Logic {
    kill: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Logic {
    fn start(dir: &Path) -> Self {
        let mut source = orion_config::Source::parse("", []).expect("no settings");
        let mut settings = NodeSettings::from_source(&mut source).expect("defaults");
        settings.config.socket = dir.join("orion.sock");
        settings.config.state_dir = dir.join("state");
        settings.files_dir = dir.join("files");
        settings.audit = None;
        settings.config.node_name = "board".parse().expect("name");
        let (kill, killed) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let mut node = Node::load(settings).expect("node logic starts");
                tokio::select! {
                    () = node.run(std::future::pending()) => {}
                    _ = killed => {}
                }
            });
        });
        Self {
            kill: Some(kill),
            thread: Some(thread),
        }
    }
}

impl Drop for Logic {
    fn drop(&mut self) {
        if let (Some(kill), Some(thread)) = (self.kill.take(), self.thread.take()) {
            let _ = kill.send(());
            let _ = thread.join();
        }
    }
}

/// The key the board trusts: the brief's test vector key (seed 0x07 x 32), owned by the test.
const VECTOR_PUBLIC: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
/// The vector's signature record for the small image (108 bytes, hex).
const VECTOR_RECORD: &str = "4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889bd894b37df1a05915c0c6baa7606d57c8cc2a6aa10a01451a3e4f67b8edfd517df6794fbcdeeaadfd7f46e099cea00be896e99445a8942f5e7592ec31c1b6c09";

/// lemnosd serving the flash driver over `dir`'s target, with the vector key on its list.
fn start_lemnosd(dir: &Path, socket: &Path, target: &Path, size_ok: bool) -> Service {
    let mountinfo = dir.join("mountinfo");
    std::fs::write(&mountinfo, "1 0 0:1 / / rw - rootfs rootfs rw\n").expect("mountinfo");
    let keys = dir.join("keys");
    std::fs::write(&keys, format!("{VECTOR_PUBLIC} flash test key\n")).expect("keys");
    let _ = size_ok;
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"flash-test\"\n\
         [host]\ncall_deadline_ms = 30000\n\
         [[devices]]\nid = \"flash\"\ndriver = \"system-flash\"\n\
         config = {{ path = \"{}\", kind = \"file\", allow_first_flash = true, mountinfo = \"{}\", keys = \"{}\" }}\n",
        target.display(),
        mountinfo.display(),
        keys.display()
    );
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(&text).expect("board"),
        drivers: vec![entry::<SystemFlash>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

struct Bridge {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn start_bridge(lemnosd: &Path, orion: &Path, incoming: &Path) -> Bridge {
    let shared = Arc::new(Shared::new().expect("shared"));
    let config = Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: None,
        run_as: None,
        files: vec![("flash".to_owned(), incoming.to_path_buf())],
        links: Vec::new(),
        streams: vec![(
            "flash".to_owned(),
            vec!["image.img.xz".to_owned(), "image.img".to_owned()],
        )],
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || calls::run(&socket, &queue, &shared))
        },
        {
            let (config, shared) = (config.clone(), Arc::clone(&shared));
            std::thread::spawn(move || files::run(&config, &shared))
        },
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || publish::run(&config, &shared, &jobs))
        },
    ];
    Bridge { shared, threads }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shared.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Everything up: the node, lemnosd with the flash device, the bridge, and a dashboard that sees
/// the files it has pushed through.
struct Setup {
    dir: PathBuf,
    target: PathBuf,
    dashboard: Orion,
    _bridge: Bridge,
    _service: Service,
    _logic: Logic,
    _board: Board,
}

fn setup(test: &str, target_bytes: u64) -> Setup {
    let dir = dir(test);
    let socket = dir.join("lemnosd.sock");
    let target = dir.join("disk.img");
    std::fs::File::create(&target)
        .expect("target")
        .set_len(target_bytes)
        .expect("size");
    let board = Board::start_for_node(&dir);
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&dir, &socket, &target, true);
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&incoming).expect("incoming");
    let bridge = start_bridge(&socket, board.socket(), &incoming);
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "dashboard").ok()
    });
    wait_for("the flash device", || {
        let found = dashboard.resources("lemnos.flash").ok()?;
        found.iter().any(|f| f.name == "lemnos.flash").then_some(())
    });
    Setup {
        dir,
        target,
        dashboard,
        _bridge: bridge,
        _service: service,
        _logic: logic,
        _board: board,
    }
}

impl Setup {
    fn incoming(&self) -> PathBuf {
        self.dir.join("incoming")
    }

    /// Pushes `bytes` to `lemnos/flash/<name>`, trying again until the bridge is the receiver.
    fn push(&self, name: &str, bytes: &[u8]) -> orion::files::Transfer {
        let destination = format!("lemnos/flash/{name}");
        wait_for("a push the bridge answers", || {
            self.dashboard
                .files()
                .push(&destination, &mut Cursor::new(bytes), |_, _| {})
                .ok()
        })
    }
}

fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

const SMALL_SHA: &str = "f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58";
const SMALL: u64 = 16 * 1024;

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "data", name]
        .iter()
        .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The transport manifest for the small image: its board, the image's kind (`disk`), its size and
/// hash, and the record in the `signature` field when there is one.
fn manifest(board: &str, sha: &str, signature: Option<&str>) -> Vec<u8> {
    let sig = signature.map_or(String::new(), |s| format!(r#","signature":"{s}""#));
    format!(
        r#"{{"schema":1,"board":"{board}","format":"disk","image":{{"size":{SMALL},"sha256":"{sha}"}}{sig}}}"#
    )
    .into_bytes()
}

#[test]
fn a_streamed_image_is_written_to_the_target_and_only_its_manifest_is_on_disk() {
    let s = setup("stream", SMALL + 4096);
    let image = fixture("crc64.img.xz");
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &image);
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    assert_eq!(
        sent.detail,
        format!("{SMALL} bytes written; signature not checked: unsigned")
    );
    assert_eq!(
        &std::fs::read(&s.target).expect("target")[..SMALL as usize],
        fixture("small.img").as_slice()
    );
    // The manifest was a file; the image was streamed and never written to the bridge's directory.
    assert!(s.incoming().join("image.img.xz.json").exists());
    assert!(
        !s.incoming().join("image.img.xz").exists(),
        "{:?}",
        entries(&s.incoming())
    );
}

#[test]
fn a_signed_image_is_verified_by_the_device_from_its_manifest_and_its_own_record() {
    let s = setup("signed", SMALL + 4096);
    assert_eq!(
        s.push(
            "image.img.xz.json",
            &manifest(BOARD, SMALL_SHA, Some(VECTOR_RECORD))
        )
        .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("signed.img.xz"));
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    assert_eq!(
        sent.detail,
        format!("{SMALL} bytes written; signature verified")
    );
    assert_eq!(
        &std::fs::read(&s.target).expect("target")[..SMALL as usize],
        fixture("small.img").as_slice()
    );
}

#[test]
fn a_signed_file_whose_manifest_gives_no_record_fails_the_job() {
    // The file's own record must be the one the manifest gave: none given, one in the file.
    let s = setup("nogiven", SMALL + 4096);
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("signed.img.xz"));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(sent.detail.contains("refused the data"), "{}", sent.detail);
}

#[test]
fn an_image_whose_hash_is_not_the_manifests_is_refused_and_says_why() {
    let s = setup("hash", SMALL + 4096);
    let wrong = "0".repeat(64);
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, &wrong, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("crc64.img.xz"));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(sent.detail.contains("refused the data"), "{}", sent.detail);
}

#[test]
fn a_stream_without_a_manifest_is_refused_before_any_byte_is_sent() {
    let s = setup("nomanifest", SMALL + 4096);
    let sent = s.push("image.img.xz", &fixture("crc64.img.xz"));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(sent.detail.contains("no manifest"), "{}", sent.detail);
    assert!(
        entries(&s.incoming()).is_empty(),
        "{:?}",
        entries(&s.incoming())
    );
}

#[test]
fn an_image_for_another_board_is_refused_by_the_device() {
    let s = setup("board", SMALL + 4096);
    assert_eq!(
        s.push("image.img.xz.json", &manifest("other", SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("crc64.img.xz"));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(
        sent.detail.contains("refused the manifest"),
        "{}",
        sent.detail
    );
}

#[test]
fn an_image_larger_than_the_target_does_not_start_writing() {
    let s = setup("small-target", SMALL / 2);
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("crc64.img.xz"));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(
        std::fs::read(&s.target)
            .expect("target")
            .iter()
            .all(|&b| b == 0)
    );
}
