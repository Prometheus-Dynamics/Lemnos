//! The packaged unit, read as the service manager reads the settings this check carries out.
//!
//! The unit files are built in from `packaging/`, so the check always runs what the repository
//! ships. Only the settings the check applies are read; any other is the service manager's alone.

/// `packaging/systemd/lemnosd.service`.
pub const BASE: &str = include_str!("../../../../packaging/systemd/lemnosd.service");
/// `packaging/systemd/lemnosd.service.d/gadget.conf`, the drop-in of a board with a USB gadget.
pub const GADGET: &str =
    include_str!("../../../../packaging/systemd/lemnosd.service.d/gadget.conf");

/// `packaging/systemd/lemnosd.service.d/slots.conf`, the drop-in of a board with a slot device.
pub const SLOTS: &str = include_str!("../../../../packaging/systemd/lemnosd.service.d/slots.conf");
/// `packaging/systemd/lemnosd.service.d/security.conf`, the drop-in of a board with a [security] table.
pub const SECURITY: &str =
    include_str!("../../../../packaging/systemd/lemnosd.service.d/security.conf");
/// `packaging/systemd/lemnosd.service.d/health-tcp.conf`, the drop-in of a board with a `tcp:` source.
pub const HEALTH_TCP: &str =
    include_str!("../../../../packaging/systemd/lemnosd.service.d/health-tcp.conf");

/// A path the unit makes writable (`ReadWritePaths=`): `optional` when it is written with a leading
/// `-`, which makes a missing path fine instead of a failed start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writable {
    pub path: String,
    pub optional: bool,
}

/// `ReadWritePaths=` across the files: a list setting, so each assignment adds and an empty one
/// clears what came before.
pub fn read_write_paths(files: &[&str]) -> Vec<Writable> {
    let mut out = Vec::new();
    for value in values(files, "ReadWritePaths") {
        if value.is_empty() {
            out.clear();
        }
        for word in value.split_whitespace() {
            let (optional, path) = match word.strip_prefix('-') {
                Some(path) => (true, path),
                None => (false, word),
            };
            out.push(Writable {
                path: path.to_string(),
                optional,
            });
        }
    }
    out
}

/// `RuntimeDirectory=` across the files, each a name under `/run`.
pub fn runtime_directories(files: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values(files, "RuntimeDirectory") {
        if value.is_empty() {
            out.clear();
        }
        out.extend(value.split_whitespace().map(str::to_string));
    }
    out
}

/// Capability names as unit files write them, by number.
const NAMES: [&str; 41] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_DAC_READ_SEARCH",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_LINUX_IMMUTABLE",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_BROADCAST",
    "CAP_NET_ADMIN",
    "CAP_NET_RAW",
    "CAP_IPC_LOCK",
    "CAP_IPC_OWNER",
    "CAP_SYS_MODULE",
    "CAP_SYS_RAWIO",
    "CAP_SYS_CHROOT",
    "CAP_SYS_PTRACE",
    "CAP_SYS_PACCT",
    "CAP_SYS_ADMIN",
    "CAP_SYS_BOOT",
    "CAP_SYS_NICE",
    "CAP_SYS_RESOURCE",
    "CAP_SYS_TIME",
    "CAP_SYS_TTY_CONFIG",
    "CAP_MKNOD",
    "CAP_LEASE",
    "CAP_AUDIT_WRITE",
    "CAP_AUDIT_CONTROL",
    "CAP_SETFCAP",
    "CAP_MAC_OVERRIDE",
    "CAP_MAC_ADMIN",
    "CAP_SYSLOG",
    "CAP_WAKE_ALARM",
    "CAP_BLOCK_SUSPEND",
    "CAP_AUDIT_READ",
    "CAP_PERFMON",
    "CAP_BPF",
    "CAP_CHECKPOINT_RESTORE",
];

/// The values of `key` in `[Service]`, in the order of the files and their lines.
fn values<'a>(files: &[&'a str], key: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    for file in files {
        let mut service = false;
        for line in file.lines().map(str::trim) {
            if line.starts_with('[') {
                service = line == "[Service]";
            } else if service
                && let Some((k, v)) = line.split_once('=')
                && k.trim() == key
            {
                out.push(v.trim());
            }
        }
    }
    out
}

/// The last value of `key` in `[Service]`, as a later assignment replaces an earlier one.
pub fn last<'a>(files: &[&'a str], key: &str) -> Option<&'a str> {
    values(files, key).pop()
}

/// The capability bounding set the files give, as capability numbers: assignments add to the set,
/// an empty one clears it, none at all leaves every capability. An inverted list (`~`) or a name
/// this table does not know is an error, so the check never runs under a set it guessed.
pub fn bounding_set(files: &[&str]) -> Result<Vec<u32>, String> {
    let assignments = values(files, "CapabilityBoundingSet");
    if assignments.is_empty() {
        return Ok((0..NAMES.len() as u32).collect());
    }
    let mut set: Vec<u32> = Vec::new();
    for value in assignments {
        if value.is_empty() {
            set.clear();
            continue;
        }
        if value.starts_with('~') {
            return Err(format!(
                "CapabilityBoundingSet={value}: an inverted list is not read here"
            ));
        }
        for name in value.split_whitespace() {
            let cap = NAMES
                .iter()
                .position(|known| *known == name)
                .ok_or_else(|| format!("CapabilityBoundingSet: unknown capability {name}"))?;
            set.push(cap as u32);
        }
    }
    set.sort_unstable();
    set.dedup();
    Ok(set)
}

/// The system calls the files allow, as names: systemd's `@default` (always allowed), then every
/// `SystemCallFilter=` assignment, groups expanded as the generated tables give them. `None` when no
/// filter is set. An empty assignment clears the list; a deny list (`~`) is an error, so the check
/// never runs under a filter it guessed.
pub fn system_call_names(files: &[&str]) -> Result<Option<Vec<String>>, String> {
    let assignments = values(files, "SystemCallFilter");
    let mut names: Vec<String> = Vec::new();
    let mut set = false;
    for value in assignments {
        if value.is_empty() {
            names.clear();
            set = false;
            continue;
        }
        if value.starts_with('~') {
            return Err(format!(
                "SystemCallFilter={value}: a deny list is not read here"
            ));
        }
        for item in value.split_whitespace() {
            expand(item, &mut names, 0)?;
        }
        set = true;
    }
    if !set {
        return Ok(None);
    }
    expand("@default", &mut names, 0)?;
    names.sort_unstable();
    names.dedup();
    Ok(Some(names))
}

fn expand(item: &str, out: &mut Vec<String>, depth: u32) -> Result<(), String> {
    if depth > 8 {
        return Err(format!("{item}: groups nest too deep"));
    }
    if item.starts_with('@') {
        let (_, members) = crate::filter_tables::GROUPS
            .iter()
            .find(|(name, _)| *name == item)
            .ok_or_else(|| {
                format!(
                    "SystemCallFilter: no group {item} in systemd {}",
                    crate::filter_tables::SYSTEMD
                )
            })?;
        for member in *members {
            expand(member, out, depth + 1)?;
        }
    } else {
        out.push(item.to_owned());
    }
    Ok(())
}

/// The x86_64 numbers of `names`. Names this architecture has no call for are left out, as
/// systemd leaves them out.
pub fn x86_64_numbers(names: &[String]) -> Vec<i64> {
    let mut numbers: Vec<i64> = names
        .iter()
        .filter_map(|name| {
            crate::filter_tables::X86_64
                .iter()
                .find(|(known, _)| known == name)
                .map(|(_, nr)| i64::from(*nr))
        })
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// The errno `SystemCallErrorNumber=` names: `None` when unset (a denied call kills). Only `EPERM`
/// is read; any other is an error.
pub fn error_number(files: &[&str]) -> Result<Option<u16>, String> {
    match last(files, "SystemCallErrorNumber") {
        None | Some("") => Ok(None),
        Some("EPERM") => Ok(Some(1)),
        Some(other) => Err(format!(
            "SystemCallErrorNumber={other}: only EPERM is read here"
        )),
    }
}

/// The name of capability `cap`.
pub fn name(cap: u32) -> &'static str {
    NAMES
        .get(cap as usize)
        .copied()
        .unwrap_or("an unknown capability")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_unit_gives_the_root_phase_its_five() {
        let set = bounding_set(&[BASE]).unwrap();
        let names: Vec<&str> = set.iter().map(|&c| name(c)).collect();
        assert_eq!(
            names,
            [
                "CAP_CHOWN",
                "CAP_SETGID",
                "CAP_SETUID",
                "CAP_SETPCAP",
                "CAP_SYS_RAWIO"
            ]
        );
    }

    #[test]
    fn the_gadget_drop_in_adds_to_the_set() {
        let set = bounding_set(&[BASE, GADGET]).unwrap();
        assert!(set.contains(&21));
        assert_eq!(set.len(), 6);
    }

    #[test]
    fn an_empty_assignment_clears_and_none_leaves_all() {
        let unit = "[Service]\nCapabilityBoundingSet=CAP_CHOWN\nCapabilityBoundingSet=\nCapabilityBoundingSet=CAP_KILL\n";
        assert_eq!(bounding_set(&[unit]).unwrap(), [5]);
        assert_eq!(bounding_set(&["[Service]\n"]).unwrap().len(), NAMES.len());
        assert!(bounding_set(&["[Service]\nCapabilityBoundingSet=~CAP_KILL\n"]).is_err());
        assert!(bounding_set(&["[Service]\nCapabilityBoundingSet=CAP_NOPE\n"]).is_err());
    }

    #[test]
    fn the_base_filter_holds_the_root_phase_calls_and_default() {
        let names = system_call_names(&[BASE]).unwrap().unwrap();
        for call in [
            "prctl",
            "setresuid",
            "fchownat",
            "fchmodat",
            "execve",
            "socket",
            "bind",
        ] {
            assert!(names.iter().any(|n| n == call), "{call} is allowed");
        }
        assert!(
            !names.iter().any(|n| n == "mount"),
            "mount comes only with the gadget drop-in"
        );
        let gadget = system_call_names(&[BASE, GADGET]).unwrap().unwrap();
        assert!(gadget.iter().any(|n| n == "mount") && gadget.iter().any(|n| n == "umount2"));
        assert_eq!(error_number(&[BASE]).unwrap(), Some(1));
        let numbers = x86_64_numbers(&names);
        assert!(
            numbers.len() > 255 && numbers.len() < 1024,
            "{} calls",
            numbers.len()
        );
    }

    #[test]
    fn no_filter_and_a_cleared_filter_are_none_and_a_deny_list_is_refused() {
        assert_eq!(system_call_names(&["[Service]\n"]).unwrap(), None);
        let cleared = "[Service]\nSystemCallFilter=read\nSystemCallFilter=\n";
        assert_eq!(system_call_names(&[cleared]).unwrap(), None);
        assert!(system_call_names(&["[Service]\nSystemCallFilter=~@mount\n"]).is_err());
        assert!(system_call_names(&["[Service]\nSystemCallFilter=@nope\n"]).is_err());
    }

    #[test]
    fn the_slots_and_security_drop_ins_add_their_paths() {
        assert!(read_write_paths(&[BASE]).is_empty());
        let paths = read_write_paths(&[BASE, GADGET, SLOTS, SECURITY]);
        let names: Vec<(&str, bool)> = paths
            .iter()
            .map(|w| (w.path.as_str(), w.optional))
            .collect();
        assert_eq!(
            names,
            [
                ("/sys/kernel/config/usb_gadget", true),
                ("/data/board/slots", true),
                ("/data/slots", true),
                ("/var/lib/lemnos", true),
                ("/run/board", false),
            ]
        );
        assert_eq!(runtime_directories(&[BASE]), ["lemnos"]);
        assert_eq!(last(&[BASE], "ProtectSystem"), Some("strict"));
        assert_eq!(last(&[BASE], "ProtectKernelTunables"), Some("yes"));
        assert_eq!(last(&[BASE], "PrivateTmp"), Some("yes"));
    }

    #[test]
    fn an_empty_read_write_paths_clears() {
        let cleared = "[Service]\nReadWritePaths=\nReadWritePaths=/a\n";
        assert_eq!(
            read_write_paths(&[SLOTS, cleared]),
            [Writable {
                path: "/a".into(),
                optional: false
            }]
        );
    }

    #[test]
    fn a_key_outside_service_is_not_read() {
        let unit = "[Unit]\nRuntimeDirectory=x\n[Service]\nRuntimeDirectory=lemnos\n";
        assert_eq!(last(&[unit], "RuntimeDirectory"), Some("lemnos"));
    }
}
