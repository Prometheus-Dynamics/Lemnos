//! The packaged unit, read as the service manager reads the settings this check carries out.
//!
//! The unit files are built in from `packaging/`, so the check always runs what the repository
//! ships. Only the settings the check applies are read; any other is the service manager's alone.

/// `packaging/systemd/lemnosd.service`.
pub const BASE: &str = include_str!("../../../../packaging/systemd/lemnosd.service");
/// `packaging/systemd/lemnosd.service.d/gadget.conf`, the drop-in of a board with a USB gadget.
pub const GADGET: &str =
    include_str!("../../../../packaging/systemd/lemnosd.service.d/gadget.conf");

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
    fn a_key_outside_service_is_not_read() {
        let unit = "[Unit]\nRuntimeDirectory=x\n[Service]\nRuntimeDirectory=lemnos\n";
        assert_eq!(last(&[unit], "RuntimeDirectory"), Some("lemnos"));
    }
}
