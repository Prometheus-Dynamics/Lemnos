//! A device the daemon cannot start is refused by its own entry, and the daemon starts without it:
//! a gadget whose identity is wrong, a gadget whose root-phase setup fails, a setting in a form no
//! driver can read, a loop of `uses`. Each one shows failed with its reason, and every other device
//! runs. `lemnos-ctl validate` refuses the board for what it can know without hardware.

use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_host::Options;
use lemnos_host::board::{parse, parse_lenient};
use lemnos_linux::{Mounts, Owner, Roots};
use lemnos_sim::SimBoard;
use lemnosd::gadget;
use lemnosd::privilege;
use lemnosd::{Config, Service};

const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

/// A board file in a directory of its own, validated with `lemnos-ctl validate`. Returns the error.
fn validate(text: &str) -> Result<String, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-start-{}-{}",
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

/// A running daemon, started the way `lemnosd` starts it: the lenient reader, the gadget links
/// resolved, then the service on the simulated board. Stops when dropped.
struct Running {
    _service: Service,
    socket: std::path::PathBuf,
}

fn start(mut board: lemnos_host::board::BoardFile) -> Result<Running, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    gadget::resolve(&mut board);
    let socket = std::env::temp_dir().join(format!(
        "lemnos-start-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let config = Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board,
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

fn ctl(running: &Running, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).map(|()| String::from_utf8(out).unwrap())
}

fn row<'a>(list: &'a str, id: &str) -> &'a str {
    list.lines()
        .find(|row| row.starts_with(&format!("{id} ")))
        .unwrap_or_else(|| panic!("no row for {id}: {list}"))
}

/// A gadget that names an identity device the board does not have, next to a device that runs.
const MISSING_IDENTITY: &str = "[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\n\
    uses = { function = \"udc:udc0\" }\n\
    [devices.config]\nvendor = 4617\nproduct = 1\nidentity = \"nothing\"\ninterfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\"\n\
    [[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n";

#[test]
fn a_gadget_whose_identity_device_is_missing_fails_and_the_daemon_starts_without_it() {
    let text = format!("{HEAD}{MISSING_IDENTITY}");
    let error = validate(&text).unwrap_err();
    assert!(
        error.ends_with(
            "device \"gadget\": `identity` names nothing, which the board file does not have"
        ),
        "validate: {error}"
    );
    let daemon = start(parse_lenient(&text).unwrap()).expect("the daemon starts");
    let list = ctl(&daemon, &["list"]).unwrap();
    let gadget_row = row(&list, "gadget");
    assert!(gadget_row.contains("failed"), "{list}");
    assert!(
        gadget_row.contains("`identity` names nothing, which the board file does not have"),
        "{list}"
    );
    assert!(
        row(&list, "good").contains("running"),
        "good did not run: {list}"
    );
}

/// Roots in a directory of the test's own. `usb_gadget` is a file under configfs when `broken`, so
/// the gadget cannot be made under it; nothing under `/sys` is touched.
fn roots_in(name: &str, broken: bool) -> (std::path::PathBuf, [std::path::PathBuf; 3]) {
    let dir =
        std::env::temp_dir().join(format!("lemnos-start-roots-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (configfs, mounts, udc) = (dir.join("configfs"), dir.join("mounts"), dir.join("udc"));
    std::fs::create_dir_all(&configfs).unwrap();
    std::fs::create_dir_all(&mounts).unwrap();
    std::fs::create_dir_all(udc.join("udc0")).unwrap();
    if broken {
        std::fs::write(configfs.join("usb_gadget"), b"").unwrap();
    }
    (dir, [configfs, mounts, udc])
}

struct NoMounts;

impl Mounts for NoMounts {
    fn functionfs(&mut self, _: &str, _: &std::path::Path, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn unmount(&mut self, _: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }
}

/// A gadget that is valid, whose root-phase setup fails (configfs refuses to make it), next to a
/// device that runs. The root phase returns the gadget refused with its reason; the daemon starts
/// without it.
#[test]
fn a_root_step_that_fails_refuses_its_gadget_and_the_daemon_starts_without_it() {
    let good_identity = format!(
        "{HEAD}[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\n\
        uses = {{ function = \"udc:udc0\" }}\n\
        [devices.config]\nvendor = 4617\nproduct = 1\ninterfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\"\n\
        [[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n"
    );
    let mut board = parse_lenient(&good_identity).unwrap();
    let (dir, [configfs, mounts, udc]) = roots_in("configfs", true);
    let roots = Roots {
        configfs: &configfs,
        mounts: &mounts,
        udc_class: &udc,
    };
    let owner = Owner { uid: 987, gid: 987 };
    let refused = gadget::privileged_start_in(&board, owner, &roots, &mut NoMounts);
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(refused.len(), 1, "{refused:?}");
    privilege::refuse(&mut board, &refused);
    let daemon = start(board).expect("the daemon starts");
    let list = ctl(&daemon, &["list"]).unwrap();
    let gadget_row = row(&list, "gadget");
    assert!(gadget_row.contains("failed"), "{list}");
    assert!(
        gadget_row.contains("needs a restart of the daemon to retry"),
        "{list}"
    );
    assert!(
        row(&list, "good").contains("running"),
        "good did not run: {list}"
    );
}

/// A setting that is a table, not a value a driver can read: the board's own entry is refused, and
/// the rest of the board runs. `validate` refuses it by device and key.
#[test]
fn a_setting_in_no_readable_form_refuses_its_device_and_the_daemon_starts() {
    let text = format!(
        "{HEAD}[[devices]]\nid = \"bad\"\ndriver = \"ws2812\"\n[devices.config]\ncount = 16\nextra = {{ a = 1 }}\n\
         [[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n"
    );
    let error = validate(&text).unwrap_err();
    assert!(
        error.contains("device \"bad\" config key \"extra\""),
        "{error}"
    );
    assert!(parse(&text).is_err());
    let daemon = start(parse_lenient(&text).unwrap()).expect("the daemon starts");
    let list = ctl(&daemon, &["list"]).unwrap();
    assert!(row(&list, "bad").contains("failed"), "{list}");
    assert!(
        row(&list, "good").contains("running"),
        "good did not run: {list}"
    );
}

/// Two devices that use each other: the one that closes the loop is refused with the loop named,
/// and the rest of the board runs.
#[test]
fn a_loop_of_uses_refuses_the_device_that_closes_it_and_the_daemon_starts() {
    let text = format!(
        "{HEAD}[[devices]]\nid = \"a\"\ndriver = \"ws2812\"\nuses = {{ n = \"b\" }}\n[devices.config]\ncount = 16\n\
         [[devices]]\nid = \"b\"\ndriver = \"ws2812\"\nuses = {{ n = \"a\" }}\n[devices.config]\ncount = 16\n\
         [[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n"
    );
    let error = validate(&text).unwrap_err();
    assert!(error.contains("lead back to itself"), "{error}");
    let daemon = start(parse_lenient(&text).unwrap()).expect("the daemon starts");
    let list = ctl(&daemon, &["list"]).unwrap();
    let a = row(&list, "a");
    assert!(
        a.contains("failed") && a.contains("through device \"b\""),
        "{list}"
    );
    assert!(
        row(&list, "good").contains("running"),
        "good did not run: {list}"
    );
}
