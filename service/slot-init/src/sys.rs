//! [`Ops`] on a board: lemnos-linux-sys's safe wrappers over the system calls.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::rootfs::{self, Loop, MountFlags};
use lemnos_slot_store::manifest::{BLOCK, Verity};
use lemnos_slot_store::ubi::{self, Ubi};

use crate::boot::INIT;
use crate::dog::{self, Armed, Device, FEED_EVERY};
use crate::ops::Ops;

/// The system calls, with the device nodes in `dev`.
pub struct System {
    pub dev: PathBuf,
    /// The loop devices under the verity devices, held until `switch_root` closes them.
    loops: RefCell<Vec<Loop>>,
    /// The armed watchdog device, once [`Ops::arm_watchdog`] has opened it, with its feed interval
    /// and any refusal (see [`crate::dog`]). Never closed with the magic character, so the device
    /// stays armed after this process is gone; the descriptor is close-on-exec, so the exec of
    /// `switch_root` is the one hand-over that closes it.
    dog: RefCell<Option<Armed<Node>>>,
    /// Whether the watchdog is fed. Turned off once `boot` has returned, so a slot-init that
    /// fails and loops is not kept alive by its own log lines.
    feeding: Cell<bool>,
}

impl System {
    pub fn new() -> Self {
        Self {
            dev: PathBuf::from("/dev"),
            loops: RefCell::new(Vec::new()),
            dog: RefCell::new(None),
            feeding: Cell::new(true),
        }
    }

    /// One feed of the armed watchdog. A write that fails feeds nothing, so the board resets.
    fn feed(&self) {
        if !self.feeding.get() {
            return;
        }
        if let Some(armed) = self.dog.borrow_mut().as_mut() {
            let _ = armed.device.feed();
        }
    }

    /// How often the armed watchdog is fed: [`FEED_EVERY`] when nothing is armed.
    fn feed_every(&self) -> Duration {
        self.dog
            .borrow()
            .as_ref()
            .map_or(FEED_EVERY, |armed| armed.feed_every)
    }
}

/// The watchdog device node, `<dev>/watchdog`, opened for writing (an open arms it).
pub struct Node(fs::File);

impl Device for Node {
    fn set_timeout(&mut self, seconds: u32) -> io::Result<u32> {
        lemnos_driver_hw_watchdog_sys::set_timeout(self.0.as_fd(), seconds)
    }

    fn timeout(&mut self) -> io::Result<u32> {
        lemnos_driver_hw_watchdog_sys::get_timeout(self.0.as_fd())
    }

    fn feed(&mut self) -> io::Result<()> {
        self.0.write_all(&[FEED_BYTE])
    }
}

/// The byte a feed writes. Any byte but the magic close character `V` feeds the watchdog; `V`
/// would stop it, and it is never written.
pub const FEED_BYTE: u8 = 0;

/// Opens `<dev>/watchdog` for writing: the open arms the device.
fn open_node(dev: &Path) -> io::Result<Node> {
    fs::OpenOptions::new()
        .write(true)
        .open(dev.join("watchdog"))
        .map(Node)
}

/// The major and minor numbers in a `dev_t` (glibc's and musl's encoding).
fn libc_major(rdev: u64) -> u64 {
    ((rdev >> 32) & 0xffff_f000) | ((rdev >> 8) & 0x0fff)
}

fn libc_minor(rdev: u64) -> u64 {
    ((rdev >> 12) & 0xffff_ff00) | (rdev & 0x00ff)
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
        self.feed();
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

    fn bind_read_only(&self, source: &Path, target: &Path) -> Result<(), String> {
        rootfs::bind(source, target).map_err(|e| format!("bind mount: {e}"))?;
        if let Err(error) = rootfs::remount_read_only(target) {
            let _ = rootfs::umount(target);
            return Err(format!("read-only remount: {error}"));
        }
        Ok(())
    }

    fn umount(&self, target: &Path) {
        if let Err(error) = rootfs::umount(target) {
            self.log(&format!(
                "slot-init: unmounting {}: {error}",
                target.display()
            ));
        }
    }

    fn dm_targets(&self) -> Result<Vec<String>, String> {
        rootfs::dm_targets(&self.dev).map_err(|e| e.to_string())
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
        // The last feed before the exec. The exec closes the descriptor (close-on-exec), and the
        // device then runs unfed until lemnosd's first feed: the gap is bounded by the applied period
        // (docs/slots.md, "The watchdog chain").
        self.feed();
        Err(rootfs::switch_root(new_root, &init).to_string())
    }

    fn sleep(&self, time: Duration) {
        // A wait that is meant to end feeds the watchdog as it goes, so a long wait (the rescue's)
        // does not reset the board on purpose. The feeding stops with `stop_feeding`.
        let mut left = time;
        while !left.is_zero() {
            self.feed();
            let step = left.min(self.feed_every());
            std::thread::sleep(step);
            left -= step;
        }
    }

    fn arm_watchdog(&self) {
        let node = self.dev.join("watchdog");
        let mut open = || open_node(&self.dev);
        // A wait between opens does not feed (there is no device to feed yet); the kernel feeds the
        // device it probed until its open timeout (docs/kernel-config.md, "Watchdog").
        let mut pause = std::thread::sleep;
        match dog::arm(&mut open, &mut pause) {
            Ok(armed) => {
                let every = armed.feed_every.as_millis();
                let note = armed.note.clone();
                let refusal = armed.refusal.clone();
                let applied = armed
                    .applied_s
                    .map_or_else(|| "unknown".to_string(), |s| format!("{s} s"));
                *self.dog.borrow_mut() = Some(armed);
                if let Some(note) = note {
                    self.log(&format!("slot-init: watchdog: {note}"));
                }
                match refusal {
                    None => self.log(&format!(
                        "slot-init: watchdog armed: {applied} (asked for {} s); slot-init feeds it every {every} ms while it works and never closes it with the magic character",
                        dog::DOG_SECONDS
                    )),
                    Some(refusal) => self.log(&format!(
                        "slot-init: WATCHDOG REFUSED ({}): {}: {}: slot-init keeps feeding it every {every} ms while boot runs; the board's watchdog period is outside the bound (contracts/slot-kernel.toml, [watchdog])",
                        refusal.reason,
                        node.display(),
                        refusal.detail
                    )),
                }
            }
            Err(absent) => self.log(&format!(
                "slot-init: NO WATCHDOG ({}): {}: {}: the device is not armed by slot-init; only the kernel's feeding, until its open timeout, runs before lemnosd binds it",
                absent.reason,
                node.display(),
                absent.detail
            )),
        }
    }

    fn stop_feeding(&self) {
        self.feeding.set(false);
    }

    fn request_flash_mode(&self) -> Result<(), String> {
        // To confirm on a board: devtmpfs makes /dev/vcio when the firmware driver is built in or
        // loaded; wait for it as the UBI block node's wait does.
        let node = self.dev.join("vcio");
        for _ in 0..50 {
            if node.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        lemnos_driver_rpi_firmware_sys::Mailbox::open(&node)
            .and_then(|mailbox| mailbox.request_rpiboot())
            .map_err(|e| format!("{}: {e}", node.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A directory with `watchdog` as a plain file. The stand-in differs from a watchdog device in
    /// that it refuses the watchdog's requests (ENOTTY) and takes writes as ordinary file bytes.
    fn stand_in() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "slot-init-dog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let node = dir.join("watchdog");
        fs::write(&node, b"").unwrap();
        (dir, node)
    }

    /// The armed state of a plain file, as a device the arm would have given (fed every 5 s).
    fn stand_in_armed(node: &Path) -> Armed<Node> {
        Armed {
            device: Node(fs::OpenOptions::new().write(true).open(node).unwrap()),
            applied_s: Some(15),
            feed_every: FEED_EVERY,
            refusal: None,
            note: None,
        }
    }

    #[test]
    fn the_feed_byte_is_never_the_magic_close() {
        assert_ne!(FEED_BYTE, b'V');
    }

    #[test]
    fn a_plain_file_is_refused_its_timeout_and_its_period_cannot_be_read() {
        // Real-failing case: a plain file refuses SETTIMEOUT (ENOTTY), and GETTIMEOUT on it fails too.
        // The arm keeps the device (it was opened) and names the refusal: it is never dropped.
        let (dir, _node) = stand_in();
        let mut open = || open_node(&dir);
        let armed = dog::arm(&mut open, &mut |_| {}).expect("the node opens");
        assert_eq!(
            armed.refusal.map(|r| r.reason),
            Some(dog::REASON_UNREADABLE)
        );
        assert_eq!(armed.feed_every, FEED_EVERY);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_feed_writes_one_byte_that_is_not_the_magic_close_and_a_stopped_feed_writes_nothing() {
        let (dir, node) = stand_in();
        let system = System {
            dev: dir.clone(),
            loops: RefCell::new(Vec::new()),
            dog: RefCell::new(None),
            feeding: Cell::new(true),
        };
        *system.dog.borrow_mut() = Some(stand_in_armed(&node));
        system.feed();
        assert_eq!(fs::read(&node).unwrap(), vec![FEED_BYTE]);
        system.stop_feeding();
        system.feed();
        assert_eq!(
            fs::read(&node).unwrap(),
            vec![FEED_BYTE],
            "a stopped feed writes nothing"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_long_wait_feeds_while_it_runs_and_a_stopped_one_does_not() {
        let (dir, node) = stand_in();
        let system = System {
            dev: dir.clone(),
            loops: RefCell::new(Vec::new()),
            dog: RefCell::new(Some(stand_in_armed(&node))),
            feeding: Cell::new(true),
        };
        system.sleep(Duration::from_millis(12));
        assert!(!fs::read(&node).unwrap().is_empty(), "a wait feeds");
        fs::write(&node, b"").unwrap();
        system.stop_feeding();
        system.sleep(Duration::from_millis(12));
        assert!(
            fs::read(&node).unwrap().is_empty(),
            "a stopped system does not feed"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
