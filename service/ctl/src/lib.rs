//! `lemnos-ctl`: everything it knows about a device comes from that device's
//! description, so it works on devices nobody has written tooling for.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lemnos_client::{Client, DeviceEntry, DeviceHealth, DeviceState, Error, PackageMode, follow};
use lemnos_core::{ItemKind, Status};
use lemnos_slot_store::goodboot;
use lemnos_wire::text::{
    Call, encode_call, find_item, find_method, method_of, render, render_reply, shape_name,
    too_small, watchable,
};
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod, OwnedShape};

mod boot_add;
mod boot_dir;
#[cfg(test)]
mod boot_dir_tests;
mod boot_merge;
mod boot_partition;
mod bundle;
mod data_partition;
mod firmware_check;
mod show;

pub const USAGE: &str = "\
lemnos-ctl [--socket PATH] COMMAND
  list                           every device and how it stands
  chain DEVICE                   the devices it leads to through `link`, in order
  describe DEVICE                what it provides
  show DEVICE                    the device as a panel: name, kind, bus, state, each value's
                                 unit, range, limit and reading type
  read DEVICE [ITEM]             one value, or all of them
  watch DEVICE ITEM [--period-ms N] [--count N] [--wait-ms N | --wait-forever]
                                 each value as it comes. While the device answers `try again`,
                                 say so on stderr, and stop once it has answered for the wait
                                 (see below); --wait-ms sets that wait, --wait-forever has none
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
  security render (--record FILE | --boot-image FILE) [--dir DIR]
                                 render the switch's view from its record, with no daemon (the
                                 image runs it before the daemon starts; DIR is /run/board).
                                 --boot-image reads SECURE.REC in a boot partition image instead
  security set --boot-image FILE on|off
                                 set the switch in a boot partition image, offline (a new image or
                                 a board's copy); a board's switch is set through the daemon
  packages                       driver packages: which loaded, which were refused and why
  files DEVICE [NAME [FILE]]     the named files a node serves (`lemnos/`): its index, or one
                                 file to stdout, or to FILE, which must not exist yet
  validate [--socs DIR] [--no-live] BOARD
                                 check a board file and show the host settings it gives, with no
                                 daemon; with a `soc`, also check the live device tree against it
  boot-partition --layout-of BOARD [--socs DIR] [--slot ID --payload DIR --size BYTES --out FILE]
                 | --min-size [--payload DIR] | --verify FILE [--dtmerge TOOL]
                 | --add-to DUMP --layout-of BOARD [--socs DIR] --slot ID --payload DIR
                   [--firmware DIR] [--set N] [--replace-trial] --out FILE --changed-out LIST
                   --undo-out UNDO | --undo UNDO --of FILE --out FILE
                                  write the boot partition image of a board (its payload bays and
                                  the firmware text), print its minimum size, or verify an image
                                  or a dump of a real partition, read-only; --dtmerge TOOL also
                                  applies the declared overlays on this host, not loaded by firmware
                                  --add-to adds the board's files to a dump of a FAT16 boot
                                  partition another system boots from (never a device), beside
                                  that system's files, and writes the changed sector ranges and
                                  an undo file; --undo puts those ranges back
  firmware-check IMAGE --dtb NAME... [--trial | --dir PREFIX] [--filter NAME]... [--kernel NAME]...
                 [--needs-overlay NAME]... [--boot-count N]
                                 read a boot partition image as the Pi firmware does, read-only,
                                 and print one ok/BAD line per rule for every file it loads (exact
                                 sizes, default names, overlays that apply, case as vfat writes it);
                                 non-zero exit when any rule fails
  bundle --layout-of BOARD [--socs DIR] --slot ID --payload DIR --rootfs FILE --verity-table FILE
         --product P --version V [--variant TEXT] [--kernel TEXT] [--display-name TEXT]
         [--key-file FILE [--expect-key-id HEX]] [--payload-out DIR] --out FILE
         | --verify FILE [--layout-of BOARD [--socs DIR]] [--pubkey FILE]
                                 write a slot bundle (its manifest, the erofs image with its hash tree
                                 held to the verity table, the signature when --key-file names a raw
                                 Ed25519 seed) from the same payload directory `boot-partition`
                                 takes, or verify one with the installer's own checks, read-only:
                                 one line per check
  data-partition --layout-of BOARD [--socs DIR] --install BUNDLE --boot-image FILE --keys FILE
                 --time UTC --data-size BYTES --out DIR [--switch off]
                 | --verify DIR [--layout-of BOARD [--socs DIR]] [--boot-image FILE] [--deep]
                                 write the data partition's tree for a board's first slot, as the
                                 daemon's install, trial and confirm leave it, with nothing written
                                 outside DIR (which must not exist), or verify one, read-only
  grow-retry DATA
                                 clear a data-partition grow the boot refused because its last
                                 attempt never returned, so the next boot tries it once more
                                 (DATA is the data partition's mount point; docs/slots.md)
  tree [--socs DIR | --map FILE] BOARD [-o FILE] [--boot FILE] [--uboot FILE]
                                 [--uboot-config FILE]
                                 [--modprobe FILE] [--cmdline FILE] [--udev FILE] [--mdev FILE]
                                 [--bridge-env FILE] [--bridge-dropin FILE]
                                 [--security-unit FILE] [--good-boot-unit FILE]
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
                                 and the bridge's settings (--bridge-env FILE): its
                                 `LEMNOS_ORION_LINKS` line for the board's USB link and its
                                 `LEMNOS_ORION_FILES` and `LEMNOS_ORION_STREAMS` lines for the
                                 devices that take files, with the unit drop-in that lets it write
                                 their directories (--bridge-dropin FILE), with no daemon";

pub mod bridge_env;
#[cfg(test)]
mod bridge_env_tests;
mod files;
mod grow_retry;

const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
const REPLY_MAX: usize = 64 * 1024;
const WATCH_WAIT: Duration = Duration::from_secs(2);
/// The least `watch` waits, without `--wait-ms`, while a device answers `try again`. A device
/// that powers up, or is retried after a failed step, can take this long to give its first value:
/// the drivers' own retry schedule (`lemnos-drivers-bmi088`, a first retry at 100 ms, doubling, to
/// the 8 s cap) reaches its sixth retry at 6.3 s and the seventh at 12.7 s, so 10 s covers the
/// first six. The power-up steps themselves are milliseconds (3 ms for the magnetometer, 10 ms
/// for the OV9782's first read).
const WAIT_FLOOR: Duration = Duration::from_secs(10);
/// Without `--wait-ms`, `watch` waits this many sampling periods of silence, or [`WAIT_FLOOR`] if
/// that is longer. Fifty periods of silence mean the device is not sampling at all.
const WAIT_PERIODS: u64 = 50;
/// How long `call` waits for a long job between looks at its progress. It waits as long as
/// the job runs.
const JOB_WAIT: Duration = Duration::from_secs(1);

type Outcome = Result<(), String>;

/// What a device answers to a read while a subscription is silent (see `Tool::watch`).
enum Answer {
    Value(Vec<u8>),
    Again,
    Other,
}

fn text(error: Error) -> String {
    error.to_string()
}

/// A call's error as the tool prints it. A reply that did not fit the out buffer says how to ask
/// for more ([`too_small`]).
/// The line for a call whose answer never came, from the outcome the service gave (or why it gave
/// none). `Ok` only for an applied call, which exits 0; the others are `Err`, which exits nonzero.
fn lost_reply(
    device: &str,
    error: &Error,
    outcome: Result<lemnos_client::Outcome, Error>,
) -> Result<String, String> {
    let (applied, outcome) = match outcome {
        Ok(lemnos_client::Outcome::Applied(status)) => {
            (true, format!("applied (status {})", status.0))
        }
        Ok(lemnos_client::Outcome::NotApplied(status)) => {
            (false, format!("not applied (status {})", status.0))
        }
        Ok(lemnos_client::Outcome::Pending) => (false, "still running".to_string()),
        Ok(lemnos_client::Outcome::Unknown) => (false, "unknown".to_string()),
        Err(why) => (false, format!("unknown ({why})")),
    };
    let text = format!("{device}: {error}; outcome: {outcome}");
    if applied { Ok(text) } else { Err(text) }
}

/// Whether a call's error leaves open if it applied: the answer never came back.
fn reply_lost(error: &Error) -> bool {
    matches!(error, Error::TimedOut | Error::Disconnected)
        || matches!(error, Error::Refused { status, .. } if *status == Status::TIMED_OUT)
}

/// A token for one call: unique to this process and moment, and never 0.
fn call_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(0));
    let pid = u64::from(std::process::id());
    (nanos ^ (pid << 40) ^ NEXT.fetch_add(1, Ordering::Relaxed)) | 1
}

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

/// The sentence that says why a device is not running, `None` when it is running. It is the one
/// `read` and `watch` print, and `call` prints it when the host refuses a call to such a device.
fn not_running(name: &str, device: &DeviceEntry) -> Option<String> {
    if device.state == DeviceState::Running {
        return None;
    }
    let state = state_name(device.state);
    Some(match device.reason.as_str() {
        "" => format!("{name} is {state}"),
        // The host's reason for a retry already starts with the state, as `retrying: attempt 1,`.
        reason if reason.starts_with(state) => format!("{name} is {reason}"),
        reason => format!("{name} is {state}: {reason}"),
    })
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
    err: &'a mut dyn Write,
}

/// How long `watch` waits while the device answers `try again`, before it gives up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Patience {
    /// Not given: [`WAIT_FLOOR`] or [`WAIT_PERIODS`] periods, whichever is longer.
    Derived,
    /// `--wait-ms N`.
    Bound(Duration),
    /// `--wait-forever`: never gives up.
    Forever,
}

/// The longest `watch` waits in silence for a device with a sampling period of `period_ms`, or
/// `None` for no bound.
fn patience_bound(period_ms: u64, patience: Patience) -> Option<Duration> {
    match patience {
        Patience::Derived => Some(WAIT_FLOOR.max(Duration::from_millis(
            WAIT_PERIODS.saturating_mul(period_ms),
        ))),
        Patience::Bound(bound) => Some(bound),
        Patience::Forever => None,
    }
}

/// Runs one command. `args` excludes the program's name. Messages that are not the result go to
/// stderr.
pub fn run(args: &[String], out: &mut dyn Write) -> Outcome {
    run_with_stderr(args, out, &mut std::io::stderr())
}

/// [`run`], with the stream the notices go to given.
pub fn run_with_stderr(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Outcome {
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
    if command == "security"
        && tail.first().is_some_and(|word| word == "set")
        && tail.get(1).is_some_and(|word| word == "--boot-image")
    {
        return security_set_image(&tail[1..], out);
    }
    if command == "boot-partition" {
        return boot_partition::run(tail, out);
    }
    if command == "firmware-check" {
        return firmware_check::run(tail, out);
    }
    if command == "grow-retry" {
        return grow_retry::run(tail, out);
    }
    if command == "bundle" {
        return bundle::run(tail, out);
    }
    if command == "data-partition" {
        return data_partition::run(tail, out);
    }
    let client = Client::connect(&socket, "lemnos-ctl").map_err(|e| format!("{socket}: {e}"))?;
    let mut tool = Tool { client, out, err };
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
    // A backend resource name is parsed with the one grammar the backend reads (`lemnos_linux_names`):
    // a name of no known form, or a malformed one, is refused by name on every board. A name the SoC
    // map can check (an `i2c-N` the SoC has no bus for, an `i2c:of=` node it lacks, a `udc:` no
    // controller of the SoC has) is then refused against the map, which is the SoC's
    // (`socs/<soc>.toml`, or `--socs`). A board that names such a name and a `soc` needs that map.
    if let Some(refusal) = lemnos_devtree::resources::grammar_refusals(&board)
        .into_iter()
        .next()
    {
        return Err(format!("{path}: {refusal}"));
    }
    if board.board.soc.is_some() && lemnos_devtree::resources::any_checkable(&board) {
        let map = load_map(&board, socs).map_err(|e| format!("{path}: {e}"))?;
        if let Some(refusal) = lemnos_devtree::resources::refusals(&board, &map)
            .into_iter()
            .next()
        {
            return Err(format!("{path}: {refusal}"));
        }
    }
    // Each driver's own check of its settings, the one the daemon's start runs before it makes a
    // device: a setting a driver refuses is refused here, by device and key.
    lemnos_host::check_settings(&board, &lemnosd::builtin::drivers())
        .map_err(|e| format!("{path}: {e}"))?;
    // The `[boot]` keys of a board whose slot device keeps a boot partition record (the firmware's
    // names and files, the EEPROM keys): the rules the image builder applies, so a board file is
    // refused here, not when its image is built.
    if board.devices.iter().any(|d| d.driver == "system-slots") {
        let record = boot_dir::record_of(&board).map_err(|e| format!("{path}: {e}"))?;
        if record == "tryboot" || record == "uboot" {
            boot_partition::check_board_boot(&board).map_err(|e| format!("{path}: {e}"))?;
        }
    }
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
    // The board's declared watchdog period (the hw-watchdog device's `timeout_s`) is held to the slot
    // contract's bound (`contracts/slot-kernel.toml`, `[watchdog]`): a period outside 1 to
    // WATCHDOG_MAX_APPLIED_S is refused here, under the reason slot-init gives a device it refuses.
    for device in &board.devices {
        if device.driver != "hw-watchdog" {
            continue;
        }
        let Some(value) = device.config.get("timeout_s") else {
            continue;
        };
        let outside = match value.as_integer() {
            Some(seconds) if (1..=WATCHDOG_MAX_APPLIED_S).contains(&seconds) => None,
            Some(seconds) => Some(format!("timeout_s is {seconds} s")),
            None => Some(format!(
                "timeout_s is a {}, not a whole number of seconds",
                value.type_str()
            )),
        };
        if let Some(what) = outside {
            return Err(format!(
                "{path}: device {:?}: watchdog-period-outside-bound: {what}; a slot board's watchdog period is 1 to {WATCHDOG_MAX_APPLIED_S} s (contracts/slot-kernel.toml, [watchdog] max_applied_s)",
                device.id
            ));
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

/// The longest watchdog period a board may declare, in seconds: `max_applied_s` of
/// `contracts/slot-kernel.toml`'s `[watchdog]` (a unit test reads the file and holds the two equal).
pub const WATCHDOG_MAX_APPLIED_S: i64 = 30;

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
pub fn driver_manifests() -> Vec<lemnos_core::Manifest> {
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

/// `tree BOARD [-o FILE] [--boot FILE] [--uboot FILE]`: the overlay, the boot fragment and the U-Boot
/// environment locator for a board. The boot partition is `boot-partition`'s (I5i8).
/// What `tree --boot-dir` says now: the directory it wrote is the image builder's (I5i8).
pub const BOOT_DIR_REMOVED: &str = "tree --boot-dir was removed: the boot partition is written as one image by `lemnos-ctl boot-partition --layout-of BOARD --slot ID --payload DIR --size BYTES --out FILE`, which holds its files, pointers and payload bays (docs/slots.md, \"The boot partition: payload bays\"); `tree --manifest-boot`, `--cmdline-slot` and `--uboot-config` remain";

fn tree(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut socs = None;
    let mut map_file = None;
    let mut overlay_out = None;
    let mut boot_out = None;
    let mut uboot_out = None;
    let mut uboot_config_out = None;
    let mut modprobe_out = None;
    let mut cmdline_out = None;
    let mut udev_out = None;
    let mut mdev_out = None;
    let mut bridge_env_out = None;
    let mut bridge_dropin_out = None;
    let mut security_out = None;
    let mut good_boot_out = None;
    let mut manifest_boot_out = None;
    let mut cmdline_slot_out = None;
    let mut slot = None;
    let mut board_path = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].as_str(), value) {
            ("--socs", Some(dir)) => socs = Some(dir),
            ("--map", Some(file)) => map_file = Some(file),
            ("-o", Some(file)) => overlay_out = Some(file),
            ("--boot", Some(file)) => boot_out = Some(file),
            ("--uboot", Some(file)) => uboot_out = Some(file),
            ("--uboot-config", Some(file)) => uboot_config_out = Some(file),
            ("--modprobe", Some(file)) => modprobe_out = Some(file),
            ("--cmdline", Some(file)) => cmdline_out = Some(file),
            ("--udev", Some(file)) => udev_out = Some(file),
            ("--mdev", Some(file)) => mdev_out = Some(file),
            ("--bridge-env", Some(file)) => bridge_env_out = Some(file),
            ("--bridge-dropin", Some(file)) => bridge_dropin_out = Some(file),
            ("--security-unit", Some(file)) => security_out = Some(file),
            ("--good-boot-unit", Some(file)) => good_boot_out = Some(file),
            ("--manifest-boot", Some(file)) => manifest_boot_out = Some(file),
            ("--cmdline-slot", Some(file)) => cmdline_slot_out = Some(file),
            ("--slot", Some(id)) => slot = Some(id),
            ("--boot-dir", _) => return Err(BOOT_DIR_REMOVED.to_string()),
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
    // The rules come before the overlay: a board whose overlay is refused still gets its late grants.
    if let Some(file) = udev_out {
        std::fs::write(&file, lemnos_devtree::udev::fragment(&board))
            .map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = mdev_out {
        std::fs::write(&file, lemnos_devtree::mdev::fragment(&board))
            .map_err(|e| format!("{file}: {e}"))?;
    }
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
    if let Some(file) = bridge_env_out {
        let text = bridge_env::text(&board, &driver_manifests())
            .map_err(|e| format!("{path}: --bridge-env: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = bridge_dropin_out {
        let text = bridge_env::dropin(&board, &driver_manifests())
            .map_err(|e| format!("{path}: --bridge-dropin: {e}"))?;
        std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = good_boot_out {
        let device = board
            .devices
            .iter()
            .find(|device| device.driver == "system-slots")
            .ok_or_else(|| {
                format!("{path}: --good-boot-unit: this board has no system-slots device, so there is no boot to judge")
            })?;
        let value = match device.config.get(goodboot::KEY) {
            None => None,
            Some(value) => Some(value.as_integer().ok_or_else(|| {
                format!(
                    "{path}: --good-boot-unit: system-slots device \"{}\": {} must be an integer",
                    device.id,
                    goodboot::KEY
                )
            })?),
        };
        let deadline = goodboot::deadline_from_board(value).map_err(|why| {
            format!(
                "{path}: --good-boot-unit: system-slots device \"{}\": {why}",
                device.id
            )
        })?;
        std::fs::write(&file, good_boot_dropin(deadline)).map_err(|e| format!("{file}: {e}"))?;
    }
    if let Some(file) = security_out {
        let security = board
            .security
            .as_ref()
            .ok_or_else(|| format!("{path}: --security-unit: the board has no [security] table"))?;
        let unit =
            security_unit(&security.record).map_err(|e| format!("{path}: --security-unit: {e}"))?;
        std::fs::write(&file, unit).map_err(|e| format!("{file}: {e}"))?;
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

/// The drop-in that gives the good-boot unit (`lemnos-good-boot.service`) this board's deadline:
/// the command's `--deadline-s` and the unit's start timeout, which are the same by construction.
fn good_boot_dropin(deadline: u32) -> String {
    format!(
        "# Generated by `lemnos-ctl tree --good-boot-unit` from the system-slots device's {key}.\n\
         # The good-boot deadline of this board: a boot that is not good within it is restarted.\n\
         # Change the board file, not this drop-in.\n\
         [Service]\n\
         ExecStart=\n\
         ExecStart=/usr/bin/lemnos-good-boot --deadline-s {deadline}\n\
         TimeoutStartSec={deadline}\n",
        key = goodboot::KEY,
    )
}

/// The unit that renders the security switch's view before the daemon and the comm core start. It
/// makes `/run/board` (the directory `/etc/board` links to on the image) and writes `security` into it
/// from the record. An absent record renders off; an unreadable one renders on, as the switch reads it.
///
/// It also makes the places the daemon writes later, so lemnosd chowns nothing. It runs as the
/// daemon's user and group: `/run/board` is theirs, mode 0775, so the daemon (and the bridge, in the
/// group) re-render the view; a record file's directory is a state directory under
/// `/var/lib/lemnos`, theirs, mode 0750, and systemd gives a record already there to them too. The lemnosd drop-in `security.conf` grants both paths under
/// `ProtectSystem=strict`.
///
/// # Errors
///
/// A record file outside `/var/lib/lemnos`: no drop-in grants it, so the daemon could never write it.
fn security_unit(record: &str) -> Result<String, String> {
    let state = {
        let dir = std::path::Path::new(record)
            .parent()
            .and_then(|d| d.strip_prefix("/var/lib").ok())
            .filter(|d| d.starts_with("lemnos"))
            .and_then(|d| d.to_str())
            .ok_or_else(|| {
                format!(
                    "the record {record:?} is not under /var/lib/lemnos: the daemon's unit grants \
                     only that directory (packaging/systemd/lemnosd.service.d/security.conf), so the \
                     record could never be written"
                )
            })?;
        format!("StateDirectory={dir}\nStateDirectoryMode=0750\n")
    };
    let render = format!(
        "ExecStart=/usr/bin/lemnos-ctl security render --record \"{record}\" --dir /run/board\n"
    );
    Ok(format!(
        "# Generated by `lemnos-ctl tree --security-unit` from the board file's [security] table.\n\
         # Renders the security switch's view (/run/board/security) from its record before the daemon\n\
         # and the comm core start, and makes the places the daemon writes later: /run/board and the\n\
         # record's directory, owned by the daemon's user. Change the board file, not this unit.\n\
         [Unit]\n\
         Description=Lemnos: the board security view\n\
         DefaultDependencies=no\n\
         After=local-fs.target systemd-sysusers.service\n\
         Before=lemnosd.service lemnos-orion.service\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         User=lemnos\nGroup=lemnos\n\
         RuntimeDirectory=board\n\
         RuntimeDirectoryMode=0775\n\
         RuntimeDirectoryPreserve=yes\n\
         # Runs as the daemon's user, so /run/board and the record's directory are the daemon's, and\n\
         # systemd gives an existing record to it too. It reads one record and writes one file.\n\
         CapabilityBoundingSet=\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ProtectHome=yes\n\
         PrivateTmp=yes\n\
         PrivateDevices=yes\n\
         PrivateNetwork=yes\n\
         PrivateUsers=no\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectKernelLogs=yes\n\
         ProtectControlGroups=yes\n\
         ProtectHostname=yes\n\
         ProtectClock=yes\n\
         ProtectProc=invisible\n\
         ProcSubset=pid\n\
         LockPersonality=yes\n\
         MemoryDenyWriteExecute=yes\n\
         RestrictRealtime=yes\n\
         RestrictSUIDSGID=yes\n\
         RestrictNamespaces=yes\n\
         RemoveIPC=yes\n\
         SystemCallArchitectures=native\n\
         RestrictAddressFamilies=AF_UNIX\n\
         SystemCallFilter=@system-service\n\
         SystemCallFilter=~@privileged @resources\n\
         SystemCallErrorNumber=EPERM\n\
         UMask=0022\n\
         IPAddressDeny=any\n\
         DevicePolicy=closed\n\
         {state}\
         {render}\
         \n\
         [Install]\n\
         WantedBy=sysinit.target\n"
    ))
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

/// The switch's record, named by exactly one of `--record FILE` (a file) and `--boot-image FILE` (the
/// `SECURE.REC` of a boot partition image): its name for the messages, and the store.
fn switch_store(
    record: Option<String>,
    boot_image: Option<String>,
) -> Result<(String, Box<dyn lemnos_slot_store::security::Store>), String> {
    use lemnos_slot_store::security::{BootSwitch, PlainFile};
    match (record, boot_image) {
        (Some(path), None) => Ok((path.clone(), Box::new(PlainFile::new(path)))),
        (None, Some(image)) => Ok((image.clone(), Box::new(BootSwitch::new(image)))),
        _ => Err("security needs exactly one of --record FILE and --boot-image FILE".into()),
    }
}

/// `security set --boot-image FILE on|off`: the switch in a boot partition image, written offline.
/// A board's switch is set through the daemon; this writes only the image it is given.
fn security_set_image(args: &[String], out: &mut dyn Write) -> Outcome {
    let [flag, image, value] = args else {
        return Err(USAGE.to_string());
    };
    if flag != "--boot-image" {
        return Err(USAGE.to_string());
    }
    let on = match value.as_str() {
        "on" => true,
        "off" => false,
        _ => return Err(USAGE.to_string()),
    };
    let store = lemnos_slot_store::security::BootSwitch::new(image.as_str());
    let generation = lemnos_slot_store::security::Store::write(&store, on)
        .map_err(|error| format!("{image}: the switch can't be set: {error}"))?;
    line(
        out,
        format_args!(
            "switch {} in {image}: generation {generation}",
            if on { "on" } else { "off" }
        ),
    )
}

/// `security render`: the view from the record, written with no daemon. An absent record is off;
/// an unreadable one renders on, as the switch reads it.
fn security_render(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut record = None;
    let mut boot_image = None;
    let mut dir = lemnos_slot_store::security::RUN_DIR.to_string();
    let mut i = 0;
    while i < args.len() {
        match (args[i].as_str(), args.get(i + 1)) {
            ("--record", Some(path)) if record.is_none() => record = Some(path.clone()),
            ("--boot-image", Some(path)) if boot_image.is_none() => boot_image = Some(path.clone()),
            ("--dir", Some(path)) => dir.clone_from(path),
            _ => return Err(USAGE.to_string()),
        }
        i += 2;
    }
    let (record, store) = switch_store(record, boot_image)?;
    let switch = match store.read() {
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

    /// Refuses to read or watch a device that is not running: it has no items, and the host
    /// answers a call to it with NO_DEVICE. The error says its state and reason, so the empty list
    /// is never printed as if the device had nothing to read.
    fn must_run(&self, name: &str) -> Outcome {
        let device = self.device(name)?;
        match not_running(name, &device) {
            Some(text) => Err(text),
            None => Ok(()),
        }
    }

    /// A refused call's text. On a device that is not running, the sentence read and watch print
    /// (its state and reason) replaces the host's refusal, which says only that the call failed.
    /// Only the refusal path looks the device up again: a call that runs costs nothing extra.
    /// The outcome of a call whose answer never came, as the service last heard of it. `Ok` is the
    /// line for a call that applied, which exits 0 because the call took effect. Anything else is an
    /// `Err` and exits nonzero, as a plain timeout does.
    fn asked(&self, device: &str, token: u64, error: &Error) -> Result<String, String> {
        lost_reply(device, error, self.client.outcome(token))
    }

    fn refused(&self, device: &str, text: String) -> String {
        match self.device(device) {
            Ok(entry) => not_running(device, &entry).unwrap_or(text),
            Err(_) => text,
        }
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
            // The jobs open on it, from the host's table: `job 0x1a2b00000003 running 40%`, last on
            // the row, and nothing when there is none.
            if !d.jobs.is_empty() {
                let jobs: Vec<String> = d.jobs.iter().map(lemnos_wire::RunningJob::words).collect();
                row.push_str(&format!("  {}", jobs.join(", ")));
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
        let (device, description) = self.description(name)?;
        for row in show::panel(&description, Some(health_name(device.health))) {
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
        // The long jobs open on it, from the host's table.
        for job in &description.jobs {
            line(self.out, format_args!("{}", job.words()))?;
        }
        Ok(())
    }

    /// Makes the call. `None` is a call that applied whose reply was lost: its line is printed here,
    /// and the caller has no value to show.
    fn invoke(
        &mut self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
        call: &Call,
    ) -> Result<Option<Vec<u8>>, String> {
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let mut reply = vec![0; REPLY_MAX.max(call.reply_len())];
        let token = call_token();
        let n = match self
            .client
            .call_token(&mut handle, method.id, &call.args, &mut reply, token)
        {
            Ok(n) => n,
            // No answer came back, so the call may still have applied: the service keeps the outcome.
            Err(error) if reply_lost(&error) => {
                let applied = self.asked(device, token, &error)?;
                line(self.out, format_args!("{applied}"))?;
                return Ok(None);
            }
            Err(error) => return Err(self.refused(device, refusal(error, call))),
        };
        reply.truncate(n);
        Ok(Some(reply))
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
        let Some(reply) = self.invoke(device, description, contract, method, &call)? else {
            return Ok(());
        };
        self.print_value(item, method, &reply)
    }

    fn print_value(&mut self, item: &OwnedItem, method: &OwnedMethod, reply: &[u8]) -> Outcome {
        let value = render(&method.returns, method.has_out_buffer, reply, item)?;
        line(self.out, format_args!("{} = {value}", item.name))
    }

    fn read_all(&mut self, name: &str) -> Outcome {
        self.must_run(name)?;
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
        self.must_run(name)?;
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        self.read_item(name, &description, item)
    }

    /// Prints each value of `wanted` as it comes, `count` of them. While the device answers
    /// `try again` (`AGAIN`) the subscription has no value to give, so `watch` asks the device
    /// itself once per silent slice: a value it gives is printed, `AGAIN` is waited on, and any
    /// other answer keeps waiting as it always did. Once `AGAIN` has gone on for the
    /// [`patience_bound`] since the last value, `watch` fails, with the values it printed counted
    /// in the message.
    fn watch(&mut self, name: &str, wanted: &str, options: &[String]) -> Outcome {
        let mut period_ms = 100u64;
        let mut count = u64::MAX;
        let mut explicit = None;
        let mut forever = false;
        let mut i = 0;
        while i < options.len() {
            match options[i].as_str() {
                "--wait-forever" => {
                    forever = true;
                    i += 1;
                    continue;
                }
                option @ ("--period-ms" | "--count" | "--wait-ms") => {
                    let value = options.get(i + 1).ok_or_else(|| USAGE.to_string())?;
                    let number: u64 = value.parse().map_err(|_| format!("{value:?}: a number"))?;
                    match option {
                        "--period-ms" => period_ms = number,
                        "--count" => count = number,
                        _ => explicit = Some(number),
                    }
                    i += 2;
                }
                _ => return Err(USAGE.to_string()),
            }
        }
        let patience = match (explicit, forever) {
            (Some(_), true) => {
                return Err("--wait-ms and --wait-forever: give one of them".to_string());
            }
            (Some(0), false) => {
                return Err(
                    "--wait-ms 0: a wait must be above 0 (--wait-forever waits without one)"
                        .to_string(),
                );
            }
            (Some(ms), false) => Patience::Bound(Duration::from_millis(ms)),
            (None, true) => Patience::Forever,
            (None, false) => Patience::Derived,
        };
        let bound = patience_bound(period_ms, patience);
        self.must_run(name)?;
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
        let mut seen = 0u64;
        // The start of the silence: the subscription, or the last value.
        let mut silent_since = Instant::now();
        let mut waiting = false;
        while seen < count {
            let left = bound.map(|bound| bound.saturating_sub(silent_since.elapsed()));
            // The slice ends at the bound, so a bound is met on time; past it (or with none) the
            // slice is WATCH_WAIT, and a device that is not answering `try again` keeps waiting.
            let slice = match left {
                Some(left) if !left.is_zero() => left.min(WATCH_WAIT),
                _ => WATCH_WAIT,
            };
            if let Some(value) = self.client.next_value(&mut into, slice).map_err(text)? {
                self.print_value(item, method, &into[..value.len])?;
                seen += 1;
                silent_since = Instant::now();
                waiting = false;
                continue;
            }
            match self.ask(name, &description, contract, method)? {
                Answer::Value(reply) => {
                    self.print_value(item, method, &reply)?;
                    seen += 1;
                    silent_since = Instant::now();
                    waiting = false;
                }
                Answer::Again => {
                    if !waiting {
                        waiting = true;
                        let wait = match bound {
                            Some(bound) => format!(
                                "waiting up to {} ms (--wait-ms N sets the wait, --wait-forever waits without one)",
                                bound.as_millis()
                            ),
                            None => "waiting without a bound (--wait-forever)".to_string(),
                        };
                        line(
                            self.err,
                            format_args!(
                                "{name} {wanted}: the device answered \"try again\"; {wait}"
                            ),
                        )?;
                    }
                    if bound.is_some_and(|bound| silent_since.elapsed() >= bound) {
                        let after = if seen == 0 {
                            String::new()
                        } else {
                            format!(" after {seen} value{}", if seen == 1 { "" } else { "s" })
                        };
                        return Err(format!(
                            "{name} {wanted}: no value in {} ms{after}; the device kept answering \"try again\"",
                            bound.map_or(0, |bound| bound.as_millis())
                        ));
                    }
                }
                Answer::Other => waiting = false,
            }
        }
        Ok(())
    }

    /// One read of `method`, asked while a subscription is silent: what the device answers now.
    fn ask(
        &self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
    ) -> Result<Answer, String> {
        let call = encode_call(method, &[] as &[&str])?;
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let mut reply = vec![0; REPLY_MAX.max(call.reply_len())];
        Ok(
            match self
                .client
                .call(&mut handle, method.id, &call.args, &mut reply)
            {
                Ok(n) => {
                    reply.truncate(n);
                    Answer::Value(reply)
                }
                Err(Error::Refused { status, .. }) if status == Status::AGAIN => Answer::Again,
                Err(_) => Answer::Other,
            },
        )
    }

    /// Runs a job method as a long job: prints its number, its progress as it comes, and its
    /// result in the form its contract declares (text as it is, bytes in hex), and fails if it ends with anything but success.
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
            .map_err(|error| self.refused(device, text(error)))?;
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
                    Status::TORN => " (torn: a frame's buffer was reused while it was read)",
                    Status::FAILED_PRECONDITION => {
                        " (failed precondition: a step the call needs has not been taken)"
                    }
                    _ => "",
                };
                // A job the host ended with a reason says it in its result (UTF-8).
                let why = match std::str::from_utf8(&result) {
                    Ok(text) if !text.is_empty() => format!(": {text}"),
                    _ => String::new(),
                };
                return Err(format!(
                    "job {job:#x} failed: status {}{how}{why}",
                    status.0
                ));
            }
            // The result in the form the contract declares: text as it is, bytes as hex.
            let shown = match method.returns {
                OwnedShape::Str => std::str::from_utf8(&result)
                    .map(str::to_owned)
                    .map_err(|_| {
                        format!("job {job:#x} done, but its result is not the UTF-8 text its contract declares")
                    })?,
                _ => result
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            return if shown.is_empty() {
                line(self.out, format_args!("job {job:#x} done"))
            } else {
                line(self.out, format_args!("job {job:#x} done: {shown}"))
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
        let Some(reply) = self.invoke(name, &description, contract, method, &call)? else {
            return Ok(());
        };
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
        let unit = security_unit("/var/lib/lemnos/security.rec").unwrap();
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

#[cfg(test)]
mod good_boot_unit_tests {
    use super::good_boot_dropin;

    #[test]
    fn the_drop_in_gives_the_waiter_and_its_timeout_the_same_deadline() {
        let text = good_boot_dropin(420);
        assert!(
            text.contains("ExecStart=/usr/bin/lemnos-good-boot --deadline-s 420\n"),
            "{text}"
        );
        assert!(text.contains("TimeoutStartSec=420\n"), "{text}");
        assert!(
            text.contains("ExecStart=\n"),
            "the base unit's command is cleared first: {text}"
        );
    }
}

#[cfg(test)]
mod watchdog_bound_tests {
    use super::{WATCHDOG_MAX_APPLIED_S, validate};

    /// The example board with its keep-alive watchdog's `timeout_s` set to `seconds`, written to a
    /// temporary file. The example validates as it is (the board-files test says so).
    fn board_with_timeout(seconds: &str, name: &str) -> std::path::PathBuf {
        let text = include_str!("../../../docs/examples/full-board.toml");
        assert!(
            text.contains("timeout_s = 15"),
            "the example names the 15 s timeout"
        );
        let path =
            std::env::temp_dir().join(format!("lemnos-wdt-{}-{name}.toml", std::process::id()));
        std::fs::write(
            &path,
            text.replace("timeout_s = 15", &format!("timeout_s = {seconds}")),
        )
        .unwrap();
        path
    }

    /// A declared period above the bound is refused by name. A period of 0 never reaches this check: the
    /// hw-watchdog driver refuses it first, as a setting out of range.
    #[test]
    fn a_board_whose_watchdog_period_is_outside_the_bound_is_refused_by_name() {
        let path = board_with_timeout("60", "long");
        let result = validate(path.to_str().unwrap(), None, false, &mut Vec::new());
        std::fs::remove_file(&path).unwrap();
        let error = result.expect_err("a period above the bound is refused");
        assert!(error.contains("watchdog-period-outside-bound"), "{error}");
        assert!(error.contains("timeout_s is 60 s"), "{error}");

        let path = board_with_timeout("0", "zero");
        let result = validate(path.to_str().unwrap(), None, false, &mut Vec::new());
        std::fs::remove_file(&path).unwrap();
        let error = result.expect_err("a period of 0 is refused");
        assert!(error.contains("timeout_s"), "{error}");
    }

    #[test]
    fn a_board_whose_watchdog_period_is_inside_the_bound_passes_that_check() {
        let path = board_with_timeout("15", "inside");
        let result = validate(path.to_str().unwrap(), None, false, &mut Vec::new());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result, Ok(()));
    }

    /// The bound this crate checks is the contract's: the two cannot drift apart unnoticed.
    #[test]
    fn the_board_bound_is_the_contracts_max_applied_s() {
        let file: toml::Table = include_str!("../../../contracts/slot-kernel.toml")
            .parse()
            .unwrap();
        let max = file["watchdog"]["max_applied_s"].as_integer().unwrap();
        assert_eq!(max, WATCHDOG_MAX_APPLIED_S);
    }
}

#[cfg(test)]
mod watch_wait_tests {
    use super::*;

    #[test]
    fn the_derived_bound_is_the_floor_or_fifty_periods_whichever_is_longer() {
        let derived = |period_ms| patience_bound(period_ms, Patience::Derived);
        // At the default 100 ms, fifty periods are 5 s: the floor of 10 s is what waits.
        assert_eq!(derived(100), Some(Duration::from_secs(10)));
        assert_eq!(derived(0), Some(Duration::from_secs(10)));
        // At 1 s, fifty periods are 50 s, past the floor.
        assert_eq!(derived(1000), Some(Duration::from_secs(50)));
    }

    #[test]
    fn an_explicit_bound_replaces_the_derived_one_and_forever_has_none() {
        let bound = Duration::from_millis(250);
        assert_eq!(patience_bound(100, Patience::Bound(bound)), Some(bound));
        assert_eq!(patience_bound(1000, Patience::Bound(bound)), Some(bound));
        assert_eq!(patience_bound(100, Patience::Forever), None);
    }
}

#[cfg(test)]
mod lost_reply_tests {
    use super::{Error, lost_reply};
    use lemnos_client::Outcome;
    use lemnos_core::Status;

    #[test]
    fn only_an_applied_call_is_ok_and_every_other_outcome_is_an_error() {
        let lost = Error::TimedOut;
        assert_eq!(
            lost_reply("s", &lost, Ok(Outcome::Applied(Status::OK))),
            Ok("s: no answer in time; outcome: applied (status 0)".to_string())
        );
        assert_eq!(
            lost_reply("s", &lost, Ok(Outcome::NotApplied(Status::NOT_FOUND))),
            Err("s: no answer in time; outcome: not applied (status -2)".to_string())
        );
        assert_eq!(
            lost_reply("s", &lost, Ok(Outcome::Pending)),
            Err("s: no answer in time; outcome: still running".to_string())
        );
        assert_eq!(
            lost_reply("s", &lost, Ok(Outcome::Unknown)),
            Err("s: no answer in time; outcome: unknown".to_string())
        );
        assert_eq!(
            lost_reply("s", &lost, Err(Error::Disconnected)),
            Err(
                "s: no answer in time; outcome: unknown (not connected to the service)".to_string()
            )
        );
    }
}
