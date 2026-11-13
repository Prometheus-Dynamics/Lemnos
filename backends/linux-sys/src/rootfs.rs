//! What an initramfs needs to start a root filesystem: mounts, a read-only loop device over an
//! image file, a dm-verity device over that, kernel modules, the kernel's log, a restart (with
//! the boot loader's argument), and the switch into the new root.
//!
//! Every call here changes the running system, so the tests cover only the structures, the
//! request numbers and the argument building. Nothing here is called by a test.

use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::ioctl::{ioctl_int, ioctl_ptr, iowr};

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn c_text(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn check(ret: libc::c_int) -> io::Result<()> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// How a filesystem is mounted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MountFlags {
    /// `MS_RDONLY`.
    pub read_only: bool,
    /// `MS_NOATIME`.
    pub no_atime: bool,
}

impl MountFlags {
    /// The `MS_*` bits.
    pub fn bits(self) -> libc::c_ulong {
        let mut bits = 0;
        if self.read_only {
            bits |= libc::MS_RDONLY;
        }
        if self.no_atime {
            bits |= libc::MS_NOATIME;
        }
        bits
    }
}

fn mount_raw(
    source: Option<&Path>,
    target: &Path,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    let source = source.map(c_path).transpose()?;
    let target = c_path(target)?;
    let fstype = fstype.map(c_text).transpose()?;
    let data = c_text(data)?;
    // SAFETY: every pointer is a NUL-terminated string alive for the call, or null where mount(2)
    // allows it (source and fstype for a move or a bind).
    check(unsafe {
        libc::mount(
            source.as_ref().map_or(core::ptr::null(), |s| s.as_ptr()),
            target.as_ptr(),
            fstype.as_ref().map_or(core::ptr::null(), |s| s.as_ptr()),
            flags,
            data.as_ptr().cast(),
        )
    })
}

/// `mount -t <fstype> -o <data> <source> <target>`.
pub fn mount(
    source: &Path,
    target: &Path,
    fstype: &str,
    flags: MountFlags,
    data: &str,
) -> io::Result<()> {
    mount_raw(Some(source), target, Some(fstype), flags.bits(), data)
}

/// A bind mount of `source` at `target`.
pub fn bind(source: &Path, target: &Path) -> io::Result<()> {
    mount_raw(Some(source), target, None, libc::MS_BIND, "")
}

/// Moves the mount at `source` to `target`.
pub fn move_mount(source: &Path, target: &Path) -> io::Result<()> {
    mount_raw(Some(source), target, None, libc::MS_MOVE, "")
}

/// Unmounts `target`.
pub fn umount(target: &Path) -> io::Result<()> {
    let target = c_path(target)?;
    // SAFETY: `target` is a NUL-terminated string alive for the call.
    check(unsafe { libc::umount2(target.as_ptr(), 0) })
}

/// `struct loop_info64`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoopInfo64 {
    /// `lo_device`, as the kernel's uAPI has it.
    pub lo_device: u64,
    /// `lo_inode`, as the kernel's uAPI has it.
    pub lo_inode: u64,
    /// `lo_rdevice`, as the kernel's uAPI has it.
    pub lo_rdevice: u64,
    /// `lo_offset`, as the kernel's uAPI has it.
    pub lo_offset: u64,
    /// `lo_sizelimit`, as the kernel's uAPI has it.
    pub lo_sizelimit: u64,
    /// `lo_number`, as the kernel's uAPI has it.
    pub lo_number: u32,
    /// `lo_encrypt_type`, as the kernel's uAPI has it.
    pub lo_encrypt_type: u32,
    /// `lo_encrypt_key_size`, as the kernel's uAPI has it.
    pub lo_encrypt_key_size: u32,
    /// `lo_flags`, as the kernel's uAPI has it.
    pub lo_flags: u32,
    /// `lo_file_name`, as the kernel's uAPI has it.
    pub lo_file_name: [u8; 64],
    /// `lo_crypt_name`, as the kernel's uAPI has it.
    pub lo_crypt_name: [u8; 64],
    /// `lo_encrypt_key`, as the kernel's uAPI has it.
    pub lo_encrypt_key: [u8; 32],
    /// `lo_init`, as the kernel's uAPI has it.
    pub lo_init: [u64; 2],
}

/// `struct loop_config`, for `LOOP_CONFIGURE`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoopConfig {
    /// `fd`, as the kernel's uAPI has it.
    pub fd: u32,
    /// `block_size`, as the kernel's uAPI has it.
    pub block_size: u32,
    /// `info`, as the kernel's uAPI has it.
    pub info: LoopInfo64,
    /// `reserved`, as the kernel's uAPI has it.
    pub reserved: [u64; 8],
}

// SAFETY: integers and integer arrays only; every bit pattern is a value.
unsafe impl crate::ioctl::Pod for LoopConfig {}

/// `LOOP_CTL_GET_FREE`.
pub const LOOP_CTL_GET_FREE: u32 = 0x4C82;
/// `LOOP_CONFIGURE`.
pub const LOOP_CONFIGURE: u32 = 0x4C0A;
/// `LO_FLAGS_READ_ONLY`.
pub const LO_FLAGS_READ_ONLY: u32 = 1;
/// `LO_FLAGS_AUTOCLEAR`: the loop device lets go of the file when its last user closes it.
pub const LO_FLAGS_AUTOCLEAR: u32 = 4;

/// The configuration of a read-only loop device over the file open as `fd`.
pub fn read_only_loop(fd: u32) -> LoopConfig {
    let mut config: LoopConfig = crate::ioctl::zeroed();
    config.fd = fd;
    config.info.lo_flags = LO_FLAGS_READ_ONLY | LO_FLAGS_AUTOCLEAR;
    config
}

/// A loop device and its device number.
#[derive(Debug)]
pub struct Loop {
    /// Held open: the device stays bound while it is (it clears itself after the last close).
    pub file: File,
    /// The device node.
    pub path: PathBuf,
    /// Its major number.
    pub major: u32,
    /// Its minor number.
    pub minor: u32,
}

/// A free loop device, bound read-only to `image`. `dev` is where the device nodes are.
pub fn attach_loop(dev: &Path, image: &Path) -> io::Result<Loop> {
    let control = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dev.join("loop-control"))?;
    let number = ioctl_int(control.as_fd(), LOOP_CTL_GET_FREE, 0)?;
    let path = dev.join(format!("loop{number}"));
    let device = OpenOptions::new().read(true).open(&path)?;
    let backing = File::open(image)?;
    let fd = u32::try_from(backing.as_raw_fd())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut config = read_only_loop(fd);
    // SAFETY: LOOP_CONFIGURE reads one `loop_config`; `config` is one, alive for the call, and
    // the descriptor in it is open.
    unsafe { ioctl_ptr(device.as_fd(), LOOP_CONFIGURE, &raw mut config) }?;
    let rdev = device.metadata()?.rdev();
    Ok(Loop {
        file: device,
        path,
        major: libc::major(rdev),
        minor: libc::minor(rdev),
    })
}

/// `struct dm_ioctl`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmIoctl {
    /// `version`, as the kernel's uAPI has it.
    pub version: [u32; 3],
    /// `data_size`, as the kernel's uAPI has it.
    pub data_size: u32,
    /// `data_start`, as the kernel's uAPI has it.
    pub data_start: u32,
    /// `target_count`, as the kernel's uAPI has it.
    pub target_count: u32,
    /// `open_count`, as the kernel's uAPI has it.
    pub open_count: i32,
    /// `flags`, as the kernel's uAPI has it.
    pub flags: u32,
    /// `event_nr`, as the kernel's uAPI has it.
    pub event_nr: u32,
    /// `padding`, as the kernel's uAPI has it.
    pub padding: u32,
    /// `dev`, as the kernel's uAPI has it.
    pub dev: u64,
    /// `name`, as the kernel's uAPI has it.
    pub name: [u8; 128],
    /// `uuid`, as the kernel's uAPI has it.
    pub uuid: [u8; 129],
    /// `data`, as the kernel's uAPI has it.
    pub data: [u8; 7],
}

/// `struct dm_target_spec`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmTargetSpec {
    /// `sector_start`, as the kernel's uAPI has it.
    pub sector_start: u64,
    /// `length`, as the kernel's uAPI has it.
    pub length: u64,
    /// `status`, as the kernel's uAPI has it.
    pub status: i32,
    /// `next`, as the kernel's uAPI has it.
    pub next: u32,
    /// `target_type`, as the kernel's uAPI has it.
    pub target_type: [u8; 16],
}

// SAFETY: integers and integer arrays only; every bit pattern is a value.
unsafe impl crate::ioctl::Pod for DmIoctl {}
// SAFETY: integers and integer arrays only; every bit pattern is a value.
unsafe impl crate::ioctl::Pod for DmTargetSpec {}

/// `DM_DEV_CREATE`.
pub const DM_DEV_CREATE: u32 = iowr::<DmIoctl>(0xfd, 3);
/// `DM_DEV_SUSPEND`: resumes a device when the suspend flag is clear.
pub const DM_DEV_SUSPEND: u32 = iowr::<DmIoctl>(0xfd, 6);
/// `DM_TABLE_LOAD`.
pub const DM_TABLE_LOAD: u32 = iowr::<DmIoctl>(0xfd, 9);
/// `DM_READONLY_FLAG`.
pub const DM_READONLY_FLAG: u32 = 1;

/// A `dm_ioctl` for device `name`, with room for `extra` bytes after it, as one buffer.
pub fn dm_request(name: &str, extra: usize) -> io::Result<Vec<u8>> {
    if name.is_empty() || name.len() >= 128 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let size = (size_of::<DmIoctl>() + extra).next_multiple_of(8);
    let mut head: DmIoctl = crate::ioctl::zeroed();
    head.version = [4, 0, 0];
    head.data_size =
        u32::try_from(size).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    head.data_start = size_of::<DmIoctl>() as u32;
    head.name[..name.len()].copy_from_slice(name.as_bytes());
    let mut buf = vec![0u8; size];
    buf[..size_of::<DmIoctl>()].copy_from_slice(as_bytes(&head));
    Ok(buf)
}

/// A table load of one target over `sectors` sectors: `target` (`"verity"`) with `params`.
pub fn dm_table(name: &str, sectors: u64, target: &str, params: &str) -> io::Result<Vec<u8>> {
    if target.len() >= 16 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let spec_len = size_of::<DmTargetSpec>() + params.len() + 1;
    let mut buf = dm_request(name, spec_len.next_multiple_of(8))?;
    let mut head: DmIoctl = crate::ioctl::from_bytes(&buf);
    head.target_count = 1;
    head.flags = DM_READONLY_FLAG;
    buf[..size_of::<DmIoctl>()].copy_from_slice(as_bytes(&head));
    let mut spec: DmTargetSpec = crate::ioctl::zeroed();
    spec.length = sectors;
    spec.target_type[..target.len()].copy_from_slice(target.as_bytes());
    let at = size_of::<DmIoctl>();
    buf[at..at + size_of::<DmTargetSpec>()].copy_from_slice(as_bytes(&spec));
    let params_at = at + size_of::<DmTargetSpec>();
    buf[params_at..params_at + params.len()].copy_from_slice(params.as_bytes());
    Ok(buf)
}

fn as_bytes<T: crate::ioctl::Pod>(value: &T) -> &[u8] {
    // SAFETY: `T` is plain data with no padding the kernel reads as meaningful; the slice covers
    // exactly the value and lives as long as the borrow.
    unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

fn dm_call(control: &File, request: u32, buf: &mut [u8]) -> io::Result<DmIoctl> {
    // SAFETY: `buf` starts with a `dm_ioctl` whose `data_size` is `buf`'s length, so the kernel
    // reads and writes within it; it is alive for the call.
    unsafe { ioctl_ptr(control.as_fd(), request, buf.as_mut_ptr()) }?;
    Ok(crate::ioctl::from_bytes(buf))
}

/// A read-only dm-verity device `name` with `table` (`<data_blocks*8>` sectors, the verity
/// parameters after the target name), created, loaded and resumed. Returns its node in `dev`.
pub fn create_verity(dev: &Path, name: &str, sectors: u64, params: &str) -> io::Result<PathBuf> {
    let control = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dev.join("mapper/control"))?;
    let mut create = dm_request(name, 0)?;
    let created = dm_call(&control, DM_DEV_CREATE, &mut create)?;
    let mut table = dm_table(name, sectors, "verity", params)?;
    dm_call(&control, DM_TABLE_LOAD, &mut table)?;
    let mut resume = dm_request(name, 0)?;
    dm_call(&control, DM_DEV_SUSPEND, &mut resume)?;
    let minor = libc::minor(created.dev);
    Ok(dev.join(format!("dm-{minor}")))
}

/// Loads the kernel module in `file` with `params`.
pub fn load_module(file: &Path, params: &str) -> io::Result<()> {
    let module = File::open(file)?;
    let params = c_text(params)?;
    // SAFETY: finit_module takes a descriptor, a NUL-terminated string alive for the call, and
    // flags.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_finit_module,
            module.as_raw_fd(),
            params.as_ptr(),
            0 as libc::c_int,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The kernel's log buffer, as text.
pub fn kernel_log() -> io::Result<String> {
    let mut buf = vec![0u8; 256 * 1024];
    let len = libc::c_int::try_from(buf.len()).unwrap_or(libc::c_int::MAX);
    // SAFETY: SYSLOG_ACTION_READ_ALL (3) writes at most `len` bytes into `buf`, which is that
    // long and alive for the call.
    let got = unsafe { libc::klogctl(3, buf.as_mut_ptr().cast(), len) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(usize::try_from(got).unwrap_or(0));
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Writes what is cached to the disks.
pub fn sync() {
    // SAFETY: sync takes nothing and cannot fail.
    unsafe { libc::sync() };
}

/// Restarts the system now, with `argument` for the boot loader when given (`0 tryboot`).
/// Returns only when it couldn't.
pub fn restart(argument: Option<&str>) -> io::Error {
    sync();
    match argument {
        None => {
            // SAFETY: reboot(RB_AUTOBOOT) takes no memory.
            unsafe { libc::reboot(libc::RB_AUTOBOOT) };
        }
        Some(argument) => {
            let Ok(argument) = c_text(argument) else {
                return io::Error::from(io::ErrorKind::InvalidInput);
            };
            // SAFETY: the reboot system call with RESTART2 reads one NUL-terminated string,
            // alive for the call.
            unsafe {
                libc::syscall(
                    libc::SYS_reboot,
                    libc::LINUX_REBOOT_MAGIC1,
                    libc::LINUX_REBOOT_MAGIC2,
                    libc::LINUX_REBOOT_CMD_RESTART2,
                    argument.as_ptr(),
                )
            };
        }
    }
    io::Error::last_os_error()
}

/// Makes `new_root` the root and runs `init` in it as this process (PID 1): chdir there, move
/// its mount onto `/`, chroot, and exec. Returns only when it couldn't.
pub fn switch_root(new_root: &Path, init: &CStr) -> io::Error {
    if let Err(error) = std::env::set_current_dir(new_root) {
        return error;
    }
    if let Err(error) = move_mount(Path::new("."), Path::new("/")) {
        return error;
    }
    // SAFETY: "." is a NUL-terminated string literal.
    if unsafe { libc::chroot(c".".as_ptr()) } < 0 {
        return io::Error::last_os_error();
    }
    if let Err(error) = std::env::set_current_dir("/") {
        return error;
    }
    let argv = [init.as_ptr(), core::ptr::null()];
    // SAFETY: `init` is NUL-terminated and `argv` a null-terminated array of such strings, both
    // alive for the call; execv returns only on failure.
    unsafe { libc::execv(init.as_ptr(), argv.as_ptr()) };
    io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structures_are_the_kernels_sizes() {
        assert_eq!(size_of::<LoopInfo64>(), 232);
        assert_eq!(size_of::<LoopConfig>(), 304);
        assert_eq!(size_of::<DmIoctl>(), 312);
        assert_eq!(size_of::<DmTargetSpec>(), 40);
    }

    #[test]
    fn the_request_numbers_are_the_kernels() {
        assert_eq!(DM_DEV_CREATE, 0xc138_fd03);
        assert_eq!(DM_DEV_SUSPEND, 0xc138_fd06);
        assert_eq!(DM_TABLE_LOAD, 0xc138_fd09);
    }

    #[test]
    fn a_loop_device_is_read_only_and_clears_itself() {
        let config = read_only_loop(7);
        assert_eq!(config.fd, 7);
        assert_eq!(
            config.info.lo_flags,
            LO_FLAGS_READ_ONLY | LO_FLAGS_AUTOCLEAR
        );
    }

    #[test]
    fn a_table_load_holds_one_read_only_target_and_its_parameters() {
        let buf = dm_table("slot", 2400, "verity", "1 7:0 7:0 4096").unwrap();
        assert_eq!(buf.len() % 8, 0);
        let head: DmIoctl = crate::ioctl::from_bytes(&buf);
        assert_eq!(head.version, [4, 0, 0]);
        assert_eq!(head.data_size as usize, buf.len());
        assert_eq!(head.data_start, 312);
        assert_eq!(head.target_count, 1);
        assert_eq!(head.flags, DM_READONLY_FLAG);
        assert_eq!(&head.name[..5], b"slot\0");
        let spec: DmTargetSpec = crate::ioctl::from_bytes(&buf[312..]);
        assert_eq!((spec.sector_start, spec.length), (0, 2400));
        assert_eq!(&spec.target_type[..7], b"verity\0");
        let params = &buf[352..];
        assert!(params.starts_with(b"1 7:0 7:0 4096\0"));
        assert!(dm_request("", 0).is_err());
        assert!(dm_request(&"n".repeat(128), 0).is_err());
        assert!(dm_table("slot", 1, "a-very-long-target", "").is_err());
    }

    #[test]
    fn mount_flags_are_the_kernels_bits() {
        let flags = MountFlags {
            read_only: true,
            no_atime: true,
        };
        assert_eq!(flags.bits(), libc::MS_RDONLY | libc::MS_NOATIME);
        assert_eq!(MountFlags::default().bits(), 0);
    }
}
