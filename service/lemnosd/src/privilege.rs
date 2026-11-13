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

/// One step of the privileged start: runs as root, before the drop. A step that fails stops the
/// daemon from starting, because a half-set-up board is worse than none.
///
/// Board-file-driven setup belongs here, as a new entry in [`STEPS`] after the built-in ones. The
/// USB gadget's configfs and FunctionFS setup is planned as the first such step. Do not add
/// `ExecStartPre=+`.
pub struct Step {
    /// The name, for the error when the step fails.
    pub name: &'static str,
    /// The step itself.
    pub run: fn(&Context<'_>) -> io::Result<()>,
}

/// The privileged start's steps, in order. The first is built in: the socket's directory, owned by
/// the daemon's user, so the socket is made in it after the drop.
pub const STEPS: &[Step] = &[
    Step {
        name: "runtime directory",
        run: runtime_directory,
    },
    Step {
        name: "usb gadget",
        run: crate::gadget::privileged_step,
    },
];

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

/// Runs the privileged start and the drop, and verifies the result.
///
/// # Errors
///
/// A request that does not resolve, a step that fails, a drop that fails, or a state after the drop
/// that is not the one asked for. Each is a reason to stop, with the reason in words.
pub fn start(request: &Request, board: &BoardFile, socket: &Path) -> Result<(), String> {
    let (uid, gid) = resolve(request)?;
    let cx = Context {
        board,
        socket,
        uid,
        gid,
    };
    for step in STEPS {
        (step.run)(&cx).map_err(|error| format!("privileged start, {}: {error}", step.name))?;
    }
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
    // after the drop, so a node the group cannot open stops the start with its name.
    let group = request
        .group
        .clone()
        .unwrap_or_else(|| request.user.clone());
    check_nodes(&module_and_heap_nodes(board), now.euid, now.egid, &group)
        .map_err(|why| format!("refusing to run: {why}"))?;
    Ok(())
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

fn runtime_directory(cx: &Context<'_>) -> io::Result<()> {
    use std::os::unix::fs::{PermissionsExt, chown};
    let Some(dir) = cx.socket.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    chown(dir, Some(cx.uid), Some(cx.gid))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))
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
    fn node(name: &str, mode: u32) -> (std::path::PathBuf, u32, u32) {
        let dir = std::env::temp_dir().join(format!("lemnosd-node-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("node");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let meta = fs::metadata(&path).unwrap();
        (path, meta.uid(), meta.gid())
    }

    #[test]
    fn a_node_the_group_can_open_is_accepted() {
        let (path, _uid, gid) = node("group-rw", 0o660);
        // A user outside the file's owner but in its group: the group bits are read and written.
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid, "lemnos").is_ok());
    }

    #[test]
    fn a_node_only_root_can_open_is_named_with_the_fix() {
        // The reference board's state before the udev fragment: root:root, mode 600.
        let (path, _uid, _gid) = node("root-only", 0o600);
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
        let (path, _uid, gid) = node("other-rw", 0o606);
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid + 1, "lemnos").is_ok());
        let (path, _uid, gid) = node("other-r", 0o604);
        assert!(check_nodes(std::slice::from_ref(&path), 987_654, gid + 1, "lemnos").is_err());
    }
}
