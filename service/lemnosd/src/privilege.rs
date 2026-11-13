//! The root phase: what lemnosd does as root before it drops to its own user.
//!
//! lemnosd starts as root when it is given `--user` (or `LEMNOSD_USER`). It then runs the privileged
//! start, every step in [`STEPS`] in order, with the board file, and drops to that user and group,
//! keeping only the capabilities in [`KEEP`]. It verifies the state after the drop and refuses to
//! run on a mismatch. Started without root, it runs as it is and says which capabilities it lacks.
//! The system's own unit and init script use this one path: neither gives the daemon a user.

use std::io;
use std::path::Path;

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
