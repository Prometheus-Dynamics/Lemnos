//! Dropping root: the one path every Lemnos service takes when it starts as root.
//!
//! [`drop_to`] makes the last changes of the root phase, in order: it clears the bounding set of
//! every capability the [`Plan`] does not keep, sets the supplementary groups and the group and
//! user ids, keeps exactly the planned capabilities in the effective and permitted sets, and sets
//! `no_new_privs`. [`observe`] reads what the process holds now, and [`verify`] compares that with
//! the plan, so a caller refuses to run on a mismatch. `verify` works on plain values, so its
//! refusals are tested without privileges.

use std::io;

/// `CAP_CHOWN`: change file owners (the runtime directory's owner, set before the drop).
pub const CAP_CHOWN: u32 = 0;
/// `CAP_SETGID`: set group ids and supplementary groups.
pub const CAP_SETGID: u32 = 6;
/// `CAP_SETUID`: set user ids.
pub const CAP_SETUID: u32 = 7;
/// `CAP_SETPCAP`: change the capability sets and the bounding set.
pub const CAP_SETPCAP: u32 = 8;
/// `CAP_SYS_RAWIO`: map a block's registers through `/dev/lemnos`.
pub const CAP_SYS_RAWIO: u32 = 17;

/// What the process should be after the drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The user id, for both the real and the effective id.
    pub uid: u32,
    /// The group id, for both the real and the effective id.
    pub gid: u32,
    /// The supplementary groups, exactly these (in any order).
    pub groups: Vec<u32>,
    /// The capabilities kept in the effective and permitted sets. Every other one is gone, and
    /// leaves the bounding set.
    pub keep: Vec<u32>,
}

/// The credentials and capability sets of a process, as [`observe`] reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The real user id.
    pub uid: u32,
    /// The effective user id.
    pub euid: u32,
    /// The real group id.
    pub gid: u32,
    /// The effective group id.
    pub egid: u32,
    /// The supplementary groups.
    pub groups: Vec<u32>,
    /// The effective capability set, as a bit mask.
    pub effective: u64,
    /// The permitted capability set, as a bit mask.
    pub permitted: u64,
    /// The inheritable capability set, as a bit mask.
    pub inheritable: u64,
    /// The ambient capability set, as a bit mask.
    pub ambient: u64,
    /// The bounding set, as a bit mask.
    pub bounding: u64,
    /// Whether `no_new_privs` is set.
    pub no_new_privs: bool,
}

/// The bit mask of `caps`.
///
/// # Errors
///
/// `InvalidInput` for a capability at or past 64.
pub fn mask(caps: &[u32]) -> io::Result<u64> {
    caps.iter().try_fold(0u64, |acc, &cap| {
        if cap >= 64 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        Ok(acc | (1u64 << cap))
    })
}

/// Reads the calling process's credentials and capability sets.
///
/// # Errors
///
/// The system call that failed.
pub fn observe() -> io::Result<Observed> {
    let (effective, permitted, inheritable) = capget()?;
    Ok(Observed {
        // SAFETY: `getuid` takes no arguments and cannot fail.
        uid: unsafe { libc::getuid() },
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        euid: unsafe { libc::geteuid() },
        // SAFETY: `getgid` takes no arguments and cannot fail.
        gid: unsafe { libc::getgid() },
        // SAFETY: `getegid` takes no arguments and cannot fail.
        egid: unsafe { libc::getegid() },
        groups: groups()?,
        effective,
        permitted,
        inheritable,
        ambient: ambient()?,
        bounding: bounding()?,
        no_new_privs: prctl_flag(libc::PR_GET_NO_NEW_PRIVS, 0)? == 1,
    })
}

/// Makes the calling process's ids and capability sets the plan's, and sets `no_new_privs`.
///
/// Must run while the process is root, with `CAP_SETUID`, `CAP_SETGID` and `CAP_SETPCAP`. Only
/// the root phase calls it; a process that is already another user cannot run it.
///
/// # Errors
///
/// The first system call that fails. The process may then hold some of the change, so the caller
/// must stop, not carry on.
pub fn drop_to(plan: &Plan) -> io::Result<()> {
    let keep = mask(&plan.keep)?;
    let before = observe()?;
    // The bounding set first: once the ids change, the process cannot clear it.
    for cap in 0..64u32 {
        if before.bounding & (1u64 << cap) != 0 && keep & (1u64 << cap) == 0 {
            prctl_flag(libc::PR_CAPBSET_DROP, cap as libc::c_ulong)?;
        }
    }
    // No inherited or ambient capability reaches a program the daemon starts.
    capset(before.effective, before.permitted, 0)?;
    let clear = libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong;
    // SAFETY: `PR_CAP_AMBIENT` with `CLEAR_ALL` reads its integer arguments only.
    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            clear,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // Keep the permitted set across the change of user id, which otherwise clears it.
    prctl_flag(libc::PR_SET_KEEPCAPS, 1)?;
    let mut want = plan.groups.clone();
    want.sort_unstable();
    want.dedup();
    if sorted(before.groups.clone()) != want {
        set_groups(&want)?;
    }
    set_ids(plan.gid, plan.uid)?;
    prctl_flag(libc::PR_SET_KEEPCAPS, 0)?;
    capset(keep, keep, 0)?;
    prctl_flag(libc::PR_SET_NO_NEW_PRIVS, 1)?;
    Ok(())
}

/// Compares what the process holds with the plan: the ids, the groups, the effective and permitted
/// sets, the inheritable and ambient sets, the bounding set outside the kept capabilities, and
/// `no_new_privs`.
///
/// # Errors
///
/// The first difference, in words.
pub fn verify(plan: &Plan, now: &Observed) -> Result<(), String> {
    let keep = mask(&plan.keep).map_err(|error| error.to_string())?;
    let mut want = plan.groups.clone();
    want.sort_unstable();
    want.dedup();
    let checks: [(bool, String); 9] = [
        (
            now.uid == plan.uid,
            format!("real uid is {}, not {}", now.uid, plan.uid),
        ),
        (
            now.euid == plan.uid,
            format!("effective uid is {}, not {}", now.euid, plan.uid),
        ),
        (
            now.gid == plan.gid,
            format!("real gid is {}, not {}", now.gid, plan.gid),
        ),
        (
            now.egid == plan.gid,
            format!("effective gid is {}, not {}", now.egid, plan.gid),
        ),
        (
            sorted(now.groups.clone()) == want,
            format!("supplementary groups are {:?}, not {want:?}", now.groups),
        ),
        (
            now.effective == keep,
            format!(
                "effective capabilities are {:#x}, not {keep:#x}",
                now.effective
            ),
        ),
        (
            now.permitted == keep,
            format!(
                "permitted capabilities are {:#x}, not {keep:#x}",
                now.permitted
            ),
        ),
        (
            now.inheritable == 0 && now.ambient == 0,
            format!(
                "inheritable {:#x} or ambient {:#x} is set",
                now.inheritable, now.ambient
            ),
        ),
        (
            now.bounding & !keep == 0,
            format!(
                "bounding set {:#x} keeps capabilities not planned",
                now.bounding
            ),
        ),
    ];
    for (ok, reason) in checks {
        if !ok {
            return Err(reason);
        }
    }
    if !now.no_new_privs {
        return Err("no_new_privs is not set".to_owned());
    }
    Ok(())
}

fn sorted(mut values: Vec<u32>) -> Vec<u32> {
    values.sort_unstable();
    values.dedup();
    values
}

fn prctl_flag(option: libc::c_int, arg: libc::c_ulong) -> io::Result<libc::c_int> {
    // SAFETY: `prctl` with these options reads only its integer arguments.
    let rc = unsafe {
        libc::prctl(
            option,
            arg,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc)
}

fn bounding() -> io::Result<u64> {
    let mut bits = 0u64;
    for cap in 0..64u32 {
        match prctl_flag(libc::PR_CAPBSET_READ, cap as libc::c_ulong) {
            Ok(1) => bits |= 1u64 << cap,
            Ok(_) => {}
            // EINVAL: past the last capability the kernel knows.
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => break,
            Err(error) => return Err(error),
        }
    }
    Ok(bits)
}

fn ambient() -> io::Result<u64> {
    let mut bits = 0u64;
    for cap in 0..64u32 {
        // `PR_CAP_AMBIENT_IS_SET` takes the capability in arg3, so it cannot use `prctl_flag`.
        let is_set = libc::PR_CAP_AMBIENT_IS_SET as libc::c_ulong;
        // SAFETY: `PR_CAP_AMBIENT` with `IS_SET` reads its integer arguments only.
        let rc = unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                is_set,
                libc::c_ulong::from(cap),
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(error);
        }
        if rc == 1 {
            bits |= 1u64 << cap;
        }
    }
    Ok(bits)
}

fn groups() -> io::Result<Vec<u32>> {
    // SAFETY: a zero count with a null list only asks for the number of groups.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut list = vec![0 as libc::gid_t; count as usize];
    // SAFETY: `list` holds `count` group ids, and getgroups writes at most that many.
    let got = unsafe { libc::getgroups(count, list.as_mut_ptr()) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    list.truncate(got as usize);
    Ok(list)
}

fn set_groups(groups: &[u32]) -> io::Result<()> {
    let list: Vec<libc::gid_t> = groups.to_vec();
    // SAFETY: `list` is live for the call, and its length is passed with it.
    if unsafe { libc::setgroups(list.len(), list.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_ids(gid: u32, uid: u32) -> io::Result<()> {
    // SAFETY: `setgid` takes an id and changes no memory.
    if unsafe { libc::setgid(gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `setuid` takes an id and changes no memory.
    if unsafe { libc::setuid(uid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The kernel's capability header and data, version 3: two 32-bit words per set.
#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const CAP_VERSION_3: u32 = 0x2008_0522;

fn capget() -> io::Result<(u64, u64, u64)> {
    let mut header = CapHeader {
        version: CAP_VERSION_3,
        pid: 0,
    };
    let mut data = [CapData::default(); 2];
    // SAFETY: the header and the two data words are live for the call, in the layout version 3
    // expects.
    let rc = unsafe { libc::syscall(libc::SYS_capget, &raw mut header, data.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        join(&data, |d| d.effective),
        join(&data, |d| d.permitted),
        join(&data, |d| d.inheritable),
    ))
}

fn capset(effective: u64, permitted: u64, inheritable: u64) -> io::Result<()> {
    let mut header = CapHeader {
        version: CAP_VERSION_3,
        pid: 0,
    };
    let data = [
        CapData {
            effective: effective as u32,
            permitted: permitted as u32,
            inheritable: inheritable as u32,
        },
        CapData {
            effective: (effective >> 32) as u32,
            permitted: (permitted >> 32) as u32,
            inheritable: (inheritable >> 32) as u32,
        },
    ];
    // SAFETY: the header and the two data words are live for the call, in the layout version 3
    // expects.
    let rc = unsafe { libc::syscall(libc::SYS_capset, &raw mut header, data.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn join(data: &[CapData; 2], field: impl Fn(&CapData) -> u32) -> u64 {
    u64::from(field(&data[0])) | (u64::from(field(&data[1])) << 32)
}

/// The user id and primary group id of the user `name`, from the system's account database.
///
/// # Errors
///
/// A name the database cannot be read for, or a buffer too small for an entry (not expected).
/// An unknown name is `Ok(None)`.
pub fn user_ids(name: &str) -> io::Result<Option<(u32, u32)>> {
    let name = c_name(name)?;
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `name` is a valid C string, `entry` and `buf` are live for the call, and `buf` is
    // passed with its length; `found` is set to a pointer into `entry` or left null.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            entry.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut found,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    if found.is_null() {
        return Ok(None);
    }
    // SAFETY: `getpwnam_r` returned a non-null entry, which it initialized in `entry`.
    let entry = unsafe { entry.assume_init() };
    Ok(Some((entry.pw_uid, entry.pw_gid)))
}

/// The group id of the group `name`, from the system's account database. An unknown name is
/// `Ok(None)`.
///
/// # Errors
///
/// As [`user_ids`].
pub fn group_id(name: &str) -> io::Result<Option<u32>> {
    let name = c_name(name)?;
    let mut entry = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::group = std::ptr::null_mut();
    // SAFETY: `name` is a valid C string, `entry` and `buf` are live for the call, and `buf` is
    // passed with its length; `found` is set to a pointer into `entry` or left null.
    let rc = unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            entry.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut found,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    if found.is_null() {
        return Ok(None);
    }
    // SAFETY: `getgrnam_r` returned a non-null entry, which it initialized in `entry`.
    let entry = unsafe { entry.assume_init() };
    Ok(Some(entry.gr_gid))
}

/// The user id and group id a root phase runs as: `user` and `group` each a name or a number. A
/// missing group is the user's primary group.
///
/// # Errors
///
/// A name the account database does not know, or cannot be read for. The message says which.
pub fn resolve(user: &str, group: Option<&str>) -> Result<(u32, u32), String> {
    let (uid, primary) = match user.parse::<u32>() {
        Ok(uid) => (uid, None),
        Err(_) => match user_ids(user) {
            Ok(Some((uid, gid))) => (uid, Some(gid)),
            Ok(None) => return Err(format!("no user named {user:?}")),
            Err(error) => return Err(format!("user {user:?}: {error}")),
        },
    };
    let gid = match group {
        None => primary
            .ok_or_else(|| format!("user {uid} has no primary group to use; give a group"))?,
        Some(group) => resolve_group(group)?,
    };
    Ok((uid, gid))
}

/// The group id of `group`, a name or a number, as [`resolve`] reads it.
///
/// # Errors
///
/// As [`resolve`].
pub fn resolve_group(group: &str) -> Result<u32, String> {
    match group.parse::<u32>() {
        Ok(gid) => Ok(gid),
        Err(_) => match group_id(group) {
            Ok(Some(gid)) => Ok(gid),
            Ok(None) => Err(format!("no group named {group:?}")),
            Err(error) => Err(format!("group {group:?}: {error}")),
        },
    }
}

fn c_name(name: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn plan() -> Plan {
        Plan {
            uid: 999,
            gid: 999,
            groups: vec![],
            keep: vec![CAP_SYS_RAWIO],
        }
    }

    /// The state a process that did the drop as planned reports.
    fn right() -> Observed {
        Observed {
            uid: 999,
            euid: 999,
            gid: 999,
            egid: 999,
            groups: vec![],
            effective: 1 << CAP_SYS_RAWIO,
            permitted: 1 << CAP_SYS_RAWIO,
            inheritable: 0,
            ambient: 0,
            bounding: 1 << CAP_SYS_RAWIO,
            no_new_privs: true,
        }
    }

    #[test]
    fn the_planned_state_verifies() {
        assert_eq!(verify(&plan(), &right()), Ok(()));
    }

    #[test]
    fn each_wrong_part_is_refused_with_its_reason() {
        type Change = fn(&mut Observed);
        let cases: [(&str, Change); 11] = [
            ("real uid", |o| o.uid = 0),
            ("effective uid", |o| o.euid = 0),
            ("real gid", |o| o.gid = 0),
            ("effective gid", |o| o.egid = 0),
            ("groups", |o| o.groups = vec![4]),
            ("effective", |o| o.effective = 0),
            ("permitted", |o| {
                o.permitted = (1 << CAP_SYS_RAWIO) | (1 << CAP_CHOWN)
            }),
            ("inheritable", |o| o.inheritable = 1 << CAP_SYS_RAWIO),
            ("ambient", |o| o.ambient = 1 << CAP_SYS_RAWIO),
            ("bounding", |o| {
                o.bounding = (1 << CAP_SYS_RAWIO) | (1 << CAP_SETUID)
            }),
            ("no_new_privs", |o| o.no_new_privs = false),
        ];
        for (what, change) in cases {
            let mut now = right();
            change(&mut now);
            let reason = verify(&plan(), &now).expect_err(what);
            assert!(!reason.is_empty(), "{what}");
        }
    }

    #[test]
    fn groups_are_compared_as_a_set() {
        let plan = Plan {
            groups: vec![5, 4],
            ..plan()
        };
        let mut now = right();
        now.groups = vec![4, 5, 5];
        assert_eq!(verify(&plan, &now), Ok(()));
        now.groups = vec![4];
        assert!(verify(&plan, &now).is_err());
    }

    #[test]
    fn a_capability_past_63_is_not_a_mask() {
        assert_eq!(mask(&[CAP_SYS_RAWIO]).expect("mask"), 1 << 17);
        assert!(mask(&[64]).is_err());
    }

    #[test]
    fn observe_reads_the_calling_process() {
        // SAFETY: `getuid` takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        let now = observe().expect("observe");
        assert_eq!(now.uid, uid);
    }

    /// The drop, run as a child of this test binary so that the test process keeps its own
    /// privileges. Capabilities belong to a thread, so the child runs its one test on one thread.
    /// Only `LEMNOS_USERNS_TEST` (set by hand under `unshare -Ur`) runs it; elsewhere it is skipped.
    #[test]
    fn drop_to_under_a_user_namespace() {
        if std::env::var_os("LEMNOS_USERNS_TEST").is_none() {
            let _ = writeln!(
                io::stderr(),
                "skipped: set LEMNOS_USERNS_TEST=1 under `unshare -Ur`"
            );
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "privilege::tests::drop_child",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("LEMNOS_PRIV_CHILD", "1")
            .status()
            .expect("spawn the child");
        assert!(status.success(), "the child refused: {status}");
    }

    #[test]
    fn drop_child() {
        if std::env::var_os("LEMNOS_PRIV_CHILD").is_none() {
            return;
        }
        // Inside `unshare -Ur` the process is uid 0 and holds every capability of its namespace,
        // which is enough to set the bounding set and the capability sets. The change of user id
        // to another, unmapped uid cannot happen there: that part is for the board check.
        // A namespace that denies `setgroups` keeps the groups it has, so the plan keeps them too.
        let groups = observe().expect("observe").groups;
        let plan = Plan {
            uid: 0,
            gid: 0,
            groups,
            keep: vec![CAP_SYS_RAWIO],
        };
        drop_to(&plan).expect("drop");
        let now = observe().expect("observe");
        verify(&plan, &now).expect("the state after the drop is the plan");
        assert_eq!(now.effective, 1 << CAP_SYS_RAWIO);
    }

    #[test]
    fn root_is_in_the_account_database() {
        assert_eq!(user_ids("root").expect("lookup"), Some((0, 0)));
        assert_eq!(group_id("root").expect("lookup"), Some(0));
        assert_eq!(user_ids("no-such-user-lemnos-test").expect("lookup"), None);
    }
}
