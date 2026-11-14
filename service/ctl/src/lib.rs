//! `lemnos-ctl`: everything it knows about a device comes from that device's
//! description, so it works on devices nobody has written tooling for.

use std::io::Write;
use std::time::Duration;

use lemnos_client::{Client, DeviceEntry, DeviceHealth, DeviceState, Error, PackageMode, follow};
use lemnos_core::{ItemKind, Status};
use lemnos_wire::text::{
    Call, encode_call, find_item, find_method, method_of, render, render_reply, shape_name,
    too_small, watchable,
};
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod, OwnedShape};

mod boot_dir;
#[cfg(test)]
mod boot_dir_tests;
mod show;

pub const USAGE: &str = "\
lemnos-ctl [--socket PATH] COMMAND
  list                           every device and how it stands
  chain DEVICE                   the devices it leads to through `link`, in order
  describe DEVICE                what it provides
  show DEVICE                    the device as a panel: name, kind, bus, state, each value's
                                 unit, range, limit and reading type
  read DEVICE [ITEM]             one value, or all of them
  watch DEVICE ITEM [--period-ms N] [--count N]
  set DEVICE ITEM [VALUE...]     a control or an action
  call DEVICE METHOD [ARG...]    any method, as NAME or TAG:NAME; a job method runs as a
                                 long job, its progress printed until it ends (interrupting
                                 the tool cancels it)
  off DEVICE | on DEVICE
  security DEVICE state          the board's security switch: on or off, its generation, the view
  security DEVICE on             change the switch on (the device's `set`: its precheck first)
  security DEVICE off [--authorization FILE]
                                 change the switch off; a board with a trust root needs FILE, the
                                 authorization signed for the generation `request-off` names
  security DEVICE request-off    what an authorization to turn the switch off must sign, from the board
  security DEVICE precheck on|off  what would stop the change, with no change made
  security render --record FILE [--dir DIR]
                                 render the switch's view from its record, with no daemon (the
                                 image runs it before the daemon starts; DIR is /run/board)
  packages                       driver packages: which loaded, which were refused and why
  files DEVICE [NAME [FILE]]     the named files a node serves (`lemnos/`): its index, or one
                                 file to stdout, or to FILE, which must not exist yet
  validate [--socs DIR] [--no-live] BOARD
                                 check a board file and show the host settings it gives, with no
                                 daemon; with a `soc`, also check the live device tree against it
  tree [--socs DIR | --map FILE] BOARD [-o FILE] [--boot FILE] [--boot-dir DIR [--slot ID]] [--uboot FILE]
                                 [--uboot-config FILE]
                                 [--modprobe FILE] [--cmdline FILE] [--udev FILE] [--mdev FILE]
                                 [--bridge-env FILE]
                                 [--security-unit FILE]
                                 [--manifest-boot FILE] [--cmdline-slot FILE --slot ID]
                                 write the device-tree overlay for a board (stdout by default),
                                 its firmware boot fragment, its U-Boot fragment (`gpio set`
                                 and `gpio clear` for a boot script), the Kconfig fragment the
                                 board's U-Boot is built with for the uboot record
                                 (--uboot-config), the kernel module's
                                 modprobe configuration, and for a module built into the kernel
                                 the same parameters as kernel command-line words, and the udev
                                 rules that give the daemon's nodes to its group, the same
                                 permissions as mdev.conf lines for a BusyBox image (--mdev FILE),
                                 and the bridge's `LEMNOS_ORION_LINKS=<gadget>=<link>` line for
                                 the board's USB link (--bridge-env FILE), with no daemon";

pub mod bridge_env;
#[cfg(test)]
mod bridge_env_tests;
mod files;

const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
const REPLY_MAX: usize = 64 * 1024;
const WATCH_WAIT: Duration = Duration::from_secs(2);
/// How long `call` waits for a long job between looks at its progress. It waits as long as
/// the job runs.
const JOB_WAIT: Duration = Duration::from_secs(1);

type Outcome = Result<(), String>;

fn text(error: Error) -> String {
    error.to_string()
}

/// A call's error as the tool prints it. A reply that did not fit the out buffer says how to ask
/// for more ([`too_small`]).
fn refusal(error: Error, call: &Call) -> String {
    match &error {
        Error::Refused { status, .. } if *status == Status::BUFFER_TOO_SMALL => {
            format!("{error}: {}", too_small(call))
        }
        _ => error.to_string(),
    }
}

fn line(out: &mut dyn Write, message: core::fmt::Arguments<'_>) -> Outcome {
    writeln!(out, "{message}").map_err(|e| e.to_string())
}

const fn state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Running => "running",
        DeviceState::BackingOff => "retrying",
        DeviceState::Off => "off",
    }
}

const fn health_name(health: DeviceHealth) -> &'static str {
    match health {
        DeviceHealth::Ok => "ok",
        DeviceHealth::Degraded => "degraded",
        DeviceHealth::Failed => "failed",
    }
}

struct Tool<'a> {
    client: Client,
    out: &'a mut dyn Write,
}

/// Runs one command. `args` excludes the program's name.
pub fn run(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut socket = std::env::var("LEMNOSD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    let mut rest = args;
    if let [flag, path, tail @ ..] = rest
        && flag == "--socket"
    {
        socket.clone_from(path);
        rest = tail;
    }
    let [command, tail @ ..] = rest else {
        return Err(USAGE.to_string());
    };
    if command == "validate" {
        // `--socs DIR` and `--no-live` in either order, then the board file.
        let mut socs = None;
        let mut live = true;
        let mut board = None;
        let mut i = 0;
        while i < tail.len() {
            match tail[i].as_str() {
                "--socs" if socs.is_none() => {
                    socs = Some(tail.get(i + 1).ok_or_else(|| USAGE.to_string())?.as_str());
                    i += 1;
                }
                "--no-live" => live = false,
                flag if board.is_none() && !flag.starts_with('-') => board = Some(flag),
                _ => return Err(USAGE.to_string()),
            }
            i += 1;
        }
        let board = board.ok_or_else(|| USAGE.to_string())?;
        return validate(board, socs, live, out);
    }
    if command == "tree" {
        return tree(tail, out);
    }
    if command == "security" && tail.first().is_some_and(|word| word == "render") {
        return security_render(&tail[1..], out);
    }
    let client = Client::connect(&socket, "lemnos-ctl").map_err(|e| format!("{socket}: {e}"))?;
    let mut tool = Tool { client, out };
    match (command.as_str(), tail) {
        ("list", []) => tool.list(),
        ("chain", [device]) => tool.chain(device),
        ("describe", [device]) => tool.describe(device),
        ("show", [device]) => tool.show(device),
        ("read", [device]) => tool.read_all(device),
        ("read", [device, item]) => tool.read_one(device, item),
        ("watch", [device, item, options @ ..]) => tool.watch(device, item, options),
        ("set", [device, item, values @ ..]) => tool.set(device, item, values),
        ("call", [device, method, values @ ..]) => tool.call(device, method, values),
        ("off", [device]) => tool.power(device, false),
        ("on", [device]) => tool.power(device, true),
        ("packages", []) => tool.packages(),
        ("files", [device]) => tool.files_index(device),
        ("files", [device, name]) => tool.files_fetch(device, name, None),
        ("files", [device, name, path]) => tool.files_fetch(device, name, Some(path)),
        ("security", [device, word]) if word == "state" => tool.call(device, "state", &[]),
        ("security", [device, word]) if word == "request-off" => {
            tool.call(device, "request_off", &[])
        }
        ("security", [device, word]) if word == "on" => {
            tool.call(device, "set", &["on".to_string(), String::new()])
        }
        ("security", [device, word]) if word == "off" => {
            tool.call(device, "set", &["off".to_string(), String::new()])
        }
        ("security", [device, word, flag, file]) if word == "off" && flag == "--authorization" => {
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            tool.call(device, "set", &["off".to_string(), hex])
        }
        ("security", [_, word, flag, _]) if word == "on" && flag == "--authorization" => {
            Err("--authorization belongs to off: turning the switch on needs none".to_string())
        }
        ("security", [device, verb, word])
            if verb == "precheck" && (word == "on" || word == "off") =>
        {
            tool.call(device, "precheck", std::slice::from_ref(word))
        }
        _ => Err(USAGE.to_string()),
    }
}

/// Reads and checks the board file at `path` as `lemnosd` would, and prints what the host will
/// run with: its settings, defaults filled in, and the devices that opted in to auto-off.
fn validate(path: &str, socs: Option<&str>, live: bool, out: &mut dyn Write) -> Outcome {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    // A gadget whose `identity` names no device of this board, or one that is not the identity
    // device, or sits under `uses`, is refused here by the same check the daemon's start makes: the
    // daemon would start without that gadget. It runs first, as the daemon's does, so a misplaced
    // `identity` is named as that and not as a missing one.
    if let Some((device, reason)) = lemnosd::gadget::link_refusals(&board).into_iter().next() {
        return Err(format!("{path}: device {device:?}: {reason}"));
    }
    // Each driver's own check of its settings, the one the daemon's start runs before it makes a
    // device: a setting a driver refuses is refused here, by device and key.
    lemnos_host::check_settings(&board, &lemnosd::builtin::drivers())
        .map_err(|e| format!("{path}: {e}"))?;
    // A link names a device of this board, and not the device itself. Whether the two ends agree
    // is only known to a running host, which checks it at bind.
    for device in &board.devices {
        let Some(link) = &device.link else { continue };
        if link == &device.id {
            return Err(format!(
                "{path}: device {:?} is linked to itself",
                device.id
            ));
        }
        if !board.devices.iter().any(|d| &d.id == link) {
            return Err(format!(
                "{path}: device {:?} is linked to {link:?}, which the board doesn't have",
                device.id
            ));
        }
    }
    for device in &board.devices {
        for name in device
            .bus
            .iter()
            .chain(device.uses.values())
            .filter(|name| unstable_name(name))
        {
            line(
                out,
                format_args!(
                    "warning: device {:?} uses {name:?}: the kernel renumbers gpio, pwm and watchdog \
                     devices, so this name can point at another device after a reboot; use \
                     gpio:LABEL:LINE, pwm:NODE:CHANNEL or watchdog:NODE",
                    device.id
                ),
            )?;
        }
    }
    let findings = match (&board.board.soc, live) {
        (Some(_), true) => live_problems(&board, socs)?,
        _ => lemnos_devtree::check::Findings::default(),
    };
    let problems = findings.problems;
    for note in &findings.notes {
        line(out, format_args!("live note: {note}"))?;
    }
    validate_settings(path, &board, out)?;
    if !problems.is_empty() {
        for problem in &problems {
            line(out, format_args!("live: {problem}"))?;
        }
        return Err(format!(
            "{path}: {} problems with the live system",
            problems.len()
        ));
    }
    Ok(())
}

/// A name of a GPIO chip, PWM chip or watchdog by its number, which the kernel may renumber.
fn unstable_name(name: &str) -> bool {
    name.starts_with("gpiochip")
        || name.starts_with("pwmchip")
        || name
            .strip_prefix("watchdog")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The kernel drivers the tool knows, by their manifests: the built-in list `lemnosd` runs, so
/// what a driver needs is what the driver declares, never a list kept here.
fn driver_manifests() -> Vec<lemnos_core::Manifest> {
    lemnosd::builtin::drivers()
        .iter()
        .map(|entry| *entry.manifest())
        .collect()
}

/// The problems the live device tree and platform devices have against the board's hardware, and
/// the facts the operator must confirm on the board.
fn live_problems(
    board: &lemnos_host::board::BoardFile,
    socs: Option<&str>,
) -> Result<lemnos_devtree::check::Findings, String> {
    let map = load_map(board, socs)?;
    let chips: Vec<(String, u32)> = lemnos_linux::gpio_chips(std::path::Path::new("/dev"))
        .into_iter()
        .map(|chip| (chip.label, chip.lines))
        .collect();
    Ok(lemnos_devtree::check(
        board,
        &map,
        &lemnos_devtree::check::Live {
            tree: std::path::Path::new("/proc/device-tree"),
            sys: std::path::Path::new("/sys/bus/platform/devices"),
            gpio_chips: &chips,
            udc: std::path::Path::new("/sys/class/udc"),
            gpio_debug: std::path::Path::new("/sys/kernel/debug/gpio"),
            regulators: std::path::Path::new("/sys/class/regulator"),
        },
    ))
}

/// The SoC map a board names, read from `socs` (`socs/<soc>.toml`, the repository's folder by
/// default).
fn load_map(
    board: &lemnos_host::board::BoardFile,
    socs: Option<&str>,
) -> Result<lemnos_devtree::SocMap, String> {
    let soc = board.board.soc.as_deref().ok_or("[board] soc is not set")?;
    let dir = socs.unwrap_or("socs");
    let path = format!("{dir}/{soc}.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    lemnos_devtree::SocMap::parse(&text).map_err(|e| format!("{path}: {e}"))
}

/// `tree BOARD [-o FILE] [--boot FILE] [--boot-dir DIR [--slot ID]]`: the overlay, the boot fragment,
/// and the Raspberry Pi firmware's boot files for a board.
fn tree(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut socs = None;
    let mut map_file = None;
    let mut overlay_out = None;
    let mut boot_out = None;
    let mut boot_dir_out = None;
    let mut slot = None;
    let mut uboot_out = None;
    let mut uboot_config_out = None;
    let mut modprobe_out = None;
    let mut cmdline_out = None;
    let mut udev_out = None;
    let mut mdev_out = None;
    let mut bridge_env_out = None;
    let mut security_out = None;
    let mut manifest_boot_out = None;
    let mut cmdline_slot_out = None;
    let mut board_path = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].as_str(), value) {
            ("--socs", Some(dir)) => socs = Some(dir),
            ("--map", Some(file)) => map_file = Some(file),
            ("-o", Some(file)) => overlay_out = Some(file),
            ("--boot", Some(file)) => boot_out = Some(file),
            ("--boot-dir", Some(dir)) => boot_dir_out = Some(dir),
            ("--slot", Some(id)) => slot = Some(id),
            ("--uboot", Some(file)) => uboot_out = Some(file),
            ("--uboot-config", Some(file)) => uboot_config_out = Some(file),
            ("--modprobe", Some(file)) => modprobe_out = Some(file),
            ("--cmdline", Some(file)) => cmdline_out = Some(file),
            ("--udev", Some(file)) => udev_out = Some(file),
            ("--mdev", Some(file)) => mdev_out = Some(file),
            ("--bridge-env", Some(file)) => bridge_env_out = Some(file),
            ("--security-unit", Some(file)) => security_out = Some(file),
            ("--manifest-boot", Some(file)) => manifest_boot_out = Some(file),
            ("--cmdline-slot", Some(file)) => cmdline_slot_out = Some(file),
            (flag, _) if board_path.is_none() && !flag.starts_with('-') => {
                board_path = Some(flag.to_string());
                i += 1;
                continue;
            }
            _ => return Err(USAGE.to_string()),
        }
        i += 2;
    }
    let path = board_path.ok_or_else(|| USAGE.to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let map = match map_file {
        Some(file) => {
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
            lemnos_devtree::SocMap::parse(&text).map_err(|e| format!("{file}: {e}"))?
        }
        None => load_map(&board, socs.as_deref())?,
    };
    let generated = lemnos_devtree::generate(&board, &map, &driver_manifests())
        .map_err(|e| format!("{path}: {e}"))?;
    match overlay_out {
        Some(file) => {
            std::fs::write(&file, &generated.overlay).map_err(|e| format!("{file}: {e}"))?
        }
        None => line(out, format_args!("{}", generated.overlay.trim_end()))?,
    }
    if let Some(file) = boot_out {
        std::fs::write(&file, &generated.boot).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(dir) = boot_dir_out {
        let files = boot_dir::files(&board, &generated.boot, slot.as_deref().unwrap_or(""))
            .map_err(|e| format!("{path}: --boot-dir: {e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
        for (name, bytes) in files {
            let target = std::path::Path::new(&dir).join(&name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    if let Some(file) = uboot_out {
        let text = generated
            .uboot
            .map_err(|why| format!("{path}: --uboot: {why}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = uboot_config_out {
        let text =
            boot_dir::uboot_config(&board).map_err(|e| format!("{path}: --uboot-config: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = modprobe_out {
        std::fs::write(&file, &generated.modprobe).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = cmdline_out {
        let line = kernel_cmdline(&generated.modprobe);
        std::fs::write(&file, line).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = udev_out {
        std::fs::write(&file, lemnos_devtree::udev::fragment(&board))
            .map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = mdev_out {
        std::fs::write(&file, lemnos_devtree::mdev::fragment(&board))
            .map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = bridge_env_out {
        let text = bridge_env::text(&board).map_err(|e| format!("{path}: --bridge-env: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = security_out {
        let security = board
            .security
            .as_ref()
            .ok_or_else(|| format!("{path}: --security-unit: the board has no [security] table"))?;
        std::fs::write(&file, security_unit(&security.record))
            .map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = cmdline_slot_out {
        let id = slot.as_deref().ok_or_else(|| {
            format!("{path}: --cmdline-slot needs --slot: the line names the slot it is for")
        })?;
        let text = boot_dir::cmdline_slot(&board, id)
            .map_err(|e| format!("{path}: --cmdline-slot: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = manifest_boot_out {
        let text = boot_dir::manifest_boot(&board.board.id, generated.overlay_sha256.as_deref())
            .map_err(|e| format!("{path}: --manifest-boot: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    Ok(())
}

/// The unit that renders the security switch's view before the daemon and the comm core start. It
/// makes `/run/board` (the directory `/etc/board` links to on the image) and writes `security` into it
/// from the record. An absent record renders off; an unreadable one renders on, as the switch reads it.
fn security_unit(record: &str) -> String {
    format!(
        "# Generated by `lemnos-ctl tree --security-unit` from the board file's [security] table.\n\
         # Renders the security switch's view (/run/board/security) from its record before the daemon\n\
         # and the comm core start. Change the board file, not this unit.\n\
         [Unit]\n\
         Description=Lemnos: the board security view\n\
         DefaultDependencies=no\n\
         After=local-fs.target\n\
         Before=lemnosd.service lemnos-orion.service\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         RuntimeDirectory=board\n\
         RuntimeDirectoryMode=0755\n\
         ExecStart=/usr/bin/lemnos-ctl security render --record \"{record}\" --dir /run/board\n\
         \n\
         [Install]\n\
         WantedBy=sysinit.target\n"
    )
}

/// The kernel module's parameters as kernel command-line words, for a module built into the
/// kernel (`lemnos.owned=...`), one line. It is read from the `options lemnos` line of the modprobe
/// fragment, so the two carry the same values by construction. A `blacklist` has no command-line
/// form: a built-in driver is not stopped by one (the fragment says so).
fn kernel_cmdline(modprobe: &str) -> String {
    let words: Vec<String> = modprobe
        .lines()
        .find_map(|line| line.strip_prefix("options lemnos "))
        .map(|rest| {
            rest.split_whitespace()
                .map(|option| format!("lemnos.{option}"))
                .collect()
        })
        .unwrap_or_default();
    let mut line = words.join(" ");
    line.push('\n');
    line
}

/// `security render`: the view from the record, written with no daemon. An absent record is off;
/// an unreadable one renders on, as the switch reads it.
fn security_render(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut record = None;
    let mut dir = lemnos_slot_store::security::RUN_DIR.to_string();
    let mut i = 0;
    while i < args.len() {
        match (args[i].as_str(), args.get(i + 1)) {
            ("--record", Some(path)) if record.is_none() => record = Some(path.clone()),
            ("--dir", Some(path)) => dir.clone_from(path),
            _ => return Err(USAGE.to_string()),
        }
        i += 2;
    }
    let record = record.ok_or_else(|| USAGE.to_string())?;
    let store = lemnos_slot_store::security::PlainFile::new(record.as_str());
    let switch = match lemnos_slot_store::security::Store::read(&store) {
        Ok(Some(switch)) => switch,
        Ok(None) => lemnos_slot_store::security::Switch {
            on: false,
            generation: 0,
        },
        Err(error) => {
            line(
                out,
                format_args!("record {record} unreadable ({error}): the switch reads on"),
            )?;
            lemnos_slot_store::security::Switch {
                on: true,
                generation: 0,
            }
        }
    };
    lemnos_slot_store::security::render_at(std::path::Path::new(&dir), switch)
        .map_err(|error| format!("{dir}: {error}"))?;
    line(
        out,
        format_args!(
            "security view in {dir}: {} generation {}",
            if switch.on { "on" } else { "off" },
            switch.generation
        ),
    )
}

fn validate_settings(
    path: &str,
    board: &lemnos_host::board::BoardFile,
    out: &mut dyn Write,
) -> Outcome {
    // The per-device key is removed: the board's security switch replaces it.
    let refused: Vec<&str> = board
        .devices
        .iter()
        .filter(|d| d.config.contains_key("require_signed"))
        .map(|d| d.id.as_str())
        .collect();
    if !refused.is_empty() {
        return Err(format!(
            "refused: {} still set require_signed, which was removed: the board's security switch replaces it (docs/slots.md, \"The security switch\")",
            refused.join(", ")
        ));
    }
    if let Some(security) = &board.security {
        line(
            out,
            format_args!(
                "security: record {}, trust root {}, orion {}",
                security.record,
                security.keys.as_deref().unwrap_or("none"),
                if security.orion { "present" } else { "absent" }
            ),
        )?;
    }
    let options = board.host.apply(lemnos_host::Options::default());
    line(
        out,
        format_args!(
            "{path}: board {:?}, {} devices",
            board.board.id,
            board.devices.len()
        ),
    )?;
    line(
        out,
        format_args!(
            "host: call deadline {} ms, first retry {} ms, retry cap {} ms",
            options.call_deadline.as_millis(),
            options.retry_first.as_millis(),
            options.retry_cap.as_millis(),
        ),
    )?;
    let opted: Vec<String> = board
        .devices
        .iter()
        .filter_map(|d| d.auto_off_after.map(|n| format!("{} after {n}", d.id)))
        .collect();
    if opted.is_empty() {
        line(
            out,
            format_args!("auto-off: none, every device is retried forever"),
        )
    } else {
        line(out, format_args!("auto-off: {}", opted.join(", ")))
    }
}

impl Tool<'_> {
    fn device(&self, name: &str) -> Result<DeviceEntry, String> {
        self.client
            .list()
            .map_err(text)?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or_else(|| format!("no device {name:?}"))
    }

    fn description(&self, name: &str) -> Result<(DeviceEntry, DeviceDescription), String> {
        let device = self.device(name)?;
        let description = self.client.describe(device.index).map_err(text)?;
        Ok((device, description))
    }

    fn list(&mut self) -> Outcome {
        let devices = self.client.list().map_err(text)?;
        let width = devices.iter().map(|d| d.name.len()).max().unwrap_or(0);
        for d in devices {
            let tags = d.tags.join(",");
            let mut row = format!(
                "{:width$}  {}  {}  {}  {tags}",
                d.name,
                d.driver,
                state_name(d.state),
                health_name(d.health),
            );
            if let Some(link) = &d.link {
                row.push_str(&format!("  -> {link}"));
            }
            if !d.reason.is_empty() {
                row.push_str(&format!("  ({})", d.reason));
            }
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    /// What `name` is wired to, and what that is wired to, one device a row: a camera sensor,
    /// its receiver, the receiver's ISP. The tool knows none of them: it follows the board's
    /// `link` from one device to the next.
    fn chain(&mut self, name: &str) -> Outcome {
        let devices = self.client.list().map_err(text)?;
        let chain = follow(&devices, name);
        let Some(last) = chain.last() else {
            return Err(format!("no device {name:?}"));
        };
        let width = chain.iter().map(|d| d.name.len()).max().unwrap_or(0);
        for d in &chain {
            let tags = d.tags.join(",");
            let mut row = format!(
                "{:width$}  {}  {}  {}  {tags}",
                d.name,
                d.driver,
                state_name(d.state),
                health_name(d.health),
            );
            // Why it is not ok, as the host says it: a link that is not confirmed, for one.
            if !d.reason.is_empty() {
                row.push_str(&format!("  ({})", d.reason));
            }
            line(self.out, format_args!("{row}"))?;
        }
        // Where it ends says why: nothing further, a name the board doesn't have, or a circle.
        match &last.link {
            None => Ok(()),
            Some(link) if devices.iter().any(|d| &d.name == link) => {
                line(self.out, format_args!("-> {link}: already above"))
            }
            Some(link) => line(self.out, format_args!("-> {link}: no such device")),
        }
    }

    /// How the service runs driver packages, then every file it looked at: the driver a loaded
    /// one became, or why a refused one was refused.
    fn packages(&mut self) -> Outcome {
        let list = self.client.packages().map_err(text)?;
        let mode = match list.mode {
            PackageMode::Off => "off: no package directory is configured",
            PackageMode::Signed => "signed: only packages signed by a trusted key run",
            PackageMode::Open => "open: any package in the directory runs",
        };
        line(self.out, format_args!("packages {mode}"))?;
        if !list.note.is_empty() {
            line(self.out, format_args!("{}", list.note))?;
        }
        let width = list
            .packages
            .iter()
            .map(|p| p.path.len())
            .max()
            .unwrap_or(0);
        for package in list.packages {
            let row = match package.driver {
                Some(driver) => format!("{:width$}  loaded   {driver}", package.path),
                None => format!("{:width$}  refused  {}", package.path, package.reason),
            };
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    /// The device as a panel, from its description (see [`show`](crate::show)).
    fn show(&mut self, name: &str) -> Outcome {
        let (_, description) = self.description(name)?;
        for row in show::panel(&description) {
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    fn describe(&mut self, name: &str) -> Outcome {
        let (device, description) = self.description(name)?;
        line(
            self.out,
            format_args!("{} ({})", device.name, device.driver),
        )?;
        for contract in &description.contracts {
            line(self.out, format_args!("{}", contract.tag))?;
            for method in &contract.methods {
                let params = method
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, shape_name(&p.shape)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let returns = if method.job {
                    "job".to_string()
                } else {
                    shape_name(&method.returns)
                };
                line(
                    self.out,
                    format_args!("  {}({params}) -> {returns}", method.name),
                )?;
            }
        }
        for item in &description.items {
            let kind = match item.kind {
                ItemKind::Value => "value",
                ItemKind::Control => "control",
                ItemKind::Action => "action",
            };
            let mut row = format!("{kind} {}", item.name);
            if !item.unit.is_empty() {
                row.push_str(&format!(" [{}]", item.unit));
            }
            if let (Some(min), Some(max)) = (item.min, item.max) {
                row.push_str(&format!(" {min}..{max}"));
            }
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    fn invoke(
        &self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
        call: &Call,
    ) -> Result<Vec<u8>, String> {
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let mut reply = vec![0; REPLY_MAX.max(call.reply_len())];
        let n = self
            .client
            .call(&mut handle, method.id, &call.args, &mut reply)
            .map_err(|error| refusal(error, call))?;
        reply.truncate(n);
        Ok(reply)
    }

    fn read_item(
        &mut self,
        device: &str,
        description: &DeviceDescription,
        item: &OwnedItem,
    ) -> Outcome {
        let (contract, method) = method_of(description, item)?;
        if !method.params.is_empty() {
            return Err(format!("{} takes arguments; use call", item.name));
        }
        let call = encode_call(method, &[] as &[&str])?;
        let reply = self.invoke(device, description, contract, method, &call)?;
        self.print_value(item, method, &reply)
    }

    fn print_value(&mut self, item: &OwnedItem, method: &OwnedMethod, reply: &[u8]) -> Outcome {
        let value = render(&method.returns, method.has_out_buffer, reply, item)?;
        line(self.out, format_args!("{} = {value}", item.name))
    }

    fn read_all(&mut self, name: &str) -> Outcome {
        let (_, description) = self.description(name)?;
        for item in description
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Value)
        {
            self.read_item(name, &description, item)?;
        }
        Ok(())
    }

    fn read_one(&mut self, name: &str, wanted: &str) -> Outcome {
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        self.read_item(name, &description, item)
    }

    fn watch(&mut self, name: &str, wanted: &str, options: &[String]) -> Outcome {
        let mut period_ms = 100u64;
        let mut count = u64::MAX;
        let mut options = options.iter();
        while let Some(option) = options.next() {
            let value = options.next().ok_or_else(|| USAGE.to_string())?;
            let number = value.parse().map_err(|_| format!("{value:?}: a number"))?;
            match option.as_str() {
                "--period-ms" => period_ms = number,
                "--count" => count = number,
                _ => return Err(USAGE.to_string()),
            }
        }
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        let (contract, method) = method_of(&description, item)?;
        watchable(method)?;
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(name, tag).map_err(text)?;
        self.client
            .subscribe(&mut handle, method.id, period_ms * 1000, false)
            .map_err(text)?;
        let mut into = vec![0; REPLY_MAX];
        let mut seen = 0;
        while seen < count {
            let Some(value) = self
                .client
                .next_value(&mut into, WATCH_WAIT)
                .map_err(text)?
            else {
                continue;
            };
            self.print_value(item, method, &into[..value.len])?;
            seen += 1;
        }
        Ok(())
    }

    /// Runs a job method as a long job: prints its number, its progress as it comes, and its
    /// result in hex, and fails if it ends with anything but success.
    fn job(
        &mut self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
        args: &[u8],
    ) -> Outcome {
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let job = self
            .client
            .start_job(&mut handle, method.id, args)
            .map_err(text)?;
        line(self.out, format_args!("job {job:#x} started"))?;
        loop {
            let mut shown = Ok(());
            let out = &mut *self.out;
            let ended = self.client.wait_job(job, JOB_WAIT, &mut |done, total| {
                if shown.is_ok() {
                    shown = if total == 0 {
                        line(out, format_args!("progress {done}"))
                    } else {
                        line(out, format_args!("progress {done}/{total}"))
                    };
                }
            });
            shown?;
            let Some((status, result)) = ended else {
                continue;
            };
            if !status.is_ok() {
                let how = match status {
                    Status::CANCELLED => " (cancelled)",
                    Status::INTERRUPTED => " (interrupted)",
                    _ => "",
                };
                return Err(format!("job {job:#x} failed: status {}{how}", status.0));
            }
            let hex = result
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            return if hex.is_empty() {
                line(self.out, format_args!("job {job:#x} done"))
            } else {
                line(self.out, format_args!("job {job:#x} done: {hex}"))
            };
        }
    }

    fn set(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        if item.kind == ItemKind::Value {
            return Err(format!("{wanted} is read-only"));
        }
        let (contract, method) = method_of(&description, item)?;
        let call = encode_call(method, values)?;
        if method.job {
            return self.job(name, &description, contract, method, &call.args);
        }
        self.invoke(name, &description, contract, method, &call)?;
        Ok(())
    }

    fn call(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let (contract, method) = find_method(&description, wanted)?;
        let call = encode_call(method, values)?;
        if method.job {
            return self.job(name, &description, contract, method, &call.args);
        }
        let reply = self.invoke(name, &description, contract, method, &call)?;
        if method.returns != OwnedShape::Unit {
            let value = render_reply(method, &reply)?;
            line(self.out, format_args!("{value}"))?;
        }
        Ok(())
    }

    fn power(&mut self, name: &str, on: bool) -> Outcome {
        let device = self.device(name)?;
        self.client.set_power(device.index, on).map_err(text)
    }
}

#[cfg(test)]
mod security_unit_tests {
    use super::security_unit;

    #[test]
    fn the_unit_renders_the_view_before_the_daemon_from_the_record() {
        let unit = security_unit("/var/lib/lemnos/security.rec");
        assert!(
            unit.contains("Before=lemnosd.service lemnos-orion.service"),
            "{unit}"
        );
        assert!(unit.contains("RuntimeDirectory=board"), "{unit}");
        assert!(
            unit.contains("ExecStart=/usr/bin/lemnos-ctl security render --record \"/var/lib/lemnos/security.rec\" --dir /run/board"),
            "{unit}"
        );
        assert!(unit.contains("Type=oneshot"), "{unit}");
    }
}
