//! The root phase of the daemon for USB gadgets: makes every gadget the board file describes in
//! configfs, and mounts its FunctionFS for the daemon's user, before the daemon drops to that user.
//!
//! configfs and mounts need root. [`privileged_step`] is the `usb gadget` entry of the daemon's
//! privileged start (`STEPS` in `privilege.rs`): it runs as root, before the drop, with the parsed
//! board file and the uid and gid the daemon runs as after the drop. It does what
//! `lemnos_linux::prepare_gadget` does: the tree, the mount, and the chown of each `UDC` attribute
//! to the daemon's owner. Those are the only things the unprivileged daemon writes in configfs.
//! Its open descriptors are not needed after the drop: configfs and FunctionFS check permissions
//! when a file is opened, and the chown makes the file the daemon's, so it opens `UDC` as `lemnos`
//! after the drop.
//!
//! The board's serial is read once, by [`resolve`], before the root phase: the gadget's `identity`
//! names the identity device whose serial source it is, and the result is written into the
//! gadget's settings as `board_serial` (or `board_serial_error`, with the reason). The root phase
//! and the daemon's driver both read the same settings, through `settings_for`, so the tree and the
//! descriptors, and the NCM addresses derived from the serial, agree (`docs/boards.md`).

use std::path::Path;

use crate::privilege::{Context, Refusals};
use lemnos_core::{Config, ConfigValue};
use lemnos_driver_usb_gadget::description::{Functions, Set};
use lemnos_host::board::{BoardFile, DeviceSpec};
use lemnos_linux::{Mounts, Owner, Roots, SystemMounts, prepare_gadget};

/// The driver a gadget device names in the board file.
const DRIVER: &str = "usb-gadget";
/// The driver of the device that serves the board's identity, whose serial a gadget may name.
const IDENTITY_DRIVER: &str = "system-identity";
/// The gadget's setting that names the identity device.
const IDENTITY: &str = "identity";
/// The settings `resolve` sets: the board serial, or the reason it could not be read. A board file
/// that sets either is refused.
const BOARD_SERIAL: &str = "board_serial";
const BOARD_SERIAL_ERROR: &str = "board_serial_error";
/// Where configfs, FunctionFS mount points and the controllers are, on a Linux system.
const CONFIGFS: &str = "/sys/kernel/config";
const MOUNTS: &str = "/dev/usb-ffs";
const UDC_CLASS: &str = "/sys/class/udc";
/// Said of a gadget refused at start: the daemon reads the board file and the root phase once, so
/// the fix is a board-file edit or a restart, never a retry in place.
const RESTART: &str = "needs a restart of the daemon to retry";

/// The root phase's step for USB gadgets (`STEPS` in `privilege.rs`): the gadgets the board file
/// describes, for the uid and gid the daemon runs as. A gadget that cannot be set up is returned
/// as refused, with the reason; the others are set up.
pub fn privileged_step(context: &Context<'_>) -> std::io::Result<Refusals> {
    let owner = Owner {
        uid: context.uid,
        gid: context.gid,
    };
    let roots = Roots {
        configfs: Path::new(CONFIGFS),
        mounts: Path::new(MOUNTS),
        udc_class: Path::new(UDC_CLASS),
    };
    Ok(privileged_start_in(
        context.board,
        owner,
        &roots,
        &mut SystemMounts,
    ))
}

/// The gadget devices the board file gets wrong about their identity, and the reason for each,
/// checked with no hardware read: an `identity` that names no device or one that is not a
/// `system-identity`, an `identity` put under `uses` instead of `config`, and a board file that
/// sets a key only the daemon writes. `lemnos-ctl
/// validate` refuses such a board; the daemon starts without the gadget, which shows failed with
/// the reason.
#[must_use]
pub fn link_refusals(board: &BoardFile) -> Refusals {
    let mut refused = Vec::new();
    for device in board.devices.iter().filter(|d| d.driver == DRIVER) {
        // `identity` names another device but is a setting: under `uses` it would be read as a
        // resource the gadget needs, and silently ignored.
        if device.uses.contains_key(IDENTITY) {
            refused.push((
                device.id.clone(),
                format!(
                    "`{IDENTITY}` is a setting, not a resource: put it in the device's `config`"
                ),
            ));
            continue;
        }
        if device.config.contains_key(BOARD_SERIAL)
            || device.config.contains_key(BOARD_SERIAL_ERROR)
        {
            refused.push((
                device.id.clone(),
                format!(
                    "`{BOARD_SERIAL}` and `{BOARD_SERIAL_ERROR}` are set by the daemon, not the board file: remove them"
                ),
            ));
            continue;
        }
        let Some(value) = device.config.get(IDENTITY) else {
            continue;
        };
        let Some(link) = value.as_str() else {
            refused.push((
                device.id.clone(),
                format!("`{IDENTITY}` is a device id, written as a string"),
            ));
            continue;
        };
        match board.devices.iter().find(|other| other.id == link) {
            None => refused.push((
                device.id.clone(),
                format!("`{IDENTITY}` names {link}, which the board file does not have"),
            )),
            Some(identity) if identity.driver != IDENTITY_DRIVER => refused.push((
                device.id.clone(),
                format!("`{IDENTITY}` names {link}, which is not a `{IDENTITY_DRIVER}` device"),
            )),
            Some(_) => {}
        }
    }
    refused
}

/// Reads each gadget's board serial, once, and writes it into the gadget's settings. Run by the
/// daemon right after the board file is parsed, before the root phase, so that both see it.
///
/// A gadget whose identity link is wrong is refused, with the reason in its [`DeviceSpec::refused`]
/// (see [`link_refusals`]). A serial that cannot be read is kept as `board_serial_error`: the gadget
/// fails only if it derives its addresses from the serial, and is then refused with that reason.
/// Every other device is unaffected.
pub fn resolve(board: &mut BoardFile) {
    for (id, reason) in link_refusals(board) {
        if let Some(device) = board.devices.iter_mut().find(|d| d.id == id) {
            device
                .refused
                .get_or_insert(format!("device {id:?}: {reason}"));
        }
    }
    let name = board.board.id.clone();
    for index in 0..board.devices.len() {
        let device = &board.devices[index];
        if device.driver != DRIVER || device.refused.is_some() {
            continue;
        }
        let read = read_serial(board, &board.devices[index]);
        let device = &mut board.devices[index];
        let (key, value) = match read {
            Ok(serial) => (BOARD_SERIAL, serial),
            Err(reason) => (BOARD_SERIAL_ERROR, reason),
        };
        device
            .config
            .insert(key.to_string(), toml::Value::String(value));
        if key == BOARD_SERIAL_ERROR
            && let Err(problem) = settings_for(&name, device)
        {
            let own = format!("{}: ", device.id);
            let problem = problem.strip_prefix(&own).unwrap_or(&problem).to_string();
            device.refused = Some(format!("device {:?}: {problem}; {RESTART}", device.id));
        }
    }
}

/// The board serial the gadget's identity device reads, or the reason it cannot be read.
fn read_serial(board: &BoardFile, device: &DeviceSpec) -> Result<String, String> {
    let Some(value) = device.config.get(IDENTITY) else {
        return Err("the gadget names no `identity` device, so it has no board serial".into());
    };
    let link = value
        .as_str()
        .ok_or_else(|| format!("`{IDENTITY}` is a device id"))?;
    let identity = board
        .devices
        .iter()
        .find(|other| other.id == link)
        .ok_or_else(|| format!("`{IDENTITY}` names {link}, which the board file does not have"))?;
    let entries = config_entries(identity).map_err(|reason| format!("{link}: {reason}"))?;
    lemnos_driver_system_identity::board_serial(&Config::new(&entries))
}

/// The root phase's gadget setup for each gadget device in the board file that is not refused.
/// Returns the devices whose setup failed, with the reason; the others are set up, in order.
#[must_use]
pub fn privileged_start(board: &BoardFile, owner: Owner) -> Refusals {
    let roots = Roots {
        configfs: Path::new(CONFIGFS),
        mounts: Path::new(MOUNTS),
        udc_class: Path::new(UDC_CLASS),
    };
    privileged_start_in(board, owner, &roots, &mut SystemMounts)
}

/// [`privileged_start`] with the roots and the mounts given: the paths the gadgets are made under,
/// and what mounts FunctionFS. The system's are the daemon's; a test passes its own.
pub fn privileged_start_in(
    board: &BoardFile,
    owner: Owner,
    roots: &Roots<'_>,
    system: &mut dyn Mounts,
) -> Refusals {
    let mut refused = Vec::new();
    for device in board
        .devices
        .iter()
        .filter(|device| device.driver == DRIVER)
    {
        // A gadget the daemon or the board refuses is not made. The host lists it as failed with
        // the reason, and the other gadgets are set up.
        if device.refused.is_some() || gadget_refused(device) {
            continue;
        }
        if let Err(reason) = setup_one(&board.board.id, roots, device, owner, system) {
            refused.push((
                device.id.clone(),
                format!("device {:?}: {reason}; {RESTART}", device.id),
            ));
        }
    }
    refused
}

/// Whether the gadget's own check refuses the device's settings. Checked with no hardware.
fn gadget_refused(device: &DeviceSpec) -> bool {
    match config_entries(device) {
        Ok(entries) => lemnos_driver_usb_gadget::check(&Config::new(&entries)).is_err(),
        Err(_) => true,
    }
}

fn setup_one(
    board: &str,
    roots: &Roots<'_>,
    device: &DeviceSpec,
    owner: Owner,
    system: &mut dyn Mounts,
) -> Result<(), String> {
    let function = device
        .uses
        .get("function")
        .ok_or_else(|| "no `function` need".to_string())?;
    let name = function
        .strip_prefix("udc:")
        .ok_or_else(|| "`function` must be udc:NAME".to_string())?;
    let (set, functions) = settings_for(board, device)?;
    prepare_gadget(
        roots,
        name,
        owner,
        set.bytes(),
        functions.as_slice(),
        system,
    )
    .map_err(|error| error.to_string())
}

/// The descriptor set and the functions a gadget device is set up with, from the same settings the
/// daemon's driver binds with. `board` is the board file's `[board] id`.
///
/// # Errors
///
/// When the settings are refused. A derived address with no readable board serial names the
/// reason the serial could not be read.
pub fn settings_for(board: &str, device: &DeviceSpec) -> Result<(Set, Functions), String> {
    let id = &device.id;
    let entries = config_entries(device).map_err(|reason| format!("{id}: {reason}"))?;
    let config = Config::new(&entries).on_board(board);
    lemnos_driver_usb_gadget::check(&config).map_err(|error| format!("{id}: {error}"))?;
    lemnos_driver_usb_gadget::settings(&config).map_err(|status| {
        match device
            .config
            .get(BOARD_SERIAL_ERROR)
            .and_then(|v| v.as_str())
        {
            Some(reason) if !device.config.contains_key(BOARD_SERIAL) => {
                format!("{id}: {status:?}: the board serial is not known: {reason}")
            }
            _ => format!("{id}: {status:?}"),
        }
    })
}

/// The device's settings as the driver's [`Config`] reads them. Only the four kinds a board
/// file holds are passed; a table or an array is named as the reason.
fn config_entries(device: &DeviceSpec) -> Result<Vec<(&str, ConfigValue<'_>)>, String> {
    let mut entries = Vec::with_capacity(device.config.len());
    for (key, value) in &device.config {
        let value = if let Some(int) = value.as_integer() {
            ConfigValue::Int(int)
        } else if let Some(float) = value.as_float() {
            ConfigValue::Float(float)
        } else if let Some(bool) = value.as_bool() {
            ConfigValue::Bool(bool)
        } else if let Some(text) = value.as_str() {
            ConfigValue::Str(text)
        } else {
            return Err(format!(
                "setting `{key}` is not a number, boolean or string"
            ));
        };
        entries.push((key.as_str(), value));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_driver_usb_gadget::description::Functions;
    use lemnos_hal::usb_descriptors::GadgetFunction;

    /// A board with an identity device whose serial is the device tree's `serial-number` at `PATH`,
    /// and a gadget that derives its NCM addresses (or not, per `INTERFACES`).
    const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "alpha"

[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "device-tree", serial_path = "PATH" }

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:udc0" }
config = { vendor = 0x1209, product = 1, identity = "identity", interfaces = "INTERFACES" }
"#;

    const DERIVE: &str =
        "function ncm derive\ninterface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n";
    const PLAIN: &str = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n";

    /// A board file with its serial source at `path` and the gadget's `interfaces` text.
    fn board_at(path: &str, interfaces: &str) -> BoardFile {
        let text = BOARD
            .replace("PATH", path)
            .replace("INTERFACES", &interfaces.replace('\n', "\\n"));
        lemnos_host::board::parse(&text).expect("the test board parses")
    }

    /// A file in the temp directory, unique to this test, holding `text`.
    fn serial_file(name: &str, text: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-{name}", std::process::id()));
        std::fs::write(&path, text).expect("write the serial file");
        path
    }

    fn gadget(board: &BoardFile) -> &DeviceSpec {
        board.devices.iter().find(|d| d.id == "gadget").unwrap()
    }

    fn ncm_of(functions: &Functions) -> Option<GadgetFunction> {
        functions
            .as_slice()
            .iter()
            .copied()
            .find(|f| matches!(f, GadgetFunction::Ncm { .. }))
    }

    #[test]
    fn derive_takes_the_addresses_from_the_serial_the_identity_device_reads() {
        let path = serial_file("ok", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), DERIVE);
        resolve(&mut board);
        let config = &gadget(&board).config;
        assert_eq!(
            config.get("board_serial").and_then(|v| v.as_str()),
            Some("0123456789abcdef")
        );
        assert!(!config.contains_key("board_serial_error"));

        let (_, functions) = settings_for("alpha", gadget(&board)).expect("sets up");
        let pair = lemnos_driver_usb_gadget::derive::macs("alpha", "0123456789abcdef").unwrap();
        assert_eq!(
            ncm_of(&functions),
            Some(GadgetFunction::Ncm {
                host: pair.host,
                device: pair.device,
                ifname: None,
            })
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn derive_with_an_unreadable_serial_fails_the_gadget_with_the_reason() {
        let missing =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-missing", std::process::id()));
        let mut board = board_at(&missing.to_string_lossy(), DERIVE);
        resolve(&mut board);
        let error = settings_for("alpha", gadget(&board))
            .err()
            .expect("derive needs the serial");
        assert!(error.contains("the board serial is not known"), "{error}");
        assert!(error.contains("cannot read"), "{error}");
        // The gadget is refused by the daemon, with that reason, and the reason says what to do.
        let refused = gadget(&board).refused.clone().expect("refused");
        assert!(
            refused.contains("the board serial is not known"),
            "{refused}"
        );
        assert!(refused.ends_with(RESTART), "{refused}");
    }

    #[test]
    fn a_gadget_that_does_not_derive_sets_up_without_a_serial() {
        let missing =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-none", std::process::id()));
        let mut board = board_at(&missing.to_string_lossy(), PLAIN);
        resolve(&mut board);
        assert!(gadget(&board).config.contains_key("board_serial_error"));
        assert_eq!(gadget(&board).refused, None);
        // No serial string, no addresses: the driver reports the missing serial in its health.
        assert!(settings_for("alpha", gadget(&board)).is_ok());
    }

    #[test]
    fn the_board_file_may_not_set_the_serial_the_daemon_sets() {
        let path = serial_file("reserved", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        let spec = board.devices.iter_mut().find(|d| d.id == "gadget").unwrap();
        spec.config.insert(
            "board_serial".into(),
            toml::Value::String("0123456789abcdef".into()),
        );
        let refusals = link_refusals(&board);
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert!(refusals[0].1.contains("set by the daemon"), "{refusals:?}");
        resolve(&mut board);
        assert!(
            gadget(&board)
                .refused
                .as_deref()
                .unwrap()
                .contains("set by the daemon")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_identity_link_to_nothing_or_to_the_wrong_device_is_refused() {
        let path = serial_file("links", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        let link = |board: &mut BoardFile, to: &str| {
            let gadget = board.devices.iter_mut().find(|d| d.id == "gadget").unwrap();
            gadget
                .config
                .insert("identity".into(), toml::Value::String(to.into()));
        };
        link(&mut board, "nothing");
        assert!(link_refusals(&board)[0].1.contains("does not have"));
        link(&mut board, "gadget");
        assert!(
            link_refusals(&board)[0]
                .1
                .contains("not a `system-identity`")
        );
        // Refused by the daemon at start: the gadget is marked, and the others are not.
        resolve(&mut board);
        assert!(
            gadget(&board)
                .refused
                .as_deref()
                .unwrap()
                .contains("not a `system-identity`")
        );
        assert!(
            board
                .devices
                .iter()
                .find(|d| d.id == "identity")
                .unwrap()
                .refused
                .is_none()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_identity_under_uses_is_refused_by_name_with_its_right_place() {
        let path = serial_file("misplaced", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        let spec = board.devices.iter_mut().find(|d| d.id == "gadget").unwrap();
        spec.config.remove("identity");
        spec.uses.insert("identity".into(), "identity".into());
        let refusals = link_refusals(&board);
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert_eq!(
            refusals[0].1,
            "`identity` is a setting, not a resource: put it in the device's `config`"
        );
        // The daemon's start marks the gadget refused with the same reason.
        resolve(&mut board);
        assert!(
            gadget(&board)
                .refused
                .as_deref()
                .unwrap()
                .contains("`identity` is a setting, not a resource")
        );
        let _ = std::fs::remove_file(path);
    }

    /// Mounts that do nothing: the root phase's FunctionFS mount is not the thing under test.
    struct NoMounts;

    impl Mounts for NoMounts {
        fn functionfs(&mut self, _: &str, _: &Path, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn unmount(&mut self, _: &Path) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn owner() -> Owner {
        Owner { uid: 987, gid: 987 }
    }

    /// Roots in a directory of the test's own: configfs with `usb_gadget` given as `configfs_usb`
    /// (a file, so making the gadget under it fails), the UDC class with `udc0`. Nothing under
    /// `/sys` is touched.
    fn roots_in(name: &str, usb_is_a_file: bool) -> (std::path::PathBuf, [std::path::PathBuf; 3]) {
        let dir = std::env::temp_dir().join(format!(
            "lemnosd-gadget-roots-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (configfs, mounts, udc) = (dir.join("configfs"), dir.join("mounts"), dir.join("udc"));
        std::fs::create_dir_all(&configfs).unwrap();
        std::fs::create_dir_all(&mounts).unwrap();
        std::fs::create_dir_all(udc.join("udc0")).unwrap();
        if usb_is_a_file {
            std::fs::write(configfs.join("usb_gadget"), b"").unwrap();
        }
        (dir, [configfs, mounts, udc])
    }

    #[test]
    fn a_gadget_whose_configfs_setup_fails_is_refused_with_the_reason_and_the_others_are_set_up() {
        let path = serial_file("configfs", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        resolve(&mut board);
        let (dir, [configfs, mounts, udc]) = roots_in("configfs", true);
        let roots = Roots {
            configfs: &configfs,
            mounts: &mounts,
            udc_class: &udc,
        };
        let refused = privileged_start_in(&board, owner(), &roots, &mut NoMounts);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].0, "gadget");
        assert!(
            refused[0].1.starts_with("device \"gadget\": "),
            "{refused:?}"
        );
        assert!(refused[0].1.ends_with(RESTART), "{refused:?}");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_gadget_the_daemon_refused_is_not_set_up_and_a_good_one_is() {
        let path = serial_file("refused", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        let gadget = board.devices.iter_mut().find(|d| d.id == "gadget").unwrap();
        gadget.refused = Some("refused by the board".to_string());
        let (dir, [configfs, mounts, udc]) = roots_in("refused", true);
        let roots = Roots {
            configfs: &configfs,
            mounts: &mounts,
            udc_class: &udc,
        };
        // Refused gadgets are skipped, so the file that would make the configfs setup fail is not
        // even tried: the list is empty, and nothing was made under the configfs root.
        assert!(privileged_start_in(&board, owner(), &roots, &mut NoMounts).is_empty());
        assert!(!configfs.join("usb_gadget").is_dir());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }
}
