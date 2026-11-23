//! Files sent through a real Orion node to a device that takes them, and a long job run through
//! an action: the board's slot driver over temporary directories, a real lemnosd, a real Orion
//! comm core and node logic, in-process. Nothing touches a disk but the test's own directory.

use std::io::Cursor;
use std::path::{Path, PathBuf};
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
    // The command line is the bundle's, byte for byte: it names its own slot and root hash (the
    // board's `boot_args` prefix is `os`).
    let cmdline = format!(
        "console=tty1 rootwait os.slot={id} os.roothash={} os.src.rpi-boot-mode-1=/soc/mmc@7e202000 panic=5\n",
        "ab".repeat(32)
    );
    // Every file of a bay set the contract requires (the installer writes each into its bay).
    let boot: [(&str, &[u8]); 5] = [
        ("kernel8.img", b"kernel"),
        ("cmdline.txt", cmdline.as_bytes()),
        ("initramfs.cpio", b"initramfs"),
        ("board.dtb", b"device tree"),
        ("overlays/lemnos-board.dtbo", b"overlay"),
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
    // The order the rules give: slot.json, then rootfs.erofs, then boot/ and its files.
    let mut out = Vec::new();
    for (name, bytes) in &members {
        if name == "rootfs.erofs" {
            out.extend(header(name, b'0', bytes.len()));
            out.extend(bytes);
            out.resize(out.len().div_ceil(512) * 512, 0);
            out.extend(header("boot/", b'5', 0));
            continue;
        }
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

/// lemnosd serving the slot driver over `dir`'s data and boot directories.
fn start_lemnosd(dir: &Path, socket: &Path, reserve_mb: u64, running: &str) -> Service {
    let (data, boot) = (dir.join("data"), dir.join("boot"));
    std::fs::create_dir_all(data.join("slots")).expect("data");
    std::fs::create_dir_all(boot.join("slots")).expect("boot");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [host]\ncall_deadline_ms = 30000\n\
         [[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{}\", boot = \"{}\", record = \"file\", partuuid = \"abcd-02\", boot_sources = [\"rpi-boot-mode-1=/soc/mmc@7e202000\"], boot_args = \"os\", reserve_mb = {reserve_mb}, running = \"{running}\" }}\n",
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
        period: lemnos_orion::config::PERIOD,
        orion: Address::Local(orion.to_path_buf()),
        node_id: None,
        run_as: None,
        files: vec![("slots".to_owned(), incoming.to_path_buf())],
        links: Vec::new(),
        streams: Vec::new(),
        lease: Some(lemnos_orion::config::LEASE),
        stall: lemnos_orion::files::IDLE,
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared, lemnos_orion::config::PERIOD))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || {
                calls::run(&socket, &queue, &shared, Some(lemnos_orion::config::LEASE))
            })
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
    dashboard: Orion,
    _bridge: Bridge,
    service: Option<Service>,
    socket: PathBuf,
    _logic: Logic,
    _board: Board,
    dir: Scratch,
}

fn setup(test: &str, reserve_mb: u64) -> Setup {
    setup_running(test, reserve_mb, "")
}

/// As [`setup`], with the slot driver told slot `running` is the one running.
fn setup_running(test: &str, reserve_mb: u64, running: &str) -> Setup {
    let dir = dir(test);
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
    let board = Board::start_for_node(&dir);
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&dir, &socket, reserve_mb, running);
    let incoming = dir.join("data/slots/.incoming");
    let bridge = start_bridge(&socket, board.socket(), &incoming);
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect_once(
            &Address::Local(board.socket().to_path_buf()),
            "dashboard",
            orion::Options::default(),
        )
        .ok()
    });
    wait_for("the slots resource", || {
        let found = dashboard.resources("lemnos.slots").ok()?;
        found.iter().any(|f| f.name == "lemnos.slots").then_some(())
    });
    Setup {
        dir,
        dashboard,
        _bridge: bridge,
        service: Some(service),
        socket,
        _logic: logic,
        _board: board,
    }
}

impl Setup {
    /// Stops lemnosd and starts it again on the same directories, its slot driver told slot
    /// `running` is the one running; returns once the bridge reaches it again.
    fn restart_lemnosd(&mut self, running: &str) {
        self.service = None;
        self.service = Some(start_lemnosd(&self.dir, &self.socket, 0, running));
        wait_for("the bridge at the new lemnosd", || {
            let mut run = self.start("boots");
            let finished = run.finish(WAIT).ok()?;
            (finished.outcome == Outcome::Succeeded).then_some(())
        });
    }

    fn incoming(&self) -> PathBuf {
        self.dir.join("data/slots/.incoming")
    }

    /// Pushes `bytes` to `lemnos/slots/<name>`, trying again until the bridge is the receiver.
    fn push(&self, name: &str, bytes: &[u8]) -> orion::files::Transfer {
        let destination = format!("lemnos/slots/{name}");
        // Retried until the bridge answers. The last error is in the message if it never does.
        let deadline = Instant::now() + WAIT;
        loop {
            let last =
                match self
                    .dashboard
                    .files()
                    .push(&destination, &mut Cursor::new(bytes), |_, _| {})
                {
                    Ok(transfer) => return transfer,
                    Err(error) => format!("{error}"),
                };
            assert!(
                Instant::now() < deadline,
                "timed out: a push the bridge answers (last error: {last})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
                // The bytes, then the install's five finish steps (`FINISH_UNITS`).
                assert_eq!(total, bundle.len() as u64 + 5);
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

/// A cancel of a run, and the install it cancels, always agree: when the run says it was cancelled,
/// the slot is not installed; when it says it completed, the slot is. The driver decides which, and
/// the run reports what lemnosd says the job did, so the two can't disagree. Each round cancels at a
/// different point: at once, after the install's first progress, and after the slot is in place (so
/// the install may already have finished). Either outcome passes; a mix does not.
#[test]
fn a_cancelled_run_and_its_install_always_agree() {
    const ROUNDS: u32 = 12;
    for round in 0..ROUNDS {
        let s = setup(&format!("agree{round}"), 0);
        let bundle = bundle("os-3", 8 * 1024);
        assert_eq!(s.push("big.tar", &bundle).state, FileState::Committed);
        let slot = s.dir.join("data/slots/os-3.erofs");
        let mut run = s.start("install_file big.tar false false");
        let mut progressed = false;
        let mut finished = None;
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "round {round}: timed out");
            if finished.is_some() {
                break;
            }
            let ready = match round % 3 {
                0 => true,
                1 => progressed,
                _ => slot.exists(),
            };
            if ready {
                break;
            }
            match run.wait(Duration::from_millis(5)).expect("news") {
                Some(Update::Doing(_)) => progressed = true,
                Some(Update::Finished(f)) => finished = Some(f),
                None => {}
            }
        }
        let finished = match finished {
            // The run ended before the cancel: its end is the answer.
            Some(finished) => finished,
            None => {
                run.cancel().expect("cancel");
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "round {round}: the run never ended"
                    );
                    if let Some(Update::Finished(f)) =
                        run.wait(Duration::from_millis(50)).expect("news")
                    {
                        break f;
                    }
                }
            }
        };
        let installed = slot.exists();
        match finished.outcome {
            Outcome::Cancelled => {
                assert!(
                    !installed,
                    "round {round}: the run says cancelled and os-3 is installed: {}",
                    finished.detail
                );
                wait_for("the cancelled file deleted", || {
                    entries(&s.incoming()).is_empty().then_some(())
                });
            }
            Outcome::Succeeded => assert!(
                installed,
                "round {round}: the run says it completed and os-3 is not there: {}",
                finished.detail
            ),
            other => panic!(
                "round {round}: unexpected outcome {other:?}: {}",
                finished.detail
            ),
        }
    }
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
    // A run for a file that isn't there is refused by the driver with NOT_FOUND (-2): refused without
    // a run, so it ends `Rejected`, with lemnosd's status as the code.
    let mut run = s.start("install_file nothing.tar false false");
    let finished = run.finish(WAIT).expect("finished");
    assert_eq!(finished.outcome, Outcome::Rejected, "{}", finished.detail);
    assert_eq!(finished.code, 2, "{finished:?}");
}

// ---- Refusal words: each cause a status can't tell apart reaches the client in words ----

use lemnos_core::Status;
use lemnos_driver_system_slots::words;

/// Pushes a bundle for slot `id` and installs it through the action; the run must succeed.
fn install(s: &Setup, id: &str) {
    let name = format!("{id}.tar");
    let sent = s.push(&name, &bundle(id, 16));
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    let mut run = s.start(&format!("install_file {name} false false"));
    let finished = run.finish(WAIT).expect("finished");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{}", finished.detail);
}

/// Sets `key` in slot `id`'s meta file, as the boot side would have.
fn set_meta(s: &Setup, id: &str, key: &str, value: &str) {
    let path = s.dir.join(format!("data/board/slots/{id}.meta"));
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = text
        .lines()
        .filter(|line| !line.starts_with(&format!("{key}=")))
        .map(str::to_owned)
        .collect();
    kept.push(format!("{key}={value}"));
    std::fs::write(&path, kept.join("\n") + "\n").expect("meta");
}

/// Sets `key` in the slots state, as a trial the boot side armed would have.
fn set_state(s: &Setup, key: &str, value: &str) {
    let path = s.dir.join("data/board/slots/state.env");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = text
        .lines()
        .filter(|line| !line.starts_with(&format!("{key}=")))
        .map(str::to_owned)
        .collect();
    kept.push(format!("{key}='{value}'"));
    std::fs::write(&path, kept.join("\n") + "\n").expect("state");
}

/// Whether the run ended with lemnosd's `status`: as its code (a run refused before it ran), or
/// named in its detail (a run that failed).
fn ended_with(finished: &orion::actions::Finished, status: Status) -> bool {
    finished.code == status.0.unsigned_abs()
        || finished.detail.contains(&format!("refused ({})", status.0))
}

/// Runs `args` and asserts it is refused with `status`, its detail carrying `words`.
fn refused_with(s: &Setup, args: &str, status: Status, words: &str) {
    let mut run = s.start(args);
    let finished = run.finish(WAIT).expect("finished");
    assert_ne!(finished.outcome, Outcome::Succeeded, "{args}");
    assert!(ended_with(&finished, status), "{args}: {finished:?}");
    assert!(
        finished.detail.contains(words),
        "{args}: the words {words:?} are not in {:?}",
        finished.detail
    );
}

#[test]
fn set_default_says_whether_the_slot_failed_a_trial_or_never_booted_good() {
    let s = setup("words-default", 0);
    install(&s, "os-2");
    refused_with(
        &s,
        "set_default os-2 false",
        Status::NOT_PERMITTED,
        words::NEVER_GOOD,
    );
    // Force does not override a slot that never booted good.
    refused_with(
        &s,
        "set_default os-2 true",
        Status::NOT_PERMITTED,
        words::NEVER_GOOD,
    );
    set_meta(&s, "os-2", "FAILED", "yes");
    refused_with(
        &s,
        "set_default os-2 false",
        Status::NOT_PERMITTED,
        words::FAILED_TRIAL,
    );
}

#[test]
fn remove_says_which_of_its_causes_refused_it() {
    let s = setup("words-remove", 0);
    install(&s, "os-2");
    refused_with(&s, "remove os-2", Status::BUSY, words::REMOVE_ONLY);
    install(&s, "os-3");
    let mut pin = s.start("pin os-3 true");
    assert_eq!(
        pin.finish(WAIT).expect("finished").outcome,
        Outcome::Succeeded
    );
    refused_with(
        &s,
        "remove os-3",
        Status::NOT_PERMITTED,
        words::REMOVE_PINNED,
    );
    set_meta(&s, "os-3", "GOOD", "yes");
    let mut default = s.start("set_default os-3 false");
    let set = default.finish(WAIT).expect("finished");
    assert_eq!(set.outcome, Outcome::Succeeded, "{}", set.detail);
    refused_with(&s, "remove os-3", Status::BUSY, words::REMOVE_DEFAULT);
    set_state(&s, "TRIAL", "os-2");
    refused_with(&s, "remove os-2", Status::BUSY, words::REMOVE_TRIAL);
}

#[test]
fn install_file_says_why_it_takes_no_file() {
    let s = setup("words-install", 0);
    refused_with(
        &s,
        "install_file nothing.tar false false",
        Status::NOT_FOUND,
        words::NOT_IN_INCOMING,
    );
    refused_with(
        &s,
        "install_file a~b false false",
        Status::INVALID_ARGUMENT,
        words::BAD_NAME,
    );
    std::fs::create_dir_all(s.incoming().join("dir.tar")).expect("dir");
    refused_with(
        &s,
        "install_file dir.tar false false",
        Status::INVALID_ARGUMENT,
        words::NOT_A_FILE,
    );
    std::fs::write(s.dir.join("elsewhere.tar"), b"x").expect("target");
    std::os::unix::fs::symlink(s.dir.join("elsewhere.tar"), s.incoming().join("link.tar"))
        .expect("link");
    refused_with(
        &s,
        "install_file link.tar false false",
        Status::INVALID_ARGUMENT,
        words::A_LINK,
    );
    // Another install runs: a second one is refused, by that cause.
    let big = bundle("os-9", 16 * 1024);
    let sent = s.push("big.tar", &big);
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    let sent = s.push("small.tar", &bundle("os-8", 16));
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    let mut first = s.start("install_file big.tar false false");
    wait_for("the first install under way", || {
        matches!(first.wait(WAIT).expect("news"), Some(Update::Doing(_))).then_some(())
    });
    refused_with(
        &s,
        "install_file small.tar false false",
        Status::BUSY,
        words::INSTALL_RUNNING,
    );
    let _ = first.finish(WAIT);
}

#[test]
fn a_write_while_the_state_cannot_be_written_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let mut s = setup("words-state", 0);
    install(&s, "os-2");
    // The daemon finds its state dir unwritable when it starts (a grant it never got).
    let state = s.dir.join("data/board/slots");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    s.restart_lemnosd("");
    let mut run = s.start("pin os-2 true");
    let finished = run.finish(WAIT).expect("finished");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(
        ended_with(&finished, Status::FAILED_PRECONDITION),
        "{finished:?}"
    );
    assert!(
        finished.detail.contains(words::STATE_UNWRITABLE),
        "{:?}",
        finished.detail
    );
}

/// A failed install job ends with its cause in words: the run's detail carries them after the
/// status, not a result.
#[test]
fn a_failed_install_job_ends_with_its_words() {
    let s = setup("words-job", 0);
    // Not a bundle: the install starts, and fails on what it reads.
    let sent = s.push("junk.tar", &[0x55u8; 8192]);
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    let mut run = s.start("install_file junk.tar false false");
    let finished = run.finish(WAIT).expect("finished");
    assert_eq!(finished.outcome, Outcome::Failed, "{}", finished.detail);
    let (_, words) = finished
        .detail
        .split_once(": ")
        .unwrap_or_else(|| panic!("no words after the status: {:?}", finished.detail));
    assert!(!words.is_empty(), "{:?}", finished.detail);
    assert!(finished.output.is_empty(), "a failed job has no result");
}

#[test]
fn trial_says_whether_the_slot_failed_a_trial_or_is_the_one_running() {
    let mut s = setup("words-trial", 0);
    install(&s, "os-2");
    install(&s, "os-3");
    s.restart_lemnosd("os-3");
    set_meta(&s, "os-2", "FAILED", "yes");
    refused_with(
        &s,
        "trial os-2 false false false",
        Status::NOT_PERMITTED,
        words::FAILED_TRIAL,
    );
    refused_with(
        &s,
        "trial os-3 false false false",
        Status::BUSY,
        words::TRIAL_RUNNING,
    );
    refused_with(&s, "remove os-3", Status::BUSY, words::REMOVE_RUNNING);
}
