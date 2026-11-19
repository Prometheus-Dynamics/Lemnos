//! A device's settings are checked before any device is made, with no hardware: `lemnos-ctl
//! validate` and the daemon's start run the same check, so a board file refused by one is refused
//! by the other, naming the device and the key.

use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

/// Writes `text` to a board file of its own and runs `validate` on it, with no daemon.
fn validate(text: &str) -> Result<String, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-settings-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    let mut out = Vec::new();
    let args = [
        "--socket".to_string(),
        dir.join("nobody").display().to_string(),
        "validate".to_string(),
        path.display().to_string(),
    ];
    let result = lemnos_ctl::run(&args, &mut out).map(|()| String::from_utf8(out).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
    result
}

/// A running daemon on `text`, with the built-in drivers and the simulated board, which makes no
/// hardware. The service stops when it is dropped.
struct Running {
    _service: Service,
    socket: std::path::PathBuf,
}

fn running(text: &str) -> Result<Running, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let socket = std::env::temp_dir().join(format!(
        "lemnos-settings-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let config = Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: lemnosd::builtin::drivers(),
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    };
    let service = Service::start(config).map_err(|error| error.to_string())?;
    Ok(Running {
        _service: service,
        socket,
    })
}

/// Runs the tool against a running daemon.
fn ctl(running: &Running, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).map(|()| String::from_utf8(out).unwrap())
}

/// A board with one device `d` of `driver` and the given `config` lines.
fn device(driver: &str, config: &str) -> String {
    format!("{HEAD}[[devices]]\nid = \"d\"\ndriver = \"{driver}\"\n[devices.config]\n{config}\n")
}

/// Each refusal: the device's driver, its config, and the words the refusal must name. The device
/// and the key are named in every one, and the refusal is the same from `validate` and the start.
const REFUSED: &[(&str, &str, &str)] = &[
    (
        "generic-magnetometer",
        "address = 44\ndescription = \"axis x 0x01 16 little signed 1\"\ndescription_file = \"/etc/lemnos/chips/x.chip\"",
        "device \"d\": setting \"description_file\" is given with description: give one of them, not both",
    ),
    (
        "generic-magnetometer",
        "address = 44\ndescription = \"/etc/lemnos/chips/x.chip\"",
        "device \"d\": setting \"description\" is not a chip description: a file's path goes in description_file",
    ),
    (
        "modbus-magnetometer",
        "address = 7\ndescription = \"/etc/lemnos/chips/x.chip\"",
        "device \"d\": setting \"description\" is not a chip description: a file's path goes in description_file",
    ),
    (
        "modbus-magnetometer",
        "address = 7\ndescription = \"modbus input\\nvalue temperature 0 16 big signed 1\"\ndescription_file = \"/etc/lemnos/chips/x.chip\"",
        "device \"d\": setting \"description_file\" is given with description: give one of them, not both",
    ),
    (
        "generic-magnetometer",
        "address = 44\ndescription_file = \"/nonexistent/lemnos/chip.chip\"",
        // A build with a filesystem names the missing file; one without refuses the key by name.
        // Which build this is depends on the features the workspace unified, so either is accepted.
        "device \"d\": setting \"description_file\" names no file: check the path|device \"d\": setting \"description_file\" needs a build with a filesystem: give the chip's text as description",
    ),
    (
        "usb-gadget",
        "vendor = 4617\nproduct = 1\ninterfaces = \"function ffs\\n\"\ninterfaces_file = \"/etc/lemnos/i.txt\"",
        "device \"d\": setting \"interfaces_file\" is given with interfaces: give one of them, not both",
    ),
    (
        "ws2812",
        "count = 16\nbrightnes = 5",
        "device \"d\": setting \"brightnes\" is not a setting this driver reads (did you mean \"brightness\"?)",
    ),
    (
        "ws2812",
        "count = 16\nzzz = 5",
        "device \"d\": setting \"zzz\" is not a setting this driver reads",
    ),
    (
        "ws2812",
        "count = \"16\"",
        "device \"d\": setting \"count\" must be a whole number",
    ),
    (
        "ws2812",
        "wire = \"rgb\"",
        "device \"d\": setting \"count\" is required",
    ),
];

#[test]
fn a_setting_a_driver_refuses_is_refused_by_validate_and_the_daemon_starts_without_that_device() {
    for (driver, config, named) in REFUSED {
        let text = device(driver, config);
        let error = validate(&text).unwrap_err();
        assert!(
            named.split('|').any(|one| error.ends_with(one)),
            "validate, {driver}: {error}"
        );
        // The daemon starts, and the device is listed failed, with the same reason.
        let daemon = running(&text).unwrap_or_else(|error| panic!("start, {driver}: {error}"));
        let list = ctl(&daemon, &["list"]).unwrap();
        let row = list
            .lines()
            .find(|row| row.starts_with("d "))
            .unwrap_or_else(|| panic!("no row for d: {list}"));
        assert!(row.contains("retrying  failed"), "{driver}: {row}");
        assert!(
            named.split('|').any(|one| row.contains(one)),
            "start, {driver}: {row}"
        );
    }
}

#[test]
fn a_board_whose_settings_are_right_validates_and_starts() {
    let text = device("ws2812", "count = 16\nbrightness = 128");
    validate(&text).expect("validate accepts the settings");
    let daemon = running(&text).expect("the start accepts the settings");
    let list = ctl(&daemon, &["list"]).unwrap();
    assert!(!list.contains("setting"), "{list}");
}

/// One misspelled key on one device: validate refuses the board by device and key; the daemon
/// starts, the other device runs, and the bad one is listed failed with the key and the reason.
#[test]
fn one_misspelled_key_fails_one_device_and_the_rest_of_the_board_runs() {
    let text = format!(
        "{HEAD}[[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n\
         [[devices]]\nid = \"bad\"\ndriver = \"ws2812\"\n[devices.config]\ncount = 16\nbrightnes = 5\n"
    );
    let error = validate(&text).unwrap_err();
    assert!(
        error.ends_with(
            "device \"bad\": setting \"brightnes\" is not a setting this driver reads (did you mean \"brightness\"?)"
        ),
        "{error}"
    );
    let daemon = running(&text).expect("the daemon starts");
    let list = ctl(&daemon, &["list"]).unwrap();
    let good = list.lines().find(|row| row.starts_with("good ")).unwrap();
    assert!(good.contains("running"), "good device did not run: {list}");
    let bad = list.lines().find(|row| row.starts_with("bad ")).unwrap();
    assert!(bad.contains("retrying  failed"), "{list}");
    assert!(
        bad.contains("device \"bad\": setting \"brightnes\" is not a setting this driver reads"),
        "{list}"
    );
}

#[test]
fn a_refused_setting_is_named_in_the_state_of_its_device_only() {
    let text = format!(
        "{HEAD}[[devices]]\nid = \"good\"\ndriver = \"ws2812\"\n[devices.config]\ncount = 16\n\
         [[devices]]\nid = \"bad\"\ndriver = \"ws2812\"\n[devices.config]\ncount = 0\n"
    );
    let error = validate(&text).unwrap_err();
    assert!(
        error.ends_with("device \"bad\": setting \"count\" must be a whole number from 1 to 65535"),
        "{error}"
    );
    assert!(!error.contains("\"good\""), "{error}");
    let daemon = running(&text).unwrap();
    let list = ctl(&daemon, &["list"]).unwrap();
    assert!(
        !list
            .lines()
            .find(|r| r.starts_with("good "))
            .unwrap()
            .contains("setting")
    );
    assert!(
        list.lines()
            .find(|r| r.starts_with("bad "))
            .unwrap()
            .contains("must be a whole number")
    );
}

/// A gadget with an `identity` device, the link named under `place` (`uses` or `config`).
fn gadget_board(place: &str) -> String {
    let (uses, config) = match place {
        "uses" => (", identity = \"ident\"", ""),
        _ => ("", ", identity = \"ident\""),
    };
    format!(
        "{HEAD}[[devices]]\nid = \"ident\"\ndriver = \"system-identity\"\nconfig = {{ serial = \"device-tree\", serial_path = \"/nonexistent/serial-number\" }}\n\
         [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc:udc0\"{uses} }}\nconfig = {{ vendor = 4617, product = 1{config} }}\n"
    )
}

#[test]
fn an_identity_under_uses_is_refused_by_validate_by_name_and_in_config_it_is_accepted() {
    let error = validate(&gadget_board("uses")).unwrap_err();
    assert!(
        error.ends_with(
            "device \"gadget\": `identity` is a setting, not a resource: put it in the device's `config`"
        ),
        "{error}"
    );
    validate(&gadget_board("config")).expect("the same board with identity in config validates");
}
