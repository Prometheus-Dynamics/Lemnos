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

use crate::privilege::Context;
use lemnos_core::{Config, ConfigValue};
use lemnos_driver_usb_gadget::description::{Functions, Set};
use lemnos_host::board::{BoardFile, DeviceSpec};
use lemnos_linux::{Owner, Roots, SystemMounts, prepare_gadget};

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

/// The root phase's step for USB gadgets (`STEPS` in `privilege.rs`): the gadgets the board file
/// describes, for the uid and gid the daemon runs as.
pub fn privileged_step(context: &Context<'_>) -> std::io::Result<()> {
    let owner = Owner {
        uid: context.uid,
        gid: context.gid,
    };
    privileged_start(context.board, owner)
        .map(|_| ())
        .map_err(std::io::Error::other)
}

/// Reads each gadget's board serial, once, and writes it into the gadget's settings. Run by the
/// daemon right after the board file is parsed, before the root phase, so that both see it.
///
/// A gadget whose serial cannot be read is not an error here: the reason is kept as
/// `board_serial_error`, and the gadget fails at setup only if it derives its addresses from the
/// serial. A board file that names no such device, names one that is not an identity device, or
/// sets `board_serial` itself is an error.
///
/// # Errors
///
/// The board file's gadget settings name something that is not there, or set a key only the
/// daemon writes.
pub fn resolve(board: &mut BoardFile) -> Result<(), String> {
    for index in 0..board.devices.len() {
        if board.devices[index].driver != DRIVER {
            continue;
        }
        let device = &board.devices[index];
        if device.config.contains_key(BOARD_SERIAL)
            || device.config.contains_key(BOARD_SERIAL_ERROR)
        {
            return Err(format!(
                "{}: `{BOARD_SERIAL}` and `{BOARD_SERIAL_ERROR}` are set by the daemon, not the board file",
                device.id
            ));
        }
        let read = match device.config.get(IDENTITY) {
            None => Err("the gadget names no `identity` device, so it has no board serial".into()),
            Some(value) => {
                let link = value
                    .as_str()
                    .ok_or_else(|| format!("{}: `identity` is a device id", device.id))?;
                let identity = board
                    .devices
                    .iter()
                    .find(|other| other.id == link)
                    .ok_or_else(|| {
                        format!(
                            "{}: `identity` names {link}, which the board file does not have",
                            device.id
                        )
                    })?;
                if identity.driver != IDENTITY_DRIVER {
                    return Err(format!(
                        "{}: `identity` names {link}, which is not a `{IDENTITY_DRIVER}` device",
                        device.id
                    ));
                }
                let entries =
                    config_entries(identity).map_err(|reason| format!("{link}: {reason}"))?;
                lemnos_driver_system_identity::board_serial(&Config::new(&entries))
            }
        };
        let device = &mut board.devices[index];
        let (key, value) = match read {
            Ok(serial) => (BOARD_SERIAL, serial),
            Err(reason) => (BOARD_SERIAL_ERROR, reason),
        };
        device
            .config
            .insert(key.to_string(), toml::Value::String(value));
    }
    Ok(())
}

/// The root phase's gadget setup for each gadget device in the board file. Returns how many
/// were set up. A device that can't be set up is named in the error, and the ones before it
/// stay set up.
pub fn privileged_start(board: &BoardFile, owner: Owner) -> Result<usize, String> {
    let mut done = 0;
    for device in board
        .devices
        .iter()
        .filter(|device| device.driver == DRIVER)
    {
        setup_one(&board.board.id, device, owner)?;
        done += 1;
    }
    Ok(done)
}

fn setup_one(board: &str, device: &DeviceSpec, owner: Owner) -> Result<(), String> {
    let id = &device.id;
    let function = device
        .uses
        .get("function")
        .ok_or_else(|| format!("{id}: no `function` need"))?;
    let name = function
        .strip_prefix("udc:")
        .ok_or_else(|| format!("{id}: `function` must be udc:NAME"))?;
    let (set, functions) = settings_for(board, device)?;
    let roots = Roots {
        configfs: Path::new(CONFIGFS),
        mounts: Path::new(MOUNTS),
        udc_class: Path::new(UDC_CLASS),
    };
    prepare_gadget(
        &roots,
        name,
        owner,
        set.bytes(),
        functions.as_slice(),
        &mut SystemMounts,
    )
    .map_err(|error| format!("{id}: {error}"))
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
        resolve(&mut board).expect("resolves");
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
                device: pair.device
            })
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn derive_with_an_unreadable_serial_fails_the_gadget_with_the_reason() {
        let missing =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-missing", std::process::id()));
        let mut board = board_at(&missing.to_string_lossy(), DERIVE);
        resolve(&mut board).expect("a serial that cannot be read is not a resolve error");
        let error = settings_for("alpha", gadget(&board))
            .err()
            .expect("derive needs the serial");
        assert!(error.contains("gadget"), "{error}");
        assert!(error.contains("the board serial is not known"), "{error}");
        assert!(error.contains("cannot read"), "{error}");
    }

    #[test]
    fn a_gadget_that_does_not_derive_sets_up_without_a_serial() {
        let missing =
            std::env::temp_dir().join(format!("lemnosd-gadget-{}-none", std::process::id()));
        let mut board = board_at(&missing.to_string_lossy(), PLAIN);
        resolve(&mut board).expect("resolves");
        assert!(gadget(&board).config.contains_key("board_serial_error"));
        // No serial string, no addresses: the driver reports the missing serial in its health.
        assert!(settings_for("alpha", gadget(&board)).is_ok());
    }

    #[test]
    fn the_board_file_may_not_set_the_serial_the_daemon_sets() {
        let path = serial_file("reserved", b"0123456789abcdef\0");
        let mut board = board_at(&path.to_string_lossy(), PLAIN);
        let gadget = board.devices.iter_mut().find(|d| d.id == "gadget").unwrap();
        gadget.config.insert(
            "board_serial".into(),
            toml::Value::String("0123456789abcdef".into()),
        );
        assert!(
            resolve(&mut board)
                .unwrap_err()
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
        assert!(resolve(&mut board).unwrap_err().contains("does not have"));
        link(&mut board, "gadget");
        assert!(
            resolve(&mut board)
                .unwrap_err()
                .contains("not a `system-identity`")
        );
        let _ = std::fs::remove_file(path);
    }
}
