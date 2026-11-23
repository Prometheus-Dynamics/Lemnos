//! `callers = "users"`: a device only the devices that use it may drive. On the simulated board an
//! LED strip is used by an indicator; a client's call that changes the strip is refused with the
//! reason in words, its reads answer, and the indicator drives it as before.

#[path = "support/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lemnos_client::{Client, Error};
use lemnos_contract_pixels::Pixels;
use lemnos_core::Status;
use lemnos_host::Options;
use lemnos_host::board::{BoardError, parse};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
    _dir: Scratch,
}

fn board_text(strip: &str, rest: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"light\"\ndriver = \"ws2812\"\nbus = \"spidev0.0\"\n{strip}\
         config = {{ count = 8 }}\n{rest}"
    )
}

const INDICATOR: &str =
    "[[devices]]\nid = \"indicator\"\ndriver = \"indicator\"\nuses = { pixels = \"light\" }\n";

fn start(text: &str) -> Running {
    let dir = Scratch(lemnos_socket_dir::scratch_dir().unwrap());
    std::fs::create_dir_all(&*dir).unwrap();
    let socket = lemnos_socket_dir::socket(&dir, "sock").unwrap();
    let board = SimBoard::new();
    board.add_spi("spidev0.0");
    let service = Service::start(Config {
        options: Options {
            call_deadline: Duration::from_millis(500),
            ..Options::default()
        },
        ..Config::new(
            socket.clone(),
            "t".to_string(),
            parse(text).unwrap(),
            lemnosd::builtin::drivers(),
            Box::new(board),
        )
    })
    .unwrap();
    Running {
        _service: service,
        socket,
        _dir: dir,
    }
}

/// A call's answer once the device is bound: a device still binding answers "no device" or "busy".
fn settled<T>(mut call: impl FnMut() -> Result<T, Status>) -> Result<T, Status> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match call() {
            Err(Status::NO_DEVICE | Status::BUSY) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// `pixels/1`'s `show`, which takes nothing and changes the strip.
const SHOW: u16 = 3;

fn show(running: &Running, device: &str) -> String {
    let args = vec![
        "--socket".to_string(),
        running.socket.display().to_string(),
        "show".to_string(),
        device.to_string(),
    ];
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_device_only_its_users_may_drive_refuses_a_clients_change_and_answers_its_reads() {
    let running = start(&board_text("callers = \"users\"\n", INDICATOR));
    let client = Client::connect(&running.socket, "app").unwrap();
    let mut strip = client.open::<dyn Pixels>("light").unwrap();

    // Reading is anyone's.
    assert_eq!(settled(|| strip.pixel_count()).unwrap(), 8);

    // Changing it is not: refused, and told who drives it.
    assert_eq!(strip.set(0, &[1, 2, 3, 0]), Err(Status::NOT_PERMITTED));
    assert_eq!(strip.show(), Err(Status::NOT_PERMITTED));
    let mut raw = client.open_tag("light", "pixels/1").unwrap();
    match client.call(&mut raw, SHOW, &[], &mut []) {
        Err(Error::Refused { status, text }) => {
            assert_eq!(status, Status::NOT_PERMITTED);
            assert_eq!(
                text,
                "light is driven only by the devices that use it (indicator): call one of those"
            );
        }
        other => panic!("a client's change of the strip was not refused: {other:?}"),
    }

    // The way in is the device that uses it: the indicator takes the client's frames, and its
    // own calls to the strip go through.
    let mut layer = client.open::<dyn Pixels>("indicator").unwrap();
    assert_eq!(settled(|| layer.pixel_count()).unwrap(), 8);
    settled(|| layer.set(0, &[1, 2, 3, 0])).unwrap();
    settled(|| layer.show()).unwrap();
    let list = client.list().unwrap();
    for device in ["light", "indicator"] {
        let entry = list.iter().find(|d| d.name == device).unwrap();
        assert_eq!(
            entry.health,
            lemnos_client::DeviceHealth::Ok,
            "{device}: {}",
            entry.reason
        );
    }

    // The description says who may drive it, and the panel shows it.
    let index = list.iter().find(|d| d.name == "light").unwrap().index;
    assert_eq!(client.describe(index).unwrap().driven_by, ["indicator"]);
    let panel = show(&running, "light");
    assert!(panel.contains("\n  driven   only by indicator "), "{panel}");
    let index = list.iter().find(|d| d.name == "indicator").unwrap().index;
    assert!(client.describe(index).unwrap().driven_by.is_empty());
}

#[test]
fn a_device_that_does_not_say_is_driven_by_anyone_as_before() {
    let running = start(&board_text("", INDICATOR));
    let client = Client::connect(&running.socket, "app").unwrap();
    let mut strip = client.open::<dyn Pixels>("light").unwrap();
    settled(|| strip.set(0, &[1, 2, 3, 0])).unwrap();
    settled(|| strip.show()).unwrap();
    let list = client.list().unwrap();
    let index = list.iter().find(|d| d.name == "light").unwrap().index;
    assert!(client.describe(index).unwrap().driven_by.is_empty());
    assert!(!show(&running, "light").contains("driven"));
}

#[test]
fn a_device_only_its_users_may_drive_needs_a_user() {
    let text = board_text("callers = \"users\"\n", "");
    let error = parse(&text).unwrap_err();
    assert!(
        matches!(&error, BoardError::NoUsers(id) if id == "light"),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "device \"light\": `callers = \"users\"` and no device uses it: nothing could ever drive it"
    );
    // With a user it is taken, and `any` is the default spelled out.
    assert!(parse(&board_text("callers = \"users\"\n", INDICATOR)).is_ok());
    assert!(parse(&board_text("callers = \"any\"\n", "")).is_ok());
    assert!(parse(&board_text("callers = \"nobody\"\n", INDICATOR)).is_err());
}
