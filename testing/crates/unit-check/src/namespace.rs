//! The unit's file system sandbox, carried out as the service manager carries it out before the exec:
//! a private mount namespace in which everything is read-only (`ProtectSystem=strict`) except `/dev`,
//! `/proc`, the unit's runtime directory, a private `/tmp` and `/var/tmp` (`PrivateTmp=yes`) and its
//! `ReadWritePaths=`. With `ProtectKernelTunables=yes`, the paths of systemd's tunables tables are
//! read-only ([`TUNABLES`]: `/proc/sys` and its neighbours, `/sys/kernel/debug`, `/sys/kernel/tracing`),
//! each bound onto itself first when it is not a mount, as the service manager does; `/sys` itself is
//! left as it is mounted, read-write per mount, which is what systemd 259 leaves it (seen on 259.10 and
//! on the image's systemd: the unit's own mountinfo reads `/sys rw`, `/proc/sys ro`). With
//! `ProtectControlGroups=yes`, `/sys/fs/cgroup` is read-only.
//!
//! What it does not carry out: `ProtectHome=` (the guest has no home directories), `ProtectProc=`,
//! and the other settings that hide rather than make read-only. A write the unit refuses is refused
//! here with the same errno (`EROFS`), and a write the unit lets through reaches the file's own mode
//! (a sysfs `export`, 0200 root's, is `EACCES` for the daemon's user), which is what the stanzas that
//! start lemnosd through this launcher must see.

use std::fs;
use std::path::Path;

use lemnos_linux_sys::sandbox::{self, Kept, Propagation};

use crate::unit;

/// The paths `ProtectKernelTunables=yes` makes read-only (systemd v259, `src/core/namespace.c`,
/// `protect_kernel_tunables_proc_table` and `_sys_table`; `/proc/kallsyms` and `/proc/kcore` it
/// makes inaccessible, which is not carried out here). A path that is not there is left out, as
/// systemd's entries marked to be ignored when missing are.
pub const TUNABLES: &[&str] = &[
    "/proc/acpi",
    "/proc/apm",
    "/proc/asound",
    "/proc/bus",
    "/proc/fs",
    "/proc/irq",
    "/proc/latency_stats",
    "/proc/mtrr",
    "/proc/scsi",
    "/proc/sys",
    "/proc/sysrq-trigger",
    "/proc/timer_stats",
    "/sys/kernel/debug",
    "/sys/kernel/tracing",
];

/// What the sandbox did, one line each, for the launcher to print.
pub type Notes = Vec<String>;

/// `path` is `root` or below it, by whole components.
fn under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    root.is_empty() || path == root || path.starts_with(&format!("{root}/"))
}

/// A mountinfo field with its octal escapes (`\040` for a space) decoded.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(n);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mount points of `/proc/self/mountinfo` and each one's own options, in its order.
fn mounts() -> Result<Vec<(String, String)>, String> {
    let text = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| format!("read /proc/self/mountinfo: {e}"))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let point = fields.nth(4)?;
            let options = fields.next()?;
            Some((unescape(point), options.to_string()))
        })
        .collect())
}

/// Whether a mount at `point` is left writable: under a writable path, or under `/dev`, `/proc` or
/// `/sys`, except the tunables table with the tunables protected and `/sys/fs/cgroup` with the
/// control groups protected.
fn writable(point: &str, writable: &[String], tunables: bool, cgroups: bool) -> bool {
    if writable.iter().any(|w| under(point, w)) {
        return true;
    }
    if tunables && TUNABLES.iter().any(|t| under(point, t)) {
        return false;
    }
    if under(point, "/sys/fs/cgroup") {
        return !cgroups;
    }
    under(point, "/dev") || under(point, "/proc") || under(point, "/sys")
}

/// Shapes this process's mount namespace as the unit `files` shape it, with `runtime` standing for
/// the unit's runtime directory. Runs as full root, before the capability drop.
pub fn enter(files: &[&str], runtime: &[String]) -> Result<Notes, String> {
    let mut notes = Notes::new();
    match unit::last(files, "ProtectSystem") {
        Some("strict") => {}
        other => {
            return Err(format!(
                "the unit sets ProtectSystem={}, and only strict is carried out here",
                other.unwrap_or("")
            ));
        }
    }
    let tunables = unit::last(files, "ProtectKernelTunables") == Some("yes");
    let cgroups = unit::last(files, "ProtectControlGroups") == Some("yes");
    let private_tmp = unit::last(files, "PrivateTmp") == Some("yes");
    // The places the stand-in needs before the namespace: the guest's root may lack them.
    for dir in runtime {
        fs::create_dir_all(dir).map_err(|e| format!("make the runtime directory {dir}: {e}"))?;
    }
    if private_tmp {
        fs::create_dir_all("/tmp").map_err(|e| format!("make /tmp: {e}"))?;
    }
    sandbox::unshare_mounts().map_err(|e| format!("unshare the mount namespace: {e}"))?;
    // As the service manager sets a unit's namespace up: slave first, so nothing done here reaches
    // the host, and shared once it is set up (its default MountFlags=).
    sandbox::propagate(Propagation::Slave).map_err(|e| format!("make the mounts slave: {e}"))?;
    let mut open: Vec<String> = Vec::new();
    for dir in runtime {
        sandbox::bind_self(Path::new(dir))
            .map_err(|e| format!("the runtime directory {dir}: {e}"))?;
        open.push(dir.clone());
    }
    if private_tmp {
        for tmp in ["/tmp", "/var/tmp"] {
            if Path::new(tmp).is_dir() {
                sandbox::private_tmp(Path::new(tmp))
                    .map_err(|e| format!("a private {tmp}: {e}"))?;
                open.push(tmp.to_string());
            }
        }
    }
    for entry in unit::read_write_paths(files) {
        if !Path::new(&entry.path).exists() {
            if entry.optional {
                notes.push(format!("ReadWritePaths=-{}: absent, left out", entry.path));
                continue;
            }
            // The service manager fails the start (226/NAMESPACE) on a required path that is missing.
            return Err(format!(
                "ReadWritePaths={} does not exist, so the service manager would not start the unit",
                entry.path
            ));
        }
        sandbox::bind_self(Path::new(&entry.path))
            .map_err(|e| format!("ReadWritePaths={}: {e}", entry.path))?;
        open.push(entry.path);
    }
    if tunables {
        // Each tunable that is not a mount of its own is bound onto itself, so it can be made
        // read-only alone.
        let points: Vec<String> = mounts()?.into_iter().map(|(point, _)| point).collect();
        for path in TUNABLES {
            if Path::new(path).exists() && !points.iter().any(|p| p == path) {
                sandbox::bind_self(Path::new(path)).map_err(|e| format!("bind {path}: {e}"))?;
            }
        }
    }
    let mut read_only = 0;
    for (point, options) in mounts()? {
        if writable(&point, &open, tunables, cgroups) {
            continue;
        }
        let kept = Kept::parse(&options);
        if options.split(',').any(|o| o == "ro") {
            continue;
        }
        sandbox::remount(Path::new(&point), true, kept)
            .map_err(|e| format!("make {point} read-only: {e}"))?;
        read_only += 1;
    }
    sandbox::propagate(Propagation::Shared).map_err(|e| format!("make the mounts shared: {e}"))?;
    notes.push(format!(
        "ProtectSystem=strict{}: {read_only} mounts read-only; writable: /dev, /proc{}, {}",
        if tunables {
            ", ProtectKernelTunables=yes"
        } else {
            ""
        },
        ", /sys",
        open.join(", ")
    ));
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_is_by_whole_components() {
        assert!(under("/data/slots", "/data/slots"));
        assert!(under("/data/slots/x", "/data/slots"));
        assert!(!under("/data/slotsx", "/data/slots"));
        assert!(under("/anything", "/"));
    }

    #[test]
    fn the_unit_leaves_dev_proc_sys_and_its_paths_writable_and_the_tunables_read_only() {
        let open = vec![
            "/run/lemnos".to_string(),
            "/sys/kernel/config/usb_gadget".to_string(),
        ];
        assert!(writable("/dev", &open, true, true));
        assert!(writable("/dev/pts", &open, true, true));
        assert!(writable("/proc", &open, true, true));
        assert!(!writable("/proc/sys", &open, true, true));
        assert!(writable("/proc/sys", &open, false, true));
        // systemd 259 leaves /sys as mounted under ProtectKernelTunables=yes.
        assert!(writable("/sys", &open, true, true));
        assert!(writable("/sys/kernel/config", &open, true, true));
        assert!(writable("/sys/kernel/config/usb_gadget", &open, true, true));
        assert!(!writable("/sys/kernel/debug", &open, true, true));
        assert!(!writable("/sys/kernel/tracing", &open, true, true));
        assert!(!writable("/sys/fs/cgroup", &open, true, true));
        assert!(writable("/sys/fs/cgroup", &open, true, false));
        assert!(!writable("/", &open, true, true));
        assert!(!writable("/run", &open, true, true));
        assert!(writable("/run/lemnos", &open, true, true));
    }

    #[test]
    fn mountinfo_escapes_are_decoded() {
        assert_eq!(unescape("/a\\040b"), "/a b");
        assert_eq!(unescape("/plain"), "/plain");
    }
}
