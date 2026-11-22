//! A flash image pushed through a real Orion node, streamed into the flash driver's write job as it
//! arrives: the manifest goes to disk like any file, the image is never written to the bridge's disk,
//! and the job's result is the sender's answer. Temporary files and a fake mount table only.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_driver_system_flash::SystemFlash;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, files, publish, source};

#[path = "support/proxy.rs"]
mod proxy;
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

/// A scratch directory under TMPDIR, removed when the test ends, whether it passed or panicked.
struct Scratch(std::path::PathBuf);

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(_test: &str) -> Scratch {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    Scratch(dir)
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
        settings.config.socket = lemnos_socket_dir::socket(dir, "orion.sock").unwrap();
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
         [security]\nrecord = \"{}\"\nkeys = \"{}\"\n\
         [[devices]]\nid = \"flash\"\ndriver = \"system-flash\"\n\
         config = {{ path = \"{}\", kind = \"file\", allow_first_flash = true, mountinfo = \"{}\" }}\n\
         [[devices]]\nid = \"tracked\"\ndriver = \"tracked\"\n",
        dir.join("security.rec").display(),
        keys.display(),
        target.display(),
        mountinfo.display()
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
    /// The proxies between the bridge and lemnosd, and between the bridge and the node, when the test asked.
    proxies: Option<(proxy::Proxy, proxy::Proxy)>,
    target: PathBuf,
    dashboard: Orion,
    _bridge: Bridge,
    _service: Service,
    _logic: Logic,
    _board: Board,
    dir: Scratch,
}

fn setup(test: &str, target_bytes: u64) -> Setup {
    setup_stalled(test, target_bytes, lemnos_orion::files::IDLE)
}

fn setup_stalled(test: &str, target_bytes: u64, stall: Duration) -> Setup {
    setup_with(test, target_bytes, stall, Some(lemnos_orion::config::LEASE))
}

fn setup_with(test: &str, target_bytes: u64, stall: Duration, lease: Option<Duration>) -> Setup {
    setup_full(test, target_bytes, stall, lease, false)
}

/// As [`setup`], with the bridge reaching lemnosd and the node through proxies ([`proxy::Proxy`]).
fn setup_proxied(test: &str, target_bytes: u64) -> Setup {
    setup_full(
        test,
        target_bytes,
        lemnos_orion::files::IDLE,
        Some(lemnos_orion::config::LEASE),
        true,
    )
}

fn setup_full(
    test: &str,
    target_bytes: u64,
    stall: Duration,
    lease: Option<Duration>,
    proxied: bool,
) -> Setup {
    let dir = dir(test);
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
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
    let proxies = proxied.then(|| {
        (
            proxy::Proxy::start(&dir.join("lemnosd-proxy.sock"), &socket),
            proxy::Proxy::start(&dir.join("node-proxy.sock"), board.socket()),
        )
    });
    let (to_lemnosd, to_node) = proxies.as_ref().map_or_else(
        || (socket.clone(), board.socket().to_path_buf()),
        |(l, n)| (l.path().to_path_buf(), n.path().to_path_buf()),
    );
    let bridge = start_bridge_stalled(&to_lemnosd, &to_node, &incoming, stall, lease);
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect_once(
            &Address::Local(board.socket().to_path_buf()),
            "dashboard",
            orion::Options::default(),
        )
        .ok()
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
        proxies,
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
            deadline_ms: 0,
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
            Orion::connect_once(
                &Address::Local(socket.clone()),
                "steady",
                orion::Options::default(),
            )
            .ok()
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

/// A push that keeps sending renews the caller's lease: its hold on the tracked device survives a
/// push three lease periods long, with the caller sending nothing else. The lease is five seconds, so
/// the bridge's own device writes (two to four seconds each on a loaded machine) fit inside it. The
/// sender reads 8 KiB every 60 ms, a chunk each few hundred milliseconds. Mutation: with the accepted
/// chunk not renewing the lease the hold is released at the lease and this test fails.
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
            Orion::connect_once(
                &Address::Local(socket.clone()),
                "steady",
                orion::Options::default(),
            )
            .ok()
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

/// A push that starts and then sends no chunk does not renew the lease: the caller's hold on the
/// tracked device is released once the lease runs out, though the transfer itself is still open
/// (the stall bound of the files, not the lease, is what ends it). Mutation: with `expire` a no-op
/// the hold stays and this test fails.
#[test]
fn a_stalled_push_lets_the_lease_expire() {
    use sha2::{Digest, Sha256};
    const SIZE: usize = 64 * 1024;
    let lease = Duration::from_secs(5);
    let s = setup_with(
        "push-stalled",
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
        pace: None,
        first: Some(0),
        wait,
    };
    let socket = s._board.socket().to_path_buf();
    let pushing = std::thread::spawn(move || {
        let sender = wait_for("a stalled sender", || {
            Orion::connect_once(
                &Address::Local(socket.clone()),
                "stalled",
                orion::Options::default(),
            )
            .ok()
        });
        let mut quiet = quiet;
        let _ = sender
            .files()
            .push("lemnos/flash/image.img", &mut quiet, |_, _| {});
    });
    wait_for("the stream to begin", || device_busy(&s).then_some(()));
    keep_open(&s.dashboard, lease * 3);
    let held = tracked_call(&s.dashboard, "held");
    let _ = let_go.send(());
    let _ = pushing.join();
    assert_eq!(
        held, "0",
        "a stalled push kept the caller's hold past the lease"
    );
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
            deadline_ms: 0,
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
            Orion::connect_once(
                &Address::Local(socket.clone()),
                "quiet",
                orion::Options::default(),
            )
            .ok()
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

/// The manifest rule after a broken push: the manifest is a file in the device's directory, and it
/// stays there until a push of the same name replaces it. A push that breaks mid-image, on a sender
/// session that then goes, is begun again by a new session with no manifest pushed first; the
/// begun-again stream ends the broken one (its write is cancelled) and gives the manifest to the
/// device again, and the image is committed with its fields. The broken stream is still open when the
/// new one begins (the stall bound is the default, far longer than the test), so this is the case
/// where the device's earlier write has to be ended before its manifest is taken again.
#[test]
fn an_image_begun_again_on_a_new_session_after_a_broken_push_needs_no_manifest_again() {
    let s = setup("again", SMALL + 4096);
    let manifest = manifest(BOARD, SMALL_SHA, None);
    assert_eq!(
        s.push("image.img.xz.json", &manifest).state,
        FileState::Committed
    );
    let image = fixture("crc64.img.xz");
    let (let_go, wait) = mpsc::channel();
    let quiet = Quiet {
        limit: image.len(),
        image: image.clone(),
        at: 0,
        read_all: false,
        pace: None,
        first: None,
        wait,
    };
    let socket = s._board.socket().to_path_buf();
    let broken = std::thread::spawn(move || {
        let sender = wait_for("the first sender", || {
            Orion::connect_once(
                &Address::Local(socket.clone()),
                "first",
                orion::Options::default(),
            )
            .ok()
        });
        let mut quiet = quiet;
        let pushed = sender
            .files()
            .push("lemnos/flash/image.img.xz", &mut quiet, |_, _| {});
        // The sender's session goes with its thread: the push is broken, never committed.
        pushed.map(|t| t.state)
    });
    wait_for("the broken stream to hold the device", || {
        device_busy(&s).then_some(())
    });
    let _ = let_go.send(());
    let first = broken.join().expect("the first sender");
    assert!(
        !matches!(first, Ok(FileState::Committed)),
        "the broken push was committed: {first:?}"
    );
    // A new session begins the image again, and pushes no manifest.
    let again = wait_for("a new sender", || {
        Orion::connect_once(
            &Address::Local(s._board.socket().to_path_buf()),
            "again",
            orion::Options::default(),
        )
        .ok()
    });
    let sent = again
        .files()
        .push(
            "lemnos/flash/image.img.xz",
            &mut Cursor::new(&image),
            |_, _| {},
        )
        .expect("the push begun again");
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    assert_eq!(sent.detail, format!("raw_bytes={SMALL} outcome=1"));
    assert_eq!(
        &std::fs::read(&s.target).expect("target")[..SMALL as usize],
        fixture("small.img").as_slice()
    );
    assert_eq!(
        std::fs::read(s.incoming().join("image.img.xz.json")).expect("the manifest stays"),
        manifest
    );
}

/// A verify run through the bridge's call action ends with its result in fields, the commit's form:
/// `raw_bytes=<n>`, never eight bytes shown as text or hex by what they happen to be.
#[test]
fn a_verify_called_through_the_bridge_answers_in_fields() {
    let s = setup("verify", SMALL + 4096);
    assert_eq!(
        s.push("image.img.xz.json", &manifest(BOARD, SMALL_SHA, None))
            .state,
        FileState::Committed
    );
    let sent = s.push("image.img.xz", &fixture("crc64.img.xz"));
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    let mut run = s
        .dashboard
        .run(&ActionCall {
            name: "lemnos.flash.call",
            target: Target::Resource("lemnos.flash".parse().expect("name")),
            args: b"verify",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    let finished = run.finish(WAIT).expect("the verify ends");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    assert_eq!(
        String::from_utf8(finished.output).expect("text"),
        format!("raw_bytes={SMALL}")
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
    assert_eq!(sent.detail, format!("raw_bytes={SMALL} outcome=1"));
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
    assert_eq!(sent.detail, format!("raw_bytes={SMALL} outcome=0"));
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

/// A raw image of `size` bytes that is not all one value, and its manifest's SHA-256 in hex.
fn raw_image(size: usize) -> (Vec<u8>, String) {
    use sha2::{Digest, Sha256};
    let image: Vec<u8> = (0..size).map(|i| (i * 7 + i / 4096) as u8).collect();
    let sha = Sha256::digest(&image)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (image, sha)
}

/// A sender session that waits for the bridge's answers as long as the node is heard from (`Reply::Alive`),
/// as a flasher's client does for a slow device.
fn patient_sender(socket: &Path, name: &str) -> Orion {
    let sender = wait_for("a patient sender", || {
        Orion::connect_once(
            &Address::Local(socket.to_path_buf()),
            name,
            orion::Options::default(),
        )
        .ok()
    });
    sender.set_reply(orion::Reply::Alive {
        ceiling: Some(Duration::from_secs(120)),
    });
    sender
}

/// FLASH-BRIDGE-PUMP: a device call that takes longer than a sender waits for an answer does not stop the
/// bridge answering its other files. The stream's first lemnosd reply is held 4 s (a slow device, as the bridge
/// sees it; held under the lemnos client's own 5 s request bound, so the call still succeeds); meanwhile a
/// plain file pushed to the same device is committed while that reply is still held. Then the stream itself
/// commits. Before: the bridge made the stream's calls on the thread that pumps its Orion session, so the
/// plain file waited out the whole hold (and a hold past the sender's 5 s status bound fails its push).
#[test]
fn a_device_call_held_past_the_senders_bound_does_not_stop_the_bridge_answering_other_files() {
    const SIZE: usize = 256 * 1024;
    let s = setup_proxied("held-call", SIZE as u64 + 4096);
    let (image, sha) = raw_image(SIZE);
    assert_eq!(
        s.push("image.img.json", &manifest_of(SIZE as u64, &sha))
            .state,
        FileState::Committed
    );
    let (to_lemnosd, _) = s.proxies.as_ref().expect("proxied");
    // Under the lemnos client's own 5 s request bound, so the held call still succeeds.
    let hold = Duration::from_secs(4);
    to_lemnosd.hold_next(lemnos_orion::stream::STREAMS_CLIENT_NAME.as_bytes(), hold);
    let socket = s._board.socket().to_path_buf();
    let streaming = std::thread::spawn(move || {
        let sender = patient_sender(&socket, "streamer");
        sender
            .files()
            .push(
                "lemnos/flash/image.img",
                &mut Cursor::new(&image),
                |_, _| {},
            )
            .map(|t| (t.state, t.detail))
    });
    // The stream's begin is the first thing its connection is answered: wait until it is being held.
    std::thread::sleep(Duration::from_secs(1));
    let asked = Instant::now();
    let other = s.dashboard.files().push(
        "lemnos/flash/other.json",
        &mut Cursor::new(b"{}".to_vec()),
        |_, _| {},
    );
    let took = asked.elapsed();
    assert!(
        matches!(&other, Ok(t) if t.state == FileState::Committed),
        "the plain file was not committed while the stream's call was held ({took:?}): {other:?}"
    );
    assert!(
        to_lemnosd.held().is_none(),
        "the plain file was answered only after the held call was let go ({took:?}); the hold proved nothing"
    );
    let streamed = streaming.join().expect("the streaming sender");
    assert!(
        to_lemnosd.held().is_some_and(|h| h >= hold),
        "the stream's reply was never held"
    );
    assert!(
        matches!(&streamed, Ok((FileState::Committed, _))),
        "{streamed:?}"
    );
    assert_eq!(
        streamed.expect("committed").1,
        format!("raw_bytes={SIZE} outcome=1")
    );
}

/// Pushes `image` as `lemnos/flash/image.img` from a patient sender reaching the node at `socket`, paced (8 KiB
/// every 20 ms), and records every `received` the push reports. With `cut_at`, once that much is taken
/// `cut` is called (it breaks a session) and the sender is let go.
fn paced_push(
    socket: &Path,
    name: &str,
    image: &[u8],
    cut_at: Option<(u64, &dyn Fn())>,
) -> (Result<(FileState, String), String>, Vec<u64>) {
    let (let_go, wait) = mpsc::channel();
    let mut paced = Quiet {
        limit: image.len(),
        image: image.to_vec(),
        at: 0,
        read_all: false,
        pace: Some((Duration::from_millis(20), 8 * 1024)),
        first: None,
        wait,
    };
    let progress = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let (socket, seen, name) = (socket.to_path_buf(), Arc::clone(&progress), name.to_owned());
    let pushing = std::thread::spawn(move || {
        let sender = patient_sender(&socket, &name);
        sender
            .files()
            .push("lemnos/flash/image.img", &mut paced, |done, _| {
                seen.lock().unwrap().push(done);
            })
            .map(|t| (t.state, t.detail))
            .map_err(|e| format!("{e:?}"))
    });
    if let Some((at, cut)) = cut_at {
        wait_for("the bytes before the cut taken", || {
            let seen = progress.lock().unwrap();
            (seen.last().copied().unwrap_or(0) >= at || pushing.is_finished()).then_some(())
        });
        cut();
    }
    let pushed = pushing.join().expect("the sender");
    drop(let_go);
    let seen = progress.lock().unwrap().clone();
    (pushed, seen)
}

/// The image `paced_push` sent again after a broken first push: resumed at the bytes the device had taken
/// (its first report is past the first push's last, never 0), never going backwards, committed, and on the
/// target.
fn assert_resumed(
    s: &Setup,
    image: &[u8],
    first: &[u64],
    again: &(Result<(FileState, String), String>, Vec<u64>),
) {
    let (pushed, seen) = again;
    assert!(
        matches!(pushed, Ok((FileState::Committed, _))),
        "the second push: {pushed:?}; progress {seen:?}; the first push's {first:?}"
    );
    let taken = first.last().copied().unwrap_or(0);
    let resumed_at = seen.iter().copied().find(|&r| r > 0).unwrap_or(0);
    assert!(
        taken > 0 && resumed_at >= taken,
        "the second push did not resume at the bytes taken ({taken}): {seen:?}"
    );
    assert!(
        seen.windows(2).all(|w| w[1] >= w[0]),
        "the transfer went backwards: {seen:?}"
    );
    assert_eq!(
        &std::fs::read(&s.target).expect("target")[..image.len()],
        image
    );
}

/// FLASH-BRIDGE-PUMP: the bridge's own Orion session breaks mid-stream (the node then ends that push for the
/// sender). The stream and its job are kept across the break: the sender's next push of the same image is
/// answered with the bytes the device has taken and resumes from there, and the image commits. Before: the
/// bridge built its stream state with each Orion session, so the break lost the stream with its job still
/// open on the device, and the next push could not start one (`-19`) or started over at 0.
#[test]
fn a_stream_whose_bridge_session_breaks_resumes_at_the_bytes_taken_and_commits() {
    const SIZE: usize = 1024 * 1024;
    let s = setup_proxied("cut-bridge", SIZE as u64 + 4096);
    let (image, sha) = raw_image(SIZE);
    assert_eq!(
        s.push("image.img.json", &manifest_of(SIZE as u64, &sha))
            .state,
        FileState::Committed
    );
    let socket = s._board.socket().to_path_buf();
    let (_, to_node) = s.proxies.as_ref().expect("proxied");
    let cut = || to_node.cut();
    let (broken, first) = paced_push(&socket, "first", &image, Some(((SIZE / 4) as u64, &cut)));
    assert!(
        !matches!(broken, Ok((FileState::Committed, _))),
        "the push across the cut committed: the cut proved nothing ({first:?})"
    );
    // The bridge's files session is back once a plain file is answered.
    let _ = s.push("again.json", b"{}");
    let again = paced_push(&socket, "again", &image, None);
    assert_resumed(&s, &image, &first, &again);
}

/// FLASH-BRIDGE-PUMP: the sender's own session breaks mid-stream (the board's case: the PC's link to the node
/// was lost). The sender then does what a flasher's client does on every attempt: it pushes the same manifest
/// again (a plain file: committed again, and the open stream and its job are left alone), then the same image,
/// which is answered with the bytes the device has taken, resumes from there, and commits. Before: the bridge's resumed begin ended the stream's job and started it over at 0,
/// which a sender resuming within its push reads as a transfer that went backwards.
#[test]
fn a_stream_whose_sender_session_breaks_resumes_at_the_bytes_taken_and_commits() {
    const SIZE: usize = 1024 * 1024;
    let s = setup_proxied("cut-sender", SIZE as u64 + 4096);
    let (image, sha) = raw_image(SIZE);
    assert_eq!(
        s.push("image.img.json", &manifest_of(SIZE as u64, &sha))
            .state,
        FileState::Committed
    );
    let through = proxy::Proxy::start(&s.dir.join("sender-proxy.sock"), s._board.socket());
    let cut = || through.cut();
    let (broken, first) = paced_push(
        through.path(),
        "first",
        &image,
        Some(((SIZE / 4) as u64, &cut)),
    );
    assert!(
        !matches!(broken, Ok((FileState::Committed, _))),
        "the push across the cut committed: the cut proved nothing ({first:?})"
    );
    let manifest = manifest_of(SIZE as u64, &sha);
    assert_eq!(
        s.push("image.img.json", &manifest).state,
        FileState::Committed,
        "the same manifest pushed again"
    );
    assert!(
        device_busy(&s),
        "the manifest pushed again ended the open stream"
    );
    let again = paced_push(s._board.socket(), "again", &image, None);
    assert_resumed(&s, &image, &first, &again);
}

/// FLASH-BRIDGE-PUMP: an image whose commit was answered, pushed again (a sender that lost the answer and asks
/// again), is answered with how it ended, `committed` and its fields, and nothing is sent or written again.
/// Before: the bridge started the stream over, so the whole image was sent and written a second time.
#[test]
fn a_committed_image_pushed_again_is_answered_with_its_outcome_and_not_sent_again() {
    const SIZE: usize = 256 * 1024;
    let s = setup_proxied("again-committed", SIZE as u64 + 4096);
    let (image, sha) = raw_image(SIZE);
    assert_eq!(
        s.push("image.img.json", &manifest_of(SIZE as u64, &sha))
            .state,
        FileState::Committed
    );
    let socket = s._board.socket().to_path_buf();
    let (first, _) = paced_push(&socket, "first", &image, None);
    assert!(
        matches!(&first, Ok((FileState::Committed, _))),
        "the first push: {first:?}"
    );
    // The target is changed behind the bridge: a second write of the image would put it back.
    let mut target = std::fs::read(&s.target).expect("target");
    target[..4].copy_from_slice(b"MARK");
    std::fs::write(&s.target, &target).expect("mark");
    let (again, seen) = paced_push(&socket, "again", &image, None);
    assert_eq!(
        again,
        Ok((FileState::Committed, format!("raw_bytes={SIZE} outcome=1"))),
        "progress {seen:?}"
    );
    assert!(
        seen.iter().all(|&r| r == 0 || r == SIZE as u64),
        "chunks were sent again: {seen:?}"
    );
    assert_eq!(
        &std::fs::read(&s.target).expect("target")[..4],
        b"MARK",
        "the image was written again"
    );
}

/// FLASH-INFO-NOTFOUND: a device's `call` is an action of the device's resource, so it is addressed to the
/// resource (`Target::Resource("lemnos.<device>")`). `lemnos.flash.call info` there answers the device's
/// `info` as text: the target, the state and the manifest. The same call addressed to the node is not one of
/// the node's actions: the node refuses it "not found", and nothing runs. That is the refusal a flasher's
/// client saw when it sent `info` to the node.
#[test]
fn a_devices_call_is_answered_on_its_resource_and_refused_not_found_on_the_node() {
    let s = setup("info-target", SMALL + 4096);
    let call = |target| {
        s.dashboard.run(&ActionCall {
            name: "lemnos.flash.call",
            target,
            args: b"info",
            deadline_ms: 0,
            detached: false,
        })
    };
    let mut run = call(Target::Resource("lemnos.flash".parse().expect("name"))).expect("run");
    let finished = run.finish(WAIT).expect("info ends");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    let info = String::from_utf8(finished.output).expect("text");
    assert!(
        info.starts_with("{\"kind\":\"file\"") && info.contains("\"allow_first_flash\":true"),
        "the device's info: {info}"
    );
    let node: orion::types::NodeName = s
        .dashboard
        .node()
        .expect("node")
        .name
        .parse()
        .expect("name");
    // The refusal comes at the run or at its end, as the node sends it: either way it is the node's "not found".
    let on_node: Result<Outcome, String> = match call(Target::Node(node)) {
        Ok(mut run) => run
            .finish(WAIT)
            .map(|f| f.outcome)
            .map_err(|e| format!("{e} ({e:?})")),
        Err(e) => Err(format!("{e} ({e:?})")),
    };
    assert!(
        matches!(&on_node, Err(why) if why.contains("not found")),
        "the node answered a resource's action addressed to it: {on_node:?}"
    );
}
