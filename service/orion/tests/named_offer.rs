//! The named files offered to Orion: `serve` and the restart loop over fakes, and the bridge binary
//! end to end over a real Orion node, board stub and lemnosd with the `named-files` device.

use std::cell::{Cell, RefCell};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_driver_named_files::NamedFilesDriver;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::backoff::Backoff;
use lemnos_orion::named::{Asker, Ended, Node, Snapshot, digest, keep_offering, serve};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::files::{FileState, Giving, Known};
use orion::{Address, Options, Orion};
use orion_node_app::{Node as NodeApp, NodeSettings};
use orion_stubs::Board;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(20);

// ---- fakes: a node and an asked file -------------------------------------------------------------

#[derive(Default)]
struct NodeState {
    files: Vec<(String, Vec<u8>)>,
    open: Vec<(u64, Vec<u8>)>,
    next: u64,
    reads: u32,
    /// The read with this number (counting from 1) fails: a node that went away.
    fail_read: Option<u32>,
    /// `open` refuses with this reason.
    refuse_open: Option<String>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<NodeState>>);

impl Node for Fake {
    fn open(&mut self, name: &str) -> Result<u64, String> {
        let mut s = self.0.borrow_mut();
        if let Some(why) = s.refuse_open.clone() {
            return Err(why);
        }
        let bytes = s
            .files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| "NOT_FOUND".to_string())?;
        s.next += 1;
        let handle = s.next;
        s.open.push((handle, bytes));
        Ok(handle)
    }

    fn size(&mut self, handle: u64) -> Result<u64, String> {
        let s = self.0.borrow();
        s.open
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, b)| b.len() as u64)
            .ok_or_else(|| "NOT_FOUND".to_string())
    }

    fn read(&mut self, handle: u64, from: u64, into: &mut [u8]) -> Result<usize, String> {
        let mut s = self.0.borrow_mut();
        s.reads += 1;
        if s.fail_read == Some(s.reads) {
            return Err("lemnosd is not there".into());
        }
        let bytes = s
            .open
            .iter()
            .find(|(h, _)| *h == handle)
            .ok_or_else(|| "NOT_FOUND".to_string())?
            .1
            .clone();
        if from as usize >= bytes.len() {
            return Ok(0);
        }
        let n = into.len().min(bytes.len() - from as usize);
        into[..n].copy_from_slice(&bytes[from as usize..from as usize + n]);
        Ok(n)
    }

    fn close(&mut self, handle: u64) -> Result<(), String> {
        self.0.borrow_mut().open.retain(|(h, _)| *h != handle);
        Ok(())
    }
}

/// What the fake client was given, and why it was refused.
#[derive(Default)]
struct Client {
    refused: Option<String>,
    received: Vec<u8>,
    digest: Option<Known>,
}

/// An asked file with the client's offset. `give` reads the snapshot from the offset as the client
/// would, into `client`, so the bytes it got can be compared.
struct FakeAsk {
    name: String,
    resume: u64,
    client: Rc<RefCell<Client>>,
}

impl Asker for FakeAsk {
    fn name(&self) -> &str {
        &self.name
    }

    fn resume_at(&self) -> u64 {
        self.resume
    }

    fn refuse(self, why: &str) -> Result<(), String> {
        self.client.borrow_mut().refused = Some(why.to_owned());
        Ok(())
    }

    fn give(self, mut source: Snapshot, known: Known) -> Result<Giving, String> {
        self.client.borrow_mut().digest = Some(known);
        source
            .seek(SeekFrom::Start(self.resume))
            .map_err(|e| format!("{e}"))?;
        let mut got = Vec::new();
        source.read_to_end(&mut got).map_err(|e| format!("{e}"))?;
        self.client.borrow_mut().received = got;
        Ok(Giving::Done {
            taken: true,
            detail: String::new(),
        })
    }
}

fn content(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i % 251) as u8).collect()
}

fn node_with(name: &str, bytes: &[u8]) -> Fake {
    let fake = Fake::default();
    fake.0
        .borrow_mut()
        .files
        .push((name.to_string(), bytes.to_vec()));
    fake
}

fn no_pump() -> Result<(), String> {
    Ok(())
}

fn asked(name: &str, resume: u64) -> (FakeAsk, Rc<RefCell<Client>>) {
    let client = Rc::new(RefCell::new(Client::default()));
    (
        FakeAsk {
            name: name.to_owned(),
            resume,
            client: client.clone(),
        },
        client,
    )
}

#[test]
fn a_named_file_is_given_byte_for_byte_with_its_digest() {
    let bytes = content(100_000);
    let fake = node_with("lemnos/tree/overlay.dts", &bytes);
    let (ask, client) = asked("lemnos/tree/overlay.dts", 0);
    let ended = serve(
        ask,
        || Ok(Box::new(fake.clone()) as Box<dyn Node>),
        &mut no_pump,
    );
    assert!(
        matches!(ended, Ended::Gave(Giving::Done { taken: true, .. })),
        "{ended:?}"
    );
    let client = client.borrow();
    assert_eq!(client.received, bytes);
    let want: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(
        client.digest,
        Some(Known {
            digest: want,
            size: bytes.len() as u64
        })
    );
    assert!(
        fake.0.borrow().open.is_empty(),
        "the snapshot is closed once given"
    );
}

#[test]
fn a_transfer_resumes_from_the_clients_offset() {
    let bytes = content(100_000);
    let fake = node_with("lemnos/tree/overlay.dts", &bytes);
    let (ask, client) = asked("lemnos/tree/overlay.dts", 40_000);
    let ended = serve(
        ask,
        || Ok(Box::new(fake.clone()) as Box<dyn Node>),
        &mut no_pump,
    );
    assert!(matches!(ended, Ended::Gave(_)), "{ended:?}");
    assert_eq!(client.borrow().received, bytes[40_000..]);
}

#[test]
fn a_name_outside_lemnos_is_refused_and_no_node_is_reached() {
    let reached = Cell::new(0u32);
    let (ask, client) = asked("system/kernel-log", 0);
    let ended = serve(
        ask,
        || {
            reached.set(reached.get() + 1);
            Err("reached".to_string())
        },
        &mut no_pump,
    );
    assert_eq!(reached.get(), 0, "no node was reached");
    let why = match ended {
        Ended::Refused(why) => why,
        other => panic!("refused expected, got {other:?}"),
    };
    assert!(why.contains("not a `lemnos/` name"), "{why}");
    assert_eq!(client.borrow().refused.as_deref(), Some(why.as_str()));
}

#[test]
fn an_unknown_name_is_refused_with_the_nodes_reason() {
    let fake = node_with("lemnos/boots", b"x");
    let (ask, client) = asked("lemnos/nope", 0);
    let ended = serve(
        ask,
        || Ok(Box::new(fake.clone()) as Box<dyn Node>),
        &mut no_pump,
    );
    assert!(
        matches!(&ended, Ended::Refused(why) if why == "NOT_FOUND"),
        "{ended:?}"
    );
    assert_eq!(client.borrow().refused.as_deref(), Some("NOT_FOUND"));
    assert!(client.borrow().received.is_empty());
}

#[test]
fn a_node_that_cannot_be_reached_is_refused_not_waited_for() {
    let (ask, client) = asked("lemnos/boots", 0);
    let ended = serve(
        ask,
        || Err("lemnosd is not there".to_string()),
        &mut no_pump,
    );
    assert!(
        matches!(&ended, Ended::Refused(why) if why == "lemnosd is not there"),
        "{ended:?}"
    );
    assert_eq!(
        client.borrow().refused.as_deref(),
        Some("lemnosd is not there")
    );
}

#[test]
fn a_node_gone_during_the_digest_is_refused_and_returns() {
    let fake = node_with("lemnos/tree/overlay.dts", &content(100_000));
    // The second read of the digest pass fails.
    fake.0.borrow_mut().fail_read = Some(2);
    let (ask, client) = asked("lemnos/tree/overlay.dts", 0);
    let ended = serve(
        ask,
        || Ok(Box::new(fake.clone()) as Box<dyn Node>),
        &mut no_pump,
    );
    assert!(matches!(ended, Ended::Refused(_)), "{ended:?}");
    assert!(
        client
            .borrow()
            .refused
            .as_deref()
            .is_some_and(|w| w.contains("lemnosd is not there"))
    );
    assert!(client.borrow().received.is_empty());
}

#[test]
fn a_node_gone_during_the_gift_breaks_the_transfer_and_returns() {
    let bytes = content(100_000);
    let fake = node_with("lemnos/tree/overlay.dts", &bytes);
    // The digest makes four reads of this file; the fifth read is the gift's, and it fails.
    fake.0.borrow_mut().fail_read = Some(5);
    let (ask, _client) = asked("lemnos/tree/overlay.dts", 0);
    let ended = serve(
        ask,
        || Ok(Box::new(fake.clone()) as Box<dyn Node>),
        &mut no_pump,
    );
    assert!(
        matches!(&ended, Ended::Broke(why) if why.contains("lemnosd is not there")),
        "{ended:?}"
    );
}

#[test]
fn the_digest_pass_pumps_the_session_before_each_read() {
    let bytes = content(100_000);
    let fake = node_with("lemnos/tree/overlay.dts", &bytes);
    let mut snapshot = Snapshot::open(Box::new(fake.clone()), "lemnos/tree/overlay.dts").unwrap();
    let pumps = Cell::new(0u32);
    let mut pump = || {
        pumps.set(pumps.get() + 1);
        Ok(())
    };
    let known = digest(&mut snapshot, &mut pump).unwrap();
    assert_eq!(known.size, bytes.len() as u64);
    // Four data reads of this file, and a pump before each read: the fifth call ends the pass.
    assert_eq!(pumps.get(), 5);
}

#[test]
fn the_loop_restarts_after_an_offer_ends_and_never_waits_more_than_8_seconds() {
    // Attempts 1 and 3 to 7 fail to connect or lose their offer; attempt 2 has an offer in place
    // before it ends, so the backoff starts again at 1 s for the attempt after it.
    let attempts = Cell::new(0u32);
    let sleeps = RefCell::new(Vec::new());
    let mut backoff = Backoff::new();
    keep_offering(
        &mut backoff,
        |backoff| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 2 {
                backoff.reset();
            }
            Err(format!("attempt {} ended", attempts.get()))
        },
        |delay| sleeps.borrow_mut().push(delay.as_secs()),
        || attempts.get() >= 7,
        |_| {},
    );
    assert_eq!(attempts.get(), 7);
    let sleeps = sleeps.into_inner();
    assert_eq!(sleeps, [1, 1, 2, 4, 8, 8, 8]);
    assert!(sleeps.iter().all(|s| *s <= 8));
}

// ---- end to end: the bridge binary, a real Orion node, lemnosd with the named-files device --------

static NEXT: AtomicU32 = AtomicU32::new(0);

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

/// A scratch directory under TMPDIR. Socket paths must stay under 108 bytes, so the name is short.
fn scratch(test: &str) -> Scratch {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("no-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    Scratch(dir)
}

/// The Orion node in-process, as the file tests run it.
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
                let mut node = NodeApp::load(settings).expect("node logic starts");
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

/// The bridge binary, killed when the test ends (its own PID only).
struct Bridge(Child);

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Everything up: Orion's node and board, lemnosd with `named-files` serving `tree/overlay.dts` (the
/// `overlay` bytes), the bridge binary offering `lemnos/`, and a dashboard that fetches from Orion.
struct Setup {
    dashboard: Orion,
    _bridge: Bridge,
    _service: Service,
    _logic: Logic,
    _board: Board,
    _dir: Scratch,
}

fn setup(test: &str, overlay: &Path) -> Setup {
    let dir = scratch(test);
    let socket = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let logic = Logic::start(&dir);
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [host]\ncall_deadline_ms = 30000\n\
         [[devices]]\nid = \"files\"\ndriver = \"named-files\"\n\
         config = {{ data = \"{d}/data\", boot = \"{d}/boot\", sys_block = \"{d}/sys\", module = \"{d}/module\", tree_overlay = \"{o}\" }}\n",
        d = dir.display(),
        o = overlay.display()
    );
    let service = Service::start(LemnosdConfig {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(&text).expect("board"),
        drivers: vec![entry::<NamedFilesDriver>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd");
    let stderr = std::fs::File::create(dir.join("bridge.log")).expect("log");
    let child = Command::new(env!("CARGO_BIN_EXE_lemnos-orion"))
        .env("LEMNOSD_SOCKET", &socket)
        .env(
            "ORION_ADDRESS",
            format!("unix:{}", board.socket().display()),
        )
        .env("LEMNOS_ORION_NAMED_FILES", "files")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("the bridge binary starts");
    let dashboard = wait_for("a dashboard session", || {
        Orion::connect_once(
            &Address::Local(board.socket().to_path_buf()),
            "dashboard",
            Options::default(),
        )
        .ok()
    });
    Setup {
        dashboard,
        _bridge: Bridge(child),
        _service: service,
        _logic: logic,
        _board: board,
        _dir: dir,
    }
}

/// The overlay the node serves: 100 KB, so a transfer takes several pieces.
fn overlay_file(dir: &Path) -> PathBuf {
    let path = dir.join("overlay.dts");
    std::fs::write(&path, content(100_000)).expect("overlay");
    path
}

#[test]
fn a_named_file_is_served_end_to_end_with_equal_bytes() {
    let dir = scratch("e2e-served");
    let overlay = overlay_file(&dir);
    let s = setup("e2e-served", &overlay);
    let want = content(100_000);
    // Fetches until the bridge has the offer; the first answer that is not "nobody offers" is taken.
    let got = wait_for("the overlay fetched and committed", || {
        let mut into = Cursor::new(Vec::new());
        let transfer = s
            .dashboard
            .files()
            .fetch("lemnos/tree/overlay.dts", &mut into, 0, |_, _| {})
            .ok()?;
        (transfer.state == FileState::Committed).then(|| into.into_inner())
    });
    assert_eq!(got, want);
}

#[test]
fn a_transfer_resumes_at_an_offset_end_to_end() {
    let dir = scratch("e2e-resume");
    let overlay = overlay_file(&dir);
    let s = setup("e2e-resume", &overlay);
    let want = content(100_000);
    wait_for("the offer is up", || {
        let mut into = Cursor::new(Vec::new());
        let transfer = s
            .dashboard
            .files()
            .fetch("lemnos/tree/overlay.dts", &mut into, 0, |_, _| {})
            .ok()?;
        (transfer.state == FileState::Committed).then_some(())
    });
    // The client has the first 40 000 bytes; the rest is given from there.
    let mut into = Cursor::new(want[..40_000].to_vec());
    let transfer = s
        .dashboard
        .files()
        .fetch("lemnos/tree/overlay.dts", &mut into, 40_000, |_, _| {})
        .expect("the resumed fetch");
    assert_eq!(transfer.state, FileState::Committed, "{transfer:?}");
    assert_eq!(into.into_inner(), want);
}

#[test]
fn an_unknown_name_is_refused_end_to_end_with_its_reason() {
    let dir = scratch("e2e-unknown");
    let overlay = overlay_file(&dir);
    let s = setup("e2e-unknown", &overlay);
    wait_for("the offer is up", || {
        let mut into = Cursor::new(Vec::new());
        let transfer = s
            .dashboard
            .files()
            .fetch("lemnos/tree/overlay.dts", &mut into, 0, |_, _| {})
            .ok()?;
        (transfer.state == FileState::Committed).then_some(())
    });
    let mut into = Cursor::new(Vec::new());
    let transfer = s
        .dashboard
        .files()
        .fetch("lemnos/no-such-file", &mut into, 0, |_, _| {})
        .expect("the refused fetch answers");
    assert_eq!(transfer.state, FileState::Rejected);
    assert!(transfer.detail.contains("no-such-file"), "{transfer:?}");
    assert!(into.into_inner().is_empty());
}
