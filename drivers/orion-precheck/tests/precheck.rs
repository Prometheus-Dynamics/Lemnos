//! The precheck against a real Orion comm core (`orion-stubs`): a fresh node, a node with an enrollment
//! key, a node below the precheck's level, and a node that never answers. The driver is started as the
//! host starts it, with its settings; its answers are read through the contract.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_contract_security_precondition::{SecurityPrecondition, TEXT_MAX};
use lemnos_core::{Config, ConfigValue, Health, Lifecycle};
use lemnos_driver_orion_precheck::{OrionPrecheck, OrionPrecheckNeeds};
use orion_stubs::{Board, Options as BoardOptions};

/// A comm core of Orion's own (`orion-comm-app`, as Orion's precheck test runs it) in a thread of its
/// own, until dropped. Its security switch record says off, so the precheck is asked of a switch that
/// is off, the way the board asks it before a change to on.
struct Core {
    socket: PathBuf,
    kill: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The settings of a comm core in `dir`, with `more` added (as Orion's test writes them).
fn core_settings(dir: &Path, more: &str) -> orion_comm_app::CommSettings {
    let view = dir.join("security");
    std::fs::write(&view, "off\ngeneration 1\n").expect("security record");
    let text = format!(
        "node.name = board\nsocket = {0}/orion.sock\nstate.dir = {0}/state\nkeys.dir = {0}/keys\n\
         recovery.keys = {0}/recovery_keys\n{more}",
        dir.display()
    );
    let mut source = orion_config::Source::parse(&text, Vec::new()).expect("source");
    orion_comm_app::CommSettings::from_source_with_view(&mut source, &view).expect("settings")
}

impl Core {
    fn start(dir: &Path, more: &str) -> Self {
        std::fs::create_dir_all(dir.join("keys")).expect("keys dir");
        let settings = core_settings(dir, more);
        let socket = settings.config.socket.clone();
        let (kill, killed) = std::sync::mpsc::channel::<()>();
        let (started, ready) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                let core = orion_comm_app::CommCore::start(settings)
                    .await
                    .expect("start");
                let _ = started.send(());
                // Waited for off the executor's thread, so the comm core keeps running meanwhile.
                let _ = tokio::task::spawn_blocking(move || killed.recv()).await;
                drop(core);
            });
        });
        ready.recv().expect("the comm core starts");
        Self {
            socket,
            kill: Some(kill),
            thread: Some(thread),
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let (Some(kill), Some(thread)) = (self.kill.take(), self.thread.take()) {
            let _ = kill.send(());
            let _ = thread.join();
        }
    }
}

/// How long a test waits for the worker to connect before it looks at the answers.
const CONNECT_WAIT: Duration = Duration::from_secs(20);

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

fn dir(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("lo-precheck-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    Scratch(dir)
}

/// The driver, started with the comm core at `socket` (its `address` setting).
fn device(socket: &Path) -> OrionPrecheck {
    let address = socket.display().to_string();
    let entries = [("address", ConfigValue::Str(&address))];
    let cfg = Config::new(&entries).on_board("test");
    <OrionPrecheck as Lifecycle>::start(OrionPrecheckNeeds {}, &cfg).expect("the driver starts")
}

/// Waits until the driver's session is up (its health is `Ok`).
fn connected(dev: &OrionPrecheck) {
    let until = Instant::now() + CONNECT_WAIT;
    while dev.health() != Health::Ok {
        assert!(Instant::now() < until, "the driver never connected");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One precheck, its text and how long it took.
fn ask(dev: &mut OrionPrecheck, on: bool) -> (String, Duration) {
    let mut buffer = vec![0u8; TEXT_MAX];
    let started = Instant::now();
    let n = dev
        .precondition(on, &mut buffer)
        .expect("the precondition answers");
    (
        String::from_utf8_lossy(&buffer[..n]).into_owned(),
        started.elapsed(),
    )
}

#[test]
fn a_fresh_node_with_no_operator_says_no_to_on_and_yes_to_off() {
    let dir = dir("fresh");
    let core = Core::start(&dir, "");
    let mut dev = device(&core.socket);
    connected(&dev);
    let (on, _) = ask(&mut dev, true);
    assert!(
        on.lines()
            .any(|line| line.starts_with("missing: orion: nobody but root could get in")),
        "{on}"
    );
    let (off, _) = ask(&mut dev, false);
    assert_eq!(off, "", "Orion has nothing against off");
}

#[test]
fn a_node_with_an_enrollment_key_says_yes_to_on() {
    let dir = dir("enrollment");
    let core = Core::start(&dir, "enrollment.key = shared\n");
    let mut dev = device(&core.socket);
    connected(&dev);
    let (on, _) = ask(&mut dev, true);
    assert_eq!(
        on, "",
        "a node that lets newcomers enrol has nothing against on"
    );
}

/// A real node below the precheck's interface level answers the level, not a yes or a no: the
/// client names the level it needs and the level the node speaks.
#[test]
fn a_node_below_the_precheck_level_says_the_level_needed_and_seen() {
    let dir = dir("level");
    let options = BoardOptions {
        for_node: true,
        level: 4,
        ..BoardOptions::default()
    };
    let board = Board::start_with(&dir, options);
    let mut dev = device(board.socket());
    connected(&dev);
    let (on, _) = ask(&mut dev, true);
    assert_eq!(
        on.trim_end(),
        "missing: orion: the node speaks interface level 4; the precheck needs level 5",
        "{on}"
    );
}

/// A node that accepts the connection and never answers is not a yes: the call times out within its
/// deadline, the driver says it did not answer, and the health is `Degraded` while it keeps trying.
#[test]
fn a_silent_node_is_did_not_answer_within_the_deadline() {
    let dir = dir("silent");
    let socket = dir.join("silent.sock");
    let listener = UnixListener::bind(&socket).expect("the silent node listens");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let mut dev = device(&socket);
    assert!(
        matches!(dev.health(), Health::Degraded(_)),
        "not connected while it is silent"
    );
    let (on, elapsed) = ask(&mut dev, true);
    assert!(
        on.starts_with("missing: orion: it did not answer ("),
        "{on}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "the answer comes within the host's call deadline: {elapsed:?}"
    );
    assert!(matches!(dev.health(), Health::Degraded(_)));
}

/// With no comm core at all, the driver keeps trying on its backoff and answers at once, as not
/// connected, rather than blocking a call on the connect.
#[test]
fn no_comm_core_is_did_not_answer_at_once_and_the_driver_keeps_trying() {
    let dir = dir("absent");
    let mut dev = device(&dir.join("absent.sock"));
    let (on, elapsed) = ask(&mut dev, true);
    assert!(
        on.starts_with("missing: orion: it did not answer ("),
        "{on}"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}
