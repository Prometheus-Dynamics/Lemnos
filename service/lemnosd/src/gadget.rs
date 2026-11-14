//! The USB gadgets of the board, as the root phase hands them to the gadget helper.
//!
//! configfs and FunctionFS mounts need root, and the daemon does not keep it. The root phase
//! forks the gadget helper (`helper.rs`) before the drop, with each gadget's description taken
//! from the board file here ([`describe`]): the controller, the descriptor set, the functions. The
//! helper makes a gadget's tree and mount, and binds and unbinds the controller, when the daemon
//! asks for it by index. A gadget the helper cannot make yet is not refused: its driver retries.
//! Only a board file that is wrong is refused here, and it is refused with the reason.
//!
//! The board's serial is read twice, by two readers of one fixed source. [`resolve`] reads it once
//! before the root phase: the gadget's `identity` names the identity device whose serial source it
//! is, and the result is written into the gadget's settings as `board_serial` (or
//! `board_serial_error`, with the reason) for the daemon's driver, which needs it only for its
//! health note. The helper keeps that source (the identity device's settings, fixed at fork) and
//! reads it again at every `setup`, so a source that is not readable yet (an nvmem cell that probes
//! late) makes the setup fail with the reason and be retried, not the gadget be refused for good.
//! What the tree holds, and the NCM addresses derived from the serial, are the helper's
//! ([`settings_now`]; `docs/boards.md`). The daemon never passes the helper a string.

use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::helper;
use crate::privilege::Refusals;
use crate::supervisor;
use lemnos_core::{Config, ConfigValue};
use lemnos_driver_usb_gadget::description::{Functions, Set};
use lemnos_host::board::{BoardFile, DeviceSpec};
use lemnos_linux::{Owner, Roots, is_controller_name};
use lemnos_linux_sys::privilege;

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

/// The system's roots: configfs, the FunctionFS mount points, and the controllers.
#[must_use]
pub fn system_roots() -> Roots<'static> {
    Roots {
        configfs: Path::new(CONFIGFS),
        mounts: Path::new(MOUNTS),
        udc_class: Path::new(UDC_CLASS),
    }
}

/// The gadget devices of the board, as the helper holds them: each gadget's controller, descriptor
/// set and functions, from the same settings the driver binds with. A gadget whose settings the
/// board file gets wrong is refused with the reason (validation: nothing a retry could mend). The
/// list's index is the gadget's index in the helper.
#[must_use]
pub fn describe(board: &BoardFile, owner: Owner) -> (Vec<helper::Gadget>, Refusals) {
    let mut gadgets = Vec::new();
    let mut refused = Vec::new();
    for device in board.devices.iter().filter(|d| d.driver == DRIVER) {
        if device.refused.is_some() {
            continue;
        }
        match describe_one(&board.board.id, device, owner) {
            Ok(mut gadget) => {
                gadget.identity = identity_entries(board, device);
                gadgets.push(gadget);
            }
            Err(reason) => refused.push((device.id.clone(), reason)),
        }
    }
    (gadgets, refused)
}

fn describe_one(board: &str, device: &DeviceSpec, owner: Owner) -> Result<helper::Gadget, String> {
    let function = device
        .uses
        .get("function")
        .ok_or_else(|| "no `function` need".to_string())?;
    let name = function
        .strip_prefix("udc:")
        .ok_or_else(|| "`function` must be udc:NAME".to_string())?;
    if !is_controller_name(name) {
        return Err(format!(
            "`function` names {name:?}, which is not a controller's name"
        ));
    }
    validate(device)?;
    Ok(helper::Gadget {
        udc: name.to_string(),
        owner,
        board: board.to_string(),
        config: owned_entries(device, true).map_err(|reason| format!("{}: {reason}", device.id))?,
        identity: None,
    })
}

/// The root phase's gadget helper: forks the helper's root parent (`supervisor.rs`) with the board's
/// gadgets, fixed, which forks the helper and forks it again when it dies, and returns the daemon's
/// end of the socket (`None` when the board has no gadget) with the gadgets refused by validation.
/// Only the root phase calls it, before the drop, so the parent and the helper keep root.
///
/// # Errors
///
/// The socket pair or the fork fails: the process cannot run as asked.
pub fn spawn(
    board: &BoardFile,
    owner: Owner,
) -> Result<(Option<helper::Client>, Refusals), String> {
    let (gadgets, refused) = describe(board, owner);
    if gadgets.is_empty() {
        return Ok((None, refused));
    }
    let (parent, child) =
        UnixStream::pair().map_err(|error| format!("gadget helper: socket pair: {error}"))?;
    let names: Vec<(String, u32)> = gadgets
        .iter()
        .enumerate()
        .map(|(index, gadget)| (gadget.udc.clone(), index as u32))
        .collect();
    let pid = privilege::fork().map_err(|error| format!("gadget helper: fork: {error}"))?;
    if pid == 0 {
        drop(parent);
        supervisor::run(child, &gadgets);
    }
    drop(child);
    let client =
        helper::Client::new(parent, names).map_err(|error| format!("gadget helper: {error}"))?;
    Ok((Some(client), refused))
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
/// (see [`link_refusals`]). A serial that cannot be read is kept as `board_serial_error`, and is
/// not a refusal: the helper reads the source again at each setup, so a gadget that derives its
/// addresses from the serial is retried until the source can be read. Every other device is
/// unaffected.
pub fn resolve(board: &mut BoardFile) {
    for (id, reason) in link_refusals(board) {
        if let Some(device) = board.devices.iter_mut().find(|d| d.id == id) {
            device
                .refused
                .get_or_insert(format!("device {id:?}: {reason}"));
        }
    }
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

/// The settings checks a board file can fail with no hardware read: the driver's own check.
fn validate(device: &DeviceSpec) -> Result<(), String> {
    let id = &device.id;
    let entries = config_entries(device).map_err(|reason| format!("{id}: {reason}"))?;
    lemnos_driver_usb_gadget::check(&Config::new(&entries))
        .map_err(|error| format!("{id}: {error}"))
}

/// One setting, owned: what the helper keeps of a device's `config`.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// A device's settings, owned.
pub type Entries = Vec<(String, Entry)>;

/// The device's settings, owned. With `without_serial` the two keys the daemon writes
/// (`board_serial`, `board_serial_error`) are left out: the helper sets them itself at each setup.
fn owned_entries(device: &DeviceSpec, without_serial: bool) -> Result<Entries, String> {
    let mut out = Vec::with_capacity(device.config.len());
    for (key, value) in &device.config {
        if without_serial && (key == BOARD_SERIAL || key == BOARD_SERIAL_ERROR) {
            continue;
        }
        let entry = if let Some(int) = value.as_integer() {
            Entry::Int(int)
        } else if let Some(float) = value.as_float() {
            Entry::Float(float)
        } else if let Some(bool) = value.as_bool() {
            Entry::Bool(bool)
        } else if let Some(text) = value.as_str() {
            Entry::Str(text.to_string())
        } else {
            return Err(format!(
                "setting `{key}` is not a number, boolean or string"
            ));
        };
        out.push((key.clone(), entry));
    }
    Ok(out)
}

/// The entries as the driver's [`Config`] reads them.
fn borrowed(entries: &Entries) -> Vec<(&str, ConfigValue<'_>)> {
    entries
        .iter()
        .map(|(key, entry)| {
            let value = match entry {
                Entry::Int(v) => ConfigValue::Int(*v),
                Entry::Float(v) => ConfigValue::Float(*v),
                Entry::Bool(v) => ConfigValue::Bool(*v),
                Entry::Str(v) => ConfigValue::Str(v),
            };
            (key.as_str(), value)
        })
        .collect()
}

/// The identity device's settings, when the gadget names one that is a `system-identity`.
fn identity_entries(board: &BoardFile, device: &DeviceSpec) -> Option<Entries> {
    let link = device.config.get(IDENTITY)?.as_str()?;
    let identity = board
        .devices
        .iter()
        .find(|other| other.id == link && other.driver == IDENTITY_DRIVER)?;
    owned_entries(identity, false).ok()
}

/// Why a gadget cannot be made now.
#[derive(Debug, PartialEq, Eq)]
pub enum Unready {
    /// The board serial the gadget derives from cannot be read yet: the reason. A retry may mend it.
    Serial(String),
    /// The settings are refused: nothing a retry could mend (the board file is wrong).
    Refused(String),
}

/// What a gadget is set up with now: its descriptor set and functions, with the board serial read
/// from the source the board file names, at this moment. The helper calls it at every `setup`.
///
/// # Errors
///
/// [`Unready::Serial`] when the gadget derives its addresses from a serial that cannot be read.
pub fn settings_now(gadget: &helper::Gadget) -> Result<(Set, Functions), Unready> {
    let mut entries = gadget.config.clone();
    let mut serial_error = None;
    if let Some(identity) = &gadget.identity {
        match lemnos_driver_system_identity::board_serial(&Config::new(&borrowed(identity))) {
            Ok(serial) => entries.push((BOARD_SERIAL.to_string(), Entry::Str(serial))),
            Err(reason) => {
                entries.push((BOARD_SERIAL_ERROR.to_string(), Entry::Str(reason.clone())));
                serial_error = Some(reason);
            }
        }
    }
    let borrow = borrowed(&entries);
    let config = Config::new(&borrow).on_board(&gadget.board);
    lemnos_driver_usb_gadget::settings(&config).map_err(|status| match serial_error {
        Some(reason) => Unready::Serial(reason),
        None => Unready::Refused(format!("{status:?}")),
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
    fn derive_with_an_unreadable_serial_is_not_refused_the_helper_waits_for_the_source() {
        let missing =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-missing", std::process::id()));
        let mut board = board_at(&missing.to_string_lossy(), DERIVE);
        resolve(&mut board);
        // The daemon's own read failed, and the settings that need the serial say so.
        let error = settings_for("alpha", gadget(&board))
            .err()
            .expect("derive needs the serial");
        assert!(error.contains("the board serial is not known"), "{error}");
        assert!(error.contains("cannot read"), "{error}");
        // But the gadget is not refused: the helper holds the source and reads it at each setup.
        assert_eq!(gadget(&board).refused, None);
        let (gadgets, refused) = describe(&board, Owner { uid: 1, gid: 1 });
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(gadgets.len(), 1);
        let gadget = &gadgets[0];
        assert!(gadget.identity.is_some());
        assert!(
            !gadget
                .config
                .iter()
                .any(|(key, _)| key == BOARD_SERIAL || key == BOARD_SERIAL_ERROR),
            "the daemon's one read is not handed on"
        );
        match settings_now(gadget) {
            Err(Unready::Serial(reason)) => assert!(reason.contains("cannot read"), "{reason}"),
            other => panic!("{:?}", other.map(|_| ())),
        }
        // The source appears: the same description now gives its settings, with no restart.
        std::fs::write(&missing, b"0123456789abcdef\0").unwrap();
        let (_, functions) = settings_now(gadget).expect("readable now");
        let pair = lemnos_driver_usb_gadget::derive::macs("alpha", "0123456789abcdef").unwrap();
        assert_eq!(
            ncm_of(&functions),
            Some(GadgetFunction::Ncm {
                host: pair.host,
                device: pair.device,
                ifname: None,
            })
        );
        let _ = std::fs::remove_file(missing);
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
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    fn board(udc: &str) -> BoardFile {
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alpha\"\n\
             [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\n\
             uses = {{ function = \"udc:{udc}\" }}\n\
             config = {{ vendor = 0x1209, product = 1, interfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\" }}\n"
        );
        lemnos_host::board::parse(&text).expect("the test board parses")
    }

    fn owner() -> Owner {
        Owner { uid: 987, gid: 987 }
    }

    #[test]
    fn a_gadget_whose_tree_cannot_be_made_yet_is_described_not_refused() {
        // Nothing is made here: the description only needs the settings and the controller name.
        let (gadgets, refused) = describe(&board("udc0"), owner());
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(gadgets.len(), 1);
        assert_eq!(gadgets[0].udc, "udc0");
        assert_eq!(gadgets[0].owner, owner());
        assert_eq!(gadgets[0].board, "alpha");
        assert!(!gadgets[0].config.is_empty());
    }

    #[test]
    fn a_controller_name_that_is_not_one_directory_name_is_refused_as_validation() {
        let (gadgets, refused) = describe(&board("../x"), owner());
        assert!(gadgets.is_empty());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].1.contains("not a controller's name"),
            "{refused:?}"
        );
    }

    #[test]
    fn a_gadget_the_daemon_already_refused_is_left_out_of_the_helper() {
        let mut board = board("udc0");
        board.devices[0].refused = Some("refused before the root phase".into());
        let (gadgets, refused) = describe(&board, owner());
        assert!(gadgets.is_empty());
        assert!(refused.is_empty());
    }
}
