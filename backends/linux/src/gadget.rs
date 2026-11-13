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
//!    `functions/ncm.usb0` with `host_addr` and `dev_addr`; `acm` makes `functions/acm.GS0`.
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
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_hal::blocks::{ControlSetup, UsbFunctionEvent, iso_interval_bytes};
use lemnos_hal::usb_descriptors::{
    self, CONFIGURATION_LEN, GadgetFunction, INTERFACE, UsbDescriptors,
};
use lemnos_hal::{ErrorKind, UsbFunction};
use lemnos_linux_sys::aio;
use lemnos_linux_sys::functionfs::{self, Endpoint, Ep0, Event, Setup};

use crate::Claim;

pub(crate) const PREFIX: &str = "udc:";

/// The longest configfs takes for a function directory's name (`ffs.NAME`).
const MAX_FUNCTION_NAME: usize = 39;
/// The longest string configfs takes, less the newline written after it.
const MAX_STRING: usize = 125;
/// The highest alternate setting FunctionFS takes (`MAX_ALT_SETTINGS` in `f_fs.c`).
pub const MAX_ALTERNATE: u8 = 2;
/// Transfers an isochronous endpoint keeps with the kernel.
pub const ISO_TRANSFERS: usize = 8;

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
    match error.raw_os_error() {
        Some(libc::ENOENT) => ErrorKind::NotFound,
        Some(libc::EBUSY) => ErrorKind::Busy,
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
    descriptors: Vec<u8>,
    function_strings: Vec<u8>,
    /// Each endpoint's address, and for an isochronous one its bytes per interval.
    endpoints: Vec<(u8, Option<u32>)>,
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
        let endpoints = usb_descriptors::endpoints(set.full_speed())
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
        let flags = functionfs::VIRTUAL_ADDR | functionfs::ALL_CTRL_RECIP;
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
            descriptors: functionfs::descriptors_blob(flags, Some(full), high).map_err(invalid)?,
            function_strings: functionfs::strings_blob(language.unwrap_or(0), &table)
                .map_err(invalid)?,
            endpoints,
        })
    }
}

/// Writes `value` and a newline to the attribute `file` of `dir`.
fn attribute(dir: &Path, file: &str, value: &str) -> io::Result<()> {
    fs::write(dir.join(file), format!("{value}\n"))
}

/// Removes a directory configfs made for us. Its attribute files go with it there; in a
/// stand-in tree they are files like any other and are removed first.
fn remove_dir(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| !kind.is_dir()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

/// The directories in `dir`, not following links.
fn directories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
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
}

/// The function directories the setup makes, by the name configfs gives each.
const NCM_DIR: &str = "ncm.usb0";
const ACM_DIR: &str = "acm.GS0";

fn invalid(kind: ErrorKind) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{kind:?}"))
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
    /// makes is what says the setup ran.
    fn ready(&self, configuration: &str) -> bool {
        let link = self
            .gadget
            .join("configs")
            .join(configuration)
            .join(self.function_name());
        self.udc.exists()
            && self.function().is_dir()
            && fs::symlink_metadata(link).is_ok()
            && self.mount.join("ep0").exists()
    }

    /// Takes the gadget off its controller. The attribute exists in configfs; it is not made
    /// where it doesn't. Never fails: a gadget that isn't bound is already off.
    fn unbind(&self) {
        let udc = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(self.gadget.join("UDC"));
        if let Ok(mut udc) = udc {
            let _ = udc.write(b"\n");
        }
    }

    /// Removes every configuration and strings directory of the gadget, and the links in them.
    /// Never fails: what isn't there is already gone. Setup only.
    fn dismantle(&self) {
        for configuration in directories(&self.gadget.join("configs")) {
            // The functions linked in, then the strings, then the configuration.
            if let Ok(entries) = fs::read_dir(&configuration) {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
            for strings in directories(&configuration.join("strings")) {
                remove_dir(&strings);
            }
            // In configfs `strings` is part of the configuration; in a stand-in tree it is a
            // directory to remove.
            let _ = fs::remove_dir(configuration.join("strings"));
            remove_dir(&configuration);
        }
        for strings in directories(&self.gadget.join("strings")) {
            remove_dir(&strings);
        }
    }

    /// Removes the function directories the setup makes. Setup only, after the configurations
    /// are gone and the FunctionFS mount is unmounted.
    fn remove_functions(&self) {
        for name in [self.function_name().as_str(), NCM_DIR, ACM_DIR] {
            remove_dir(&self.gadget.join("functions").join(name));
        }
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
/// Nothing is bound. A failure leaves whatever it made, and the next setup removes it.
pub fn prepare(
    roots: &Roots<'_>,
    udc_name: &str,
    owner: Owner,
    descriptors: &[u8],
    functions: &[GadgetFunction],
    system: &mut dyn Mounts,
) -> io::Result<()> {
    let Roots {
        configfs,
        mounts,
        udc_class,
    } = *roots;
    let need = format!("{PREFIX}{udc_name}");
    let name = controller(&need).ok_or_else(|| invalid(ErrorKind::InvalidInput))?;
    let paths = Paths {
        gadget: configfs.join("usb_gadget").join(format!("lemnos-{name}")),
        mount: mounts.join(name),
        udc: udc_class.join(name),
        name: name.to_string(),
    };
    let plan = Plan::of(&UsbDescriptors::parse(descriptors).map_err(invalid)?).map_err(invalid)?;
    if !paths.udc.exists() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    }
    paths.unbind();
    paths.dismantle();
    // `unmount` of a point that isn't mounted fails, which is the state a fresh image is in.
    let _ = system.unmount(&paths.mount);
    paths.remove_functions();

    fs::create_dir_all(&paths.gadget)?;
    for (file, value) in &plan.identity {
        attribute(&paths.gadget, file, value)?;
    }
    // Kernels before 5.12 have no `max_speed`; they run at what the controller offers.
    if let Ok(mut speed) = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(paths.gadget.join("max_speed"))
    {
        speed.write_all(format!("{}\n", plan.max_speed).as_bytes())?;
    }
    if let Some(language) = &plan.language
        && !plan.strings.is_empty()
    {
        let strings = paths.gadget.join("strings").join(language);
        fs::create_dir_all(&strings)?;
        for (file, value) in &plan.strings {
            attribute(&strings, file, value)?;
        }
    }
    let configuration = paths.gadget.join("configs").join(&plan.configuration);
    fs::create_dir_all(&configuration)?;
    attribute(&configuration, "MaxPower", &plan.max_power)?;
    attribute(&configuration, "bmAttributes", &plan.attributes)?;
    if let (Some(language), Some(name)) = (&plan.language, &plan.configuration_name) {
        let strings = configuration.join("strings").join(language);
        fs::create_dir_all(&strings)?;
        attribute(&strings, "configuration", name)?;
    }

    // The functions, each made in configfs, FunctionFS last of the three for its mount.
    let functions_dir = paths.gadget.join("functions");
    let mut links = Vec::with_capacity(functions.len());
    for function in functions {
        match function {
            GadgetFunction::Ffs => {
                let dir = paths.function();
                fs::create_dir(&dir)?;
                fs::create_dir_all(&paths.mount)?;
                let options = format!("uid={},gid={},rmode=0500,fmode=0600", owner.uid, owner.gid);
                system.functionfs(&paths.name, &paths.mount, &options)?;
                links.push(paths.function_name());
            }
            GadgetFunction::Ncm { host, device } => {
                let dir = functions_dir.join(NCM_DIR);
                fs::create_dir(&dir)?;
                attribute(&dir, "host_addr", &mac_text(host))?;
                attribute(&dir, "dev_addr", &mac_text(device))?;
                links.push(NCM_DIR.to_string());
            }
            GadgetFunction::Acm => {
                fs::create_dir(functions_dir.join(ACM_DIR))?;
                links.push(ACM_DIR.to_string());
            }
        }
    }
    for name in &links {
        std::os::unix::fs::symlink(functions_dir.join(name), configuration.join(name))?;
    }

    // The daemon writes the controller's name here, and nothing else in the gadget.
    let udc = paths.gadget.join("UDC");
    std::os::unix::fs::chown(&udc, Some(owner.uid), Some(owner.gid))?;
    fs::set_permissions(&udc, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

impl Paths {
    /// Steps 3 to 5 of the daemon's [`FunctionFsGadget::enable`]: ep0, the endpoint files and
    /// the bind. On an error the caller unbinds. A piece that went missing since `ready`
    /// looked is `ENOENT` from the step that needs it.
    fn attach(&self, plan: &Plan) -> io::Result<FfsWire> {
        let ep0 = Ep0::open(&self.mount.join("ep0"))?;
        ep0.describe(&plan.descriptors, &plan.function_strings)?;
        let mut endpoints = Vec::with_capacity(plan.endpoints.len());
        for (address, _) in &plan.endpoints {
            let file = self.mount.join(format!("ep{address:02x}"));
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
        attribute(&self.gadget, "UDC", &self.name)?;
        Ok(FfsWire {
            ep0,
            endpoints,
            queues,
        })
    }
}

/// The FunctionFS files of an enabled function, as [`Session`] uses them. Endpoints are
/// numbered by their place in the plan.
trait Wire {
    fn next_event(&mut self) -> io::Result<Option<Event>>;
    fn send(&mut self, data: &[u8]) -> io::Result<usize>;
    fn receive(&mut self, data: &mut [u8]) -> io::Result<usize>;
    fn stall(&mut self, request_is_in: bool) -> io::Result<()>;
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
}

impl Wire for FfsWire {
    fn next_event(&mut self) -> io::Result<Option<Event>> {
        self.ep0.next_event()
    }

    fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        self.ep0.send(data)
    }

    fn receive(&mut self, data: &mut [u8]) -> io::Result<usize> {
        self.ep0.receive(data)
    }

    fn stall(&mut self, request_is_in: bool) -> io::Result<()> {
        self.ep0.stall(request_is_in)
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
    configured: bool,
    /// The gadget was unbound from its controller.
    released: bool,
    pending: Option<Setup>,
}

impl<W: Wire> Session<W> {
    fn new(wire: W, endpoints: Vec<(u8, Option<u32>)>) -> Self {
        Self {
            wire,
            endpoints,
            configured: false,
            released: false,
            pending: None,
        }
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
        if let Some(setup) = self.pending.take() {
            // Read past without an answer. If the host has let it go there is nothing to
            // stall, and the event that says so is next.
            let _ = self.wire.stall(setup.is_in());
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
                    return Err(ErrorKind::Unavailable);
                }
                Event::Enable => {
                    // Nothing of the configuration before this one reaches the new one.
                    self.wire.forget();
                    self.configured = true;
                    UsbFunctionEvent::Configured
                }
                Event::Disable => {
                    if !std::mem::take(&mut self.configured) {
                        continue;
                    }
                    UsbFunctionEvent::Deconfigured
                }
                Event::Suspend => UsbFunctionEvent::Suspended,
                Event::Resume => UsbFunctionEvent::Resumed,
                Event::Setup(setup) => {
                    self.pending = Some(setup);
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

    fn control_in(&mut self, data: &[u8]) -> Result<usize, ErrorKind> {
        let setup = self.take_pending(true)?;
        let data = &data[..data.len().min(usize::from(setup.length))];
        self.wire.send(data).map_err(|e| wire_kind(&e))
    }

    fn control_out(&mut self, data: &mut [u8]) -> Result<usize, ErrorKind> {
        let length = usize::from(self.pending.map_or(0, |setup| setup.length));
        if data.len() < length {
            return Err(ErrorKind::InvalidInput);
        }
        self.take_pending(false)?;
        self.wire
            .receive(&mut data[..length])
            .map_err(|e| wire_kind(&e))
    }

    fn control_stall(&mut self) -> Result<(), ErrorKind> {
        let setup = self.pending.take().ok_or(ErrorKind::InvalidInput)?;
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

/// This board as a USB device through one controller: see the module docs.
pub struct FunctionFsGadget {
    paths: Paths,
    session: Option<Session<FfsWire>>,
    _claim: Option<Claim>,
}

impl FunctionFsGadget {
    /// The gadget for the controller `udc`, a name as `/sys/class/udc` lists it. `configfs` is
    /// where configfs is mounted (`/sys/kernel/config`), `mounts` the directory that holds the
    /// FunctionFS mount point named after the controller (`/dev/usb-ffs`), and `udc_class`
    /// where the controllers are listed (`/sys/class/udc`). Nothing is touched until
    /// [`UsbFunction::enable`]. `None` for a name that can't be a controller's.
    pub fn new(configfs: &Path, mounts: &Path, udc_class: &Path, udc: &str) -> Option<Self> {
        let need = format!("{PREFIX}{udc}");
        let name = controller(&need)?;
        Some(Self {
            paths: Paths {
                gadget: configfs.join("usb_gadget").join(format!("lemnos-{name}")),
                mount: mounts.join(name),
                udc: udc_class.join(name),
                name: name.to_string(),
            },
            session: None,
            _claim: None,
        })
    }

    pub(crate) fn holding(mut self, claim: Claim) -> Self {
        self._claim = Some(claim);
        self
    }

    fn session(&mut self) -> Result<&mut Session<FfsWire>, ErrorKind> {
        self.session.as_mut().ok_or(ErrorKind::Unavailable)
    }
}

impl UsbFunction for FunctionFsGadget {
    type Error = ErrorKind;

    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        if self.session.is_some() {
            return Err(ErrorKind::Busy);
        }
        let plan = Plan::of(&UsbDescriptors::parse(descriptors)?)?;
        // Checked before anything is written, so a board that is not set up stays as it is.
        if !self.paths.ready(&plan.configuration) {
            return Err(ErrorKind::NotFound);
        }
        self.paths.unbind();
        match self.paths.attach(&plan) {
            Ok(wire) => {
                self.session = Some(Session::new(wire, plan.endpoints));
                Ok(())
            }
            Err(error) => {
                self.paths.unbind();
                Err(setup_kind(&error))
            }
        }
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        if self.session.is_some() {
            // Unbind first, then close the files. The gadget stays as the setup made it.
            self.paths.unbind();
            self.session = None;
        }
        Ok(())
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        self.session()?.next_event()
    }

    /// Waits for the host: FunctionFS can't abandon a data stage.
    fn control_in(&mut self, data: &[u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        self.session()?.control_in(data)
    }

    /// Waits for the host: FunctionFS can't abandon a data stage.
    fn control_out(&mut self, data: &mut [u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        self.session()?.control_out(data)
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
    }
}

#[cfg(test)]
mod tests;
