//! This board as a USB device ([`UsbFunction`]) on Linux: one gadget in configfs, made by the
//! privileged setup ([`prepare`]) and attached by the daemon ([`FunctionFsGadget`]).
//!
//! Untested on board: the checks are `testing/board-runs/R1.md`.
//!
//! # Who does what
//!
//! configfs and mounts need root, and the daemon runs as the `lemnos` user. So the two halves
//! split at the gadget's configuration:
//!
//! - [`prepare`], run as root in the daemon's root phase (`lemnosd`, before it drops to the
//!   `lemnos` user; see `service/lemnosd/src/gadget.rs`), makes the
//!   gadget `lemnos-NAME` in configfs: its identity and strings, configuration `c.1`, and the
//!   functions in the order the board file lists them. The FunctionFS function `ffs.NAME` is
//!   mounted at `/dev/usb-ffs/NAME` with the daemon's uid and gid. The CDC network function
//!   `ncm.usb0` and the serial function `acm.GS0` (the console, `ttyGS0` for the first one)
//!   are configfs only. Last, it gives the daemon the `UDC` attribute: owned by its uid and gid,
//!   mode 0600. Nothing is bound.
//! - [`FunctionFsGadget::enable`], in the daemon, takes ep0 and writes the FunctionFS
//!   descriptors and strings to it, opens the endpoint files, and then writes the controller's
//!   name to `UDC`. The daemon never makes or removes a configfs directory.
//!
//! The order is fixed by FunctionFS: a FunctionFS function does not bind until its descriptors
//! are written to ep0, so the gadget binds only at the last step, after them.
//!
//! # What [`prepare`] writes
//!
//! Inside `usb_gadget/lemnos-NAME`, from the descriptor set ([`lemnos_hal::usb_descriptors`]):
//!
//! 1. Unbinds the controller, then removes what an earlier setup left: every configuration,
//!    its links and strings, the gadget's strings, the function directories and the mount.
//! 2. Writes the device descriptor's fields to `idVendor`, `idProduct`, `bcdDevice`, `bcdUSB`,
//!    `bDeviceClass`, `bDeviceSubClass`, `bDeviceProtocol` and `bMaxPacketSize0`, and the
//!    highest speed the set has a configuration for to `max_speed` where the kernel has that
//!    attribute (5.12 on).
//! 3. Makes `strings/0xLANG` and writes `manufacturer`, `product` and `serialnumber`.
//! 4. Makes `configs/c.N` (`N` is `bConfigurationValue`), writes `MaxPower` (in mA) and
//!    `bmAttributes`, and the configuration's name.
//! 5. Walks the board file's functions in order: `ffs` makes `functions/ffs.NAME` and mounts
//!    FunctionFS on it with `uid=`, `gid=`, `rmode=0500` and `fmode=0600`; `ncm` makes
//!    `functions/ncm.usb0` with `host_addr` and `dev_addr`, and writes its `ifname` when the
//!    board names one (see [`NCM_IFNAME_REFUSED`]); `acm` makes `functions/acm.GS0`.
//! 6. Links each function into `configs/c.N`, in the same order.
//! 7. Gives `UDC` to the owner, mode 0600.
//!
//! # What the daemon does in [`FunctionFsGadget::enable`]
//!
//! 1. Checks the controller, the function directory and `ep0` are there (`NotFound` and
//!    nothing touched otherwise).
//! 2. Writes `UDC` as empty, in case an earlier run left the gadget bound.
//! 3. Opens `ep0` in the mount and writes the FunctionFS descriptors (the interface, endpoint
//!    and class descriptors of each speed) and strings (the whole string table, so a string
//!    index means the same as in the set).
//! 4. Opens the endpoint files, which FunctionFS names by the addresses in the descriptors.
//! 5. Writes the controller's name to `UDC`, which connects.
//!
//! A failure at any step undoes the steps before it. [`UsbFunction::disable`] and dropping the
//! function unbind the controller and close the files. Nothing is taken apart: the gadget is
//! the setup's, and the next setup replaces it.
//!
//! # What Linux decides
//!
//! - The controller picks the endpoint addresses and FunctionFS renumbers interfaces and
//!   strings, so the host can see other numbers than the set has. The function keeps using its
//!   own, as [`UsbFunction`] says.
//! - The gadget framework answers the standard requests, and FunctionFS refuses descriptor
//!   types it does not know, which `enable` reports as `InvalidInput`.
//! - FunctionFS takes alternate settings 0 to [`MAX_ALTERNATE`] of an interface, and one
//!   endpoint address in one setting only: a set with more settings, or an address in two, is
//!   `Unsupported`. It answers `SET_INTERFACE` itself and enables every endpoint of the
//!   function whichever setting the host picks; each pick is reported as
//!   [`Configured`](UsbFunctionEvent::Configured) again (FunctionFS says `ENABLE` again).
//! - The NCM interface's name is the kernel's to choose from the template the board gives
//!   (`lemnos%d` is `lemnos0`, the first free number). Kernels before 5.12 have no writable
//!   `ifname`: [`prepare`] refuses such a board with [`NCM_IFNAME_REFUSED`] and makes nothing
//!   more. Once bound, [`UsbFunction::interface_name`] reads back the name the kernel gave, and
//!   it is `usb<N>` when the board names none.
//! - A configuration with no interface, and a manufacturer, product, serial or configuration
//!   string over 125 bytes of UTF-8, are `Unsupported`.
//! - The data stage of a control request can't be abandoned once begun: `control_in` and
//!   `control_out` wait for the host, whatever their timeout.
//! - Each endpoint has one transfer with the kernel at a time, on [`TRANSFER_BYTES`] of memory
//!   of its own, so that a transfer outlasts the call that began it:
//!   - `read` begins a read of that size when none is under way, and a `read` that times out
//!     leaves it there for the next to finish. Nothing the host sends is lost between calls.
//!     The host's transfer ends the read at its short packet, and a longer one comes in
//!     pieces of that size.
//!   - `write` takes at most that much, hands it to the kernel and returns; it waits only for
//!     the write before it. How that write ended is reported by the next `write`.
//!   - When the host configures the function again, what was under way or read and not yet
//!     taken under the configuration before is dropped.
//! - An isochronous endpoint has [`ISO_TRANSFERS`] transfers with the kernel instead, each of
//!   one service interval's bytes (the larger of the two speeds' [`iso_interval_bytes`]), on
//!   memory of their own, as [`UsbFunction`] says of isochronous endpoints:
//!   - `write` takes at most an interval's bytes, else `InvalidInput`, into a free transfer
//!     and hands it to the kernel; `Timeout` when all are with the kernel. How a write ended
//!     is never reported: FunctionFS gives each its own request, and the controller sends it
//!     at the next interval the host asks.
//!   - `read` needs room for an interval's bytes, else `InvalidInput`. Every transfer waits
//!     with the kernel as a read, and each call takes the oldest that is done and gives it
//!     back at once, so the host's packets find a request while the caller works.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lemnos_hal::blocks::{
    BufferHandle, ControlSetup, TookUp, UsbFunctionEvent, iso_interval_bytes,
};
use lemnos_hal::usb_descriptors::{
    self, CONFIGURATION_LEN, GadgetFunction, INTERFACE, UsbDescriptors, ms_os,
};
use lemnos_hal::{ErrorKind, UsbFunction};
use lemnos_linux_sys::aio;
use lemnos_linux_sys::confined::Dir;
use lemnos_linux_sys::functionfs::{self, CompatEntry, Endpoint, Ep0, Event, ExtProp, Setup};

use crate::Claim;

mod keep;
mod tree;
pub use keep::{FdStore, is_kept_name};
pub use tree::{HELD_LIMIT, Outcome, REBIND_GAP, Rebind, Trees};

/// Whether a gadget's tree carries a USB serial string: what a helper's setup tells the daemon,
/// from the tree it made or adopted, not from the settings it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialString {
    /// The tree has a serial string.
    Set,
    /// The tree has none.
    Absent,
}

pub(crate) const PREFIX: &str = "udc:";

/// A failure that carries the errno a reply reports and the words for the log: what the setup says
/// when a tree is not the helper's, or is bound and differs.
#[derive(Debug)]
struct Fault {
    errno: i32,
    text: String,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Fault {}

/// A failure with `errno` and `text`, as the helper reports one: [`errno_of`] gives the errno back.
#[must_use]
pub fn fault(errno: i32, text: String) -> io::Error {
    io::Error::other(Fault { errno, text })
}

/// The errno of a failure: the kernel's, or the one a [`Fault`] carries.
#[must_use]
pub fn errno_of(error: &io::Error) -> Option<i32> {
    error.raw_os_error().or_else(|| {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Fault>())
            .map(|fault| fault.errno)
    })
}

/// The longest configfs takes for a function directory's name (`ffs.NAME`).
const MAX_FUNCTION_NAME: usize = 39;
/// The longest string configfs takes, less the newline written after it.
const MAX_STRING: usize = 125;
/// The highest alternate setting FunctionFS takes (`MAX_ALT_SETTINGS` in `f_fs.c`).
pub const MAX_ALTERNATE: u8 = 2;
/// Transfers an isochronous endpoint keeps with the kernel.
pub const ISO_TRANSFERS: usize = 8;

/// Whether `name` (without `udc:`) is a name a controller can have here: one directory name.
#[must_use]
pub fn is_controller_name(name: &str) -> bool {
    controller(&format!("{PREFIX}{name}")).is_some()
}

/// The controller a board file's name means, if the name is one of ours and can be a
/// directory name: no separator, no leading dot, nothing but what a kernel device name has.
pub(crate) fn controller(need: &str) -> Option<&str> {
    let name = need.strip_prefix(PREFIX)?;
    let plain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'@');
    let fits = !name.is_empty()
        && !name.starts_with('.')
        && name.len() + "ffs.".len() <= MAX_FUNCTION_NAME
        && name.bytes().all(plain);
    fits.then_some(name)
}

/// What a failed step of setting the gadget up amounts to.
fn setup_kind(error: &io::Error) -> ErrorKind {
    match errno_of(error) {
        Some(libc::ENOENT) => ErrorKind::NotFound,
        Some(libc::EBUSY) => ErrorKind::Busy,
        // The board serial the gadget derives from is not readable yet (the helper's reply).
        Some(libc::ENODATA) => ErrorKind::Configuration,
        Some(libc::EINVAL | libc::EOVERFLOW | libc::ERANGE | libc::ENAMETOOLONG) => {
            ErrorKind::InvalidInput
        }
        _ => crate::kind_of(error),
    }
}

/// What a failed transfer on a FunctionFS file amounts to.
fn wire_kind(error: &io::Error) -> ErrorKind {
    match error.raw_os_error() {
        // An endpoint the host has not enabled, a transfer cut by a reset or a pull, a request
        // the host abandoned, a function that is no longer bound.
        Some(libc::EAGAIN | libc::ESHUTDOWN | libc::ECONNRESET | libc::ENODEV) => {
            ErrorKind::Unavailable
        }
        Some(functionfs::ABANDONED | functionfs::NO_REQUEST) => ErrorKind::Unavailable,
        _ if error.kind() == io::ErrorKind::TimedOut => ErrorKind::Timeout,
        _ => crate::kind_of(error),
    }
}

/// Where a gadget's parts are.
#[derive(Debug, Clone)]
struct Paths {
    /// `usb_gadget/lemnos-NAME` in configfs.
    gadget: PathBuf,
    /// The FunctionFS mount point.
    mount: PathBuf,
    /// The controller's entry under `/sys/class/udc`.
    udc: PathBuf,
    /// The controller's name.
    name: String,
}

impl Paths {
    fn function_name(&self) -> String {
        format!("ffs.{}", self.name)
    }
}

/// What a descriptor set comes to in configfs and FunctionFS terms, worked out before
/// anything is touched.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    /// Attribute files of the gadget directory and what goes in each.
    identity: Vec<(&'static str, String)>,
    /// The highest speed there is a configuration for, as `max_speed` spells it.
    max_speed: &'static str,
    /// `strings/0xLANG`, if the set has a string table.
    language: Option<String>,
    /// Attribute files of the gadget's strings directory.
    strings: Vec<(&'static str, String)>,
    /// `c.N`.
    configuration: String,
    max_power: String,
    attributes: String,
    configuration_name: Option<String>,
    /// The descriptors blob, with the link's Microsoft OS 1.0 sections after the descriptors.
    descriptors: Vec<u8>,
    /// `os_desc` attributes of the gadget's configfs OS descriptors (`use`, `b_vendor_code`,
    /// `qw_sign`): what makes the kernel answer string 0xEE and the vendor requests for the
    /// configurations it links there.
    os_attributes: Vec<(&'static str, String)>,
    function_strings: Vec<u8>,
    /// Each endpoint's address, and for an isochronous one its bytes per interval.
    endpoints: Vec<(u8, Option<u32>)>,
    /// The positions in `endpoints` of the bulk OUT and bulk IN endpoints, when the function is
    /// exactly that pair: the two that [`UsbFunction::endpoint_handles`] lends.
    lend: Option<[usize; 2]>,
}

impl Plan {
    fn of(set: &UsbDescriptors<'_>) -> Result<Self, ErrorKind> {
        let full = &set.full_speed()[CONFIGURATION_LEN..];
        let high = set.high_speed().map(|high| &high[CONFIGURATION_LEN..]);
        let alternates =
            usb_descriptors::descriptors(full).any(|d| d[1] == INTERFACE && d[3] > MAX_ALTERNATE);
        // The same shape at both speeds, so one speed tells whether an address comes twice.
        let mut seen = 0u32;
        let twice = usb_descriptors::endpoints(set.full_speed()).any(|e| {
            let bit = 1u32 << (u32::from(e.address & 0x0f) + u32::from(e.address >> 7) * 16);
            let again = seen & bit != 0;
            seen |= bit;
            again
        });
        let configuration = set.configuration();
        if alternates || twice || configuration.interfaces == 0 {
            return Err(ErrorKind::Unsupported);
        }
        // An isochronous endpoint's bytes per interval, the larger of the two speeds'.
        let high_endpoints = set.high_speed().map(usb_descriptors::endpoints);
        let mut high_endpoints = high_endpoints.into_iter().flatten();
        let transfers: Vec<u8> = usb_descriptors::endpoints(set.full_speed())
            .map(|e| e.attributes & 3)
            .collect();
        let endpoints: Vec<(u8, Option<u32>)> = usb_descriptors::endpoints(set.full_speed())
            .map(|e| {
                let high = high_endpoints
                    .next()
                    .map_or(0, |h| iso_interval_bytes(h.max_packet));
                let iso = e.attributes & 3 == 1;
                (
                    e.address,
                    iso.then(|| iso_interval_bytes(e.max_packet).max(high)),
                )
            })
            .collect();
        let lend = bulk_pair(&transfers, &endpoints);

        let text = |index: u8| -> Result<Option<String>, ErrorKind> {
            let Some(units) = set.text(index) else {
                return Ok(None);
            };
            let units: Vec<u16> = units.collect();
            String::from_utf16(&units)
                .map(Some)
                .map_err(|_| ErrorKind::InvalidInput)
        };
        // What goes in a configfs attribute: configfs has no empty string and a length limit.
        let attribute = |index: u8| -> Result<Option<String>, ErrorKind> {
            match text(index)? {
                Some(text) if text.len() > MAX_STRING => Err(ErrorKind::Unsupported),
                Some(text) if text.is_empty() => Ok(None),
                other => Ok(other),
            }
        };

        let identity = set.identity();
        let mut strings = Vec::new();
        for (file, index) in [
            ("manufacturer", identity.manufacturer),
            ("product", identity.product_name),
            ("serialnumber", identity.serial),
        ] {
            if let Some(text) = attribute(index)? {
                strings.push((file, text));
            }
        }

        // Every string of the table, so an index in a descriptor is its place here too.
        let mut table = Vec::new();
        for index in 1..set.string_count() {
            let index = u8::try_from(index).map_err(|_| ErrorKind::InvalidInput)?;
            table.push(text(index)?.unwrap_or_default());
        }
        let table: Vec<&str> = table.iter().map(String::as_str).collect();
        let language = set.language();
        // No FUNCTIONFS_VIRTUAL_ADDR: with it the kernel writes the descriptors' own addresses back
        // over the ones the controller gave (f_fs.c, `__ffs_func_bind_do_descs`), so the host is
        // shown an address the endpoint doesn't have. Where they differ, the configuration can't
        // be set: `config_ep_by_speed` finds no descriptor for the real endpoint and returns -EIO,
        // and another function's real endpoint can carry the same address (a duplicate on the
        // wire). A dwc2 controller gives the link ep1in and ep1out, where the descriptors say 0x81
        // and 0x02. Without the flag the kernel writes the real addresses, and the endpoint files
        // are numbered in descriptor order (`ep1`, `ep2`, ...).
        let flags = functionfs::ALL_CTRL_RECIP;
        let invalid = |_| ErrorKind::InvalidInput;

        Ok(Self {
            identity: vec![
                ("idVendor", format!("0x{:04x}", identity.vendor)),
                ("idProduct", format!("0x{:04x}", identity.product)),
                ("bcdDevice", format!("0x{:04x}", identity.release)),
                ("bcdUSB", format!("0x{:04x}", identity.usb)),
                ("bDeviceClass", format!("0x{:02x}", identity.class)),
                ("bDeviceSubClass", format!("0x{:02x}", identity.subclass)),
                ("bDeviceProtocol", format!("0x{:02x}", identity.protocol)),
                ("bMaxPacketSize0", format!("0x{:02x}", identity.max_packet)),
            ],
            max_speed: if high.is_some() {
                "high-speed"
            } else {
                "full-speed"
            },
            language: language.map(|language| format!("0x{language:x}")),
            strings,
            configuration: format!("c.{}", configuration.value),
            // `bMaxPower` counts 2 mA; the attribute is in mA.
            max_power: (u16::from(configuration.max_power) * 2).to_string(),
            attributes: format!("0x{:02x}", configuration.attributes),
            configuration_name: attribute(configuration.name)?,
            descriptors: functionfs::descriptors_blob_with_os(
                flags,
                Some(full),
                high,
                &link_sections().map_err(invalid)?,
            )
            .map_err(invalid)?,
            os_attributes: vec![
                ("use", "1".to_string()),
                ("b_vendor_code", format!("0x{:02x}", ms_os::VENDOR_CODE)),
                ("qw_sign", ms_os::QW_SIGN.to_string()),
            ],
            function_strings: functionfs::strings_blob(language.unwrap_or(0), &table)
                .map_err(invalid)?,
            endpoints,
            lend,
        })
    }
}

/// The Microsoft OS 1.0 sections of the link, which every gadget of ours presents: the
/// FunctionFS function is always in the list (`description::functions` adds it when the board
/// does not name it), and its first interface is the link. The compatible ID is WinUSB and the
/// property is the link GUID, so Windows binds WinUSB to that interface only.
fn link_sections() -> io::Result<Vec<Vec<u8>>> {
    Ok(vec![
        functionfs::os_compat_section(&[CompatEntry {
            interface: 0,
            compatible: ms_os::LINK_COMPATIBLE_ID,
            sub_compatible: [0; 8],
        }])?,
        functionfs::os_prop_section(
            0,
            &[ExtProp {
                name: ms_os::LINK_GUID_PROPERTY,
                data: ms_os::LINK_GUID,
            }],
        )?,
    ])
}

/// The text configfs takes for an 8-byte ID: its bytes up to the NULs.
fn id_text(id: &[u8; 8]) -> &[u8] {
    let end = id.iter().position(|&b| b == 0).unwrap_or(id.len());
    &id[..end]
}

/// `[OUT, IN]` positions when the transfer types are exactly one bulk OUT and one bulk IN.
fn bulk_pair(transfers: &[u8], endpoints: &[(u8, Option<u32>)]) -> Option<[usize; 2]> {
    const BULK: u8 = 2;
    let [a, b] = transfers else { return None };
    if *a != BULK || *b != BULK {
        return None;
    }
    let out = endpoints
        .iter()
        .position(|(address, _)| address & 0x80 == 0)?;
    let input = endpoints
        .iter()
        .position(|(address, _)| address & 0x80 != 0)?;
    Some([out, input])
}

/// Writes `value` and a newline to the attribute `file` of `dir`.
fn attribute(dir: &Path, file: &str, value: &str) -> io::Result<()> {
    fs::write(dir.join(file), format!("{value}\n"))
}

/// The owner the setup gives the FunctionFS mount and the gadget's `UDC` attribute: the
/// daemon's user and group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

/// The mounts the setup makes. The system's are [`SystemMounts`]; a test records them.
pub trait Mounts {
    /// Mounts FunctionFS at `target` with `options`, as `f_fs.c` parses them.
    fn functionfs(&mut self, source: &str, target: &Path, options: &str) -> io::Result<()>;
    /// Unmounts `target`. Nothing mounted there is an error.
    fn unmount(&mut self, target: &Path) -> io::Result<()>;
    /// Whether the mount point `dir` has a FunctionFS mounted on it, with its `ep0`. The default
    /// looks for `ep0` in it, as a stand-in tree has it; the system's looks at the filesystem the
    /// directory is on, since the mount point is closed to everyone but the daemon's user.
    fn mounted(&self, dir: &Dir) -> bool {
        dir.exists("ep0")
    }
}

/// The system's mounts: `mount(2)` and `umount2(2)`, as root.
pub struct SystemMounts;

impl Mounts for SystemMounts {
    fn functionfs(&mut self, source: &str, target: &Path, options: &str) -> io::Result<()> {
        lemnos_linux_sys::mount::functionfs(source, target, options)
    }

    fn unmount(&mut self, target: &Path) -> io::Result<()> {
        lemnos_linux_sys::mount::unmount(target)
    }

    fn mounted(&self, dir: &Dir) -> bool {
        dir.on_filesystem(lemnos_linux_sys::confined::FUNCTIONFS_MAGIC)
            .unwrap_or(false)
    }
}

/// The function directories the setup makes, by the name configfs gives each.
const NCM_DIR: &str = "ncm.usb0";
/// The refusal when a board names an NCM interface and this kernel can't take the name: its
/// `f_ncm` has no `ifname` attribute (Linux before 3.17), or one that is read-only (3.17 to
/// 5.11). The name is writable from Linux 5.12 (`u_ether_configfs.h`, `gether_set_ifname`).
pub const NCM_IFNAME_REFUSED: &str = "this kernel's NCM function can't be given an interface \
     name: its ifname attribute is missing or read-only (Linux 5.12 or later is needed)";
const ACM_DIR: &str = "acm.GS0";
/// What the gadget lacks in this boot: a bind failed, failed again on a network function made
/// fresh for it, and worked without one (see `Trees::bind`). The kernel's NCM function then cannot
/// register its network device until the board restarts (on 7.3, after the USB controller's
/// device went away and came back with such a device once registered under it), so the gadget is
/// bound without it. Lemnos does not restart the board.
pub const NETWORK_REFUSED: &str = "the kernel refuses to register the gadget's network device \
     again in this boot (a bind failed twice, the second time on a fresh NCM function): the \
     gadget is bound without its USB network function until the board restarts";

fn invalid(kind: ErrorKind) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{kind:?}"))
}

/// What a refused write to `ifname` amounts to: the attribute is missing (`ENOENT`, `EPERM`) or
/// has no store to take the name (`EINVAL`, or `EACCES` for a caller that is not root). The
/// board's name is checked before it is written (see [`lemnos_hal::usb_descriptors::IfName`]),
/// so `EINVAL` here is the kernel saying it takes no name, not a bad one.
fn ifname_refused(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::EPERM | libc::EINVAL | libc::EACCES | libc::EROFS) => {
            io::Error::new(io::ErrorKind::Unsupported, NCM_IFNAME_REFUSED)
        }
        _ => error,
    }
}

fn mac_text(mac: &[u8; 6]) -> String {
    let octets: Vec<String> = mac.iter().map(|octet| format!("{octet:02x}")).collect();
    octets.join(":")
}

impl Paths {
    fn function(&self) -> PathBuf {
        self.gadget.join("functions").join(self.function_name())
    }

    /// Whether the controller and what the setup made are all there: the FunctionFS function,
    /// linked into configuration `configuration`, and `ep0` in the mount. A link only the setup
    /// makes is what says the setup ran. `held_ep0`: the session holds ep0 open already (a let-go
    /// it kept), so the mount is not asked for: a session taken up from the service manager's store
    /// was mounted by a daemon before this one, in a mount namespace this one does not see.
    fn ready(&self, configuration: &str, held_ep0: bool) -> bool {
        let link = self
            .gadget
            .join("configs")
            .join(configuration)
            .join(self.function_name());
        self.udc.exists()
            && self.function().is_dir()
            && fs::symlink_metadata(link).is_ok()
            && (held_ep0 || self.mount.join("ep0").exists())
    }

    /// Takes the gadget off its controller. The attribute exists in configfs; it is not made
    /// where it doesn't. Never fails: a gadget that isn't bound is already off. Returns whether
    /// it was bound: then a host may hold it configured, and the bind that follows waits
    /// [`REBIND_GAP`] so the host sees it go.
    fn unbind(&self) -> bool {
        let was_bound = fs::read_to_string(self.gadget.join("UDC"))
            .is_ok_and(|text| !text.trim_matches(['\n', '\0', ' ']).is_empty());
        let udc = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(self.gadget.join("UDC"));
        if let Ok(mut udc) = udc {
            let _ = udc.write(b"\n");
        }
        was_bound
    }
}

/// Where the setup finds the system's parts: configfs (`/sys/kernel/config`), the directory of
/// FunctionFS mount points (`/dev/usb-ffs`) and the controllers (`/sys/class/udc`).
#[derive(Debug, Clone, Copy)]
pub struct Roots<'a> {
    pub configfs: &'a Path,
    pub mounts: &'a Path,
    pub udc_class: &'a Path,
}

/// What the privileged setup does, in the order of the module docs. Runs as root, once before
/// the daemon, from the same settings the daemon's driver reads: `descriptors` is the set
/// [`lemnos_hal::usb_descriptors`] checks, and `functions` the board file's functions in order.
/// `udc_name` is the `NAME` of `udc:NAME`.
///
/// A tree that is already there is adopted when it is the same, remade when it differs and is
/// unbound, and left alone when it is not ours (see [`Trees`]). Nothing is bound. A failure leaves
/// whatever it made, and the next setup removes it.
pub fn prepare(
    roots: &Roots<'_>,
    udc_name: &str,
    owner: Owner,
    descriptors: &[u8],
    functions: &[GadgetFunction],
    system: &mut dyn Mounts,
) -> io::Result<()> {
    let udc = resolve_udc(roots.udc_class, udc_name).unwrap_or_else(|| udc_name.to_string());
    Trees::new(roots)
        .setup(&udc, owner, descriptors, functions, system, Udc::Owner)
        .map(drop)
}

/// Who writes the gadget's `UDC` attribute once it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Udc {
    /// The daemon's user: `UDC` is handed to the owner at mode 0600 ([`prepare`]).
    Owner,
    /// The root process that made the gadget: `UDC` stays root's, and the daemon binds through
    /// it ([`prepare_for_helper`]).
    Root,
}

/// [`prepare`] for a helper that keeps root (`lemnosd`'s gadget helper). The tree, the mount and
/// the owner of the FunctionFS mount are as [`prepare`] makes them, but `UDC` is not handed over:
/// the daemon never writes it in this mode, it asks the helper to bind and unbind (see
/// [`GadgetHelper`]). So the helper needs no `CAP_CHOWN`. The helper holds its own [`Trees`], opened
/// once, and calls [`Trees::setup`] on them; this is the same call on trees opened for it.
///
/// # Errors
///
/// As [`prepare`].
pub fn prepare_for_helper(
    roots: &Roots<'_>,
    udc_name: &str,
    owner: Owner,
    descriptors: &[u8],
    functions: &[GadgetFunction],
    system: &mut dyn Mounts,
) -> io::Result<()> {
    Trees::new(roots)
        .setup(udc_name, owner, descriptors, functions, system, Udc::Root)
        .map(drop)
}

/// Writes the controller's name to the gadget's `UDC`, as root: the helper's bind.
///
/// # Errors
///
/// The name is not a controller's, or the write fails (`ENOENT` when the gadget is not made).
pub fn bind(roots: &Roots<'_>, udc_name: &str) -> io::Result<()> {
    Trees::new(roots).bind(udc_name)
}

/// Writes an empty line to the gadget's `UDC`, as root: the helper's unbind. Nothing bound is
/// not an error.
///
/// # Errors
///
/// The name is not a controller's.
pub fn unbind(roots: &Roots<'_>, udc_name: &str) -> io::Result<()> {
    Trees::new(roots).unbind(udc_name)
}

/// Takes the gadget named `udc_name` down for good: unbinds it, removes its configurations and
/// functions, and unmounts its FunctionFS. The setup makes it again. A mount that is not there is
/// not an error.
///
/// # Errors
///
/// The name is not a controller's, or the mount is there and cannot be unmounted.
pub fn teardown(roots: &Roots<'_>, udc_name: &str, system: &mut dyn Mounts) -> io::Result<()> {
    Trees::new(roots).teardown(udc_name, system)
}

impl Paths {
    /// Steps 3 to 5 of the daemon's [`FunctionFsGadget::enable`]: ep0, the endpoint files and
    /// the bind. On an error the caller unbinds. A piece that went missing since `ready`
    /// looked is `ENOENT` from the step that needs it.
    fn attach(&self, plan: &Plan, bind: &mut dyn FnMut() -> io::Result<()>) -> io::Result<FfsWire> {
        let ep0 = Ep0::open(&self.mount.join("ep0"))?;
        ep0.describe(&plan.descriptors, &plan.function_strings)?;
        let mut endpoints = Vec::with_capacity(plan.endpoints.len());
        // The kernel numbers a function's endpoint files in the order its descriptors list the
        // endpoints, from 1 (f_fs.c: `ep%u`), which is the plan's order.
        for (index, _) in plan.endpoints.iter().enumerate() {
            let file = self.mount.join(format!("ep{}", index + 1));
            endpoints.push(Endpoint::open(&file)?);
        }
        let mut queues = Vec::with_capacity(endpoints.len());
        for (_, iso) in &plan.endpoints {
            queues.push(match iso {
                Some(bytes) => {
                    Transfers::Ring(Box::new(aio::Ring::new(ISO_TRANSFERS, *bytes as usize)?))
                }
                None => Transfers::One(aio::Queue::new(TRANSFER_BYTES)?),
            });
        }
        let stage = Ep0Stage::spawn()?;
        bind()?;
        Ok(FfsWire {
            ep0,
            endpoints,
            queues,
            stage,
        })
    }
}

/// The FunctionFS files of an enabled function, as [`Session`] uses them. Endpoints are
/// numbered by their place in the plan.
trait Wire {
    fn next_event(&mut self) -> io::Result<Option<Event>>;
    /// Begins the data stage of the pending request whose data goes to the host, with `data`.
    /// Never waits: the stage runs on until the host ends it, and [`stage_end`](Self::stage_end)
    /// says how it ended.
    fn begin_send(&mut self, data: &[u8]) -> io::Result<()>;
    /// Begins taking the `len` bytes of the data stage of the pending request whose data comes
    /// from the host (0: accepting one that has none). Never waits.
    fn begin_receive(&mut self, len: usize) -> io::Result<()>;
    /// How the stage begun last ended, waiting at most `timeout`: `None` while it is still under
    /// way. A receive's bytes are copied into `into`, which holds them. Once it has said `Some`,
    /// nothing is under way.
    fn stage_end(&mut self, into: &mut [u8], timeout: Duration) -> Option<io::Result<usize>>;
    /// Whether the stage begun last is still under way: the host has not ended it. Never waits.
    fn stage_under_way(&self) -> bool;
    fn stall(&mut self, request_is_in: bool) -> io::Result<()>;
    /// Whether a control request read from `ep0` is still unanswered (or was cancelled by the
    /// host and not yet said so). Never waits.
    fn request_pending(&self) -> io::Result<bool>;
    fn halt(&mut self, endpoint: usize, endpoint_is_in: bool) -> io::Result<()>;
    fn clear_halt(&mut self, endpoint: usize) -> io::Result<()>;
    /// What the host sent to the endpoint. A timeout loses nothing.
    fn read(&mut self, endpoint: usize, data: &mut [u8], timeout: Duration) -> io::Result<usize>;
    /// Hands `data`, or the first of it, to the endpoint once it is free of the write before.
    fn write(&mut self, endpoint: usize, data: &[u8], timeout: Duration) -> io::Result<usize>;
    /// Drops every transfer under way and everything read and not yet taken.
    fn forget(&mut self);
}

/// The most one transfer with the kernel carries, per endpoint: a multiple of every packet
/// size, so a longer transfer is cut only between packets.
pub const TRANSFER_BYTES: usize = 64 * 1024;

/// One endpoint's transfers with the kernel.
enum Transfers {
    /// One at a time, for bulk and interrupt endpoints.
    One(aio::Queue),
    /// [`ISO_TRANSFERS`] at once, for an isochronous endpoint.
    Ring(Box<aio::Ring>),
}

struct FfsWire {
    ep0: Ep0,
    endpoints: Vec<Endpoint>,
    /// Each endpoint's transfers, in the endpoints' order.
    queues: Vec<Transfers>,
    /// Endpoint 0's data stages, on their own thread.
    stage: Ep0Stage,
}

/// The most a data stage carries: `wLength` is 16 bits.
const STAGE_BYTES: usize = u16::MAX as usize;

/// The data stages of endpoint 0, run on a thread of their own. FunctionFS waits in the write or
/// read that is a data stage until the host ends the stage, whatever the file's flags
/// (`__ffs_ep0_queue_wait` in f_fs.c), and holds the function's lock while it waits, so other
/// reads of ep0 find it busy and say so (`EAGAIN`) instead of waiting. The host ends a stage by
/// taking it, within its own control timeout, by a new request, or by a reset; an unbind ends it
/// too. So a stage the host never ends costs this thread, idle in that one transfer, and never the
/// caller: the caller begins a stage and is told later how it ended. Each stage is given its own
/// copy of ep0 and closes it as the stage ends, so an idle thread holds nothing open. The buffer
/// is made once, with the thread, and nothing allocates per stage.
struct Ep0Stage {
    shared: Arc<StageShared>,
}

#[derive(Default)]
struct StageShared {
    state: Mutex<StageState>,
    changed: Condvar,
}

#[derive(Default)]
struct StageState {
    /// The stage to run: a copy of ep0, whether it sends, and its length.
    job: Option<(Ep0, bool, usize)>,
    /// The stage's bytes: what goes to the host, or what came from it. With the thread while
    /// a stage runs.
    buf: Vec<u8>,
    /// The thread is in a stage now.
    running: bool,
    /// How the last stage ended, until it is taken.
    done: Option<io::Result<usize>>,
    stop: bool,
}

impl StageShared {
    fn lock(&self) -> MutexGuard<'_, StageState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How often a stage that finds ep0 busy (another file operation holds the function's lock for
/// a moment) tries again, a millisecond apart, before it gives up as the busy file said.
const STAGE_BUSY_TRIES: u32 = 20;

impl Ep0Stage {
    fn spawn() -> io::Result<Self> {
        let shared = Arc::new(StageShared::default());
        shared.lock().buf = vec![0; STAGE_BYTES];
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("gadget-ep0".into())
            .spawn(move || serve_stages(&worker))?;
        Ok(Self { shared })
    }

    fn begin(&self, ep0: &Ep0, send: Option<&[u8]>, len: usize) -> io::Result<()> {
        let mut state = self.shared.lock();
        if state.job.is_some() || state.running {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        let len = len.min(state.buf.len());
        if let Some(data) = send {
            state.buf[..len].copy_from_slice(&data[..len]);
        }
        state.job = Some((ep0.try_clone()?, send.is_some(), len));
        state.done = None;
        self.shared.changed.notify_all();
        Ok(())
    }

    fn end(&self, into: &mut [u8], timeout: Duration) -> Option<io::Result<usize>> {
        let until = Instant::now() + timeout;
        let mut state = self.shared.lock();
        while state.done.is_none() && (state.job.is_some() || state.running) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let done = state.done.take()?;
        if let Ok(n) = done {
            let n = n.min(into.len()).min(state.buf.len());
            into[..n].copy_from_slice(&state.buf[..n]);
        }
        Some(done)
    }

    fn under_way(&self) -> bool {
        let state = self.shared.lock();
        state.job.is_some() || state.running
    }
}

impl Drop for Ep0Stage {
    /// The thread ends once it is idle: at once, or as the stage it is in ends, which the unbind
    /// that comes with a disable makes it do.
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
    }
}

fn serve_stages(shared: &StageShared) {
    let mut state = shared.lock();
    loop {
        let Some((ep0, send, len)) = state.job.take() else {
            if state.stop {
                return;
            }
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        state.running = true;
        let mut buf = std::mem::take(&mut state.buf);
        drop(state);
        let mut tries = 0;
        let done = loop {
            let done = if send {
                ep0.send(&buf[..len])
            } else {
                ep0.receive(&mut buf[..len])
            };
            match done {
                Err(error)
                    if error.raw_os_error() == Some(libc::EAGAIN) && tries < STAGE_BUSY_TRIES =>
                {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
                done => break done,
            }
        };
        drop(ep0);
        state = shared.lock();
        state.buf = buf;
        state.running = false;
        state.done = Some(done);
        shared.changed.notify_all();
    }
}

impl Wire for FfsWire {
    fn next_event(&mut self) -> io::Result<Option<Event>> {
        self.ep0.next_event()
    }

    fn begin_send(&mut self, data: &[u8]) -> io::Result<()> {
        self.stage.begin(&self.ep0, Some(data), data.len())
    }

    fn begin_receive(&mut self, len: usize) -> io::Result<()> {
        self.stage.begin(&self.ep0, None, len)
    }

    fn stage_end(&mut self, into: &mut [u8], timeout: Duration) -> Option<io::Result<usize>> {
        self.stage.end(into, timeout)
    }

    fn stage_under_way(&self) -> bool {
        self.stage.under_way()
    }

    fn stall(&mut self, request_is_in: bool) -> io::Result<()> {
        self.ep0.stall(request_is_in)
    }

    fn request_pending(&self) -> io::Result<bool> {
        self.ep0.request_pending()
    }

    fn halt(&mut self, endpoint: usize, endpoint_is_in: bool) -> io::Result<()> {
        self.endpoints[endpoint].halt(endpoint_is_in)
    }

    fn clear_halt(&mut self, endpoint: usize) -> io::Result<()> {
        self.endpoints[endpoint].clear_halt()
    }

    fn read(&mut self, endpoint: usize, data: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let fd = self.endpoints[endpoint].as_fd();
        match &mut self.queues[endpoint] {
            Transfers::One(queue) => queue.read(fd, data, timeout),
            Transfers::Ring(ring) => ring.read(fd, data, timeout),
        }
    }

    fn write(&mut self, endpoint: usize, data: &[u8], timeout: Duration) -> io::Result<usize> {
        let fd = self.endpoints[endpoint].as_fd();
        match &mut self.queues[endpoint] {
            Transfers::One(queue) => queue.write(fd, data, timeout),
            Transfers::Ring(ring) => ring.write(fd, data, timeout),
        }
    }

    fn forget(&mut self) {
        for queue in &mut self.queues {
            match queue {
                Transfers::One(queue) => queue.cancel(),
                Transfers::Ring(ring) => ring.cancel(),
            }
        }
    }
}

/// An enabled function: what [`UsbFunction`] says of requests, halts and the host's comings
/// and goings, over the files.
struct Session<W> {
    wire: W,
    /// Each endpoint's address, and for an isochronous one its bytes per interval, in the
    /// wire's order.
    endpoints: Vec<(u8, Option<u32>)>,
    /// The positions of the bulk OUT and bulk IN endpoints in `endpoints`, when the function
    /// is that pair: see [`Plan::lend`]. Set once at enable, `None` otherwise.
    lend: Option<[usize; 2]>,
    configured: bool,
    /// The bus is idle (a suspend came, and no resume or deconfigure since).
    suspended: bool,
    /// The gadget was unbound from its controller.
    released: bool,
    pending: Option<Setup>,
    /// What a function that took this session up is told first: its state as the function
    /// before it had it (see [`Session::taken_up`]).
    replay: std::collections::VecDeque<UsbFunctionEvent>,
    /// The data stage under way, begun and not yet reported ended: whether it goes to the host,
    /// and the request's `wLength`.
    stage: Option<(bool, usize)>,
    /// The record kept with the session's files in the service manager's store, when they are kept
    /// there ([`keep`]): the host's state is written to it at each change, and the request the
    /// host waits on an answer for.
    record: Option<keep::Record>,
    /// A request the daemon before read and did not record is being ended on the stage thread
    /// ([`Session::drain`]): nothing is read from `ep0` until it has.
    stale: bool,
}

/// The most a data stage of a request ended unheard takes (`wLength` is 16 bits).
const STALE_READ: usize = u16::MAX as usize;

/// A setup packet's eight bytes, as the record keeps them.
fn setup_bytes(setup: &Setup) -> [u8; 8] {
    let [v0, v1] = setup.value.to_le_bytes();
    let [i0, i1] = setup.index.to_le_bytes();
    let [l0, l1] = setup.length.to_le_bytes();
    [setup.request_type, setup.request, v0, v1, i0, i1, l0, l1]
}

fn setup_from(bytes: [u8; 8]) -> Setup {
    Setup {
        request_type: bytes[0],
        request: bytes[1],
        value: u16::from_le_bytes([bytes[2], bytes[3]]),
        index: u16::from_le_bytes([bytes[4], bytes[5]]),
        length: u16::from_le_bytes([bytes[6], bytes[7]]),
    }
}

/// A request, for the daemon's log: its type, number, value, index and length.
fn request_text(setup: &Setup) -> String {
    format!(
        "{:#04x} {:#04x} value {:#06x} index {} length {}",
        setup.request_type, setup.request, setup.value, setup.index, setup.length
    )
}

impl<W: Wire> Session<W> {
    fn new(wire: W, endpoints: Vec<(u8, Option<u32>)>) -> Self {
        Self {
            wire,
            endpoints,
            lend: None,
            configured: false,
            suspended: false,
            released: false,
            pending: None,
            replay: std::collections::VecDeque::new(),
            stage: None,
            record: None,
            stale: false,
        }
    }

    /// The host's state, to the kept record if there is one.
    fn note_state(&self) {
        if let Some(record) = &self.record {
            record.note(self.configured, self.suspended, self.released);
        }
    }

    /// The request the host waits on an answer for, to the kept record if there is one.
    fn note_request(&self, setup: Option<&Setup>) {
        if let Some(record) = &self.record {
            record.note_request(setup.map(setup_bytes));
        }
    }

    /// Ends what the host did while no daemon ran, on a session taken from the service manager's
    /// store, before anything is served on it. `request`: what the record says the daemon before
    /// had read and not answered.
    ///
    /// - A recorded request is stalled (a transfer the wrong way, which never waits): the host
    ///   gets its answer at once instead of waiting out its timeout.
    /// - A request pending on `ep0` that the record does not have (the daemon before was killed
    ///   between reading it and writing the record) is ended on the stage thread, never here: a
    ///   read stalls one whose data goes to the host, and takes and drops the data of one whose
    ///   data comes from it, which waits for the host. Nothing is read from `ep0` until it has
    ///   ended.
    /// - Otherwise every event queued is read: the host's state is taken from them, and a request
    ///   the host made in that time is stalled.
    ///
    /// Returns a line for the daemon's log for each thing it did.
    fn drain(&mut self, request: Option<[u8; 8]>) -> Vec<String> {
        let mut said = Vec::new();
        if let Some(bytes) = request {
            let setup = setup_from(bytes);
            said.push(match self.wire.stall(setup.is_in()) {
                Ok(()) => format!(
                    "the request {} the daemon before read and did not answer was refused (stalled)",
                    request_text(&setup)
                ),
                Err(_) => format!(
                    "the request {} the daemon before read and did not answer had already ended",
                    request_text(&setup)
                ),
            });
        } else if self.wire.request_pending().unwrap_or(false) {
            if self.wire.begin_receive(STALE_READ).is_ok() {
                self.stage = Some((false, 0));
                self.stale = true;
            }
            said.push(
                "a request the daemon before read and did not record is pending: it is ended off the tick before any event is read".to_string(),
            );
        }
        self.note_request(None);
        let mut taken = 0;
        while !self.stale {
            match self.wire.next_event() {
                Ok(Some(Event::Setup(setup))) => {
                    let stalled = self.wire.stall(setup.is_in()).is_ok();
                    said.push(format!(
                        "the request {} the host made while no daemon ran was refused ({})",
                        request_text(&setup),
                        if stalled {
                            "stalled"
                        } else {
                            "the host had let it go"
                        }
                    ));
                }
                Ok(Some(event)) => {
                    taken += 1;
                    match event {
                        Event::Enable => {
                            self.wire.forget();
                            self.configured = true;
                        }
                        Event::Disable => {
                            self.configured = false;
                            self.suspended = false;
                        }
                        Event::Suspend => self.suspended = true,
                        Event::Resume => self.suspended = false,
                        Event::Unbind => {
                            self.released = true;
                            self.configured = false;
                            self.suspended = false;
                        }
                        Event::Bind | Event::Setup(_) => {}
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    said.push(format!(
                        "what the host did while no daemon ran could not all be read ({error})"
                    ));
                    break;
                }
            }
        }
        if taken > 0 {
            said.push(format!(
                "{taken} event(s) of the host's from while no daemon ran taken (host {})",
                if self.released {
                    "let go of it"
                } else if self.configured {
                    "configured"
                } else {
                    "not configured"
                }
            ));
        }
        self.note_state();
        said
    }

    /// The session, taken up by a new function: what the one before it saw of the host is told
    /// again, first (the configuration chosen, an idle bus, a request still to answer). The
    /// request stays pending for the new function to answer.
    fn taken_up(mut self) -> Self {
        self.replay.clear();
        if self.configured {
            self.replay.push_back(UsbFunctionEvent::Configured);
        }
        if self.suspended {
            self.replay.push_back(UsbFunctionEvent::Suspended);
        }
        if let Some(setup) = self.pending {
            self.replay.push_back(UsbFunctionEvent::Setup {
                setup: ControlSetup {
                    request_type: setup.request_type,
                    request: setup.request,
                    value: setup.value,
                    index: setup.index,
                },
                length: setup.length,
            });
        }
        self
    }

    /// The place of `endpoint` and, for an isochronous one, its bytes per interval, if the
    /// descriptors name it, in the direction asked for if one is, and the host has configured
    /// the function.
    fn usable(
        &self,
        endpoint: u8,
        to_host: Option<bool>,
    ) -> Result<(usize, Option<u32>), ErrorKind> {
        let at = self.endpoints.iter().position(|(a, _)| *a == endpoint);
        let at = at.ok_or(ErrorKind::InvalidInput)?;
        if to_host.is_some_and(|to_host| to_host != (endpoint & 0x80 != 0)) {
            return Err(ErrorKind::InvalidInput);
        }
        if !self.configured || self.released {
            return Err(ErrorKind::Unavailable);
        }
        Ok((at, self.endpoints[at].1))
    }

    /// Takes the pending request, if its data goes to the host (`to_host`) or comes from it.
    fn take_pending(&mut self, to_host: bool) -> Result<Setup, ErrorKind> {
        match self.pending {
            Some(setup) if setup.is_in() == to_host => {
                self.pending = None;
                Ok(setup)
            }
            _ => Err(ErrorKind::InvalidInput),
        }
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        if self.released {
            return Err(ErrorKind::Unavailable);
        }
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(event));
        }
        if self.stale {
            // The request the daemon before left unrecorded: its end is nobody's answer.
            if self.wire.stage_under_way() {
                return Ok(None);
            }
            let _ = self.wire.stage_end(&mut [], Duration::ZERO);
            self.stage = None;
            self.stale = false;
        }
        // The host sends nothing more on endpoint 0 while it has a stage to end, and FunctionFS
        // holds ep0 for it. An ended stage's answer is kept for `control_end`.
        if self.stage.is_some() && self.wire.stage_under_way() {
            return Ok(None);
        }
        if let Some(setup) = self.pending.take() {
            // Read past without an answer. If the host has let it go there is nothing to
            // stall, and the event that says so is next.
            let _ = self.wire.stall(setup.is_in());
            self.note_request(None);
        }
        loop {
            let Some(event) = self.wire.next_event().map_err(|e| wire_kind(&e))? else {
                return Ok(None);
            };
            return Ok(Some(match event {
                Event::Bind => continue,
                Event::Unbind => {
                    self.released = true;
                    self.configured = false;
                    self.suspended = false;
                    self.note_state();
                    return Err(ErrorKind::Unavailable);
                }
                Event::Enable => {
                    // Nothing of the configuration before this one reaches the new one.
                    self.wire.forget();
                    self.configured = true;
                    self.note_state();
                    UsbFunctionEvent::Configured
                }
                Event::Disable => {
                    self.suspended = false;
                    let was = std::mem::take(&mut self.configured);
                    self.note_state();
                    if !was {
                        continue;
                    }
                    UsbFunctionEvent::Deconfigured
                }
                Event::Suspend => {
                    self.suspended = true;
                    self.note_state();
                    UsbFunctionEvent::Suspended
                }
                Event::Resume => {
                    self.suspended = false;
                    self.note_state();
                    UsbFunctionEvent::Resumed
                }
                Event::Setup(setup) => {
                    self.pending = Some(setup);
                    self.note_request(Some(&setup));
                    UsbFunctionEvent::Setup {
                        setup: ControlSetup {
                            request_type: setup.request_type,
                            request: setup.request,
                            value: setup.value,
                            index: setup.index,
                        },
                        length: setup.length,
                    }
                }
            }));
        }
    }

    /// Begins the IN data stage and waits at most `timeout` for its end. A stage the host has not
    /// ended by then goes on: `Timeout`, and `control_end` reports it.
    fn control_in(&mut self, data: &[u8], timeout: Duration) -> Result<usize, ErrorKind> {
        let setup = self.take_pending(true)?;
        let data = &data[..data.len().min(usize::from(setup.length))];
        // A stage ended and not taken goes with the new one.
        self.stage = None;
        self.wire.begin_send(data).map_err(|e| wire_kind(&e))?;
        self.stage = Some((true, data.len()));
        self.finish(&mut [], timeout)
    }

    /// Begins taking the OUT data stage into `data`, and waits at most `timeout` for it, as
    /// `control_in` does.
    fn control_out(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, ErrorKind> {
        let length = usize::from(self.pending.map_or(0, |setup| setup.length));
        if data.len() < length {
            return Err(ErrorKind::InvalidInput);
        }
        self.take_pending(false)?;
        self.stage = None;
        self.wire.begin_receive(length).map_err(|e| wire_kind(&e))?;
        self.stage = Some((false, length));
        self.finish(&mut data[..length], timeout)
    }

    /// The end of the stage under way, waited for at most `timeout`; `Timeout` while it goes on.
    fn finish(&mut self, into: &mut [u8], timeout: Duration) -> Result<usize, ErrorKind> {
        match self.wire.stage_end(into, timeout) {
            None => Err(ErrorKind::Timeout),
            Some(done) => {
                self.stage = None;
                self.note_request(None);
                done.map_err(|e| wire_kind(&e))
            }
        }
    }

    fn control_end(&mut self, data: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        let (to_host, length) = self.stage.ok_or(ErrorKind::InvalidInput)?;
        let into = if to_host {
            &mut data[..0]
        } else {
            data.get_mut(..length).ok_or(ErrorKind::InvalidInput)?
        };
        match self.finish(into, Duration::ZERO) {
            Err(ErrorKind::Timeout) => Ok(None),
            done => done.map(Some),
        }
    }

    fn control_stall(&mut self) -> Result<(), ErrorKind> {
        let setup = self.pending.take().ok_or(ErrorKind::InvalidInput)?;
        self.note_request(None);
        self.wire.stall(setup.is_in()).map_err(|e| wire_kind(&e))
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), ErrorKind> {
        let (at, isochronous) = self.usable(endpoint, None)?;
        if isochronous.is_some() {
            return Err(ErrorKind::Unsupported);
        }
        let done = if halted {
            self.wire.halt(at, endpoint & 0x80 != 0)
        } else {
            self.wire.clear_halt(at)
        };
        done.map_err(|e| wire_kind(&e))
    }

    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        let (at, interval) = self.usable(endpoint, Some(false))?;
        // An isochronous read is one interval, and must have room for all of it.
        if interval.is_some_and(|bytes| data.len() < bytes as usize) {
            return Err(ErrorKind::InvalidInput);
        }
        self.wire.read(at, data, timeout).map_err(|e| wire_kind(&e))
    }

    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize, ErrorKind> {
        let (at, interval) = self.usable(endpoint, Some(true))?;
        // An isochronous write is one interval, and is never cut.
        if interval.is_some_and(|bytes| data.len() > bytes as usize) {
            return Err(ErrorKind::InvalidInput);
        }
        self.wire
            .write(at, data, timeout)
            .map_err(|e| wire_kind(&e))
    }
}

/// What a gadget needs from the process that keeps root, when there is one (`lemnosd`'s gadget
/// helper). The helper holds the gadget's description from the board file, fixed at start, and
/// the daemon names the gadget by its index in that description; nothing else crosses. Each call
/// is one request and its answer.
pub trait GadgetHelper: Send {
    /// Makes the gadget's tree and mount, as [`prepare_for_helper`] does, and says whether the tree
    /// carries a serial string. Fails with the errno of the step that failed, so the gadget
    /// reports what was missing.
    fn setup(&mut self, index: u32) -> io::Result<SerialString>;
    /// Writes the controller's name to the gadget's `UDC`.
    fn bind(&mut self, index: u32) -> io::Result<()>;
    /// Writes an empty line to the gadget's `UDC`.
    fn unbind(&mut self, index: u32) -> io::Result<()>;
    /// Writes `connect` or `disconnect` to the controller's `soft_connect` ([`Trees::soft_connect`]).
    fn soft_connect(&mut self, index: u32, connect: bool) -> io::Result<()>;
}

/// A helper shared by the gadgets of one daemon: one request at a time.
pub type SharedHelper = std::sync::Arc<std::sync::Mutex<dyn GadgetHelper>>;

fn poisoned() -> io::Error {
    io::Error::other("the gadget helper's lock was poisoned by a panic")
}

/// This board as a USB device through one controller: see the module docs.
pub struct FunctionFsGadget {
    /// What the gadget's own work reaches: its paths, its helper, the gap.
    wired: Wired,
    /// The gadget's own thread, which does what waits (see [`Work`]).
    work: Work,
    session: Option<Session<FfsWire>>,
    _claim: Option<Claim>,
    /// What the last successful setup left: whether the tree has a serial string.
    serial: Option<bool>,
    /// What the gadget the last enable bound lacks: [`NETWORK_REFUSED`].
    note: Option<&'static str>,
    /// Whether this function has enabled the gadget before (see `enable`).
    enabled_before: bool,
    /// The descriptors the session was enabled with: what a session parked for the next function
    /// is matched on.
    set: Vec<u8>,
    /// Where sessions wait for the next function on their controller ([`UsbFunction::park`]).
    parking: Option<Parking>,
    /// The park this function made, if it parked its session.
    parked_here: Option<u64>,
    took_up: Option<TookUp>,
}

/// What a gadget's work reaches, on the caller's thread or the gadget's own: its paths, its helper
/// and the gap before a bind.
#[derive(Clone)]
struct Wired {
    paths: Paths,
    /// The helper that makes and binds this gadget, and the gadget's index in it. `None` when the
    /// daemon writes configfs itself, as a process without a helper does.
    helper: Option<(SharedHelper, u32)>,
    /// When this gadget was last taken off a controller a host may have held it on, until the
    /// bind that follows has waited out the gap.
    unbound_at: Arc<Mutex<Option<Instant>>>,
    /// [`REBIND_GAP`]; shorter in this crate's tests, which rebind hundreds of times.
    rebind_gap: Duration,
    /// How many times the gap was waited out: what says which step waited it, where a time would
    /// depend on the machine's load.
    gap_waits: Arc<std::sync::atomic::AtomicUsize>,
    /// The tree has had its network function in this daemon's view. A bind the kernel refuses the
    /// network device for loses the function (`Trees::bind`), and that may happen in an attempt
    /// that then fails for another reason (seen: the controller busy): the attempt that binds
    /// finds no network function, and would report nothing missing without this.
    network_seen: Arc<std::sync::atomic::AtomicBool>,
}

/// What a gadget's own thread does: everything that waits, so no caller of the function waits on
/// it. A helper's answer comes when the helper gives it, a bind may wait on the kernel, the gap
/// before a bind is a wait by design, and closing a session's files waits for the kernel to let go
/// of their transfers (`io_destroy` waits out an RCU grace period). The jobs run in the order they
/// were given, so a bind never overtakes the unbind and close before it.
enum Job {
    /// Bring the gadget up on `plan` (see `Wired::bring_up`).
    Up {
        plan: Box<Plan>,
        released: bool,
        host_may_hold: bool,
    },
    /// Take the gadget off its controller (when `unbind`), then close what `close` holds: a
    /// session's files, a parked session's.
    Down {
        unbind: bool,
        close: Option<Box<dyn Send>>,
    },
}

/// What a bring-up did: the files of a fresh session (none for a bind again on the session the
/// function kept), what the setup said, and the network interface before.
struct Brought {
    wire: Option<FfsWire>,
    plan: Box<Plan>,
    serial: Option<bool>,
    network_refused: bool,
    network_before: Option<(String, u32)>,
    template: Option<String>,
}

/// Why a bring-up failed: the kind, and the words when the kind does not say it.
struct Failed {
    kind: ErrorKind,
    note: Option<&'static str>,
}

impl From<ErrorKind> for Failed {
    fn from(kind: ErrorKind) -> Self {
        Self { kind, note: None }
    }
}

/// A gadget's own thread and what it has been given.
#[derive(Default)]
struct Work {
    shared: Arc<WorkShared>,
}

#[derive(Default)]
struct WorkShared {
    state: Mutex<WorkState>,
    changed: Condvar,
}

#[derive(Default)]
struct WorkState {
    queue: std::collections::VecDeque<Job>,
    /// Whether the thread is there.
    running: bool,
    /// A bring-up was given and its outcome not yet taken.
    up_given: bool,
    /// The outcome of the last bring-up, until `enable` takes it.
    up: Option<Result<Brought, Failed>>,
    /// The function was disabled while a bring-up was under way: its outcome is undone.
    cancel_up: bool,
    /// When the bring-up under way began.
    began: Option<Instant>,
    /// The thread is doing a job now.
    busy: bool,
    /// The function is gone: the thread ends once the queue is empty.
    ending: bool,
}

/// How long a bring-up runs before the gadget says the helper or the kernel is slow; shorter in
/// this crate's tests.
const SLOW_UP: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(10)
};

/// What a gadget says while its bring-up is under way.
pub const SETTING_UP: &str = "being set up and bound, off the caller's thread";

/// What a gadget says while its bring-up has been waiting on the helper or the kernel past
/// [`SLOW_UP`]: it is never given up on, as a request the helper may still carry out must not be
/// sent again.
pub const SETTING_UP_SLOW: &str = "still being set up: the gadget helper or the kernel has not finished for over 10 s; waiting for it, not asking again";

impl WorkShared {
    fn lock(&self) -> MutexGuard<'_, WorkState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Work {
    /// Gives the thread `job`, starting it if it is not there. Without a thread (none can be
    /// made) the job runs here, as everything did before there was one.
    fn submit(&self, wired: &Wired, job: Job) {
        let mut state = self.shared.lock();
        if matches!(job, Job::Up { .. }) {
            state.up_given = true;
            state.cancel_up = false;
        }
        state.queue.push_back(job);
        self.shared.changed.notify_all();
        if state.running {
            return;
        }
        state.running = true;
        drop(state);
        let shared = Arc::clone(&self.shared);
        let thread_wired = wired.clone();
        let spawned = std::thread::Builder::new()
            .name("gadget-work".into())
            .spawn(move || serve_work(&shared, &thread_wired));
        if spawned.is_err() {
            let mut state = self.shared.lock();
            state.running = false;
            let jobs: Vec<Job> = state.queue.drain(..).collect();
            drop(state);
            for job in jobs {
                run_job(&self.shared, wired, job);
            }
        }
    }

    /// Whether a bring-up was given and its outcome is not yet taken.
    fn up_pending(&self) -> bool {
        self.shared.lock().up_given
    }

    /// The outcome of the bring-up, once it is done.
    fn take_up(&self) -> Option<Result<Brought, Failed>> {
        let mut state = self.shared.lock();
        let up = state.up.take()?;
        state.up_given = false;
        Some(up)
    }

    /// Undoes the bring-up under way, or done and not taken: the gadget comes off its controller
    /// and the files close, on the thread.
    fn cancel_up(&self, wired: &Wired) {
        let mut state = self.shared.lock();
        if !state.up_given {
            return;
        }
        state.up_given = false;
        let Some(up) = state.up.take() else {
            state.cancel_up = true;
            return;
        };
        drop(state);
        if let Ok(brought) = up
            && let Some(wire) = brought.wire
        {
            self.submit(
                wired,
                Job::Down {
                    unbind: true,
                    close: Some(Box::new(wire)),
                },
            );
        }
    }

    /// How long the bring-up under way has run.
    fn up_for(&self) -> Option<Duration> {
        self.shared.lock().began.map(|began| began.elapsed())
    }

    /// Waits until the thread has done what it was given (tests: a disable's unbind, a close).
    #[cfg(test)]
    fn wait_idle(shared: &WorkShared) {
        let until = Instant::now() + Duration::from_secs(10);
        let mut state = shared.lock();
        while (!state.queue.is_empty() || state.busy) && Instant::now() < until {
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(5))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The function is gone: the thread ends once it has done what it was given.
    fn end(&self) {
        self.shared.lock().ending = true;
        self.shared.changed.notify_all();
    }
}

fn serve_work(shared: &WorkShared, wired: &Wired) {
    loop {
        let mut state = shared.lock();
        let job = loop {
            if let Some(job) = state.queue.pop_front() {
                break job;
            }
            if state.ending {
                state.running = false;
                return;
            }
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        };
        state.busy = true;
        drop(state);
        run_job(shared, wired, job);
        shared.lock().busy = false;
        shared.changed.notify_all();
    }
}

fn run_job(shared: &WorkShared, wired: &Wired, job: Job) {
    match job {
        Job::Up {
            plan,
            released,
            host_may_hold,
        } => {
            shared.lock().began = Some(Instant::now());
            let up = wired.bring_up(plan, released, host_may_hold);
            let mut state = shared.lock();
            state.began = None;
            if std::mem::take(&mut state.cancel_up) {
                drop(state);
                // Both ways of coming up end in a bind: undone, and a fresh session's files closed.
                if let Ok(brought) = up {
                    wired.unbind_now();
                    drop(brought);
                }
                return;
            }
            state.up = Some(up);
            shared.changed.notify_all();
        }
        Job::Down { unbind, close } => {
            if unbind {
                wired.unbind_now();
            }
            drop(close);
        }
    }
}

impl Wired {
    /// The part of an enable that waits, on the gadget's own thread: the helper's setup, the unbind
    /// and the gap, the descriptors and the bind. With a helper, the gadget is made now, by the
    /// helper, or adopted if it is already there as the board file describes it; that is the one
    /// thing this asks of it: the next attempt is the driver's, on its backoff. A tree that
    /// differs and is still bound is not remade under its binding: the gadget is taken off its
    /// controller (what is done before every attach anyway) and the helper asked again, once.
    /// `released`: the controller let go and the function kept its session, so only the bind is
    /// written. `host_may_hold`: a host may hold the gadget configured, so the bind waits out the
    /// gap after the unbind.
    fn bring_up(
        &self,
        plan: Box<Plan>,
        released: bool,
        host_may_hold: bool,
    ) -> Result<Brought, Failed> {
        let network_before = self.network_now();
        self.note_network();
        let mut serial = None;
        if !released && let Some((helper, index)) = &self.helper {
            let mut asked = helper
                .lock()
                .map_err(|_| ErrorKind::Unavailable)?
                .setup(*index);
            if asked
                .as_ref()
                .is_err_and(|e| errno_of(e) == Some(libc::EBUSY))
            {
                self.unbind_now();
                asked = helper
                    .lock()
                    .map_err(|_| ErrorKind::Unavailable)?
                    .setup(*index);
            }
            serial = Some(asked.map_err(|e| setup_kind(&e))? == SerialString::Set);
        }
        // Checked before anything is written, so a board that is not set up stays as it is.
        if !self.paths.ready(&plan.configuration, released) {
            return Err(ErrorKind::NotFound.into());
        }
        // A bind that worked without the tree's network function: the kernel refused it for the
        // boot (`Trees::bind`), and the rest of the gadget is up.
        let network = self.paths.gadget.join("functions").join(NCM_DIR);
        self.note_network();
        let had_network = self.network_seen.load(std::sync::atomic::Ordering::Relaxed);
        if let Err(held) = self.name_free() {
            say(
                &self.paths.name,
                &format!(
                    "not binding: {held}, the network interface's name, is held by another network device"
                ),
            );
            return Err(Failed {
                kind: ErrorKind::Busy,
                note: Some(NETWORK_NAME_HELD),
            });
        }
        let template = self.name_template();
        if released {
            // The controller let go and the session kept ep0 open (see `disable`). The descriptors
            // are still the kernel's, and a fresh ep0 could not describe them again while a holder
            // keeps the endpoints open, so only the attribute is written.
            // A failed bind keeps the session for the holder, as a let-go does: the helper's bind
            // never leaves a network function that failed for the next one (`Trees::bind`).
            self.bind_now().map_err(|e| setup_kind(&e))?;
            return Ok(Brought {
                wire: None,
                plan,
                serial,
                network_refused: had_network && !network.is_dir(),
                network_before,
                template,
            });
        }
        self.unbind_now();
        if host_may_hold {
            // Whoever took it off (the helper's takeover or remake, the unbind above, the kernel):
            // the gap runs from now.
            self.note_unbound();
        }
        let attached = self
            .paths
            .attach(&plan, &mut || self.bind_seen(host_may_hold));
        match attached {
            Ok(wire) => Ok(Brought {
                wire: Some(wire),
                plan,
                serial,
                network_refused: had_network && !network.is_dir(),
                network_before,
                template,
            }),
            Err(error) => {
                self.unbind_now();
                Err(setup_kind(&error).into())
            }
        }
    }

    /// Takes the gadget off its controller: through the helper, or directly.
    fn unbind_now(&self) {
        match &self.helper {
            Some((helper, index)) => {
                let was_bound = self.udc_bound();
                if let Ok(mut helper) = helper.lock() {
                    let _ = helper.unbind(*index);
                }
                if was_bound {
                    self.note_unbound();
                }
            }
            None => {
                if self.paths.unbind() {
                    self.note_unbound();
                }
            }
        }
    }
    /// Whether the gadget is on a controller now, as far as this process can read: a host may
    /// hold it configured. A tree that is there and can't be read counts as bound.
    fn udc_bound(&self) -> bool {
        match fs::read_to_string(self.paths.gadget.join("UDC")) {
            Ok(text) => !text.trim_matches(['\n', '\0', ' ']).is_empty(),
            Err(_) => self.paths.gadget.is_dir(),
        }
    }
    /// The gadget was taken off a controller it was bound to (or may have been), now.
    fn note_unbound(&self) {
        *self
            .unbound_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
    }
    /// Before a bind: waits out what is left of the gap ([`REBIND_GAP`]) since this gadget was
    /// last taken off a controller a host may have held it on, so the host sees it go. Done here
    /// and not in the helper, whose system-call filter has no sleep.
    fn wait_rebind_gap(&self) {
        let since = self
            .unbound_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(since) = since {
            let left = self.rebind_gap.saturating_sub(since.elapsed());
            if !left.is_zero() {
                self.gap_wait(left);
            }
        }
    }
    /// Sleeps `for_` as (part of) the gap, and counts it.
    fn gap_wait(&self, for_: Duration) {
        self.gap_waits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(for_);
    }
    /// The bind, made so a host that may still hold the gadget configured sees the device go and
    /// enumerates it again: `host_may_hold` (see `enable`), or the gadget was taken off its
    /// controller since its last bind. Then the gadget is bound and the controller's pullup taken
    /// off for the gap ([`REBIND_GAP`]) and put back (`soft_connect`): a clear `UDC` alone is not
    /// enough where the controller keeps its pullup through the unbind (dwc2 outside its
    /// peripheral state skips the pullup call, so `D+` stays up with every endpoint stopped).
    /// Where the pullup cannot be taken off, `UDC` is cleared and kept clear for the gap instead.
    /// A pullup taken off that cannot be put back fails the bind, which the gadget's next attempt
    /// makes again. Otherwise the bind is only written.
    fn bind_seen(&self, host_may_hold: bool) -> io::Result<()> {
        let let_go = self
            .unbound_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        if !host_may_hold && !let_go {
            return self.bind_now();
        }
        self.bind_now()?;
        if let Err(error) = self.soft_connect_now(false) {
            say(
                &self.paths.name,
                &format!(
                    "the controller's pullup cannot be taken off ({error}): UDC is kept clear for {} ms instead",
                    self.rebind_gap.as_millis()
                ),
            );
            self.unbind_now();
            self.wait_rebind_gap();
            return self.bind_now();
        }
        // The gap's wait is this one: a let-go noted before is spent.
        self.unbound_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.gap_wait(self.rebind_gap);
        self.soft_connect_now(true)?;
        // `disconnect` stopped the controller (`udc_stop`), and `connect` starts it again without
        // the bind's own steps: a controller whose stop turns off its event callbacks (dummy_hcd)
        // keeps them off, and every transfer of the next enumeration fails (`EPROTO`). A bind runs
        // all of the start, so the gadget is bound once more. The host is still settling the
        // connect (its debounce is 100 ms or more), so it sees one device come.
        self.unbind_now();
        self.bind_now()?;
        self.unbound_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        say(
            &self.paths.name,
            &format!(
                "bound, then off the bus for {} ms (soft_connect), then bound again, so a host that held the gadget enumerates it again",
                self.rebind_gap.as_millis()
            ),
        );
        Ok(())
    }
    /// Writes `connect` or `disconnect` to the controller's `soft_connect`: through the helper, or
    /// directly.
    fn soft_connect_now(&self, connect: bool) -> io::Result<()> {
        match &self.helper {
            Some((helper, index)) => helper
                .lock()
                .map_err(|_| poisoned())?
                .soft_connect(*index, connect),
            None => attribute(
                &self.paths.udc,
                "soft_connect",
                if connect { "connect" } else { "disconnect" },
            ),
        }
    }
    /// Writes the controller's name to `UDC`: through the helper, or directly.
    fn bind_now(&self) -> io::Result<()> {
        match &self.helper {
            Some((helper, index)) => helper.lock().map_err(|_| poisoned())?.bind(*index),
            None => attribute(&self.paths.gadget, "UDC", &self.paths.name),
        }
    }
    /// The gadget's network interface now, as configfs and the kernel name it: its name and index.
    /// `None` without a network function, or before the kernel has registered its device (the
    /// name is still the board's template).
    fn network_now(&self) -> Option<(String, u32)> {
        let text = fs::read_to_string(
            self.paths
                .gadget
                .join("functions")
                .join(NCM_DIR)
                .join("ifname"),
        )
        .ok()?;
        let name = text.trim_end_matches(['\n', '\0']).to_string();
        if name.is_empty() || name.contains('%') {
            return None;
        }
        let index = fs::read_to_string(self.net_class().join(&name).join("ifindex"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        Some((name, index))
    }
    /// `/sys/class/net`, beside the controllers' class.
    fn net_class(&self) -> PathBuf {
        self.paths.udc.parent().and_then(Path::parent).map_or_else(
            || PathBuf::from("/sys/class/net"),
            |class| class.join("net"),
        )
    }
    /// The network function's name pin, before a bind: while the kernel has not registered the
    /// function's device, its name is the board's template, and the kernel will give it the first
    /// free number. The name the board means is the first, `0` (`lemnos%d` is `lemnos0`, what the
    /// image's network configuration matches). When another network device has that name, binding
    /// now would bring the interface up under the next number, which nothing matches: the bind
    /// waits, by name, until the name is free. `Err` with the name that is held.
    fn name_free(&self) -> Result<(), String> {
        let Ok(text) = fs::read_to_string(
            self.paths
                .gadget
                .join("functions")
                .join(NCM_DIR)
                .join("ifname"),
        ) else {
            return Ok(());
        };
        let template = text.trim_end_matches(['\n', '\0']);
        if !template.contains("%d") {
            return Ok(());
        }
        let first = template.replacen("%d", "0", 1);
        if self.net_class().join(&first).exists() {
            return Err(first);
        }
        Ok(())
    }
    /// The board's template for the network interface's name, while the kernel has not named it.
    /// Notes that the tree has its network function now (see `network_seen`).
    fn note_network(&self) {
        if self.paths.gadget.join("functions").join(NCM_DIR).is_dir() {
            self.network_seen
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn name_template(&self) -> Option<String> {
        let text = fs::read_to_string(
            self.paths
                .gadget
                .join("functions")
                .join(NCM_DIR)
                .join("ifname"),
        )
        .ok()?;
        let template = text.trim_end_matches(['\n', '\0']);
        template.contains("%d").then(|| template.to_string())
    }
}

/// Sessions parked by functions about to go, each for the next function on its controller: one
/// per daemon, shared by its gadgets.
#[derive(Clone, Default)]
pub struct Parking(std::sync::Arc<std::sync::Mutex<ParkedSessions>>);

#[derive(Default)]
struct ParkedSessions {
    next: u64,
    parked: Vec<Parked>,
    /// The service manager's descriptor store, where bound gadgets' files are kept across the
    /// daemon's restarts ([`keep`]). `None`: nothing is kept.
    store: Option<std::sync::Arc<dyn FdStore>>,
    /// The daemon is stopping: a gadget whose files are kept is left bound, for the next daemon.
    stopping: bool,
    /// Each controller whose kept files the store was asked to close, with the barrier that says
    /// when it has: a fresh instance is not described before.
    clearing: Vec<(String, std::os::fd::OwnedFd)>,
}

struct Parked {
    /// The controller's name.
    name: String,
    park: u64,
    session: Session<FfsWire>,
    set: Vec<u8>,
    serial: Option<bool>,
    note: Option<&'static str>,
}

impl Parking {
    fn lock(&self) -> std::sync::MutexGuard<'_, ParkedSessions> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The session parked for controller `name`, taken out.
    fn take(&self, name: &str) -> Option<Parked> {
        let mut parked = self.lock();
        let at = parked.parked.iter().position(|p| p.name == name)?;
        Some(parked.parked.swap_remove(at))
    }

    /// The session of park `park`, taken out, if nothing took it up.
    fn take_park(&self, park: u64) -> Option<Parked> {
        let mut parked = self.lock();
        let at = parked.parked.iter().position(|p| p.park == park)?;
        Some(parked.parked.swap_remove(at))
    }

    /// Keeps bound gadgets' files in `store` from now on ([`keep`]).
    pub fn keep_in(&self, store: std::sync::Arc<dyn FdStore>) {
        self.lock().store = Some(store);
    }

    fn store(&self) -> Option<std::sync::Arc<dyn FdStore>> {
        self.lock().store.clone()
    }

    /// The daemon is stopping: from now on a gadget whose files are kept is not taken off its
    /// controller when its function goes, and its files stay in the store for the next daemon.
    pub fn stopping(&self) {
        self.lock().stopping = true;
    }

    fn is_stopping(&self) -> bool {
        self.lock().stopping
    }

    /// Takes the gadgets the store kept for this daemon (`given`, the store's descriptors with their
    /// names; the ones that are not a gadget's are left out) as parked sessions, each for the first
    /// function on its controller. `known`: the controllers of the board's gadgets, when the board
    /// lists them; a gadget kept for another controller is let go. A gadget whose files or record
    /// are not whole is let go too. Returns a line for the daemon's log for each gadget.
    pub fn adopt_kept(
        &self,
        given: Vec<(String, std::os::fd::OwnedFd)>,
        known: Option<&[String]>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let store = self.store();
        for kept in keep::sort(given) {
            let udc = kept.udc.clone();
            let let_go_all = |count: usize| {
                if let Some(store) = &store {
                    for name in keep::names(&udc, count.max(keep::MAX_KEPT_ENDPOINTS)) {
                        store.let_go(&name);
                    }
                }
            };
            if known.is_some_and(|known| !known.contains(&udc)) {
                let_go_all(kept.files.len());
                lines.push(format!(
                    "gadget {udc}: the files kept for it were let go: the board has no gadget on that controller"
                ));
                continue;
            }
            match parked_from(kept) {
                Ok((parked, drained)) => {
                    let mut sessions = self.lock();
                    sessions.next += 1;
                    let park = sessions.next;
                    let parked = Parked { park, ..parked };
                    lines.push(format!(
                        "gadget {udc}: kept bound through the restart: taken up from the service manager's store (host {})",
                        if parked.session.released {
                            "let go of it"
                        } else if parked.session.configured {
                            "configured"
                        } else {
                            "not configured"
                        }
                    ));
                    lines.extend(
                        drained
                            .into_iter()
                            .map(|line| format!("gadget {udc}: {line}")),
                    );
                    sessions.parked.retain(|p| p.name != udc);
                    sessions.parked.push(parked);
                }
                Err((why, count)) => {
                    let_go_all(count);
                    lines.push(format!(
                        "gadget {udc}: the files kept for it were let go: {why}; it is set up again"
                    ));
                }
            }
        }
        lines
    }

    /// Asks the store to close the files kept for `name`'s session, and notes the barrier that
    /// says when it has.
    fn let_go_kept(&self, name: &str, endpoints: usize) {
        let Some(store) = self.store() else {
            return;
        };
        for kept in keep::names(name, endpoints) {
            store.let_go(&kept);
        }
        if let Some(barrier) = store.barrier() {
            let mut sessions = self.lock();
            sessions.clearing.retain(|(n, _)| n != name);
            sessions.clearing.push((name.to_string(), barrier));
        }
    }

    /// Whether the store has closed what it was asked to close for `name` (or was never asked).
    fn cleared(&self, name: &str) -> bool {
        let mut sessions = self.lock();
        let Some(at) = sessions.clearing.iter().position(|(n, _)| n == name) else {
            return true;
        };
        let done = lemnos_linux_sys::poll::poll_one(
            std::os::fd::AsFd::as_fd(&sessions.clearing[at].1),
            libc::POLLIN,
            Some(Duration::ZERO),
        )
        .map_or(true, |revents| revents != 0);
        if done {
            sessions.clearing.swap_remove(at);
        }
        done
    }
}

/// A parked session made of the files a store kept: the endpoints and state as the record says,
/// with what the host did while no daemon ran ended ([`Session::drain`]; its lines for the log
/// come with it). The error says why it can't be, with how many endpoint names to let go.
fn parked_from(kept: keep::Given) -> Result<(Parked, Vec<String>), (String, usize)> {
    let count = kept.files.len().saturating_sub(1);
    if !kept.strays.is_empty() {
        // A name twice, or one this daemon never keeps: what is kept is not one session's.
        return Err((
            format!(
                "names that are not one session's ({})",
                kept.strays.join(", ")
            ),
            count,
        ));
    }
    let record = kept
        .record
        .ok_or_else(|| ("its record is not there".to_string(), count))?;
    let (record, recorded) = keep::Record::read(record)
        .map_err(|e| (format!("its record can't be read ({e})"), count))?;
    let n = recorded.endpoints.len();
    let mut files = kept.files;
    files.sort_by_key(|(at, _)| *at);
    if files.len() != n + 1 || files.iter().enumerate().any(|(i, (at, _))| *at != i) {
        return Err((
            format!(
                "its files are not whole ({} of ep0 and {n} endpoints)",
                files.len()
            ),
            n.max(count),
        ));
    }
    let mut files = files.into_iter().map(|(_, fd)| fd);
    let ep0 = Ep0::from(files.next().ok_or_else(|| ("no ep0".to_string(), n))?);
    let endpoints: Vec<Endpoint> = files.map(Endpoint::from).collect();
    let mut queues = Vec::with_capacity(n);
    for (_, iso) in &recorded.endpoints {
        queues.push(match iso {
            Some(bytes) => Transfers::Ring(Box::new(
                aio::Ring::new(ISO_TRANSFERS, *bytes as usize)
                    .map_err(|e| (format!("its transfers can't be made ({e})"), n))?,
            )),
            None => Transfers::One(
                aio::Queue::new(TRANSFER_BYTES)
                    .map_err(|e| (format!("its transfers can't be made ({e})"), n))?,
            ),
        });
    }
    let stage = Ep0Stage::spawn().map_err(|e| (format!("its ep0 thread can't start ({e})"), n))?;
    let mut session = Session::new(
        FfsWire {
            ep0,
            endpoints,
            queues,
            stage,
        },
        recorded.endpoints,
    );
    session.lend = recorded.lend;
    session.configured = recorded.configured;
    session.suspended = recorded.suspended;
    session.released = recorded.released;
    session.record = Some(record);
    let drained = session.drain(recorded.request);
    Ok((
        Parked {
            name: kept.udc,
            park: 0,
            session,
            set: recorded.set,
            serial: recorded.serial,
            note: recorded.network_refused.then_some(NETWORK_REFUSED),
        },
        drained,
    ))
}

/// The `/sys/class/udc` entry a `udc:` name stands for. A name that is an entry of that directory is
/// itself. Otherwise the name may be the platform device the controller sits under: a controller the kernel
/// names by an index (`musb-hdrc.N.auto` on the A733's OTG port, N not fixed) sits under the device named by
/// its address (`4100000.usb`), so the name matches the one entry whose sysfs path runs through a directory of
/// that name. `None` when there is none, or more than one. Reads the directory only.
pub fn resolve_udc(udc_class: &Path, name: &str) -> Option<String> {
    if udc_class.join(name).exists() {
        return Some(name.to_string());
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(udc_class).ok()?.flatten() {
        let Ok(real) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if real.components().any(|part| part.as_os_str() == name) {
            found.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

impl FunctionFsGadget {
    /// The gadget for the controller `udc`, a name as `/sys/class/udc` lists it. `configfs` is
    /// where configfs is mounted (`/sys/kernel/config`), `mounts` the directory that holds the
    /// FunctionFS mount point named after the controller (`/dev/usb-ffs`), and `udc_class`
    /// where the controllers are listed (`/sys/class/udc`). Nothing is touched until
    /// [`UsbFunction::enable`]. `None` for a name that can't be a controller's.
    pub fn new(configfs: &Path, mounts: &Path, udc_class: &Path, udc: &str) -> Option<Self> {
        // A name that is the controller's platform device stands for the controller's entry (see `resolve_udc`).
        let udc = resolve_udc(udc_class, udc).unwrap_or_else(|| udc.to_string());
        let need = format!("{PREFIX}{udc}");
        let name = controller(&need)?;
        Some(Self {
            wired: Wired {
                paths: Paths {
                    gadget: configfs.join("usb_gadget").join(format!("lemnos-{name}")),
                    mount: mounts.join(name),
                    udc: udc_class.join(name),
                    name: name.to_string(),
                },
                helper: None,
                unbound_at: Arc::default(),
                rebind_gap: if cfg!(test) {
                    Duration::from_millis(20)
                } else {
                    REBIND_GAP
                },
                gap_waits: Arc::default(),
                network_seen: Arc::default(),
            },
            work: Work::default(),
            session: None,
            _claim: None,
            serial: None,
            note: None,
            enabled_before: false,
            set: Vec::new(),
            parking: None,
            parked_here: None,
            took_up: None,
        })
    }

    /// Parks sessions in `parking`, shared with the other gadgets of the daemon.
    #[must_use]
    pub fn parking(mut self, parking: Parking) -> Self {
        self.parking = Some(parking);
        self
    }

    /// Takes a parked session down: the files the store kept for it let go, the gadget off its
    /// controller, then its files closed.
    fn take_down(&self, parked: Parked) {
        self.let_go_kept(&parked.session);
        self.work.submit(
            &self.wired,
            Job::Down {
                unbind: true,
                close: Some(Box::new(parked)),
            },
        );
    }

    /// Asks the store to close the files it kept for `session`, if it kept them.
    fn let_go_kept<W>(&self, session: &Session<W>) {
        if session.record.is_some()
            && let Some(parking) = &self.parking
        {
            parking.let_go_kept(&self.wired.paths.name, session.endpoints.len());
        }
    }

    /// Whether the daemon is stopping and `session`'s files are kept: the gadget is left as it is
    /// for the next daemon.
    fn left_for_next<W>(&self, session: &Session<W>) -> bool {
        session.record.is_some() && self.parking.as_ref().is_some_and(Parking::is_stopping)
    }

    /// Gives the files of the session just enabled to the store, with their record, so the gadget
    /// stays bound through a restart of the daemon ([`keep`]). Without a store nothing is kept; a
    /// store that refuses one of them keeps none of them.
    fn keep_session(&mut self) {
        let Some(store) = self.parking.as_ref().and_then(Parking::store) else {
            return;
        };
        let name = self.wired.paths.name.clone();
        let serial = self.serial;
        let network_refused = self.note == Some(NETWORK_REFUSED);
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let recorded = keep::Recorded {
            configured: session.configured,
            suspended: session.suspended,
            released: session.released,
            serial,
            network_refused,
            endpoints: session.endpoints.clone(),
            lend: session.lend,
            set: self.set.clone(),
            request: session.pending.as_ref().map(setup_bytes),
        };
        let record = match keep::Record::new(&recorded) {
            Ok(record) => record,
            Err(error) => {
                say(
                    &name,
                    &format!(
                        "its files are not kept through a restart: the record can't be made ({error})"
                    ),
                );
                return;
            }
        };
        let mut kept = store.keep(&keep::record_name(&name), record.fd());
        if matches!(kept, Ok(true)) {
            kept = store.keep(&keep::file_name(&name, 0), session.wire.ep0.as_fd());
        }
        for (at, endpoint) in session.wire.endpoints.iter().enumerate() {
            if matches!(kept, Ok(true)) {
                kept = store.keep(&keep::file_name(&name, at + 1), endpoint.as_fd());
            }
        }
        match kept {
            Ok(true) => session.record = Some(record),
            Ok(false) => {}
            Err(error) => {
                say(
                    &name,
                    &format!(
                        "its files are not kept through a restart: the store refused them ({error})"
                    ),
                );
                if let Some(parking) = &self.parking {
                    parking.let_go_kept(&name, session.endpoints.len());
                }
            }
        }
    }

    /// Takes up the outcome of the bring-up the gadget's thread did for an enable of
    /// `descriptors`.
    fn brought(
        &mut self,
        up: Result<Brought, Failed>,
        descriptors: &[u8],
    ) -> Result<(), ErrorKind> {
        let brought = match up {
            Ok(brought) => brought,
            Err(failed) => {
                self.note = failed.note;
                return Err(failed.kind);
            }
        };
        self.note = brought.network_refused.then_some(NETWORK_REFUSED);
        let Some(wire) = brought.wire else {
            // Bound again on the session the function kept through the let-go.
            if let Some(session) = self.session.as_mut() {
                session.released = false;
                session.note_state();
            }
            self.check_name(brought.template.as_deref());
            self.say_network(brought.network_before.as_ref(), "bound again");
            return Ok(());
        };
        // Through a helper, what its setup reported; otherwise the strings written.
        self.serial = brought.serial.or(Some(
            brought
                .plan
                .strings
                .iter()
                .any(|(f, _)| *f == "serialnumber"),
        ));
        let plan = *brought.plan;
        let mut session = Session::new(wire, plan.endpoints);
        session.lend = plan.lend;
        self.session = Some(session);
        descriptors.clone_into(&mut self.set);
        self.check_name(brought.template.as_deref());
        self.keep_session();
        self.say_network(brought.network_before.as_ref(), "set up and bound");
        Ok(())
    }

    /// After a bind: the name the kernel gave the network interface, against the board's first
    /// (`template`, read before the bind). A mismatch is said, and kept as the gadget's note.
    fn check_name(&mut self, template: Option<&str>) {
        let (Some(template), Some((name, _))) = (template, self.wired.network_now()) else {
            return;
        };
        let first = template.replacen("%d", "0", 1);
        if name != first {
            say(
                &self.wired.paths.name,
                &format!(
                    "the network interface came up as {name}, not {first}: nothing on the board that matches {first} sees it"
                ),
            );
            self.note = Some(NETWORK_NAME_OTHER);
        }
    }

    /// The log line for the network interface across an enable: its name and index before and
    /// after, so a device the kernel made anew (a new index, or a new name) shows.
    fn say_network(&self, before: Option<&(String, u32)>, how: &str) {
        let after = self.wired.network_now();
        if before.is_none() && after.is_none() {
            return;
        }
        let text = |n: Option<&(String, u32)>| {
            n.map_or_else(
                || "none".to_string(),
                |(name, index)| format!("{name} ifindex {index}"),
            )
        };
        say(
            &self.wired.paths.name,
            &format!(
                "network {} → {} ({how})",
                text(before),
                text(after.as_ref())
            ),
        );
    }

    pub(crate) fn holding(mut self, claim: Claim) -> Self {
        self._claim = Some(claim);
        self
    }

    /// The same gadget with another gap before a bind that follows a let-go (tests).
    #[cfg(test)]
    pub(crate) fn with_rebind_gap(mut self, gap: std::time::Duration) -> Self {
        self.wired.rebind_gap = gap;
        self
    }

    /// Makes and binds this gadget through `helper`, as its index `index` there.
    #[must_use]
    pub fn with_helper(mut self, helper: SharedHelper, index: u32) -> Self {
        self.wired.helper = Some((helper, index));
        self
    }

    fn session(&mut self) -> Result<&mut Session<FfsWire>, ErrorKind> {
        self.session.as_mut().ok_or(ErrorKind::Unavailable)
    }
}

impl UsbFunction for FunctionFsGadget {
    type Error = ErrorKind;

    /// The bulk pair's files, as the session holds them: the daemon's own descriptors, which a
    /// holder dups by taking a borrow of them. Nothing is opened here.
    fn endpoint_handles(&self) -> Result<[BufferHandle<'_>; 2], ErrorKind> {
        let session = self.session.as_ref().ok_or(ErrorKind::Unavailable)?;
        // Kept open for the holder's copies after the controller let go: nothing is lent while
        // the gadget is down.
        if session.released {
            return Err(ErrorKind::Unavailable);
        }
        let [out, input] = session.lend.ok_or(ErrorKind::Unsupported)?;
        Ok([
            BufferHandle::from(session.wire.endpoints[out].as_fd()),
            BufferHandle::from(session.wire.endpoints[input].as_fd()),
        ])
    }

    /// The `ifname` configfs reports for the NCM function, once bound: the name the kernel gave
    /// the interface. `None` while not bound, with no NCM function, or with no name.
    fn interface_name(&mut self, into: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        if self.session.is_none() {
            return Ok(None);
        }
        let file = self
            .wired
            .paths
            .gadget
            .join("functions")
            .join(NCM_DIR)
            .join("ifname");
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::kind_of(&error)),
        };
        let name = text.trim_end_matches('\n').as_bytes();
        if name.is_empty() {
            return Ok(None);
        }
        into.get_mut(..name.len())
            .ok_or(ErrorKind::InvalidInput)?
            .copy_from_slice(name);
        Ok(Some(name.len()))
    }

    fn serial_string_set(&mut self) -> Result<Option<bool>, ErrorKind> {
        Ok(self.serial.filter(|_| self.session.is_some()))
    }

    /// Begins the bring-up on the gadget's own thread and says `Busy` with
    /// [`enable_pending`](UsbFunction::enable_pending); a later call with the same descriptors
    /// takes its outcome. Only what never waits is done here: a parked session taken up, the
    /// descriptors checked, the store's barrier looked at.
    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        if let Some(up) = self.work.take_up() {
            return self.brought(up, descriptors);
        }
        if self.work.up_pending() {
            self.note = Some(if self.work.up_for().is_some_and(|t| t >= SLOW_UP) {
                SETTING_UP_SLOW
            } else {
                SETTING_UP
            });
            return Err(ErrorKind::Busy);
        }
        self.took_up = None;
        if self.session.is_none()
            && let Some(parked) = self
                .parking
                .as_ref()
                .and_then(|parking| parking.take(&self.wired.paths.name))
        {
            if parked.set == descriptors {
                let released = parked.session.released;
                self.session = Some(parked.session.taken_up());
                self.serial = parked.serial;
                self.note = parked.note;
                self.set = parked.set;
                if !released {
                    // The gadget never left the bus: nothing is written, the holder's copies stay.
                    self.took_up = Some(TookUp::Adopted);
                    if let Some((name, index)) = self.wired.network_now() {
                        say(
                            &self.wired.paths.name,
                            &format!(
                                "network {name} ifindex {index} → {name} ifindex {index} (taken up as it was)"
                            ),
                        );
                    }
                    return Ok(());
                }
                // Kept while the controller had let go of it (a store's session): bound again
                // below on the same ep0, as a let-go is.
            } else {
                self.take_down(parked);
                self.took_up = Some(TookUp::Replaced);
            }
        }
        let released = match self.session.as_ref() {
            Some(session) if session.released => true,
            Some(_) => return Err(ErrorKind::Busy),
            None => false,
        };
        let plan = Plan::of(&UsbDescriptors::parse(descriptors)?)?;
        // The files a store kept for the session before are closed first: a fresh instance can't
        // be described while the old one is open anywhere.
        if !released
            && let Some(parking) = &self.parking
            && !parking.cleared(&self.wired.paths.name)
        {
            self.note = Some(KEPT_CLOSING);
            return Err(ErrorKind::Busy);
        }
        // What a host may hold configured as this enable begins: a gadget that is bound, or, the
        // first time this function enables it, a tree that was already there (a daemon before
        // this one made it, and when it came off its controller is not known). Either is bound
        // again only after `UDC` has been clear for the gap.
        let host_may_hold = !released
            && (self.wired.udc_bound()
                || (!std::mem::replace(&mut self.enabled_before, true)
                    && self.wired.paths.gadget.is_dir()));
        self.note = Some(SETTING_UP);
        self.work.submit(
            &self.wired,
            Job::Up {
                plan: Box::new(plan),
                released,
                host_may_hold,
            },
        );
        Err(ErrorKind::Busy)
    }

    fn enable_pending(&self) -> bool {
        self.work.up_pending()
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        // A bring-up under way is undone when it ends.
        self.work.cancel_up(&self.wired);
        // A session this function parked is taken down only if nothing took it up, and not when
        // the daemon is stopping with its files kept: the next daemon takes it up.
        if let Some(park) = self.parked_here.take()
            && let Some(parked) = self.parking.as_ref().and_then(|p| p.take_park(park))
            && !self.left_for_next(&parked.session)
        {
            self.take_down(parked);
        }
        match self.session.as_ref() {
            // Kept for the next daemon: the files close, the store's copies stay, and the gadget
            // stays bound.
            Some(session) if self.left_for_next(session) => {
                let session = self.session.take().map(|s| Box::new(s) as Box<dyn Send>);
                self.work.submit(
                    &self.wired,
                    Job::Down {
                        unbind: false,
                        close: session,
                    },
                );
                Ok(())
            }
            // The controller let go already, so there is nothing to unbind. The session keeps ep0
            // and the endpoints open: a holder's copies stay valid, and `enable` binds again on the
            // same ep0. Dropping the gadget closes them.
            Some(session) if session.released => Ok(()),
            Some(session) => {
                // The store's copies let go; then, on the gadget's thread, unbind and close the
                // files. The gadget stays as the setup made it.
                self.let_go_kept(session);
                let session = self.session.take().map(|s| Box::new(s) as Box<dyn Send>);
                self.work.submit(
                    &self.wired,
                    Job::Down {
                        unbind: true,
                        close: session,
                    },
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        self.session()?.next_event()
    }

    /// The stage runs on ep0's own thread: this waits at most `timeout`, and a stage the host has
    /// not ended by then goes on (see [`Ep0Stage`]).
    fn control_in(&mut self, data: &[u8], timeout: Duration) -> Result<usize, ErrorKind> {
        self.session()?.control_in(data, timeout)
    }

    /// As `control_in`.
    fn control_out(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, ErrorKind> {
        self.session()?.control_out(data, timeout)
    }

    fn control_end(&mut self, data: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        self.session()?.control_end(data)
    }

    fn enable_note(&self) -> Option<&'static str> {
        self.note
    }

    /// The bound session goes into the daemon's parking, for the next function on this
    /// controller, and the controller's claim goes with it: this function does nothing more.
    fn park(&mut self) -> bool {
        let Some(parking) = self.parking.clone() else {
            return false;
        };
        if self.parked_here.is_some() || self.session.as_ref().is_none_or(|s| s.released) {
            return false;
        }
        let Some(session) = self.session.take() else {
            return false;
        };
        let mut parked = parking.lock();
        parked.next += 1;
        let park = parked.next;
        // One parked session a controller: an older one nothing took up goes now.
        let older = parked
            .parked
            .iter()
            .position(|p| p.name == self.wired.paths.name)
            .map(|at| parked.parked.swap_remove(at));
        parked.parked.push(Parked {
            name: self.wired.paths.name.clone(),
            park,
            session,
            set: std::mem::take(&mut self.set),
            serial: self.serial,
            note: self.note,
        });
        drop(parked);
        drop(older);
        self.parked_here = Some(park);
        self._claim = None;
        true
    }

    fn took_up(&self) -> Option<TookUp> {
        self.took_up
    }

    fn control_stall(&mut self) -> Result<(), ErrorKind> {
        self.session()?.control_stall()
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), ErrorKind> {
        self.session()?.set_halt(endpoint, halted)
    }

    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.session()?.read(endpoint, data, timeout)
    }

    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize, ErrorKind> {
        self.session()?.write(endpoint, data, timeout)
    }
}

impl Drop for FunctionFsGadget {
    fn drop(&mut self) {
        let _ = self.disable();
        // A session the controller let go of is kept open by `disable` for its holder; closed now,
        // and the store's copies with it, unless they are kept for the next daemon.
        if let Some(session) = self.session.take() {
            if !self.left_for_next(&session) {
                self.let_go_kept(&session);
            }
            self.work.submit(
                &self.wired,
                Job::Down {
                    unbind: false,
                    close: Some(Box::new(session)),
                },
            );
        }
        self.work.end();
    }
}

/// A line for the daemon's log about the gadget on controller `name`.
fn say(name: &str, text: &str) {
    let _ = writeln!(io::stderr(), "lemnosd: gadget {name}: {text}");
}

/// What a gadget waits on while the store closes the files it kept for the session before.
pub const KEPT_CLOSING: &str =
    "waiting for the service manager to close the files it kept for the gadget before";

/// What a gadget waits on while another network device has the name its network interface is
/// meant to have.
pub const NETWORK_NAME_HELD: &str =
    "the network interface's name is held by another network device: not binding until it is free";

/// What a bound gadget lacks when the kernel named its network interface other than the board's
/// first name.
pub const NETWORK_NAME_OTHER: &str =
    "the network interface came up under another name than the board's";

#[cfg(test)]
mod tests;
