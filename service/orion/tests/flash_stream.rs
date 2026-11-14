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

#[path = "support/tracked.rs"]
mod tracked;
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Outcome, Target};
use orion::files::FileState;
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use tokio::sync::oneshot;
use tracked::TrackedDevice;

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
         config = {{ path = \"{}\", kind = \"file\", allow_first_flash = true, mountinfo = \"{}\", keys = \"{}\" }}\n\
         [[devices]]\nid = \"tracked\"\ndriver = \"tracked\"\n",
        target.display(),
        mountinfo.display(),
        keys.display()
    );
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(&text).expect("board"),
        drivers: vec![entry::<SystemFlash>(), entry::<TrackedDevice>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

struct Bridge {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn start_bridge_stalled(
    lemnosd: &Path,
    orion: &Path,
    incoming: &Path,
    stall: Duration,
    lease: Option<Duration>,
) -> Bridge {
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
        lease,
        stall,
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || calls::run(&socket, &queue, &shared, lease))
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
    setup_stalled(test, target_bytes, lemnos_orion::files::IDLE)
}

fn setup_stalled(test: &str, target_bytes: u64, stall: Duration) -> Setup {
    setup_with(test, target_bytes, stall, Some(lemnos_orion::config::LEASE))
}

fn setup_with(test: &str, target_bytes: u64, stall: Duration, lease: Option<Duration>) -> Setup {
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
    let bridge = start_bridge_stalled(&socket, board.socket(), &incoming, stall, lease);
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "dashboard").ok()
    });
    wait_for("the flash device", || {
        let found = dashboard.resources("lemnos.flash").ok()?;
        found.iter().any(|f| f.name == "lemnos.flash").then_some(())
    });
    wait_for("the tracked device", || {
        let found = dashboard.resources("lemnos.tracked").ok()?;
        found
            .iter()
            .any(|f| f.name == "lemnos.tracked")
            .then_some(())
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

/// Runs `args` on the tracked device (`lemnos.tracked`) through `orion`, and its reply as text.
fn tracked_call(orion: &Orion, args: &str) -> String {
    let mut run = orion
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: args.as_bytes(),
            deadline_ms: lemnos_orion::calls::RUN_DEADLINE_MS,
            detached: false,
        })
        .expect("run");
    let finished = run.finish(WAIT).expect("the run ends");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    String::from_utf8(finished.output).expect("text")
}

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

/// A raw image's manifest, for an image of `size` bytes with digest `sha`.
fn manifest_of(size: u64, sha: &str) -> Vec<u8> {
    format!(r#"{{"schema":1,"board":"{BOARD}","format":"disk","image":{{"size":{size},"sha256":"{sha}"}}}}"#)
        .into_bytes()
}

/// A sender that goes quiet mid-image. Orion reads the whole file once to take its digest, which
/// is given in the stream's Begin; then it reads again to send, and this reader gives half of the
/// image on that second pass and then waits, until the test lets it go. The read then fails, so
/// the push ends.
struct Quiet {
    image: Vec<u8>,
    at: usize,
    /// How much of the image the current pass gives.
    limit: usize,
    /// Whether a pass has read the whole image, so the next rewind starts the sending pass.
    read_all: bool,
    /// On the sending pass: a pause before each read, and the most a read gives (a steady sender).
    pace: Option<(Duration, usize)>,
    /// On the sending pass without a pace: how much it gives before it goes quiet. `None` is half.
    first: Option<usize>,
    wait: mpsc::Receiver<()>,
}

impl std::io::Read for Quiet {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.image.len() {
            self.read_all = true;
            return Ok(0);
        }
        if self.at >= self.limit {
            let _ = self.wait.recv();
            return Err(std::io::Error::other("the test let the quiet sender go"));
        }
        let mut end = self.limit.min(self.image.len()).min(self.at + buf.len());
        // Paced on the sending pass only: the digest pass reads the whole image at full speed.
        if let (true, Some((pause, most))) = (self.read_all, self.pace) {
            std::thread::sleep(pause);
            end = end.min(self.at + most);
        }
        let n = end - self.at;
        buf[..n].copy_from_slice(&self.image[self.at..end]);
        self.at = end;
        Ok(n)
    }
}

impl std::io::Seek for Quiet {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        match from {
            std::io::SeekFrom::End(0) => Ok(self.image.len() as u64),
            std::io::SeekFrom::Start(at) => {
                if at == 0 && self.read_all && self.pace.is_none() {
                    // The sending pass: half of the image, then quiet.
                    self.limit = self.first.unwrap_or(self.image.len() / 2);
                }
                self.at = at as usize;
                Ok(at)
            }
            std::io::SeekFrom::Current(0) => Ok(self.at as u64),
            other => Err(std::io::Error::other(format!("unsupported seek {other:?}"))),
        }
    }
}

/// A sender that keeps sending is not dropped by the stall rule. The Orion sender packs its reads into
/// 32 KiB chunks, and the bridge sees only those; so this sender reads 8 KiB every quarter second, which
/// is a chunk each second, inside the stall bound of two seconds. The stream is a raw image (the device
/// takes `image.img` raw), so no encoder is needed. The device stays busy at every reading for four
/// bounds. Mutation: with the streamer's `expire` removed this test still passes, because a steady
/// sender never goes quiet; the quiet test above is the one that fails then.
#[test]
fn a_stream_whose_sender_keeps_sending_is_not_dropped_by_the_stall_rule() {
    use sha2::{Digest, Sha256};
    const SIZE: usize = 512 * 1024;
    let stall = Duration::from_secs(2);
    let s = setup_stalled("paced", SIZE as u64 + 4096, stall);
    let content = vec![0_u8; SIZE];
    let sha: String = Sha256::digest(&content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = s.push("image.img.json", &manifest_of(SIZE as u64, &sha));
    assert_eq!(manifest.state, FileState::Committed, "{manifest:?}");
    let (let_go, wait) = mpsc::channel();
    let quiet = Quiet {
        limit: content.len(),
        image: content,
        at: 0,
        read_all: false,
        pace: Some((Duration::from_millis(250), 8 * 1024)),
        first: None,
        wait,
    };
    let socket = s._board.socket().to_path_buf();
    let pushing = std::thread::spawn(move || {
        let sender = wait_for("a steady sender", || {
            Orion::connect(&Address::Local(socket.clone()), "steady").ok()
        });
        let mut quiet = quiet;
        let _ = sender
            .files()
            .push("lemnos/flash/image.img", &mut quiet, |_, _| {});
    });
    wait_for("the stream to hold the device", || {
        device_busy(&s).then_some(())
    });
    // Four bounds of steady sending: the device is busy at every reading, and the sender is still at it.
    let until = Instant::now() + stall * 4;
    while Instant::now() < until {
        assert!(
            device_busy(&s),
            "the stream was dropped while its sender kept sending"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !pushing.is_finished(),
        "the sender ended before the window; the test proved nothing"
    );
    let _ = let_go.send(());
    let _ = pushing.join();
}

/// Keeps `orion`'s session open for `window` with resource queries, which are not run calls: the bridge
/// sees none of them, so they renew no caller's lease.
fn keep_open(orion: &Orion, window: Duration) {
    let until = Instant::now() + window;
    while Instant::now() < until {
        let _ = orion.resources("lemnos.flash");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A push that keeps sending: the caller's hold on the tracked device survives a push three lease periods
/// long, with the caller sending nothing else. The holder's session has a token, so it is alive and its hold
/// needs no renewal. The sender reads 8 KiB every 60 ms.
#[test]
fn a_push_lasting_three_leases_keeps_a_held_device_hold() {
    use sha2::{Digest, Sha256};
    const SIZE: usize = 4 * 1024 * 1024;
    let lease = Duration::from_secs(5);
    let s = setup_with(
        "push-lease",
        SIZE as u64 + 4096,
        lemnos_orion::files::IDLE,
        Some(lease),
    );
    tracked_call(&s.dashboard, "take");
    assert_eq!(tracked_call(&s.dashboard, "held"), "1");
    let content = vec![0_u8; SIZE];
    let sha: String = Sha256::digest(&content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = s.push("image.img.json", &manifest_of(SIZE as u64, &sha));
    assert_eq!(manifest.state, FileState::Committed, "{manifest:?}");
    let (let_go, wait) = mpsc::channel();
    let quiet = Quiet {
        limit: content.len(),
        image: content,
        at: 0,
        read_all: false,
        pace: Some((Duration::from_millis(60), 8 * 1024)),
        first: None,
        wait,
    };
    let socket = s._board.socket().to_path_buf();
    let (ended, how) = mpsc::channel();
    let pushing = std::thread::spawn(move || {
        let sender = wait_for("a steady sender", || {
            Orion::connect(&Address::Local(socket.clone()), "steady").ok()
        });
        let mut quiet = quiet;
        let result = sender
            .files()
            .push("lemnos/flash/image.img", &mut quiet, |_, _| {});
        let _ = ended.send(format!("{:?}", result.map(|t| t.state)));
    });
    wait_for("the stream to hold the device", || {
        device_busy(&s).then_some(())
    });
    // Three lease periods of pushing, with no run from the caller. Its session is kept open with
    // resource queries, which are Orion's traffic, not calls: they renew nothing.
    keep_open(&s.dashboard, lease * 3);
    let still_pushing = !pushing.is_finished();
    let held = tracked_call(&s.dashboard, "held");
    let _ = let_go.send(());
    let _ = pushing.join();
    assert!(
        still_pushing,
        "the push ended before the window ({:?}); the test proved nothing",
        how.try_recv()
    );
    assert_eq!(
        held, "1",
        "a push three leases long released the caller's hold"
    );
}

/// A push that starts and then sends no chunk ends at the stall bound: its stream is dropped and the
/// device is free again. The caller's hold is not the stall's business: the holder's session has a token,
/// so it is alive, and its hold stays with no renewal (the lease is for a session 0 caller only). The
/// stall bound is one second here. Mutation: with the stall bound off the device stays busy and the first
/// check fails.
#[test]
fn a_stalled_push_ends_at_the_stall_bound_and_keeps_a_live_callers_hold() {
    use sha2::{Digest, Sha256};
    const SIZE: usize = 64 * 1024;
    let lease = Duration::from_secs(5);
    let s = setup_with(
        "push-stalled",
        SIZE as u64 + 4096,
        Duration::from_secs(1),
        Some(lease),
    );
    tracked_call(&s.dashboard, "take");
    assert_eq!(tracked_call(&s.dashboard, "held"), "1");
    let content = vec![0_u8; SIZE];
    let sha: String = Sha256::digest(&content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = s.push("image.img.json", &manifest_of(SIZE as u64, &sha));
    assert_eq!(manifest.state, FileState::Committed, "{manifest:?}");
    let (let_go, wait) = mpsc::channel();
    let quiet = Quiet {
        limit: content.len(),
        image: content,
        at: 0,
        read_all: false,
        pace: None,
        first: Some(0),
        wait,
    };
    let socket = s._board.socket().to_path_buf();
    let pushing = std::thread::spawn(move || {
        let sender = wait_for("a stalled sender", || {
            Orion::connect(&Address::Local(socket.clone()), "stalled").ok()
        });
        let mut quiet = quiet;
        let _ = sender
            .files()
            .push("lemnos/flash/image.img", &mut quiet, |_, _| {});
    });
    wait_for("the stream to begin", || device_busy(&s).then_some(()));
    wait_for("the stalled stream is ended at the stall bound", || {
        (!device_busy(&s)).then_some(())
    });
    // Three leases on, the holder is alive and silent: its hold is still there.
    keep_open(&s.dashboard, lease * 3);
    let held = tracked_call(&s.dashboard, "held");
    let _ = let_go.send(());
    let _ = pushing.join();
    assert_eq!(held, "1", "a stalled push took a live caller's hold");
}

/// Whether the device is busy: a write (size 1, raw, which it can never start) is refused `BUSY`
/// (-16) while a stream holds its job, and refused for something else otherwise (no manifest yet,
/// or the size). Nothing is started either way. The call path names a refused job start by its
/// status only, so the probe reads the status.
fn orion_status_busy() -> i32 {
    lemnos_core::Status::BUSY.0
}

fn device_busy(s: &Setup) -> bool {
    let mut run = s
        .dashboard
        .run(&ActionCall {
            name: "lemnos.flash.call",
            target: Target::Resource("lemnos.flash".parse().expect("name")),
            args: b"write 1 0",
            deadline_ms: lemnos_orion::calls::RUN_DEADLINE_MS,
            detached: false,
        })
        .expect("run");
    let finished = run.finish(WAIT).expect("the probe ends");
    finished
        .detail
        .contains(&format!("refused ({})", orion_status_busy()))
}

/// A stream whose sender goes quiet mid-image holds the device's job until the stall bound, then
/// the job is cancelled and the device is free. Sending from a live session and dying look the
/// same to the bridge at this Orion revision, so this covers both. Mutation: with the streamer's
/// `expire` removed the device stays busy and this test fails.
#[test]
fn a_stream_whose_sender_goes_quiet_frees_its_device_after_the_stall_bound() {
    let stall = Duration::from_secs(1);
    let s = setup_stalled("stalled", SMALL + 4096, stall);
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let image = fixture("crc64.img.xz");
    let (let_go, wait) = mpsc::channel();
    let quiet = Quiet {
        limit: image.len(),
        image,
        at: 0,
        read_all: false,
        pace: None,
        first: None,
        wait,
    };
    // Orion's session is not Send: the sender connects on its own thread, and lives there.
    let socket = s._board.socket().to_path_buf();
    let pushing = std::thread::spawn(move || {
        let sender = wait_for("a quiet sender", || {
            Orion::connect(&Address::Local(socket.clone()), "quiet").ok()
        });
        let mut quiet = quiet;
        let _ = sender
            .files()
            .push("lemnos/flash/image.img.xz", &mut quiet, |_, _| {});
    });
    wait_for("the stream to hold the device", || {
        device_busy(&s).then_some(())
    });
    std::thread::sleep(stall * 3);
    let freed = !device_busy(&s);
    let _ = let_go.send(());
    let _ = pushing.join();
    assert!(
        freed,
        "the stream still held the device {stall:?} after its sender went quiet"
    );
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
