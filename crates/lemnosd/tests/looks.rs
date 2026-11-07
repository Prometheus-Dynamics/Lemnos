//! Named and inline looks: the precedence of the sources, reloads (an edit
//! takes effect; a broken edit keeps the look it had), saving, and the
//! service's answers to a look it refuses or shows.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{ClientError, ClientOptions, LookSpec, LooksOp};
use lemnos_light::{Block, Rgbw};
use lemnosd::looks::LookTable;
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnosd-looks-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn board_with(looks: &str) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

{looks}
"#
    ))
    .unwrap()
}

fn fill_of(table: &LookTable, name: &str) -> Option<Rgbw> {
    match table.get(name)?.layers[0]?.block {
        Block::Fill { color } => Some(color),
        _ => None,
    }
}

const RED: &str = "layers = [{ block = \"fill\", color = \"ff0000\" }]\n";
const GREEN: &str = "layers = [{ block = \"fill\", color = \"00ff00\" }]\n";
const BLUE: &str = "layers = [{ block = \"fill\", color = \"0000ff\" }]\n";

#[test]
fn board_then_look_dir_then_writable_directory_in_that_order() {
    let root = scratch("precedence");
    let dir = root.join("looks.d");
    let writable = root.join("override");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&writable).unwrap();
    let board = board_with(&format!(
        "[looks.\"status.ok\"]\n{RED}\n[looks.\"app.only\"]\n{RED}"
    ));
    // The board's look overrides a built-in, and the files override the board.
    let mut table = LookTable::new(&board, Some(&dir), Some(&writable));
    assert_eq!(fill_of(&table, "status.ok"), Some(Rgbw::rgb(0xff0000)));
    assert!(table.list().contains("status.ok") && table.list().contains("board.toml"));

    fs::write(
        dir.join("a.toml"),
        format!("[looks.\"status.ok\"]\n{GREEN}"),
    )
    .unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "status.ok"), Some(Rgbw::rgb(0x00ff00)));

    fs::write(
        writable.join("z.toml"),
        format!("[looks.\"status.ok\"]\n{BLUE}"),
    )
    .unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "status.ok"), Some(Rgbw::rgb(0x0000ff)));
    // Within a directory, the later file wins a name.
    fs::write(dir.join("b.toml"), format!("[looks.\"app.only\"]\n{GREEN}")).unwrap();
    fs::write(dir.join("c.toml"), format!("[looks.\"app.only\"]\n{BLUE}")).unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "app.only"), Some(Rgbw::rgb(0x0000ff)));

    // Removing the writable file and the directory file reveals the board's.
    fs::remove_file(writable.join("z.toml")).unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "status.ok"), Some(Rgbw::rgb(0x00ff00)));
    fs::remove_file(dir.join("a.toml")).unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "status.ok"), Some(Rgbw::rgb(0xff0000)));
    assert!(table.knows("status.off"), "a built-in is always known");
    assert!(!table.knows("nothing.here"));
}

#[test]
fn an_edit_takes_effect_and_a_broken_edit_keeps_the_last_good_look() {
    let root = scratch("reload");
    let dir = root.join("looks.d");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("app.toml");
    fs::write(&file, format!("[looks.\"app.x\"]\n{RED}")).unwrap();
    let mut table = LookTable::new(&board_with(""), Some(&dir), None);
    assert_eq!(fill_of(&table, "app.x"), Some(Rgbw::rgb(0xff0000)));
    // An unchanged file is not read again.
    assert!(!table.scan());

    // A good edit (a different length, so the stamp moves) takes effect.
    fs::write(&file, format!("[looks.\"app.x\"]\n{GREEN}# longer\n")).unwrap();
    assert!(table.scan());
    assert_eq!(fill_of(&table, "app.x"), Some(Rgbw::rgb(0x00ff00)));

    // A broken edit is reported and the look stays as it was.
    fs::write(
        &file,
        "[looks.\"app.x\"]\nlayers = [{ block = \"nope\" }]\n",
    )
    .unwrap();
    assert!(table.scan());
    assert_eq!(fill_of(&table, "app.x"), Some(Rgbw::rgb(0x00ff00)));
    let report = table.reload();
    assert!(report.contains("error:"), "{report}");
    assert!(report.contains("looks.app.x.layers[0].block"), "{report}");
    assert!(table.list().contains("error:"));

    // Fixing it loads again; deleting the file drops its looks.
    fs::write(&file, format!("[looks.\"app.x\"]\n{BLUE}")).unwrap();
    table.scan();
    assert_eq!(fill_of(&table, "app.x"), Some(Rgbw::rgb(0x0000ff)));
    fs::remove_file(&file).unwrap();
    table.scan();
    assert_eq!(table.get("app.x"), None);
}

#[test]
fn save_writes_the_writable_directory_and_refuses_what_is_not_a_look() {
    let root = scratch("save");
    let writable = root.join("override");
    let mut table = LookTable::new(&board_with(""), None, Some(&writable));
    let text = format!("[looks.\"app.saved\"]\n{GREEN}");
    let saved = table.save("app.saved", &text).unwrap();
    assert!(saved.contains("app.saved.toml"), "{saved}");
    assert_eq!(fill_of(&table, "app.saved"), Some(Rgbw::rgb(0x00ff00)));
    assert!(writable.join("app.saved.toml").exists());

    // The text must define exactly that look.
    assert!(table.save("app.other", &text).is_err());
    assert!(table.save("Bad Name", &text).is_err());
    assert!(
        table
            .save("app.bad", "[looks.\"app.bad\"]\nlayers = []\n")
            .unwrap_err()
            .contains("layers")
    );
    // No writable directory: refused.
    let mut readonly = LookTable::new(&board_with(""), None, None);
    assert!(
        readonly
            .save("app.saved", &text)
            .unwrap_err()
            .contains("no writable looks directory")
    );
    // The show of a built-in and of an unknown name.
    let defaults = lemnos_light::Defaults::default();
    assert!(
        table
            .show("status.busy", &defaults)
            .unwrap()
            .contains("[looks.\"status.busy\"]")
    );
    assert!(
        table
            .show("nope", &defaults)
            .unwrap_err()
            .contains("unknown look")
    );
}

// ---- the service: refusals, inline and named looks, and a reload ----

struct SysBuses(PathBuf);

impl Buses for SysBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        Err(BoardError::Device {
            device: format!("i2c-{bus}"),
            kind: lemnos_hal::ErrorKind::NotFound,
            reason: "no I2C in this test".into(),
        })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }
}

fn ring_board(root: &Path) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = {{ count = 4, fade_ms = 0, status_effect = "solid", ok = 0x00ff00, look_brightness = 1.0 }}
"#,
        root = root.display()
    ))
    .unwrap()
}

/// The ring's first LED as written (`r | g << 8 | b << 16 | w << 24`).
fn first(root: &Path) -> Option<[u8; 4]> {
    let bytes = fs::read(root.join("dev/leds0")).ok()?;
    bytes.get(..4).map(|b| [b[0], b[1], b[2], b[3]])
}

/// Waits until `check` holds; on a timeout, says what the ring shows.
fn eventually(root: &Path, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "timed out; the ring is {:?}",
            fs::read(root.join("dev/leds0"))
                .map(|b| b.chunks(4).map(<[u8]>::to_vec).collect::<Vec<_>>())
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_service_refuses_unknown_and_invalid_looks_and_shows_named_and_inline_ones() {
    let root = scratch("service");
    fs::create_dir_all(root.join("dev")).unwrap();
    fs::write(root.join("dev/leds0"), "").unwrap();
    let looks_dir = root.join("looks.d");
    fs::create_dir_all(&looks_dir).unwrap();
    fs::write(
        looks_dir.join("app.toml"),
        format!("[looks.\"app.blue\"]\n{BLUE}"),
    )
    .unwrap();
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket, thread_looks) = (
        Arc::clone(&stop),
        root.clone(),
        socket.clone(),
        looks_dir.clone(),
    );
    let handle = std::thread::spawn(move || {
        let mut config = ServiceConfig::new(ring_board(&thread_root), thread_socket);
        config.looks_dir = Some(thread_looks);
        config.looks_override_dir = None;
        let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();

    let mut ctl = ClientOptions::new(&socket, "app")
        .keep_intents()
        .leds()
        .unwrap();

    // An unknown name is refused with the reason, and the ring keeps its look.
    ctl.status(lemnos_ipc::LedStatus::Ok).unwrap();
    ctl.sync().unwrap();
    // A status fill is capped at 0.7 of full (178 of 255).
    eventually(&root, || first(&root) == Some([0, 178, 0, 0]));
    match ctl.look("nothing.here") {
        Err(ClientError::Rejected(reason)) => {
            assert!(reason.contains("unknown look"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(first(&root), Some([0, 178, 0, 0]));

    // Named looks are App-layer looks, like colours: a client's own status
    // (the Status layer) sits above them, so the status goes first.
    ctl.clear().unwrap();
    ctl.sync().unwrap();
    // A look from a file, by name: it shows.
    ctl.look("app.blue").unwrap();
    eventually(&root, || first(&root) == Some([0, 0, 255, 0]));

    // An invalid inline look is refused with the reason.
    let bad = LookSpec::comet(Rgbw::rgb(0xff0000), 1_600, 3_000, 3, 0);
    match ctl.show_spec(&bad) {
        Err(ClientError::Rejected(reason)) => {
            assert_eq!(reason, "invalid look: a comet's heads must be 1 or 2");
        }
        other => panic!("{other:?}"),
    }

    // A valid inline look shows over the named one (the same layer: the most
    // recent wins), and it renders like the same look named.
    let inline = LookSpec::fill(Rgbw::rgb(0xff0000));
    ctl.show_spec(&inline).unwrap();
    eventually(&root, || first(&root) == Some([255, 0, 0, 0]));

    // Named over inline again: the later request wins.
    ctl.look("app.blue").unwrap();
    eventually(&root, || first(&root) == Some([0, 0, 255, 0]));

    // The look table answers a list and a show.
    let listed = ctl.looks(LooksOp::List).unwrap();
    assert!(
        listed.contains("app.blue") && listed.contains("status.busy"),
        "{listed}"
    );
    let shown = ctl.looks(LooksOp::Show("app.blue".into())).unwrap();
    assert!(shown.contains("[looks.\"app.blue\"]"), "{shown}");
    assert!(matches!(
        ctl.looks(LooksOp::Show("nope".into())),
        Err(ClientError::Rejected(_))
    ));

    // A named look with no file and no built-in is refused: the system looks
    // are look files now (the example set).
    assert!(matches!(
        ctl.look("system.writing"),
        Err(ClientError::Rejected(_))
    ));

    drop(ctl);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

/// The service's look files and update status are watched (inotify): a new look file is
/// known, and an update failure shows, well within the one-second poll they replaced.
#[test]
fn the_service_sees_a_new_look_file_and_an_update_failure_at_once() {
    let root = scratch("watched");
    fs::create_dir_all(root.join("dev")).unwrap();
    fs::write(root.join("dev/leds0"), "").unwrap();
    let looks_dir = root.join("looks.d");
    fs::create_dir_all(&looks_dir).unwrap();
    // The system states' looks (the failed update's), as on a device.
    fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/looks/looks.d/system.toml"
        ),
        looks_dir.join("system.toml"),
    )
    .unwrap();
    let status = root.join("run/board/update.json");
    fs::create_dir_all(status.parent().unwrap()).unwrap();
    fs::write(&status, r#"{"state": "idle"}"#).unwrap();
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket, thread_looks, thread_status) = (
        Arc::clone(&stop),
        root.clone(),
        socket.clone(),
        looks_dir.clone(),
        status.clone(),
    );
    let handle = std::thread::spawn(move || {
        let mut config = ServiceConfig::new(ring_board(&thread_root), thread_socket);
        config.looks_dir = Some(thread_looks);
        // Not there yet: watched through its parent once it is made.
        config.looks_override_dir = Some(thread_root.join("var/looks.d"));
        config.update_status = Some(thread_status);
        config.booting_ms = None;
        let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let mut ctl = ClientOptions::new(&socket, "app")
        .keep_intents()
        .leds()
        .unwrap();
    ctl.status(lemnos_ipc::LedStatus::Ok).unwrap();
    ctl.sync().unwrap();
    eventually(&root, || first(&root) == Some([0, 178, 0, 0]));

    // A look file written (whole, by a rename) is known at once.
    let known_within = |ctl: &mut lemnos_ipc::LedClient, name: &str| {
        let start = Instant::now();
        while ctl.look(name).is_err() {
            assert!(
                start.elapsed() < Duration::from_millis(400),
                "{name} unknown after {:?}",
                start.elapsed()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        ctl.clear().unwrap();
        ctl.status(lemnos_ipc::LedStatus::Ok).unwrap();
        ctl.sync().unwrap();
    };
    fs::write(root.join("new.tmp"), format!("[looks.\"app.new\"]\n{RED}")).unwrap();
    fs::rename(root.join("new.tmp"), looks_dir.join("new.toml")).unwrap();
    known_within(&mut ctl, "app.new");
    // In the writable directory, made after the service started.
    fs::create_dir_all(root.join("var/looks.d")).unwrap();
    fs::write(
        root.join("var/looks.d/later.toml"),
        format!("[looks.\"app.later\"]\n{BLUE}"),
    )
    .unwrap();
    known_within(&mut ctl, "app.later");

    // The updater reports a failure: the ring leaves the status green at once.
    eventually(&root, || first(&root) == Some([0, 178, 0, 0]));
    fs::write(
        root.join("status.tmp"),
        r#"{"state": "error", "error": "no space"}"#,
    )
    .unwrap();
    fs::rename(root.join("status.tmp"), &status).unwrap();
    let start = Instant::now();
    eventually(&root, || first(&root) != Some([0, 178, 0, 0]));
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "the failure showed after {:?}",
        start.elapsed()
    );

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(&root);
}
