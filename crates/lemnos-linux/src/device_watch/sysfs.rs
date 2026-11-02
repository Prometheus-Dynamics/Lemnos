//! sysfs as a device list: the walk behind coldplug and the rescan source.
//! Each device is an encoded `add` message keyed by devpath, so a rescan can
//! diff two walks and synthesize events the kernel never sent.

use super::Filter;
use crate::LinuxPaths;
use crate::uevent::{UeventMsg, encode};
use crate::util::read_dir_sorted;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Devices by devpath; each value is an encoded `add` message.
pub(crate) type Snapshot = BTreeMap<String, Vec<u8>>;

/// Where a subsystem's devices are listed: a class (`/sys/class/<name>`) or a
/// bus's device list (`/sys/bus/<name>/devices`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dir {
    Class(&'static str),
    Bus(&'static str),
}

/// A directory the watcher lists (and, for inotify, watches) for one
/// subsystem. `children` also lists the channels under each `pwmchip*`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Root {
    pub(crate) subsystem: &'static str,
    pub(crate) dir: Dir,
    pub(crate) children: bool,
}

impl Root {
    pub(crate) fn path(&self, paths: &LinuxPaths) -> PathBuf {
        match self.dir {
            Dir::Class(name) => paths.sys_class_root.join(name),
            Dir::Bus(name) => paths.sys_bus_root.join(name).join("devices"),
        }
    }
}

/// The sysfs directories the watcher lists (coldplug, rescan) and watches
/// (inotify), by subsystem. Netlink needs no table: it reports every
/// subsystem the filter names.
pub(crate) const ROOTS: &[Root] = &[
    Root {
        subsystem: "gpio",
        dir: Dir::Class("gpio"),
        children: false,
    },
    Root {
        subsystem: "leds",
        dir: Dir::Class("leds"),
        children: false,
    },
    Root {
        subsystem: "pwm",
        dir: Dir::Class("pwm"),
        children: true,
    },
    Root {
        subsystem: "hwmon",
        dir: Dir::Class("hwmon"),
        children: false,
    },
    Root {
        subsystem: "i2c-dev",
        dir: Dir::Class("i2c-dev"),
        children: false,
    },
    Root {
        subsystem: "i2c",
        dir: Dir::Bus("i2c"),
        children: false,
    },
    Root {
        subsystem: "spi",
        dir: Dir::Bus("spi"),
        children: false,
    },
    Root {
        subsystem: "tty",
        dir: Dir::Class("tty"),
        children: false,
    },
    Root {
        subsystem: "usb",
        dir: Dir::Bus("usb"),
        children: false,
    },
    Root {
        subsystem: "video4linux",
        dir: Dir::Class("video4linux"),
        children: false,
    },
    Root {
        subsystem: "media",
        dir: Dir::Class("media"),
        children: false,
    },
];

/// Walks the filter's subsystems and returns their devices (first root wins
/// for a device listed under two). Missing directories are empty, not errors.
pub(crate) fn snapshot(paths: &LinuxPaths, filter: &Filter) -> Snapshot {
    let sys_root = paths.sys_root();
    let mut out = Snapshot::new();
    for root in ROOTS.iter().filter(|root| filter.includes(root.subsystem)) {
        let root_path = root.path(paths);
        for entry in read_dir_sorted(&root_path).unwrap_or_default() {
            add_device(&mut out, &sys_root, filter, &entry, root.subsystem);
            if root.children && is_pwm_chip(&entry) {
                for channel in read_dir_sorted(&entry).unwrap_or_default() {
                    if is_pwm_channel(&channel) {
                        add_device(&mut out, &sys_root, filter, &channel, root.subsystem);
                    }
                }
            }
        }
    }
    out
}

fn is_pwm_chip(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("pwmchip"))
}

fn is_pwm_channel(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("pwm"))
}

/// Adds the device behind a class or bus entry (a symlink into `/sys/devices`)
/// when it passes the filter.
fn add_device(out: &mut Snapshot, sys_root: &Path, filter: &Filter, entry: &Path, subsystem: &str) {
    let real = fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf());
    let Some(devpath) = devpath_of(&real, sys_root) else {
        return;
    };
    if out.contains_key(&devpath) {
        return;
    }
    // The `uevent` attribute holds the device's variables in the kernel's
    // `KEY=VALUE` form, without the header. The header is ours to write.
    let text = fs::read_to_string(real.join("uevent")).unwrap_or_default();
    let mut vars: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| !matches!(*key, "ACTION" | "DEVPATH"))
        .collect();
    if !vars.iter().any(|(key, _)| *key == "SUBSYSTEM") {
        vars.push(("SUBSYSTEM", subsystem));
    }
    let mut bytes = Vec::new();
    encode(&mut bytes, "add", &devpath, vars.iter().copied());
    if UeventMsg::parse(&bytes).is_some_and(|msg| filter.matches(&msg)) {
        out.insert(devpath, bytes);
    }
}

/// The devpath of a resolved device: its path below the sysfs mount, with a
/// leading slash (`/devices/platform/.../3-1`).
pub(crate) fn devpath_of(real: &Path, sys_root: &Path) -> Option<String> {
    let relative = real.strip_prefix(sys_root).ok()?;
    Some(format!("/{}", relative.display()))
}

/// The messages that turn `old` into `new`: removes and changes for devices
/// that went away or changed (in devpath order), then adds for new devices.
/// A device whose identity (bus number, device number, major and minor)
/// changed under the same devpath is a remove and an add. A change of
/// `DRIVER` alone is a bind or an unbind.
pub(crate) fn diff(old: &Snapshot, new: &Snapshot) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    for (devpath, old_bytes) in old {
        let Some(new_bytes) = new.get(devpath) else {
            let Some(msg) = UeventMsg::parse(old_bytes) else {
                continue;
            };
            encode(&mut scratch, "remove", devpath, msg.vars());
            out.push(scratch.clone());
            continue;
        };
        if new_bytes == old_bytes {
            continue;
        }
        let (Some(before), Some(after)) =
            (UeventMsg::parse(old_bytes), UeventMsg::parse(new_bytes))
        else {
            continue;
        };
        if identity(&before) != identity(&after) {
            encode(&mut scratch, "remove", devpath, before.vars());
            out.push(scratch.clone());
            encode(&mut scratch, "add", devpath, after.vars());
            out.push(scratch.clone());
            continue;
        }
        let action = match (
            before.get("DRIVER").is_some(),
            after.get("DRIVER").is_some(),
        ) {
            (false, true) => "bind",
            (true, false) => "unbind",
            _ => "change",
        };
        encode(&mut scratch, action, devpath, after.vars());
        out.push(scratch.clone());
    }
    for (devpath, new_bytes) in new {
        if !old.contains_key(devpath) {
            out.push(new_bytes.clone());
        }
    }
    out
}

fn identity<'a>(msg: &UeventMsg<'a>) -> [Option<&'a str>; 4] {
    ["BUSNUM", "DEVNUM", "MAJOR", "MINOR"].map(|key| msg.get(key))
}
