//! A device that takes files on a board that gives the bridge no place for them: the bridge says
//! so once by name, the device's status topic says so in `pushes`, and a push to it is refused at
//! its start with that reason, nothing written and nothing kept by the node. A device given a place
//! is unchanged and says `accepted`. A real lemnosd, a real Orion comm core and node logic,
//! in-process; nothing touches a disk but the test's own directory.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_driver_system_flash::SystemFlash;
use lemnos_driver_system_slots::SystemSlots;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, files, publish, pushes, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::files::FileState;
use orion::topics::{FieldId, Subscriber};
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(60);
/// The status topic's `pushes` field: the fifth, after `state`, `health`, `reason`, `refusal`.
const PUSHES: u16 = 4;

/// A scratch directory under TMPDIR, removed when the test ends, whether it passed or panicked.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(test: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("lemnos-pushes-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    Scratch(dir)
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

/// lemnosd serving a slot device over `dir` and a flash device over a target that is not there
/// (it never binds; its description is its manifest's, which is all the bridge reads).
fn start_lemnosd(dir: &Path, socket: &Path) -> Service {
    let (data, boot) = (dir.join("data"), dir.join("boot"));
    std::fs::create_dir_all(data.join("slots")).expect("data");
    std::fs::create_dir_all(boot.join("slots")).expect("boot");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [host]\ncall_deadline_ms = 30000\n\
         [[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{}\", boot = \"{}\", record = \"file\", partuuid = \"abcd-02\", boot_sources = [\"rpi-boot-mode-1=/soc/mmc@7e202000\"], boot_args = \"os\", running = \"\" }}\n\
         [[devices]]\nid = \"flash\"\ndriver = \"system-flash\"\n\
         config = {{ path = \"{}\", kind = \"file\" }}\n",
        data.display(),
        boot.display(),
        dir.join("no-target").display(),
    );
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(&text).expect("board"),
        drivers: vec![entry::<SystemSlots>(), entry::<SystemFlash>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

struct Bridge {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn config(lemnosd: &Path, orion: &Path, files: Vec<(String, PathBuf)>) -> Config {
    Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: None,
        run_as: None,
        files,
        links: Vec::new(),
        streams: Vec::new(),
        lease: Some(lemnos_orion::config::LEASE),
        stall: lemnos_orion::files::IDLE,
    }
}

fn start_bridge(config: Config) -> Bridge {
    let shared = Arc::new(Shared::new().expect("shared"));
    let (jobs, queue) = mpsc::channel();
    let socket = config.lemnosd_socket.clone();
    let threads = vec![
        {
            let (socket, shared) = (socket.clone(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let shared = Arc::clone(&shared);
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

struct Setup {
    dashboard: Orion,
    socket: PathBuf,
    _bridge: Bridge,
    _service: Service,
    _logic: Logic,
    _board: Board,
    dir: Scratch,
}

/// Everything up, the bridge given a place for the files of `placed` only.
fn setup(test: &str, placed: &[&str]) -> Setup {
    let dir = dir(test);
    let socket = lemnos_socket_dir::socket(&dir, "lemnosd.sock").unwrap();
    let board = Board::start_for_node(&dir);
    let logic = Logic::start(&dir);
    let service = start_lemnosd(&dir, &socket);
    let files = placed
        .iter()
        .map(|device| ((*device).to_owned(), dir.join("data/slots/.incoming")))
        .collect();
    let bridge = start_bridge(config(&socket, board.socket(), files));
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect_once(
            &Address::Local(board.socket().to_path_buf()),
            "dashboard",
            orion::Options::default(),
        )
        .ok()
    });
    Setup {
        dir,
        socket,
        dashboard,
        _bridge: bridge,
        _service: service,
        _logic: logic,
        _board: board,
    }
}

impl Setup {
    /// Pushes `bytes` to `destination` until the push has an answer from the bridge.
    fn push(&self, destination: &str, bytes: &[u8]) -> orion::files::Transfer {
        let deadline = Instant::now() + WAIT;
        loop {
            let last =
                match self
                    .dashboard
                    .files()
                    .push(destination, &mut Cursor::new(bytes), |_, _| {})
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

    /// What `lemnos.<device>.status` says in `pushes`, once it says anything.
    fn pushes(&self, device: &str) -> String {
        let topic = format!("lemnos.{device}.status");
        let mut subscriber: Subscriber =
            wait_for("the status topic", || self.dashboard.subscribe(&topic).ok());
        wait_for("pushes on the status topic", || {
            let _ = subscriber.wait(Duration::from_millis(50));
            let bytes = subscriber.latest(FieldId(PUSHES))?.payload.to_vec();
            let text = String::from_utf8(bytes).ok()?;
            (!text.is_empty()).then_some(text)
        })
    }
}

/// Every file name under `dir`, at any depth.
fn all_names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        out.push(entry.file_name().to_string_lossy().into_owned());
        if entry.path().is_dir() {
            out.extend(all_names(&entry.path()));
        }
    }
    out
}

#[test]
fn a_push_to_a_device_with_no_place_is_refused_with_why_and_kept_nowhere() {
    let s = setup("refused", &[]);
    // The topic says it once the bridge's receivers are Orion's, so a push from now on reaches it.
    assert_eq!(s.pushes("slots"), pushes::refused_text("slots"));
    assert_eq!(s.pushes("flash"), pushes::refused_text("flash"));
    let sent = s.push("lemnos/slots/b.tar", b"a bundle that must go nowhere");
    assert_eq!(sent.state, FileState::Rejected);
    assert_eq!(sent.detail, pushes::refused_why("slots"));
    assert!(
        sent.detail.contains("LEMNOS_ORION_FILES"),
        "{}",
        sent.detail
    );
    let flash = s.push("lemnos/flash/image.img", b"an image");
    assert_eq!(flash.state, FileState::Rejected);
    assert_eq!(flash.detail, pushes::refused_why("flash"));
    // Not in the node's store, and not in the device's directory.
    for name in all_names(&s.dir) {
        assert!(
            !name.contains("b.tar") && !name.contains("image.img"),
            "{name} was kept"
        );
    }
}

#[test]
fn the_status_topic_says_whether_pushes_are_taken() {
    let s = setup("status", &["slots"]);
    assert_eq!(s.pushes("slots"), pushes::ACCEPTED);
    assert_eq!(s.pushes("flash"), pushes::refused_text("flash"));
}

#[test]
fn a_device_given_a_place_takes_its_file_as_before() {
    let s = setup("placed", &["slots"]);
    assert_eq!(s.pushes("slots"), pushes::ACCEPTED);
    let sent = s.push("lemnos/slots/b.tar", b"bytes");
    assert_eq!(sent.state, FileState::Committed, "{}", sent.detail);
    assert!(s.dir.join("data/slots/.incoming/b.tar").exists());
}

#[test]
fn the_bridge_names_each_device_with_no_place_once() {
    let s = setup("named", &["slots"]);
    let client = Client::connect(&s.socket, "test").expect("lemnosd");
    let config = config(
        &s.socket,
        Path::new("/nowhere"),
        vec![("slots".to_owned(), s.dir.join("data/slots/.incoming"))],
    );
    let unplaced = pushes::unplaced(&client, &config).expect("listed");
    assert_eq!(unplaced, ["flash"]);
    let notices = pushes::notices(&unplaced);
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].contains("flash takes no pushes"),
        "{}",
        notices[0]
    );
    let none = pushes::unplaced(&client, &config_with(&s, &["slots", "flash"])).expect("listed");
    assert!(none.is_empty(), "{none:?}");
}

fn config_with(s: &Setup, placed: &[&str]) -> Config {
    config(
        &s.socket,
        Path::new("/nowhere"),
        placed
            .iter()
            .map(|d| ((*d).to_owned(), s.dir.join("data/slots/.incoming")))
            .collect(),
    )
}
