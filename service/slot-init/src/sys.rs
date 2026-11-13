//! [`Ops`] on a board: lemnos-linux-sys's safe wrappers over the system calls.

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::rootfs::{self, Loop, MountFlags};
use lemnos_slot_store::manifest::{BLOCK, Verity};
use lemnos_slot_store::ubi::{self, Ubi};

use crate::boot::INIT;
use crate::ops::Ops;

/// The system calls, with the device nodes in `dev` and the modules in `modules`.
pub struct System {
    pub dev: PathBuf,
    pub modules: PathBuf,
    /// The loop devices under the verity devices, held until `switch_root` closes them.
    loops: RefCell<Vec<Loop>>,
}

impl System {
    pub fn new() -> Self {
        Self {
            dev: PathBuf::from("/dev"),
            modules: PathBuf::from("/lib/modules"),
            loops: RefCell::new(Vec::new()),
        }
    }
}

/// The major and minor numbers in a `dev_t` (glibc's and musl's encoding).
fn libc_major(rdev: u64) -> u64 {
    ((rdev >> 32) & 0xffff_f000) | ((rdev >> 8) & 0x0fff)
}

fn libc_minor(rdev: u64) -> u64 {
    ((rdev >> 12) & 0xffff_ff00) | (rdev & 0x00ff)
}

/// A module's name as the kernel compares it: `-` and `_` are the same.
fn module_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.split(".ko").next().unwrap_or(file);
    stem.replace('-', "_")
}

/// The files to load for `name`, dependencies first, from `modules.dep`'s text.
pub fn load_order(dep: &str, name: &str) -> Option<Vec<String>> {
    let want = module_name(name);
    dep.lines().find_map(|line| {
        let (module, deps) = line.split_once(':')?;
        (module_name(module) == want).then(|| {
            // modules.dep lists what a module needs nearest first: load the farthest first.
            let mut files: Vec<String> =
                deps.split_whitespace().rev().map(str::to_string).collect();
            files.push(module.to_string());
            files
        })
    })
}

/// The flags and the filesystem's own options in a `mount -o` string.
pub fn mount_options(opts: &str) -> (MountFlags, String) {
    let mut flags = MountFlags::default();
    let mut data = Vec::new();
    for opt in opts.split(',').filter(|o| !o.is_empty()) {
        match opt {
            "ro" => flags.read_only = true,
            "rw" => {}
            "noatime" => flags.no_atime = true,
            other => data.push(other),
        }
    }
    (flags, data.join(","))
}

impl Ops for System {
    fn log(&self, line: &str) {
        let text = format!("{line}\n");
        let written = fs::OpenOptions::new()
            .append(true)
            .open(self.dev.join("kmsg"))
            .and_then(|mut kmsg| kmsg.write_all(text.as_bytes()));
        if written.is_err() {
            let _ = io::stderr().write_all(text.as_bytes());
        }
    }

    fn mount(&self, fstype: &str, opts: &str, source: &Path, target: &Path) -> Result<(), String> {
        let (flags, data) = mount_options(opts);
        rootfs::mount(source, target, fstype, flags, &data).map_err(|e| e.to_string())
    }

    fn move_mount(&self, source: &Path, target: &Path) -> Result<(), String> {
        rootfs::move_mount(source, target).map_err(|e| e.to_string())
    }

    fn umount(&self, target: &Path) {
        if let Err(error) = rootfs::umount(target) {
            self.log(&format!(
                "slot-init: unmounting {}: {error}",
                target.display()
            ));
        }
    }

    fn load_module(&self, name: &str, release: &str) -> Result<(), String> {
        let base = self.modules.join(release);
        let dep = fs::read_to_string(base.join("modules.dep"))
            .map_err(|e| format!("{}: {e}", base.join("modules.dep").display()))?;
        let files = load_order(&dep, name)
            .ok_or("not in modules.dep: built into the kernel, or missing")?;
        for file in files {
            match rootfs::load_module(&base.join(&file), "") {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{file}: {e}")),
            }
        }
        Ok(())
    }

    fn open_verity(&self, image: &Path, name: &str, verity: &Verity) -> Result<String, String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let meta = fs::metadata(image).map_err(|e| format!("{}: {e}", image.display()))?;
        let (device, lo) = if meta.file_type().is_block_device() {
            let rdev = meta.rdev();
            (format!("{}:{}", libc_major(rdev), libc_minor(rdev)), None)
        } else {
            let lo = rootfs::attach_loop(&self.dev, image)
                .map_err(|e| format!("a loop device over {}: {e}", image.display()))?;
            (format!("{}:{}", lo.major, lo.minor), Some(lo))
        };
        let table = verity.table(&device);
        let params = table
            .split_once(" verity ")
            .map(|(_, params)| params)
            .ok_or("no verity table")?;
        let sectors = verity.data_blocks * (BLOCK / 512);
        let node =
            rootfs::create_verity(&self.dev, name, sectors, params).map_err(|e| e.to_string())?;
        self.loops.borrow_mut().extend(lo);
        Ok(node.display().to_string())
    }

    fn ubi_block(&self, ubi: &Ubi, name: &str) -> Result<PathBuf, String> {
        use lemnos_slot_store::ubi::UbiWrite;
        let node = ubi::System { ubi: ubi.clone() }
            .block(name)
            .map_err(|e| e.to_string())?;
        // udev isn't running: devtmpfs makes the node, a moment after the ioctl.
        for _ in 0..50 {
            if node.exists() {
                return Ok(node);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(format!("{} didn't appear", node.display()))
    }

    fn ubi_replace(&self, ubi: &Ubi, name: &str, bytes: &[u8]) -> Result<(), String> {
        ubi::replace(ubi, &ubi::System { ubi: ubi.clone() }, name, bytes).map_err(|e| e.to_string())
    }

    fn kernel_log(&self) -> String {
        rootfs::kernel_log().unwrap_or_else(|e| format!("(the kernel log couldn't be read: {e})\n"))
    }

    fn sync(&self) {
        rootfs::sync();
    }

    fn restart(&self, argument: Option<&str>) {
        let error = rootfs::restart(argument);
        self.log(&format!("slot-init: the restart failed: {error}"));
    }

    fn switch_root(&self, new_root: &Path) -> Result<(), String> {
        let init = std::ffi::CString::new(INIT).map_err(|e| e.to_string())?;
        Err(rootfs::switch_root(new_root, &init).to_string())
    }

    fn sleep(&self, time: Duration) {
        std::thread::sleep(time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_load_dependencies_first() {
        let dep = "kernel/drivers/md/dm-mod.ko.xz:\n\
                   kernel/drivers/md/dm-verity.ko: kernel/drivers/md/dm-bufio.ko kernel/drivers/md/dm-mod.ko.xz\n";
        assert_eq!(
            load_order(dep, "dm_verity").unwrap(),
            [
                "kernel/drivers/md/dm-mod.ko.xz",
                "kernel/drivers/md/dm-bufio.ko",
                "kernel/drivers/md/dm-verity.ko"
            ]
        );
        assert_eq!(
            load_order(dep, "dm-mod").unwrap(),
            ["kernel/drivers/md/dm-mod.ko.xz"]
        );
        assert_eq!(load_order(dep, "dm-crypt"), None);
    }

    #[test]
    fn a_device_number_splits_as_libc_does() {
        // makedev(254, 3) and makedev(4097, 300000) in glibc's and musl's encoding.
        assert_eq!((libc_major(0xfe03), libc_minor(0xfe03)), (254, 3));
        let rdev = ((4097u64 & 0xffff_f000) << 32)
            | ((4097 & 0xfff) << 8)
            | ((300_000u64 & 0xffff_ff00) << 12)
            | (300_000 & 0xff);
        assert_eq!((libc_major(rdev), libc_minor(rdev)), (4097, 300_000));
    }

    #[test]
    fn mount_options_split_into_flags_and_data() {
        let (flags, data) = mount_options("ro,noatime,errors=remount-ro");
        assert!(flags.read_only && flags.no_atime);
        assert_eq!(data, "errors=remount-ro");
        assert_eq!(mount_options("rw"), (MountFlags::default(), String::new()));
    }
}
