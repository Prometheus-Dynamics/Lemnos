//! The inotify source: a watch per directory the filter covers, and one
//! synthesized event per entry that appears, goes away or changes. The
//! devpath is the entry's path below the sysfs mount (class view, such as
//! `/class/gpio/gpiochip7`); `/dev` entries carry `DEVNAME` and no devpath.

use super::Filter;
use super::paths::LinuxPaths;
use super::read_dir_sorted;
use super::sysfs::ROOTS;
use super::uevent::{UeventMsg, UeventRef, encode};
use lemnos_linux_sys::inotify::{self as sys, mask};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

const WATCH_MASK: u32 = mask::CREATE
    | mask::DELETE
    | mask::MOVED_FROM
    | mask::MOVED_TO
    | mask::ATTRIB
    | mask::DELETE_SELF
    | mask::MOVE_SELF;
const EVENT_BUFFER_SIZE: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct Watches {
    file: File,
    buffer: Vec<u8>,
    /// Directory path -> watch descriptor.
    watched: BTreeMap<PathBuf, i32>,
    registrations: BTreeMap<i32, Registration>,
    /// Reused for each synthesized devpath.
    devpath: String,
}

#[derive(Debug, Clone)]
struct Registration {
    path: PathBuf,
    /// The devpath of the directory (`/class/gpio`), or empty for `/dev`.
    devpath: String,
    kind: Kind,
    /// A `pwmchipN` directory found by the resync (not a configured root).
    dynamic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A sysfs directory of one subsystem.
    Sysfs { subsystem: &'static str },
    /// The `/dev` directory: nodes named `video*` and `media*`.
    Dev,
}

impl Watches {
    /// Watches the filter's sysfs roots that exist, plus `/dev` when the
    /// filter has device nodes it reports.
    pub(crate) fn open(paths: &LinuxPaths, filter: &Filter) -> io::Result<Watches> {
        let fd = sys::init()?;
        let mut watches = Watches {
            file: File::from(fd),
            buffer: vec![0; EVENT_BUFFER_SIZE],
            watched: BTreeMap::new(),
            registrations: BTreeMap::new(),
            devpath: String::new(),
        };
        for root in ROOTS.iter().filter(|root| filter.includes(root.subsystem)) {
            watches.add(
                root.path(paths),
                paths,
                Kind::Sysfs {
                    subsystem: root.subsystem,
                },
                false,
            )?;
        }
        if filter.includes("video4linux") || filter.includes("media") {
            watches.add(paths.dev_root.clone(), paths, Kind::Dev, false)?;
        }
        watches.sync_pwm_chips(paths, filter)?;
        Ok(watches)
    }

    fn add(
        &mut self,
        path: PathBuf,
        paths: &LinuxPaths,
        kind: Kind,
        dynamic: bool,
    ) -> io::Result<()> {
        if self.watched.contains_key(&path) || !path.exists() {
            return Ok(());
        }
        let descriptor = sys::add_watch(self.file.as_fd(), &path, WATCH_MASK)?;
        let devpath = match kind {
            Kind::Sysfs { .. } => devpath_below_sys(&path, paths),
            Kind::Dev => String::new(),
        };
        self.watched.insert(path.clone(), descriptor);
        self.registrations.insert(
            descriptor,
            Registration {
                path,
                devpath,
                kind,
                dynamic,
            },
        );
        Ok(())
    }

    fn remove(&mut self, path: &Path) {
        let Some(descriptor) = self.watched.remove(path) else {
            return;
        };
        self.registrations.remove(&descriptor);
        // Already gone (the kernel dropped it with the directory): nothing to do.
        let _ = sys::rm_watch(self.file.as_fd(), descriptor);
    }

    /// Keeps a watch on each `pwmchipN` directory: channels are created inside
    /// them, which the class root does not see.
    fn sync_pwm_chips(&mut self, paths: &LinuxPaths, filter: &Filter) -> io::Result<()> {
        if !filter.includes("pwm") {
            return Ok(());
        }
        let Some(pwm) = ROOTS.iter().find(|root| root.subsystem == "pwm") else {
            return Ok(());
        };
        let current: Vec<PathBuf> = read_dir_sorted(&pwm.path(paths))
            .unwrap_or_default()
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("pwmchip"))
            })
            .collect();
        let stale: Vec<PathBuf> = self
            .registrations
            .values()
            .filter(|reg| reg.dynamic && !current.contains(&reg.path))
            .map(|reg| reg.path.clone())
            .collect();
        for path in stale {
            self.remove(&path);
        }
        for chip in current {
            self.add(chip, paths, Kind::Sysfs { subsystem: "pwm" }, true)?;
        }
        Ok(())
    }

    /// Reads and reports the pending events. Returns the records consumed
    /// (0: nothing was pending).
    pub(crate) fn read(
        &mut self,
        paths: &LinuxPaths,
        filter: &Filter,
        scratch: &mut Vec<u8>,
        f: &mut impl FnMut(UeventRef<'_>),
    ) -> io::Result<usize> {
        self.sync_pwm_chips(paths, filter)?;
        let n = match self.file.read(&mut self.buffer) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(0),
            Err(error) => return Err(error),
        };
        let Watches {
            buffer,
            watched,
            registrations,
            devpath,
            ..
        } = self;
        let mut consumed = 0usize;
        let mut resync_pwm = false;
        sys::for_each_event(&buffer[..n], |wd, bits, name| {
            consumed += 1;
            if bits & mask::Q_OVERFLOW != 0 {
                f(UeventRef::Overflow);
                return;
            }
            if let Some(reg) = registrations.get(&wd) {
                // A name that is not UTF-8 cannot be a device name: skipped.
                let name = match name {
                    Some(name) => match name.to_str() {
                        Some(name) => Some(name),
                        None => return,
                    },
                    None => None,
                };
                if let Some(event) = synthesize(reg, bits, name, filter, devpath, scratch) {
                    resync_pwm |= matches!(reg.kind, Kind::Sysfs { subsystem: "pwm" });
                    f(UeventRef::Event(event));
                }
            }
            if bits & mask::IGNORED != 0
                && let Some(reg) = registrations.remove(&wd)
            {
                watched.remove(&reg.path);
            }
        });
        if resync_pwm {
            self.sync_pwm_chips(paths, filter)?;
        }
        Ok(consumed)
    }
}

impl AsFd for Watches {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// The message for one inotify event, built in `scratch`. `None` when the
/// event is not for the filter or is not a change this source reports.
fn synthesize<'s>(
    reg: &Registration,
    bits: u32,
    name: Option<&str>,
    filter: &Filter,
    devpath: &mut String,
    scratch: &'s mut Vec<u8>,
) -> Option<UeventMsg<'s>> {
    let action = if bits & (mask::CREATE | mask::MOVED_TO) != 0 {
        "add"
    } else if bits & (mask::DELETE | mask::MOVED_FROM | mask::DELETE_SELF | mask::MOVE_SELF) != 0 {
        "remove"
    } else if bits & mask::ATTRIB != 0 {
        "change"
    } else {
        return None;
    };
    let subsystem = match (reg.kind, name) {
        (Kind::Sysfs { subsystem }, _) => subsystem,
        (Kind::Dev, Some(name)) if name.starts_with("video") => "video4linux",
        (Kind::Dev, Some(name)) if name.starts_with("media") => "media",
        (Kind::Dev, _) => return None,
    };
    if !filter.includes(subsystem) {
        return None;
    }
    devpath.clear();
    devpath.push_str(&reg.devpath);
    let mut vars: Vec<(&str, &str)> = vec![("SUBSYSTEM", subsystem)];
    match (reg.kind, name) {
        (Kind::Sysfs { .. }, Some(name)) => {
            devpath.push('/');
            devpath.push_str(name);
        }
        (Kind::Dev, Some(name)) => vars.push(("DEVNAME", name)),
        _ => {}
    }
    encode(scratch, action, devpath, vars.iter().copied());
    UeventMsg::parse(scratch.as_slice())
}

/// The devpath of a sysfs directory: its path below the sysfs mount.
fn devpath_below_sys(path: &Path, paths: &LinuxPaths) -> String {
    match path.strip_prefix(paths.sys_root()) {
        Ok(relative) => format!("/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}
