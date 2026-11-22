//! The root phase: what lemnosd does as root before it drops to its own user.
//!
//! lemnosd starts as root when it is given `--user` (or `LEMNOSD_USER`). It then runs the privileged
//! start, every step in [`STEPS`] in order, with the board file, and drops to that user and group,
//! keeping only the capabilities in [`KEEP`]. It verifies the state after the drop and refuses to
//! run on a mismatch. Started without root, it runs as it is and says which capabilities it lacks.
//! The system's own unit and init script use this one path: neither gives the daemon a user.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use lemnos_host::board::BoardFile;
use lemnos_linux_sys::privilege::{self, CAP_SYS_RAWIO, Observed, Plan};

/// The devices a step set aside, by id, with the reason. A step returns these for a device it cannot
/// set up; the device is refused at start and shows failed, and the others run.
pub type Refusals = Vec<(String, String)>;

/// The capabilities lemnosd keeps after the drop. Today only `CAP_SYS_RAWIO`, for the kernel module's
/// register mapping; nothing else is needed by any driver built into the daemon.
pub const KEEP: &[u32] = &[CAP_SYS_RAWIO];

/// Where the drop goes, as the operator wrote it: a name or a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The user, by name or number.
    pub user: String,
    /// The group, by name or number. `None` means the user's own primary group.
    pub group: Option<String>,
}

/// What the privileged start knows: the board file, the socket path, and the ids the daemon will
/// run as. A step may create files for those ids, before the drop.
pub struct Context<'a> {
    /// The board file, as parsed.
    pub board: &'a BoardFile,
    /// The socket the daemon listens on.
    pub socket: &'a Path,
    /// The user id the daemon will run as.
    pub uid: u32,
    /// The group id the daemon will run as.
    pub gid: u32,
}

/// One step of the privileged start: runs as root, before the drop. A step returns the devices it
/// cannot set up, each with its reason ([`Refusals`]): those devices are refused and the daemon
/// starts without them. An `Err` is for the process itself, which cannot run as asked (it cannot
/// make its socket's directory, say); it stops the daemon from starting.
///
/// Board-file-driven setup belongs here, as a new entry in [`STEPS`] after the built-in ones. The
/// USB gadget's configfs and FunctionFS setup is the second such step. Do not add `ExecStartPre=+`.
pub struct Step {
    /// The name, for the error when the step fails.
    pub name: &'static str,
    /// The step itself.
    pub run: fn(&Context<'_>) -> io::Result<Refusals>,
}

/// The privileged start's steps, in order. The one built in step makes the socket's directory,
/// owned by the daemon's user, so the socket is made in it after the drop. The USB gadgets are not
/// a step: the root phase forks the gadget helper for them, which keeps root (see `gadget::spawn`).
pub const STEPS: &[Step] = &[
    Step {
        name: "runtime directory",
        run: runtime_directory,
    },
    Step {
        name: "pwm channels",
        run: pwm_channels,
    },
    Step {
        name: "hwmon fans",
        run: hwmon_fans,
    },
];

/// The PWM channels the board names, granted before the drop (`pwm::grant`): each channel is
/// exported if the kernel has not, and its four attributes go to root:`lemnos` mode 0660, so the
/// daemon's user can write them. Only the channels a non-refused device uses. A channel that cannot
/// be granted refuses that device by name; the daemon keeps running and the others start. A channel
/// that is not there yet (`NotFound`) does not refuse: the device's driver binds it under the failure
/// rule, and the hotplug rule grants its attributes when they appear (packaging/udev/70-lemnos.rules).
fn pwm_channels(cx: &Context<'_>) -> io::Result<Refusals> {
    use std::io::Write;
    let linux = lemnos_linux::LinuxBoard::new();
    let mut refused = Vec::new();
    for device in cx.board.devices.iter().filter(|d| d.refused.is_none()) {
        for (need, name) in &device.uses {
            match linux.grant_pwm_before_drop(name, (0, cx.gid)) {
                Ok(Some(line)) => {
                    let _ = writeln!(
                        io::stderr(),
                        "lemnosd: root phase: {}: {need}: {line}",
                        device.id
                    );
                }
                Ok(None) => {}
                Err((io::ErrorKind::NotFound, why)) => {
                    let _ = writeln!(
                        io::stderr(),
                        "lemnosd: root phase: {}: {need}: not there yet, bound when it is: {why}",
                        device.id
                    );
                }
                Err((_, why)) => {
                    let _ = writeln!(
                        io::stderr(),
                        "lemnosd: root phase: {}: {need}: {why}",
                        device.id
                    );
                    refused.push((device.id.clone(), format!("{need}: {why}")));
                }
            }
        }
    }
    Ok(refused)
}

/// The hwmon fans the board names, granted before the drop (`lemnos_driver_hwmon_fan::grant_before_drop`):
/// each fan's `pwm1` and `pwm1_enable` go to root:`lemnos` mode 0660, so the daemon's user can write the
/// duty and the mode. A fan that cannot be granted because it is not there yet (no `pwm1` in its
/// directory) does not refuse its device: the driver binds it under the failure rule, and the hotplug
/// rule grants its attributes when they appear. Any other failure refuses its device by name; the
/// daemon keeps running and the others start.
fn hwmon_fans(cx: &Context<'_>) -> io::Result<Refusals> {
    use std::io::Write;
    let mut refused = Vec::new();
    for device in cx
        .board
        .devices
        .iter()
        .filter(|d| d.refused.is_none() && d.driver == "hwmon-fan")
    {
        let granted = lemnos_host::with_device_config(cx.board, device, |cfg| {
            lemnos_driver_hwmon_fan::grant_before_drop(cfg, (0, cx.gid))
        });
        match granted {
            Ok(Ok(dir)) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: fan: {}: granted to 0:{}",
                    device.id,
                    dir.display(),
                    cx.gid
                );
            }
            Ok(Err(why)) if why.kind() == io::ErrorKind::NotFound => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: fan: not there yet, bound when it is: {why}",
                    device.id
                );
            }
            Ok(Err(why)) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: fan: {why}",
                    device.id
                );
                refused.push((device.id.clone(), format!("fan: {why}")));
            }
            Err(why) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: fan: its settings can't be read: {why}",
                    device.id
                );
                refused.push((
                    device.id.clone(),
                    format!("fan: its settings can't be read: {why}"),
                ));
            }
        }
    }
    Ok(refused)
}

/// The drivers that take a step as root before the drop, by driver name: each is given its
/// device's settings and returns one line for the log. The slots and security drivers open the boot
/// partition here, which the daemon's user can't open by name, and keep the file for after the drop
/// (read-write only when the records on it are ours).
/// A driver's root phase: run as root with its device's settings, it returns the line it logs.
pub type RootPhase = fn(&lemnos_core::Config<'_>) -> String;

pub const ROOT_PHASES: &[(&str, RootPhase)] = &[
    ("system-slots", lemnos_driver_system_slots::root_phase),
    ("system-security", lemnos_driver_system_security::root_phase),
    (
        "system-reboot",
        lemnos_driver_system_slots::reboot_root_phase,
    ),
];

/// Runs [`ROOT_PHASES`] for every device of the board whose driver has one, in the board's order.
/// Nothing is refused here: a node that can't be handed over makes the driver's own writes fail by
/// name after the drop, and its health says so. Run after the gadget helper is forked, so the
/// helper (a fork with no exec, which close-on-exec does not cover) never holds the files.
fn driver_root_phases(cx: &Context<'_>) -> io::Result<Refusals> {
    use std::io::Write;
    for device in cx.board.devices.iter().filter(|d| d.refused.is_none()) {
        let Some((_, phase)) = ROOT_PHASES.iter().find(|(name, _)| *name == device.driver) else {
            continue;
        };
        let line = lemnos_host::with_device_config(cx.board, device, phase)
            .unwrap_or_else(|why| format!("its settings can't be read: {why}"));
        let _ = writeln!(io::stderr(), "lemnosd: root phase: {}: {line}", device.id);
    }
    Ok(Vec::new())
}

/// The file descriptor store's name for a held watchdog. A restart is handed back the descriptors
/// stored under it (`LISTEN_FDNAMES`).
pub const WATCHDOG_STORE_NAME: &str = "lemnos-watchdog";

/// Takes the watchdog descriptors the service manager kept for this process (`WATCHDOG_STORE_NAME`), as
/// root before the drop. Any other descriptor the manager passed is closed here.
fn stored_watchdogs() -> Vec<std::fs::File> {
    lemnos_linux_sys::listen::take()
        .into_iter()
        .filter_map(|(name, fd)| (name == WATCHDOG_STORE_NAME).then(|| std::fs::File::from(fd)))
        .collect()
}

/// Claims, as root, every node the board gives to the kernel module (`[[hardware]]` with `module`):
/// its `driver_override` is set to the module's driver the SoC map names for it (`module_driver`,
/// `lemnos` by default), the stock driver the map names lets it go when it holds the node, and the
/// module's driver binds it ([`lemnos_linux::claim::claim`]). The module has no match table and a
/// built-in stock driver binds its node at boot, so without this the module owns nothing. One line
/// for each node. A node held by a driver the map does not name is left as it is, and said. Nothing
/// here stops the start: a node the module does not hold makes its devices fail at their binds,
/// by name.
pub fn claim_module_nodes(
    board: &BoardFile,
    map: Option<&lemnos_devtree::SocMap>,
    platform: &Path,
) {
    use std::io::Write;
    let given: Vec<_> = board
        .hardware
        .iter()
        .filter(|h| h.module.is_some())
        .collect();
    if given.is_empty() {
        return;
    }
    let Some(map) = map else {
        let _ = writeln!(
            io::stderr(),
            "lemnosd: root phase: module nodes: not claimed: the board's SoC map can't be read (see the live tree line)"
        );
        return;
    };
    for hardware in given {
        let line = match map.controllers.get(&hardware.controller) {
            None => format!("not in the SoC map: {}", hardware.controller),
            Some(controller) => match &controller.sysfs {
                None => "the SoC map names no platform device for it (sysfs)".to_string(),
                Some(device) => {
                    let driver = controller.module_driver.as_deref().unwrap_or("lemnos");
                    match lemnos_linux::claim::claim(
                        platform,
                        device,
                        driver,
                        controller.stock_driver.as_deref(),
                    ) {
                        Ok(lemnos_linux::claim::Claimed::Already) => {
                            format!("{device}: already {driver}'s")
                        }
                        Ok(lemnos_linux::claim::Claimed::Bound { unbound }) => match unbound {
                            Some(stock) => {
                                format!("{device}: {stock} let it go; bound to {driver}")
                            }
                            None => format!("{device}: bound to {driver}"),
                        },
                        Ok(lemnos_linux::claim::Claimed::Waiting { unbound }) => format!(
                            "{device}: driver_override is {driver}{}; {driver} is not registered yet (the kernel binds the node when the module loads)",
                            unbound
                                .map(|s| format!(", {s} let it go"))
                                .unwrap_or_default()
                        ),
                        Err(why) => format!("not claimed: {why}"),
                    }
                }
            },
        };
        let label = hardware.name.as_deref().unwrap_or(&hardware.controller);
        // One write per line: stderr is unbuffered, and a line formatted in pieces can be split in
        // the journal by another thread's write between them.
        let text = format!("lemnosd: root phase: module node {label}: {line}\n");
        let _ = io::stderr().write_all(text.as_bytes());
    }
}

/// Holds the nodes of every keep-alive device across the drop (`LinuxBoard::hold_before_drop`), as
/// root, before the drop. A node is held in one of two ways:
///
/// - the first start opens it as root and feeds it once, so the unfed hand-over from the initramfs
///   ends here, and its bind takes the held descriptor whatever the node's owner is;
/// - a restart takes the watchdog descriptor the service manager kept in its file descriptor store
///   (`stored`), as it is, and feeds nothing. Opening a running watchdog feeds it, so a daemon that
///   died before it was healthy would otherwise be fed by each restart's open, for good.
///
/// A stored descriptor that is not a character device with a keep-alive node's device number is
/// refused by name, and the node is opened as the first start does. Returns the nodes this start
/// opened and fed: their descriptors are the ones to keep in the store ([`keep_in_store`]).
fn hold_keepalive_nodes(
    board: &BoardFile,
    linux: &lemnos_linux::LinuxBoard,
    stored: &mut Vec<std::fs::File>,
) -> Vec<std::path::PathBuf> {
    use std::io::Write;
    let mut fed = Vec::new();
    for device in board
        .devices
        .iter()
        .filter(|d| d.keepalive && d.refused.is_none())
    {
        for (need, name) in &device.uses {
            match linux.hold_before_drop(name, stored) {
                Ok(Some(hold)) => {
                    let _ = writeln!(
                        io::stderr(),
                        "lemnosd: root phase: {}: {need}: {}",
                        device.id,
                        hold.line
                    );
                    let event = if hold.fed {
                        format!("root phase: {} fed", device.id)
                    } else {
                        format!("root phase: {} adopted, not fed", device.id)
                    };
                    crate::start_trace_line(&event);
                    if hold.fed {
                        fed.push(hold.path);
                    }
                }
                Ok(None) => {}
                Err(why) => {
                    let _ = writeln!(
                        io::stderr(),
                        "lemnosd: root phase: {}: {need}: {why}",
                        device.id
                    );
                }
            }
        }
    }
    fed
}

/// Keeps the descriptors of the nodes this start fed in the service manager's store, so the next start
/// takes them back ([`hold_keepalive_nodes`]). Without a manager, nothing is kept and a restart opens
/// its node by name, as a BusyBox init's does. A store that refuses a descriptor is named, and the
/// node is then opened by name at a restart, which feeds it.
fn keep_in_store(fed: &[std::path::PathBuf], notifier: &crate::notify::Notifier) {
    use std::io::Write;
    for (path, file) in lemnos_linux::held_watchdogs() {
        if !fed.contains(&path) {
            continue;
        }
        match notifier.store(WATCHDOG_STORE_NAME, std::os::fd::AsFd::as_fd(&file)) {
            Ok(true) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: kept in the service's descriptor store, for a restart to take back",
                    path.display()
                );
            }
            Ok(false) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: not kept: no service manager; a restart opens the node by name",
                    path.display()
                );
            }
            Err(error) => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd: root phase: {}: not kept in the service's descriptor store ({error}); a restart opens the node by name",
                    path.display()
                );
            }
        }
    }
}

/// Builds the request from `--user` and `--group`, or their environment variables. A group needs a
/// user.
///
/// # Errors
///
/// A group with no user.
pub fn request(user: Option<String>, group: Option<String>) -> Result<Option<Request>, String> {
    match (user, group) {
        (None, None) => Ok(None),
        (None, Some(_)) => {
            Err("--group needs --user (LEMNOSD_GROUP needs LEMNOSD_USER)".to_owned())
        }
        (Some(user), group) => Ok(Some(Request { user, group })),
    }
}

/// The user id and group id a request names, looked up in the account database when it is a
/// name.
///
/// # Errors
///
/// A name the database does not know, or one it cannot be read for.
pub fn resolve(request: &Request) -> Result<(u32, u32), String> {
    privilege::resolve(&request.user, request.group.as_deref())
}

/// What the privileged start left: the devices a step refused, what it noticed without refusing
/// anything, and the gadget helper it forked, if the board has a gadget.
#[derive(Default)]
pub struct Started {
    /// The devices refused by a step, with the reason ([`refuse`] puts them on the board).
    pub refused: Refusals,
    /// Lines for the error output: a node the daemon cannot open, say. Devices that need it fail
    /// at their bind with their own reason.
    pub warnings: Vec<String>,
    /// The daemon's end of the gadget helper, which keeps root. `None` without a gadget, or when
    /// the start was not asked for one (the simulated board).
    pub helper: Option<crate::helper::Client>,
}

/// Runs every step of `steps` in order, as root, and returns the devices they refused.
///
/// # Errors
///
/// A step that fails for the process itself, with the step's name.
pub fn run_steps(steps: &[Step], cx: &Context<'_>) -> Result<Refusals, String> {
    let mut refused = Vec::new();
    for step in steps {
        match (step.run)(cx) {
            Ok(set_aside) => refused.extend(set_aside),
            Err(error) => return Err(format!("privileged start, {}: {error}", step.name)),
        }
    }
    Ok(refused)
}

/// Runs the privileged start and the drop, and verifies the result. With `helper`, the gadget
/// helper is forked for the board's USB gadgets, before the drop.
///
/// # Errors
///
/// A request that does not resolve, a process-level step failure, a helper that cannot be forked,
/// a drop that fails, or a state after the drop that is not the one asked for. Each is a reason to
/// stop, with the reason in words. A device a step cannot set up is not one of them: it is in
/// [`Started::refused`].
pub fn start(
    request: &Request,
    board: &BoardFile,
    map: Option<&lemnos_devtree::SocMap>,
    socket: &Path,
    helper: bool,
    before_drop: &dyn Fn(),
) -> Result<Started, String> {
    use std::io::Write;
    let (uid, gid) = resolve(request)?;
    let cx = Context {
        board,
        socket,
        uid,
        gid,
    };
    let mut stored = stored_watchdogs();
    let mut refused = run_steps(STEPS, &cx)?;
    let client = if helper {
        let (client, invalid) = crate::gadget::spawn(board, lemnos_linux::Owner { uid, gid })?;
        refused.extend(invalid);
        client
    } else {
        None
    };
    refused
        .extend(driver_root_phases(&cx).map_err(|error| format!("driver root phases: {error}"))?);
    // The module's nodes, on a real board only (`helper` is false under --sim, which touches no
    // hardware).
    if helper {
        claim_module_nodes(board, map, Path::new("/sys/bus/platform"));
    }
    // What still needs root to read (the live-tree check reads debugfs), after the claim.
    before_drop();
    let fed = hold_keepalive_nodes(board, &lemnos_linux::LinuxBoard::new(), &mut stored);
    for file in stored.drain(..) {
        let _ = writeln!(
            io::stderr(),
            "lemnosd: root phase: refused a stored watchdog descriptor: it is not a character device with a keep-alive node's device number; closed, and a node not adopted is opened by name"
        );
        drop(file);
    }
    keep_in_store(&fed, &crate::notify::Notifier::from_env());
    let plan = Plan {
        uid,
        gid,
        groups: Vec::new(),
        keep: KEEP.to_vec(),
    };
    privilege::drop_to(&plan).map_err(|error| format!("dropping to {uid}:{gid}: {error}"))?;
    let now = privilege::observe().map_err(|error| format!("after the drop: {error}"))?;
    privilege::verify(&plan, &now).map_err(|reason| format!("refusing to run: {reason}"))?;
    // The nodes the daemon opens by name must be reachable by the user it now runs as. Checked here,
    // after the drop. A node it cannot open is a warning: the device that opens it fails at its bind,
    // with the reason, and every other device runs.
    let group = request
        .group
        .clone()
        .unwrap_or_else(|| request.user.clone());
    let mut warnings = Vec::new();
    if let Err(why) = check_nodes(&module_and_heap_nodes(board), now.euid, now.egid, &group) {
        warnings.push(why);
    }
    Ok(Started {
        refused,
        warnings,
        helper: client,
    })
}

/// Puts the devices a privileged start refused on the board, as refused with their reason. A device
/// the board already refuses keeps its reason; an id the board does not have is ignored.
pub fn refuse(board: &mut BoardFile, refused: &Refusals) {
    for (id, reason) in refused {
        if let Some(device) = board
            .devices
            .iter_mut()
            .find(|device| device.id == *id && device.refused.is_none())
        {
            device.refused = Some(reason.clone());
        }
    }
}

/// The device nodes the daemon opens by name that exist now: the kernel module's `/dev/lemnos` and
/// its `/dev/lemnos-*` nodes, and each of the board's dma-heaps (`BoardFile::dma_heaps`). A node that
/// is not there is not listed as a failure here: a driver that needs it reports that when it binds.
#[must_use]
pub fn module_and_heap_nodes(board: &BoardFile) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "lemnos" || name.starts_with("lemnos-") {
                paths.push(entry.path());
            }
        }
    }
    paths.extend(
        board
            .dma_heaps()
            .into_iter()
            .map(|heap| PathBuf::from("/dev/dma_heap").join(heap)),
    );
    paths.sort();
    paths
}

/// Checks that each node that exists can be read and written by the user `uid` or the group `gid`,
/// from its owner, group and mode bits. Only the owner, the group and the other bits are looked at,
/// which is the daemon's reach after its drop: its groups are emptied by the drop, so the
/// effective group is its only group. A node that cannot be reached is named, with the fix.
///
/// # Errors
///
/// A node that exists and that the daemon cannot read and write, named with its group `group`. A
/// node whose metadata cannot be read is named with the reason.
pub fn check_nodes(paths: &[PathBuf], uid: u32, gid: u32, group: &str) -> Result<(), String> {
    for path in paths {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let (mode, owner, node_group) = (meta.mode(), meta.uid(), meta.gid());
        let reachable = (owner == uid && mode & 0o600 == 0o600)
            || (node_group == gid && mode & 0o060 == 0o060)
            || mode & 0o006 == 0o006;
        if !reachable {
            return Err(format!(
                "{}: not readable/writable by group {group}: install the generated udev fragment",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Describes what a process without root lacks, or `None` when it holds every kept capability.
///
/// # Errors
///
/// The capability state cannot be read.
pub fn lacks(now: &Observed) -> io::Result<Option<String>> {
    let missing: Vec<&str> = KEEP
        .iter()
        .filter(|&&cap| now.effective & (1u64 << cap) == 0)
        .map(|&cap| cap_name(cap))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "running as uid {} without root: lacks {}; the registers of a block mapped through /dev/lemnos are refused",
        now.euid,
        missing.join(", ")
    )))
}

fn cap_name(cap: u32) -> &'static str {
    match cap {
        privilege::CAP_CHOWN => "CAP_CHOWN",
        privilege::CAP_SETGID => "CAP_SETGID",
        privilege::CAP_SETUID => "CAP_SETUID",
        privilege::CAP_SETPCAP => "CAP_SETPCAP",
        CAP_SYS_RAWIO => "CAP_SYS_RAWIO",
        _ => "a capability",
    }
}

/// The mode of the socket's directory: the daemon's user may write it, its group may open it.
pub const RUNTIME_DIRECTORY_MODE: u32 = 0o750;

/// Makes the socket's directory, `RUNTIME_DIRECTORY_MODE` and owned by the daemon's ids.
///
/// The mode is set while root owns the directory, and only then is it handed over: a change of mode
/// on a directory root does not own needs `CAP_FOWNER`, which the packaged unit's bounding set does
/// not give. A directory already owned by someone else with another mode is taken back to root
/// first (`CAP_CHOWN`, which the root phase has). Nothing is changed that is already right.
fn runtime_directory(cx: &Context<'_>) -> io::Result<Refusals> {
    use std::os::unix::fs::{PermissionsExt, chown};
    let Some(dir) = cx.socket.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(Vec::new());
    };
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::metadata(dir)?;
    let mut owner = (meta.uid(), meta.gid());
    if meta.mode() & 0o7777 != RUNTIME_DIRECTORY_MODE {
        let me = lemnos_linux_sys::privilege::observe()?.euid;
        if owner.0 != me {
            chown(dir, Some(me), None)?;
            owner.0 = me;
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(RUNTIME_DIRECTORY_MODE))?;
    }
    if owner != (cx.uid, cx.gid) {
        chown(dir, Some(cx.uid), Some(cx.gid))?;
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_without_a_user_is_refused() {
        assert_eq!(request(None, None), Ok(None));
        assert!(request(None, Some("lemnos".to_owned())).is_err());
        assert_eq!(
            request(Some("lemnos".to_owned()), None),
            Ok(Some(Request {
                user: "lemnos".to_owned(),
                group: None
            }))
        );
    }

    #[test]
    fn numbers_resolve_without_the_account_database() {
        let request = Request {
            user: "999".to_owned(),
            group: Some("998".to_owned()),
        };
        assert_eq!(resolve(&request), Ok((999, 998)));
    }

    #[test]
    fn a_name_the_database_does_not_know_is_an_error() {
        let request = Request {
            user: "no-such-user-lemnos-test".to_owned(),
            group: None,
        };
        assert!(resolve(&request).is_err());
    }

    #[test]
    fn root_resolves_to_root() {
        let request = Request {
            user: "root".to_owned(),
            group: None,
        };
        assert_eq!(resolve(&request), Ok((0, 0)));
    }

    #[test]
    fn a_process_without_the_kept_capability_says_which() {
        let now = Observed {
            uid: 999,
            euid: 999,
            gid: 999,
            egid: 999,
            groups: vec![],
            effective: 0,
            permitted: 0,
            inheritable: 0,
            ambient: 0,
            bounding: 0,
            no_new_privs: false,
        };
        let message = lacks(&now).expect("read").expect("missing");
        assert!(message.contains("CAP_SYS_RAWIO"), "{message}");
        let mut holds = now;
        holds.effective = 1 << CAP_SYS_RAWIO;
        assert_eq!(lacks(&holds).expect("read"), None);
    }
}

#[cfg(test)]
mod node_tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::check_nodes;

    /// A file of the test's own, in a directory of its own under the test's scratch directory.
    /// A directory the test removes when it ends, pass or panic.
    struct Dir(std::path::PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn node(name: &str, mode: u32) -> (Dir, std::path::PathBuf, u32, u32) {
        let dir = std::env::temp_dir().join(format!("lemnosd-node-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let guard = Dir(dir.clone());
        let path = dir.join("node");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let meta = fs::metadata(&path).unwrap();
        (guard, path, meta.uid(), meta.gid())
    }

    #[test]
    fn a_node_the_group_can_open_is_accepted() {
        let (_dir, path, _uid, gid) = node("group-rw", 0o660);
        // A user outside the file's owner but in its group: the group bits are read and written.
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid, "lemnos").is_ok());
    }

    #[test]
    fn a_node_only_root_can_open_is_named_with_the_fix() {
        // The reference board's state before the udev fragment: root:root, mode 600.
        let (_dir, path, _uid, _gid) = node("root-only", 0o600);
        let error =
            check_nodes(std::slice::from_ref(&path), 987_654, 987_654, "lemnos").unwrap_err();
        assert_eq!(
            error,
            format!(
                "{}: not readable/writable by group lemnos: install the generated udev fragment",
                path.display()
            )
        );
    }

    #[test]
    fn a_node_that_is_not_there_is_not_this_checks_failure() {
        let missing = std::path::PathBuf::from("/nonexistent-lemnos-test/dma_heap/linux,cma");
        assert!(check_nodes(&[missing], 1, 1, "lemnos").is_ok());
    }

    #[test]
    fn a_node_readable_and_writable_by_other_is_accepted_and_one_half_open_is_not() {
        let (_dir, path, _uid, gid) = node("other-rw", 0o606);
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid + 1, "lemnos").is_ok());
        let (_dir, path, _uid, gid) = node("other-r", 0o604);
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid + 1, "lemnos").is_err());
    }
}
