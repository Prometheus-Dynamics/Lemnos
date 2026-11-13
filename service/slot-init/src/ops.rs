//! Everything the boot does to the system, behind one trait: [`crate::sys::System`] makes the
//! system calls on a board, and the tests' fake writes down what it was asked. Files on the
//! mounted partitions are read and written directly: in the tests those are temporary
//! directories.

use std::path::Path;
use std::time::Duration;

use lemnos_slot_store::manifest::Verity;

/// What `/init` does to the system. Each failure is a sentence for the log.
pub trait Ops {
    /// One line for the kernel log, where a console and the saved log show it.
    fn log(&self, line: &str);
    /// `mount -t <fstype> -o <opts> <source> <target>`. `ro` and `noatime` in `opts` are
    /// mount flags; the rest goes to the filesystem.
    fn mount(&self, fstype: &str, opts: &str, source: &Path, target: &Path) -> Result<(), String>;
    /// Moves the mount at `source` to `target`.
    fn move_mount(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn umount(&self, target: &Path);
    /// Loads a kernel module for the running kernel `release`, and what it needs first.
    fn load_module(&self, name: &str, release: &str) -> Result<(), String>;
    /// A read-only loop device over `image` and a dm-verity device named `name` over that, as
    /// `verity` describes. Returns the verity device's node.
    fn open_verity(&self, image: &Path, name: &str, verity: &Verity) -> Result<String, String>;
    /// The kernel's log, for keeping after a failure.
    fn kernel_log(&self) -> String;
    fn sync(&self);
    /// A restart, with the boot loader's argument when given (`0 tryboot`). Returns only when
    /// it couldn't.
    fn restart(&self, argument: Option<&str>);
    /// Into the new root, as PID 1. On a board it returns only when it couldn't.
    fn switch_root(&self, new_root: &Path) -> Result<(), String>;
    fn sleep(&self, time: Duration);
}
