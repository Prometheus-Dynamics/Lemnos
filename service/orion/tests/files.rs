//! Files sent through a real Orion node to a device that takes them, and a long job run through
//! an action: the board's slot driver over temporary directories, a real lemnosd, a real Orion
//! comm core and node logic, in-process. Nothing touches a disk but the test's own directory.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_driver_system_slots::SystemSlots;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, files, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Outcome, Run, Target, Update};
use orion::files::FileState;
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(60);
const BLOCK: usize = 4096;

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lf-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A ustar member header.
fn header(name: &str, typeflag: u8, size: usize) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h
}

/// The hash tree's size over `blocks` data blocks, as `veritysetup` writes it: a level of
/// `ceil(n / 128)` blocks while the count being hashed is more than 1.
fn tree_bytes(blocks: usize) -> usize {
    let (mut count, mut total) = (blocks, 0);
    while count > 1 {
        count = count.div_ceil(128);
        total += count;
    }
    total * BLOCK
}

/// An unsigned schema 3 slot bundle for board `t`, its image `data_blocks` blocks of data and
/// room for their tree.
fn bundle(id: &str, data_blocks: usize) -> Vec<u8> {
    let len = data_blocks * BLOCK + tree_bytes(data_blocks);
    let image: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let boot: [(&str, &[u8]); 3] = [
        ("kernel8.img", b"kernel"),
        ("cmdline.txt", b"console=tty1 rootwait\n"),
        ("initramfs", b"initramfs"),
    ];
    let files: Vec<String> = boot
        .iter()
        .map(|(name, bytes)| format!("\"{name}\":\"{}\"", sha(bytes)))
        .collect();
    let manifest = format!(
        "{{\"schema\":3,\"id\":\"{id}\",\"product\":\"an-os\",\"version\":\"1\",\"board\":\"t\",\
         \"image\":{{\"size\":{len},\"sha256\":\"{}\"}},\
         \"verity\":{{\"root_hash\":\"{}\",\"salt\":\"{}\",\"data_blocks\":{data_blocks},\"hash_offset\":{}}},\
         \"boot\":{{\"files\":{{{}}}}}}}",
        sha(&image),
        "ab".repeat(32),
        "cd".repeat(32),
        data_blocks * BLOCK,
        files.join(",")
    );
    let mut members: Vec<(String, Vec<u8>)> = vec![
        ("slot.json".into(), manifest.into_bytes()),
        ("rootfs.erofs".into(), image),
    ];
    for (name, bytes) in boot {
        members.push((format!("boot/{name}"), bytes.to_vec()));
    }
    let mut out = Vec::new();
    out.extend(header("boot/", b'5', 0));
    for (name, bytes) in &members {
        out.extend(header(name, b'0', bytes.len()));
        out.extend(bytes);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend([0; 1024]);
    out
}

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

/// lemnosd serving the slot driver over `dir`'s data and boot directories.
fn start_lemnosd(dir: &Path, socket: &Path, reserve_mb: u64) -> Service {
    let (data, boot) = (dir.join("data"), dir.join("boot"));
    std::fs::create_dir_all(data.join("slots")).expect("data");
    std::fs::create_dir_all(boot.join("slots")).expect("boot");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [host]\ncall_deadline_ms = 30000\n\
         [[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{}\", boot = \"{}\", record = \"file\", partuuid = \"abcd-02\", boot_args = \"os\", reserve_mb = {reserve_mb}, running = \"\" }}\n",
        data.display(),
        boot.display()
    );
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(&text).expect("board"),
        drivers: vec![entry::<SystemSlots>()],
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
        files: vec![("slots".to_owned(), incoming.to_path_buf())],
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

/// Everything up: the node, lemnosd, the bridge, and a dashboard that sees the slots resource.
struct Setup {
    dir: PathBuf,
    dashboard: Orion,
    _bridge: Bridge,
    _service: Service,
    _logic: Logic,
    _board: Board,
}

fn setup(test: &str, reserve_mb: u64) -> Setup {
    let dir = dir(test);
    let socket = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&dir, &socket, reserve_mb);
    let incoming = dir.join("data/slots/.incoming");
    let bridge = start_bridge(&socket, board.socket(), &incoming);
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "dashboard").ok()
    });
    wait_for("the slots resource", || {
        let found = dashboard.resources("lemnos.slots").ok()?;
        found.iter().any(|f| f.name == "lemnos.slots").then_some(())
    });
    Setup {
        dir,
        dashboard,
        _bridge: bridge,
        _service: service,
        _logic: logic,
        _board: board,
    }
}

impl Setup {
    fn incoming(&self) -> PathBuf {
        self.dir.join("data/slots/.incoming")
    }

    /// Pushes `bytes` to `lemnos/slots/<name>`, trying again until the bridge is the receiver.
    fn push(&self, name: &str, bytes: &[u8]) -> orion::files::Transfer {
        let destination = format!("lemnos/slots/{name}");
        wait_for("a push the bridge answers", || {
            self.dashboard
                .files()
                .push(&destination, &mut Cursor::new(bytes), |_, _| {})
                .ok()
        })
    }

    fn start(&self, args: &str) -> Run {
        self.dashboard
            .run(&ActionCall {
                name: "lemnos.slots.call",
                target: Target::Resource("lemnos.slots".parse().expect("name")),
                args: args.as_bytes(),
                deadline_ms: 0,
                detached: false,
            })
            .expect("run")
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

#[test]
fn a_bundle_pushed_through_orion_installs_through_the_install_file_action() {
    let s = setup("install", 0);
    let bundle = bundle("os-2", 1024);
    let sent = s.push("b.tar", &bundle);
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    assert_eq!(
        std::fs::read(s.incoming().join("b.tar")).expect("in place"),
        bundle
    );

    let mut run = s.start("install_file b.tar false false");
    let mut seen = Vec::new();
    let finished = loop {
        match run.wait(WAIT).expect("news of the run") {
            Some(Update::Doing(doing)) => {
                let done = u64::from_le_bytes(doing.data[..8].try_into().expect("done"));
                let total = u64::from_le_bytes(doing.data[8..16].try_into().expect("total"));
                assert_eq!(total, bundle.len() as u64);
                assert_eq!(doing.message, format!("{done} of {total}"));
                seen.push(done);
            }
            Some(Update::Finished(finished)) => break finished,
            None => panic!("the run went quiet"),
        }
    };
    assert_eq!(finished.outcome, Outcome::Succeeded, "{}", finished.detail);
    assert_eq!(finished.output, b"os-2");
    assert!(
        !seen.is_empty(),
        "the job's progress came back as the run's"
    );
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    assert!(s.dir.join("data/slots/os-2.erofs").exists());
    // The driver deleted the file it installed.
    assert!(
        entries(&s.incoming()).is_empty(),
        "{:?}",
        entries(&s.incoming())
    );
}

#[test]
fn cancelling_the_run_cancels_the_install() {
    let s = setup("cancel", 0);
    let bundle = bundle("os-3", 8 * 1024);
    assert_eq!(s.push("big.tar", &bundle).state, FileState::Committed);
    let mut run = s.start("install_file big.tar false false");
    wait_for("the install under way", || {
        matches!(
            run.wait(Duration::from_millis(100)),
            Ok(Some(Update::Doing(_)))
        )
        .then_some(())
    });
    run.cancel().expect("cancel");
    let finished = loop {
        if let Some(Update::Finished(finished)) = run.wait(WAIT).expect("news") {
            break finished;
        }
    };
    assert_eq!(finished.outcome, Outcome::Cancelled, "{}", finished.detail);
    assert!(!s.dir.join("data/slots/os-3.erofs").exists());
    wait_for("the cancelled file deleted", || {
        entries(&s.incoming()).is_empty().then_some(())
    });
}

#[test]
fn a_file_the_device_has_no_room_for_is_refused_before_it_is_sent() {
    // A reserve far past any disk: the device's accept says no.
    let s = setup("full", 1 << 40);
    let sent = s.push("b.tar", &bundle("os-2", 4));
    assert_eq!(sent.state, FileState::Rejected);
    assert!(sent.detail.contains("doesn't take"), "{}", sent.detail);
    assert!(
        entries(&s.incoming()).is_empty(),
        "{:?}",
        entries(&s.incoming())
    );
}

#[test]
fn a_name_the_bridge_refuses_makes_no_file_and_a_missing_file_fails_the_run() {
    let s = setup("digest", 0);
    // A name the bridge refuses never makes a file.
    let sent = s.push(".hidden", b"x");
    assert_eq!(sent.state, FileState::Rejected);
    assert!(entries(&s.incoming()).is_empty());
    // A run for a file that isn't there ends as the driver says.
    let mut run = s.start("install_file nothing.tar false false");
    let finished = run.finish(WAIT).expect("finished");
    assert_eq!(finished.outcome, Outcome::Failed, "{}", finished.detail);
}
