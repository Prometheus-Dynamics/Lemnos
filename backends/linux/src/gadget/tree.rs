//! The privileged half of the gadget: making, adopting, binding and taking down the configfs tree
//! `usb_gadget/lemnos-NAME` and its FunctionFS mount, through directory descriptors.
//!
//! Every file this module touches is an entry of a directory it opened once ([`Trees`]): configfs,
//! the parent of the FunctionFS mount points, and `/sys/class/udc`. An entry's name is one
//! checked name (`lemnos_linux_sys::confined`); no path is built from a name and opened. The
//! FunctionFS mount's target is `/proc/self/fd/N` of the mount point's descriptor, so the mount
//! lands on the directory that was opened, whatever the names around it turn into.
//!
//! # A tree that is already there
//!
//! [`Trees::setup`] looks at `lemnos-NAME` before it makes anything:
//!
//! - nothing there: it is made;
//! - the same content as the description asks for (every attribute, every function, every link,
//!   the mount): it is **adopted**, untouched, bound or not;
//! - the same name and a different content, with every part of a kind this module makes: when it
//!   is unbound it is taken down and made again; when it is bound it is left, and the setup fails
//!   with `EBUSY` and says so;
//! - whatever is made again, a network function that is the board file's (its addresses and its
//!   name's template) is kept, unlinked and linked again: the kernel's network device lives as long
//!   as the function's directory, so the interface keeps its name and index for everything on the
//!   board that matches it. Only a network function that differs from the board file is made anew;
//! - a part this module never makes (a function other than `ffs.NAME`, `ncm.usb0` or `acm.GS0`, a
//!   configuration not named `c.N`): the tree is not the helper's. It is left alone and the setup
//!   fails with `EEXIST`, naming the part.

use std::io;

use lemnos_hal::usb_descriptors::{GadgetFunction, IfName, UsbDescriptors};
use lemnos_linux_sys::confined::{CONFIGFS_MAGIC, Dir, Kind};

use super::{
    ACM_DIR, Mounts, NCM_DIR, Owner, PREFIX, Plan, Roots, Udc, controller, fault, id_text,
    ifname_refused, invalid, mac_text,
};
use lemnos_hal::usb_descriptors::ms_os;

/// What a setup found, and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was there: the tree was made.
    Made,
    /// The same tree was there: it is as it was, bound or not.
    Adopted,
    /// A different tree was there, unbound: it was taken down and made again.
    Remade,
    /// The board file's tree was there, its FunctionFS instance still held by a holder of a daemon
    /// before this one past [`HELD_LIMIT`]: it was unbound, taken down and made again, and the
    /// holder's endpoints are broken (their reads and writes fail from then on).
    TakenOver,
}

/// How long a held FunctionFS instance (see [`Outcome::TakenOver`]) is waited for, from the first
/// setup that finds it held. A holder of endpoints a daemon before this one lent may be in a
/// process that outlives that daemon and never closes them; the board's USB link is worth more
/// than its clean hand-over. With the failure rule's 8 s cap on retries, the link is made again
/// within `HELD_LIMIT` + 8 s of the first setup that found it held.
pub const HELD_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The least time a controller's `UDC` stays clear before a gadget the host may still hold
/// configured is bound again. A clear and a set a few milliseconds apart is a rebind a host may
/// not register: it keeps the configuration it had, against endpoints that are fresh and
/// unconfigured, and every function on the link goes silent with the device still enumerated.
/// Half a second is well over what a hub needs to report a disconnect.
pub const REBIND_GAP: std::time::Duration = std::time::Duration::from_millis(500);

/// A bind that followed a let-go of a gadget the host may have held: which way it was let go, and
/// how long `UDC` had been clear when the bind was written. The wait itself is the daemon's
/// (`FunctionFsGadget`): the helper's system-call filter has no sleep, and gets none for this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rebind {
    /// `adopted`, `taken over`, `remade` or `unbound`.
    pub path: &'static str,
    pub clear_for: std::time::Duration,
}

/// The directories a gadget is made in. Opened once where they can be, and again at the next setup
/// where they could not (configfs not mounted yet, the mount point's parent not there yet).
pub struct Trees {
    roots: PathsOf,
    verify_configfs: bool,
    configfs: Option<Dir>,
    mounts: Option<Dir>,
    udc_class: Option<Dir>,
    /// Each controller whose tree was found held, and when it was first found so.
    held: std::sync::Arc<std::sync::Mutex<Vec<(String, std::time::Instant)>>>,
    /// [`HELD_LIMIT`], shorter in tests.
    held_limit: std::time::Duration,
    /// Each controller's functions, in the board file's order, as its last setup was asked for:
    /// what [`Trees::bind`] makes a fresh network function and links again from.
    functions: Vec<(String, Vec<GadgetFunction>)>,
    /// Each controller whose kernel refused the network device on a fresh function: its gadget
    /// is bound without the network function from then on (see [`Trees::bind`]).
    network_refused: Vec<String>,
    /// Each controller whose gadget the host may still hold configured: since when its `UDC` is
    /// known clear (or, for a tree found there, since it was found), and how it came to be.
    let_go: Vec<(String, std::time::Instant, &'static str)>,
    /// Each controller whose tree the last setup found there, and what it did with it (`adopted`,
    /// `taken over`, `remade`): what the next bind is told as, in place of `unbound`.
    origin: Vec<(String, &'static str)>,
    /// What the last bind after a let-go did, until [`Trees::take_rebind`] takes it.
    rebind: Option<(String, Rebind)>,
    /// A test's kernel: a `UDC` write it refuses (with `EBUSY`), from the tree as it is then.
    #[cfg(test)]
    pub(super) refuses: Option<fn(&Dir) -> bool>,
}

/// The three roots, owned: the system's constants, or a test's stand-in directories.
struct PathsOf {
    configfs: std::path::PathBuf,
    mounts: std::path::PathBuf,
    udc_class: std::path::PathBuf,
}

/// One gadget's names and the directories it is in, for the length of one call.
struct Gadget<'a> {
    name: String,
    /// `usb_gadget` in configfs.
    gadgets: Dir,
    /// The mount points' directory.
    mounts: &'a Dir,
    /// `/sys/class/udc`.
    udc_class: &'a Dir,
}

impl Gadget<'_> {
    fn dir_name(&self) -> String {
        format!("lemnos-{}", self.name)
    }

    fn function_name(&self) -> String {
        format!("ffs.{}", self.name)
    }
}

impl Trees {
    /// Trees over `roots`. Nothing is opened until the first call that needs it.
    #[must_use]
    pub fn new(roots: &Roots<'_>) -> Self {
        Self {
            roots: PathsOf {
                configfs: roots.configfs.to_path_buf(),
                mounts: roots.mounts.to_path_buf(),
                udc_class: roots.udc_class.to_path_buf(),
            },
            verify_configfs: false,
            configfs: None,
            mounts: None,
            udc_class: None,
            held: std::sync::Arc::default(),
            held_limit: HELD_LIMIT,
            functions: Vec::new(),
            network_refused: Vec::new(),
            let_go: Vec::new(),
            origin: Vec::new(),
            rebind: None,
            #[cfg(test)]
            refuses: None,
        }
    }

    /// The gadget on `udc_name` was let go `path`'s way, now: the next bind tells how long ago. A
    /// let-go already noted stands as it is: a note is dropped at the bind that follows it, so
    /// one that is still there means `UDC` has been clear since, and its time is the true one.
    fn note_let_go(&mut self, udc_name: &str, path: &'static str) {
        if !self.let_go.iter().any(|(name, _, _)| name == udc_name) {
            self.let_go
                .push((udc_name.to_string(), std::time::Instant::now(), path));
        }
    }

    /// The last setup found `udc_name`'s tree there and did `what` with it.
    fn note_origin(&mut self, udc_name: &str, what: &'static str) {
        self.origin.retain(|(name, _)| name != udc_name);
        self.origin.push((udc_name.to_string(), what));
    }

    /// What the last bind after a let-go on `udc_name` did, once.
    pub fn take_rebind(&mut self, udc_name: &str) -> Option<Rebind> {
        match self.rebind.take() {
            Some((name, rebind)) if name == udc_name => Some(rebind),
            other => {
                self.rebind = other;
                None
            }
        }
    }

    /// The same trees with another wait for a held instance (tests).
    #[must_use]
    pub fn with_held_limit(mut self, limit: std::time::Duration) -> Self {
        self.held_limit = limit;
        self
    }

    /// Takes a configfs directory that is not a mounted configfs (an empty mount point, before
    /// the mount) as not there yet, rather than as the root. The system's helper sets this.
    #[must_use]
    pub fn verifying_configfs(mut self) -> Self {
        self.verify_configfs = true;
        self
    }

    /// Opens what is not open yet. Roots that cannot be opened stay closed and are tried again at
    /// the next call; the error is the first one.
    ///
    /// # Errors
    ///
    /// A root is not there, or configfs is not mounted (`ENOENT`).
    pub fn open(&mut self) -> io::Result<()> {
        if self.udc_class.is_none() {
            self.udc_class = Some(Dir::open_path(&self.roots.udc_class)?);
        }
        if self.configfs.is_none() {
            let dir = Dir::open_path(&self.roots.configfs)?;
            if self.verify_configfs && !dir.on_filesystem(CONFIGFS_MAGIC)? {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            self.configfs = Some(dir);
        }
        if self.mounts.is_none() {
            // The mount points' directory is made when it is not there: it is the one directory
            // this module owns outside configfs. Its parent is opened by path, here and nowhere
            // else, and the directory by name.
            let parent = self
                .roots
                .mounts
                .parent()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
            let name = self
                .roots
                .mounts
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
            let parent = Dir::open_path(parent)?;
            parent.ensure_dir(name, 0o755)?;
            self.mounts = Some(parent.open_dir(name)?);
        }
        Ok(())
    }

    /// The gadget `udc_name`'s names and directories. `usb_gadget` is made where it is not there
    /// (a stand-in tree; in configfs the kernel's own is there once `libcomposite` is loaded, and
    /// a mkdir there is refused, which leaves `ENOENT` for the open).
    fn gadget(&mut self, udc_name: &str) -> io::Result<Gadget<'_>> {
        self.open()?;
        let need = format!("{PREFIX}{udc_name}");
        let name = controller(&need)
            .ok_or_else(|| invalid(lemnos_hal::ErrorKind::InvalidInput))?
            .to_owned();
        let (Some(configfs), Some(mounts), Some(udc_class)) =
            (&self.configfs, &self.mounts, &self.udc_class)
        else {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        };
        let _ = configfs.make_dir("usb_gadget", 0o755);
        Ok(Gadget {
            name,
            gadgets: configfs.open_dir("usb_gadget")?,
            mounts,
            udc_class,
        })
    }

    /// Makes the gadget, or adopts the one that is there: see the module docs.
    ///
    /// # Errors
    ///
    /// A description that does not parse, a controller or configfs that is not there (`ENOENT`),
    /// a tree that is not ours (`EEXIST`) or differs while bound (`EBUSY`), or the step that failed.
    pub(super) fn setup(
        &mut self,
        udc_name: &str,
        owner: Owner,
        descriptors: &[u8],
        functions: &[GadgetFunction],
        system: &mut dyn Mounts,
        udc: Udc,
    ) -> io::Result<Outcome> {
        let plan =
            Plan::of(&UsbDescriptors::parse(descriptors).map_err(invalid)?).map_err(invalid)?;
        match self.functions.iter_mut().find(|(name, _)| name == udc_name) {
            Some((_, kept)) => functions.clone_into(kept),
            None => self
                .functions
                .push((udc_name.to_string(), functions.to_vec())),
        }
        // Taken before the gadget, which borrows the trees for the call.
        let held_list = std::sync::Arc::clone(&self.held);
        let limit = self.held_limit;
        let g = self.gadget(udc_name)?;
        if !g.udc_class.exists(&g.name) {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        let mut held = held_list
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outcome = match g.gadgets.kind(&g.dir_name()) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Outcome::Made,
            Err(error) => return Err(error),
            Ok(Kind::Dir) => {
                let dir = g.gadgets.open_dir(&g.dir_name())?;
                let found = classify(&g, &dir, &plan, functions, owner, system)?;
                let since = match found {
                    Found::Held => match held.iter().find(|(name, _)| *name == g.name) {
                        Some((_, since)) => Some(*since),
                        None => {
                            let now = std::time::Instant::now();
                            held.push((g.name.clone(), now));
                            Some(now)
                        }
                    },
                    _ => {
                        held.retain(|(name, _)| *name != g.name);
                        None
                    }
                };
                match found {
                    Found::Same => {
                        // The host may hold it configured from the daemon before this one. A
                        // tree found bound is noted when it is unbound; one found unbound came
                        // off its controller at a time nobody here saw, so it counts from now.
                        let unbound = !bound(&dir);
                        drop(held);
                        self.note_origin(udc_name, "adopted");
                        if unbound {
                            self.note_let_go(udc_name, "adopted");
                        }
                        return Ok(Outcome::Adopted);
                    }
                    Found::Held => {
                        let waited = since.map_or(std::time::Duration::ZERO, |s| s.elapsed());
                        if waited < limit {
                            return Err(fault(
                                libc::EBUSY,
                                format!(
                                    "the gadget tree {} is the board file's, but its FunctionFS instance is still held: a daemon before this one mounted it where this one can't see, and its endpoints are still open (f_fs refuses a second mount): left alone for {} s more, then taken over",
                                    g.dir_name(),
                                    limit.saturating_sub(waited).as_secs().max(1)
                                ),
                            ));
                        }
                        held.retain(|(name, _)| *name != g.name);
                        let _ = dir.write("UDC", b"\n");
                        Outcome::TakenOver
                    }
                    Found::Foreign(part) => {
                        return Err(fault(
                            libc::EEXIST,
                            format!(
                                "a gadget tree named {} is there with {part}, which this helper never makes: it is not the helper's, and is left alone",
                                g.dir_name()
                            ),
                        ));
                    }
                    Found::Different(why) => {
                        if bound(&dir) {
                            return Err(fault(
                                libc::EBUSY,
                                format!(
                                    "the gadget tree {} differs from the board file's ({why}) and is bound to a controller: not remade while bound",
                                    g.dir_name()
                                ),
                            ));
                        }
                        Outcome::Remade
                    }
                }
            }
            Ok(_) => {
                return Err(fault(
                    libc::EEXIST,
                    format!("{} is there and is not a directory", g.dir_name()),
                ));
            }
        };
        make(&g, &plan, owner, functions, system, udc)?;
        drop(held);
        match outcome {
            Outcome::TakenOver => {
                self.note_origin(udc_name, "taken over");
                self.note_let_go(udc_name, "taken over");
            }
            Outcome::Remade => {
                self.note_origin(udc_name, "remade");
                self.note_let_go(udc_name, "remade");
            }
            Outcome::Made | Outcome::Adopted => {}
        }
        Ok(outcome)
    }

    /// Makes the gadget for a helper that keeps root, or adopts the one that is there: as
    /// [`super::prepare_for_helper`], on trees that stay open between calls. `UDC` stays root's.
    ///
    /// # Errors
    ///
    /// As [`super::prepare`], and a tree that is not ours (`EEXIST`) or differs while bound (`EBUSY`).
    pub fn setup_for_helper(
        &mut self,
        udc_name: &str,
        owner: Owner,
        descriptors: &[u8],
        functions: &[GadgetFunction],
        system: &mut dyn Mounts,
    ) -> io::Result<Outcome> {
        self.setup(udc_name, owner, descriptors, functions, system, Udc::Root)
    }

    /// Whether the gadget's tree has a USB serial string: any language directory under `strings`
    /// that holds a non-empty `serialnumber`. Read from the tree itself, so an adopted tree is
    /// told as it is, whatever the settings now say.
    ///
    /// # Errors
    ///
    /// The name is not a controller's, or the gadget's directory cannot be opened.
    pub fn serial_string(&mut self, udc_name: &str) -> io::Result<super::SerialString> {
        let g = self.gadget(udc_name)?;
        let dir = g.gadgets.open_dir(&g.dir_name())?;
        let strings = match dir.open_dir("strings") {
            Ok(strings) => strings,
            Err(_) => return Ok(super::SerialString::Absent),
        };
        let any = directories(&dir, "strings").iter().any(|language| {
            strings
                .open_dir(language)
                .ok()
                .and_then(|d| read_text(&d, "serialnumber"))
                .is_some_and(|text| !text.is_empty())
        });
        Ok(if any {
            super::SerialString::Set
        } else {
            super::SerialString::Absent
        })
    }

    /// Writes the controller's name to the gadget's `UDC`.
    ///
    /// **No network function is bound twice.** The kernel's NCM function registers its network
    /// device on its first bind, and a registration that fails leaves that device in a state the
    /// kernel's `register_netdevice` takes for a fatal error the next time
    /// (`BUG_ON(dev->reg_state != NETREG_UNINITIALIZED)`, a panic): a second bind of the same
    /// function instance takes the board down. So when a bind fails and the tree has an NCM
    /// function, that function is removed at once and a fresh one made in its place, linked in
    /// the board file's order, and the bind is tried again on it, all here: the FunctionFS
    /// function and its session are not touched, so a holder of the endpoints keeps them.
    ///
    /// On 7.3 a controller device that went away and came back after an NCM device was registered
    /// under it makes every NCM registration fail for the rest of the boot (`docs/hardware-status.md`,
    /// "NCM after the controller came back"). A bind that fails on the fresh function too, and
    /// works without it, is taken for that state: the controller's gadget is bound without its
    /// network function, then and at every bind after (the function a setup makes again is
    /// removed before the write, never bound), so its other functions still reach the host. The
    /// caller tells it from the tree: the network function is gone after a bind that worked
    /// ([`super::NETWORK_REFUSED`]). A bind that fails without it as well leaves a fresh network
    /// function in the tree, never bound, and returns the first error.
    ///
    /// # Errors
    ///
    /// The name is not a controller's, or the write fails (`ENOENT` when the gadget is not made).
    pub fn bind(&mut self, udc_name: &str) -> io::Result<()> {
        let noted = self
            .let_go
            .iter()
            .find(|(name, _, _)| name == udc_name)
            .map(|(_, since, path)| (*since, *path));
        let result = self.bind_written(udc_name);
        // Told when it follows a let-go: the daemon kept `UDC` clear for [`REBIND_GAP`] first.
        if let (Ok(()), Some((since, path))) = (&result, noted) {
            self.let_go.retain(|(name, _, _)| name != udc_name);
            // What the setup did with a tree it found says more than `unbound`.
            let found = self
                .origin
                .iter()
                .position(|(name, _)| name == udc_name)
                .map(|at| self.origin.swap_remove(at).1);
            let path = found.unwrap_or(path);
            self.rebind = Some((
                udc_name.to_string(),
                Rebind {
                    path,
                    clear_for: since.elapsed(),
                },
            ));
        }
        result
    }

    /// The bind itself: see [`Trees::bind`].
    fn bind_written(&mut self, udc_name: &str) -> io::Result<()> {
        let functions = self
            .functions
            .iter()
            .find(|(name, _)| name == udc_name)
            .map(|(_, functions)| functions.clone());
        let was_refused = self.network_refused.iter().any(|name| name == udc_name);
        #[cfg(test)]
        let refuses = self.refuses;
        let g = self.gadget(udc_name)?;
        let dir = g.gadgets.open_dir(&g.dir_name())?;
        let controller = g.name.clone();
        let order = function_dirs(&controller, functions.as_deref().unwrap_or(&[]));
        let network = functions
            .iter()
            .flatten()
            .find_map(|function| match function {
                GadgetFunction::Ncm { .. } => Some(*function),
                _ => None,
            });
        let write = || {
            #[cfg(test)]
            if refuses.is_some_and(|refuses| refuses(&dir)) {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            attribute(&dir, "UDC", &controller)
        };
        if was_refused {
            if has_network(&dir) {
                remove_function(&dir, NCM_DIR);
            }
            return write();
        }
        let Err(error) = write() else {
            return Ok(());
        };
        if !has_network(&dir) {
            return Err(error);
        }
        let _ = dir.write("UDC", b"\n");
        // Nothing to make a fresh one from (a tree no setup of these trees made): the function
        // goes, and the next setup makes it again.
        let Some(network) = network else {
            remove_function(&dir, NCM_DIR);
            return Err(error);
        };
        if renew_network(&dir, &network, &order).is_err() || write().is_ok() {
            return if has_network(&dir) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let _ = dir.write("UDC", b"\n");
        relink_without_network(&dir, &order);
        if write().is_ok() {
            self.network_refused.push(udc_name.to_string());
            return Ok(());
        }
        let _ = dir.write("UDC", b"\n");
        let _ = renew_network(&dir, &network, &order);
        Err(error)
    }

    /// Writes an empty line to the gadget's `UDC`. A gadget that is not there, or not bound, is
    /// already off.
    ///
    /// # Errors
    ///
    /// The name is not a controller's.
    pub fn unbind(&mut self, udc_name: &str) -> io::Result<()> {
        let g = self.gadget(udc_name)?;
        let mut was_bound = false;
        if let Ok(dir) = g.gadgets.open_dir(&g.dir_name()) {
            was_bound = bound(&dir);
            let _ = dir.write("UDC", b"\n");
        }
        // Only a gadget that was bound has a host to tell: a tree that was never bound (a fresh
        // one at boot) is bound at once.
        if was_bound {
            self.note_let_go(udc_name, "unbound");
        }
        Ok(())
    }

    /// Writes `connect` or `disconnect` to the controller's `soft_connect`. With a gadget bound
    /// it goes through the core's `usb_gadget_disconnect` or `usb_gadget_connect` to the
    /// controller's pullup, so a host that held the gadget sees the device go (see
    /// [`REBIND_GAP`]).
    ///
    /// # Errors
    ///
    /// The name is not a controller's, the controller's directory cannot be found from its class
    /// entry ([`controller_dir`]), or the kernel refuses the write.
    pub fn soft_connect(&mut self, udc_name: &str, connect: bool) -> io::Result<()> {
        let root = self.roots.udc_class.clone();
        let g = self.gadget(udc_name)?;
        let controller = controller_dir(g.udc_class, &root, &g.name)?;
        controller.write(
            "soft_connect",
            if connect {
                b"connect".as_slice()
            } else {
                b"disconnect".as_slice()
            },
        )
    }

    /// Takes the gadget down: unbinds it, removes its configurations and functions, and unmounts
    /// its FunctionFS. A mount that is not there is not an error.
    ///
    /// # Errors
    ///
    /// The name is not a controller's, or the mount is there and cannot be unmounted.
    pub fn teardown(&mut self, udc_name: &str, system: &mut dyn Mounts) -> io::Result<()> {
        let g = self.gadget(udc_name)?;
        if let Ok(dir) = g.gadgets.open_dir(&g.dir_name()) {
            let _ = dir.write("UDC", b"\n");
            dismantle(&dir);
            remove_functions(&dir, &g.function_name(), false, false);
        }
        match g.mounts.open_handle(&g.name) {
            Ok(mount) => match system.unmount(&mount.proc_path()) {
                Err(error) if error.raw_os_error() != Some(libc::EINVAL) => Err(error),
                _ => Ok(()),
            },
            Err(_) => Ok(()),
        }
    }
}

/// Writes `value` and a newline to the attribute `file` of `dir`.
fn attribute(dir: &Dir, file: &str, value: &str) -> io::Result<()> {
    dir.write(file, format!("{value}\n").as_bytes())
}

/// Whether the gadget is bound to a controller: its `UDC` holds a name.
fn bound(dir: &Dir) -> bool {
    dir.read("UDC").is_ok_and(|text| {
        !String::from_utf8_lossy(&text)
            .trim_matches(['\n', '\0', ' '])
            .is_empty()
    })
}

/// What the tree there is, against what the description asks for.
enum Found {
    Same,
    /// The board file's tree, but its FunctionFS instance is mounted elsewhere or still has open
    /// files: a daemon before this one, in a mount namespace this one can't see, lent its
    /// endpoints, and a holder keeps them. Never remade or unbound while so.
    Held,
    /// Every part is of a kind this module makes; a value or a part differs. The first difference.
    Different(String),
    /// A part this module never makes.
    Foreign(String),
}

/// An attribute's text, without its newline and NULs. `None` when it cannot be read.
fn read_text(dir: &Dir, file: &str) -> Option<String> {
    dir.read(file).ok().map(|bytes| {
        String::from_utf8_lossy(&bytes)
            .trim_matches(['\n', '\0', ' '])
            .to_owned()
    })
}

/// A number as configfs spells one: decimal, or hex with `0x`.
fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Whether the attribute holds what the plan says, as text or as the same number.
fn holds(dir: &Dir, file: &str, want: &str) -> bool {
    let Some(have) = read_text(dir, file) else {
        return false;
    };
    have.eq_ignore_ascii_case(want.trim())
        || matches!((number(&have), number(want)), (Some(a), Some(b)) if a == b)
}

/// The function directory names the functions make, in the order given.
fn function_dirs(name: &str, functions: &[GadgetFunction]) -> Vec<String> {
    functions
        .iter()
        .map(|function| match function {
            GadgetFunction::Ffs => format!("ffs.{name}"),
            GadgetFunction::Ncm { .. } => NCM_DIR.to_owned(),
            GadgetFunction::Acm => ACM_DIR.to_owned(),
        })
        .collect()
}

/// `c.N`: a configuration name this module makes.
fn is_configuration_name(name: &str) -> bool {
    name.strip_prefix("c.")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn classify(
    g: &Gadget<'_>,
    dir: &Dir,
    plan: &Plan,
    functions: &[GadgetFunction],
    owner: Owner,
    system: &mut dyn Mounts,
) -> io::Result<Found> {
    // Parts that are not ours first: a tree with such a part is not the helper's, however close.
    let ours = [g.function_name(), NCM_DIR.to_owned(), ACM_DIR.to_owned()];
    if let Ok(entries) = dir.open_dir("functions").and_then(|d| d.names())
        && let Some(other) = entries.iter().find(|n| !ours.contains(n))
    {
        return Ok(Found::Foreign(format!("a function {other}")));
    }
    if let Ok(entries) = dir.open_dir("configs").and_then(|d| d.names())
        && let Some(other) = entries.iter().find(|n| !is_configuration_name(n))
    {
        return Ok(Found::Foreign(format!("a configuration {other}")));
    }
    let different = |why: &str| Ok(Found::Different(why.to_owned()));
    for (file, value) in &plan.identity {
        if !holds(dir, file, value) {
            return different(&format!("{file} is not {value}"));
        }
    }
    // Kernels before 5.12 have no `max_speed`; where it is there it must say the plan's.
    if dir.exists("max_speed") && !holds(dir, "max_speed", plan.max_speed) {
        return different("max_speed differs");
    }
    if let Some(language) = &plan.language {
        if let Ok(strings) = dir.open_dir("strings").and_then(|s| s.open_dir(language)) {
            for (file, value) in &plan.strings {
                if !holds(&strings, file, value) {
                    return different(&format!("string {file} differs"));
                }
            }
        } else if !plan.strings.is_empty() {
            return different("the strings are missing");
        }
    }
    let Ok(configs) = dir.open_dir("configs") else {
        return different("there is no configs directory");
    };
    if configs.names()? != [plan.configuration.clone()] {
        return different("the configurations differ");
    }
    let configuration = configs.open_dir(&plan.configuration)?;
    if !holds(&configuration, "MaxPower", &plan.max_power)
        || !holds(&configuration, "bmAttributes", &plan.attributes)
    {
        return different("the configuration's attributes differ");
    }
    if let (Some(language), Some(name)) = (&plan.language, &plan.configuration_name) {
        let named = configuration
            .open_dir("strings")
            .and_then(|s| s.open_dir(language));
        if !named.is_ok_and(|s| holds(&s, "configuration", name)) {
            return different("the configuration's name differs");
        }
    }
    // The links in the configuration, in the order the functions are listed (the order their names
    // sort in is not the order they were made in, so compare as sets).
    let expect = function_dirs(&g.name, functions);
    let mut links: Vec<String> = configuration
        .names()?
        .into_iter()
        .filter(|n| configuration.kind(n).is_ok_and(|k| k == Kind::Symlink))
        .collect();
    links.sort();
    let mut want = expect.clone();
    want.sort();
    if links != want {
        return different("the functions linked into the configuration differ");
    }
    let Ok(os_desc) = dir.open_dir("os_desc") else {
        return different("there is no os_desc");
    };
    for (file, value) in &plan.os_attributes {
        if !holds(&os_desc, file, value) {
            return different(&format!("os_desc {file} differs"));
        }
    }
    if !os_desc
        .kind(&plan.configuration)
        .is_ok_and(|k| k == Kind::Symlink)
    {
        return different("the OS descriptors do not name the configuration");
    }
    let Ok(function_root) = dir.open_dir("functions") else {
        return different("there is no functions directory");
    };
    let mut have = function_root.names()?;
    have.sort();
    if have != want {
        return different("the functions differ");
    }
    for function in functions {
        if matches!(function, GadgetFunction::Ncm { .. }) {
            let ncm = function_root.open_dir(NCM_DIR)?;
            if let Err(why) = network_is(&ncm, function) {
                return different(why);
            }
        }
    }
    if functions.iter().any(|f| matches!(f, GadgetFunction::Ffs))
        && !g
            .mounts
            .open_handle(&g.name)
            .is_ok_and(|m| system.mounted(&m))
    {
        // Not mounted where this process can see it. The service manager gives each start of the
        // daemon its own mount namespace, so the mount a daemon before this one made went with it,
        // while the instance lives on as long as a holder keeps its endpoints open. f_fs refuses a
        // second mount of such an instance (EBUSY), which is how it is told: it is never remade or
        // unbound under the holder. A mount that is taken is this tree's, made here: the same tree.
        return match mount_ffs(g, owner, system) {
            Ok(()) => Ok(Found::Same),
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => Ok(Found::Held),
            Err(error) => different(&format!("the FunctionFS mount is not there ({error})")),
        };
    }
    Ok(Found::Same)
}

/// Whether the network function `ncm` is the one `network` describes: its addresses, and a name the
/// template gave or would give (an `ifname` the kernel has rewritten, `lemnos%d` become `lemnos0`, is
/// the kernel's). Otherwise what differs.
fn network_is(ncm: &Dir, network: &GadgetFunction) -> Result<(), &'static str> {
    let GadgetFunction::Ncm {
        host,
        device,
        ifname,
    } = network
    else {
        return Err("the function is not a network function");
    };
    if !holds(ncm, "host_addr", &mac_text(host)) || !holds(ncm, "dev_addr", &mac_text(device)) {
        return Err("the NCM addresses differ");
    }
    if let Some(name) = ifname {
        let has = read_text(ncm, "ifname").unwrap_or_default();
        let template = name.as_str();
        let stem = template.split('%').next().unwrap_or(template);
        if !has.starts_with(stem) {
            return Err("the NCM interface name differs");
        }
    }
    Ok(())
}

/// Whether the gadget's tree has the network function `functions` asks for, as it asks for it: a
/// remake keeps it. The kernel's network device lives as long as the function's directory (f_ncm
/// registers it at the first bind and unregisters it only when the directory goes), so a function
/// kept is the same interface, name and index, to everything on the board that matched it; a
/// function made again is a new device the kernel names afresh.
fn network_kept(gadget: &Dir, functions: &[GadgetFunction]) -> bool {
    let Some(network) = functions
        .iter()
        .find(|f| matches!(f, GadgetFunction::Ncm { .. }))
    else {
        return false;
    };
    gadget
        .open_dir("functions")
        .and_then(|functions| functions.open_dir(NCM_DIR))
        .is_ok_and(|ncm| network_is(&ncm, network).is_ok())
}

/// Removes `name` of `parent` and what is under it: files and links, and directories the setup made
/// inside (the NCM function's `os_desc/interface.ncm`, which configfs will not remove with its
/// parent), deepest first. In configfs the attribute files and default groups go with their
/// directory and their removal is refused, which is not an error here. Never fails: what is not
/// there is already gone.
fn remove_dir_of(parent: &Dir, name: &str) {
    if let Ok(dir) = parent.open_dir(name) {
        for entry in dir.names().unwrap_or_default() {
            match dir.kind(&entry) {
                Ok(Kind::Dir) => remove_dir_of(&dir, &entry),
                Ok(_) => {
                    let _ = dir.remove_file(&entry);
                }
                Err(_) => {}
            }
        }
    }
    let _ = parent.remove_dir(name);
}

/// The directories of `parent`, not following links.
fn directories(parent: &Dir, name: &str) -> Vec<String> {
    let Ok(dir) = parent.open_dir(name) else {
        return Vec::new();
    };
    dir.names()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| dir.kind(entry).is_ok_and(|kind| kind == Kind::Dir))
        .collect()
}

/// Whether the gadget's tree has its network function.
fn has_network(gadget: &Dir) -> bool {
    gadget
        .open_dir("functions")
        .is_ok_and(|functions| functions.kind(NCM_DIR).is_ok_and(|kind| kind == Kind::Dir))
}

/// The network function removed, a fresh one made from `network`, and every configuration's
/// links made again in `order` (the board file's): the unbound gadget's tree as a setup makes it,
/// with a network function no bind has seen.
fn renew_network(gadget: &Dir, network: &GadgetFunction, order: &[String]) -> io::Result<()> {
    remove_function(gadget, NCM_DIR);
    let functions = gadget.open_dir("functions")?;
    let GadgetFunction::Ncm {
        host,
        device,
        ifname,
    } = network
    else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    make_network(&functions, host, device, ifname.as_ref())?;
    relink(gadget, order, true)
}

/// Every configuration's links made again in `order`, the network function left out.
fn relink_without_network(gadget: &Dir, order: &[String]) {
    remove_function(gadget, NCM_DIR);
    let _ = relink(gadget, order, false);
}

fn relink(gadget: &Dir, order: &[String], network: bool) -> io::Result<()> {
    let functions = gadget.open_dir("functions")?;
    let functions_path = functions.proc_path();
    let configs = gadget.open_dir("configs")?;
    for configuration in directories(gadget, "configs") {
        let configuration = configs.open_dir(&configuration)?;
        remove_links(&configuration);
        for name in order {
            if (network || name != NCM_DIR) && functions.kind(name).is_ok_and(|k| k == Kind::Dir) {
                configuration.symlink(&format!("{}/{name}", functions_path.display()), name)?;
            }
        }
    }
    Ok(())
}

/// Unlinks function `name` from every configuration and removes its directory.
fn remove_function(gadget: &Dir, name: &str) {
    if let Ok(configs) = gadget.open_dir("configs") {
        for configuration in directories(gadget, "configs") {
            if let Ok(configuration) = configs.open_dir(&configuration) {
                let _ = configuration.remove_file(name);
            }
        }
    }
    if let Ok(functions) = gadget.open_dir("functions") {
        remove_dir_of(&functions, name);
    }
}

/// Removes every link of `dir`.
fn remove_links(dir: &Dir) {
    for entry in dir.names().unwrap_or_default() {
        if dir.kind(&entry).is_ok_and(|kind| kind == Kind::Symlink) {
            let _ = dir.remove_file(&entry);
        }
    }
}

/// Removes every configuration and strings directory of the gadget, and the links in them.
fn dismantle(gadget: &Dir) {
    // The configurations the OS descriptors name, which configfs won't remove while linked.
    if let Ok(os_desc) = gadget.open_dir("os_desc") {
        remove_links(&os_desc);
    }
    if let Ok(configs) = gadget.open_dir("configs") {
        for name in directories(gadget, "configs") {
            let Ok(configuration) = configs.open_dir(&name) else {
                continue;
            };
            // The functions linked in, then the strings, then the configuration.
            remove_links(&configuration);
            for strings in directories(&configuration, "strings") {
                if let Ok(root) = configuration.open_dir("strings") {
                    remove_dir_of(&root, &strings);
                }
            }
            // In configfs `strings` is part of the configuration; in a stand-in tree it is a
            // directory to remove.
            let _ = configuration.remove_dir("strings");
            remove_dir_of(&configs, &name);
        }
    }
    if let Ok(strings) = gadget.open_dir("strings") {
        for name in directories(gadget, "strings") {
            remove_dir_of(&strings, &name);
        }
    }
}

/// Removes the function directories the setup makes, after the configurations are gone. With
/// `reuse` the FunctionFS directory stays, with its mount; with `keep_network` the network
/// function stays, with its network device ([`network_kept`]).
fn remove_functions(gadget: &Dir, ffs: &str, reuse: bool, keep_network: bool) {
    let Ok(functions) = gadget.open_dir("functions") else {
        return;
    };
    let mut names: Vec<&str> = vec![ACM_DIR];
    if !reuse {
        names.push(ffs);
    }
    if !keep_network {
        names.push(NCM_DIR);
    }
    for name in names {
        remove_dir_of(&functions, name);
    }
}

/// Mounts the gadget's FunctionFS instance on its mount point, owned by the daemon's user.
fn mount_ffs(g: &Gadget<'_>, owner: Owner, system: &mut dyn Mounts) -> io::Result<()> {
    g.mounts.ensure_dir(&g.name, 0o755)?;
    let mount = g.mounts.open_handle(&g.name)?;
    let options = format!("uid={},gid={},rmode=0500,fmode=0600", owner.uid, owner.gid);
    system.functionfs(&g.name, &mount.proc_path(), &options)
}

/// Makes the tree, in the order of the module docs of the gadget.
/// The network function, made in `functions` with its addresses, OS descriptor and name.
fn make_network(
    functions: &Dir,
    host: &[u8; 6],
    device: &[u8; 6],
    ifname: Option<&IfName>,
) -> io::Result<()> {
    functions.make_dir(NCM_DIR, 0o755)?;
    let dir = functions.open_dir(NCM_DIR)?;
    attribute(&dir, "host_addr", &mac_text(host))?;
    attribute(&dir, "dev_addr", &mac_text(device))?;
    // The NCM interface's OS descriptor, a group configfs makes for the function. Written raw:
    // the store takes up to 8 bytes and would keep a newline. Best effort: the link does not
    // depend on it, and the harness checks it is there.
    let _ = (|| -> io::Result<()> {
        dir.ensure_dir("os_desc", 0o755)?;
        let os = dir.open_dir("os_desc")?;
        os.ensure_dir("interface.ncm", 0o755)?;
        os.open_dir("interface.ncm")?
            .write("compatible_id", id_text(&ms_os::NCM_COMPATIBLE_ID))
    })();
    if let Some(name) = ifname {
        // Only taken while the function is unbound, which it is here.
        attribute(&dir, "ifname", name.as_str()).map_err(ifname_refused)?;
    }
    Ok(())
}

fn make(
    g: &Gadget<'_>,
    plan: &Plan,
    owner: Owner,
    functions: &[GadgetFunction],
    system: &mut dyn Mounts,
    udc: Udc,
) -> io::Result<()> {
    // A FunctionFS mount that is already there, with its ep0 in it, belongs to a holder that may
    // still have copies of the endpoints, or to an earlier daemon. It is reused: never unmounted or
    // remounted under a holder (the kernel refuses that with EBUSY, and it would drop the holder's
    // descriptors). Holds end on their own when their process goes; see `endpoints_ended`.
    let reuse = g
        .mounts
        .open_handle(&g.name)
        .is_ok_and(|m| system.mounted(&m));
    let mut keep_network = false;
    if let Ok(dir) = g.gadgets.open_dir(&g.dir_name()) {
        let _ = dir.write("UDC", b"\n");
        keep_network = network_kept(&dir, functions);
        dismantle(&dir);
        if !reuse && let Ok(mount) = g.mounts.open_handle(&g.name) {
            // `unmount` of a point that isn't mounted fails, which is the state a fresh image is in.
            let _ = system.unmount(&mount.proc_path());
        }
        remove_functions(&dir, &g.function_name(), reuse, keep_network);
    }

    g.gadgets.ensure_dir(&g.dir_name(), 0o755)?;
    let gadget = g.gadgets.open_dir(&g.dir_name())?;
    for (file, value) in &plan.identity {
        attribute(&gadget, file, value)?;
    }
    // Kernels before 5.12 have no `max_speed`; they run at what the controller offers.
    if gadget.exists("max_speed") {
        let _ = gadget.write("max_speed", format!("{}\n", plan.max_speed).as_bytes());
    }
    if let Some(language) = &plan.language
        && !plan.strings.is_empty()
    {
        gadget.ensure_dir("strings", 0o755)?;
        let root = gadget.open_dir("strings")?;
        root.ensure_dir(language, 0o755)?;
        let strings = root.open_dir(language)?;
        for (file, value) in &plan.strings {
            attribute(&strings, file, value)?;
        }
    }
    gadget.ensure_dir("configs", 0o755)?;
    let configs = gadget.open_dir("configs")?;
    configs.ensure_dir(&plan.configuration, 0o755)?;
    let configuration = configs.open_dir(&plan.configuration)?;
    attribute(&configuration, "MaxPower", &plan.max_power)?;
    attribute(&configuration, "bmAttributes", &plan.attributes)?;
    // The OS descriptors belong to the configuration linked under `os_desc` below. Setting
    // `use` first is what makes the kernel answer string 0xEE for the device.
    gadget.ensure_dir("os_desc", 0o755)?;
    let os_desc = gadget.open_dir("os_desc")?;
    for (file, value) in &plan.os_attributes {
        attribute(&os_desc, file, value)?;
    }
    if let (Some(language), Some(name)) = (&plan.language, &plan.configuration_name) {
        configuration.ensure_dir("strings", 0o755)?;
        let root = configuration.open_dir("strings")?;
        root.ensure_dir(language, 0o755)?;
        attribute(&root.open_dir(language)?, "configuration", name)?;
    }

    // The functions, each made in configfs, FunctionFS last of the three for its mount.
    gadget.ensure_dir("functions", 0o755)?;
    let functions_dir = gadget.open_dir("functions")?;
    let mut links = Vec::with_capacity(functions.len());
    for function in functions {
        match function {
            GadgetFunction::Ffs => {
                if !reuse {
                    functions_dir.make_dir(&g.function_name(), 0o755)?;
                    mount_ffs(g, owner, system)?;
                }
                links.push(g.function_name());
            }
            GadgetFunction::Ncm {
                host,
                device,
                ifname,
            } => {
                if !keep_network {
                    make_network(&functions_dir, host, device, ifname.as_ref())?;
                }
                links.push(NCM_DIR.to_string());
            }
            GadgetFunction::Acm => {
                functions_dir.make_dir(ACM_DIR, 0o755)?;
                links.push(ACM_DIR.to_string());
            }
        }
    }
    // A link's target is a path the kernel resolves (configfs takes it as the process would open
    // it, not relative to the link), so it names the directory through its descriptor.
    let functions_path = functions_dir.proc_path();
    for name in &links {
        configuration.symlink(&format!("{}/{name}", functions_path.display()), name)?;
    }
    let configs_path = configs.proc_path();
    os_desc.symlink(
        &format!("{}/{}", configs_path.display(), plan.configuration),
        &plan.configuration,
    )?;

    // The daemon writes the controller's name here, and nothing else in the gadget.
    if udc == Udc::Owner {
        gadget.set_owner("UDC", owner.uid, owner.gid)?;
        gadget.set_mode("UDC", 0o600)?;
    }
    Ok(())
}

/// The controller `name`'s own directory, from its entry in the controllers' directory (`root`,
/// opened as `class`). The kernel's entry is a relative link into `/sys/devices`: it is read, not
/// followed. Only `..` may lead it, every part after is one plain entry name, and the last is the
/// controller's own name; the directory it names is opened with no link as its last part. An entry
/// that is a directory (a stand-in tree) is opened as it is.
///
/// # Errors
///
/// No such entry (`ENOENT`), or a link of another shape (`EINVAL`).
pub(super) fn controller_dir(class: &Dir, root: &std::path::Path, name: &str) -> io::Result<Dir> {
    match class.kind(name)? {
        Kind::Dir => class.open_dir(name),
        Kind::Symlink => {
            let link = class.read_link(name)?;
            let mut path = root.to_path_buf();
            let mut down = false;
            let mut last = None;
            for part in link.split('/') {
                if part == ".." && !down {
                    if !path.pop() {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL));
                    }
                    continue;
                }
                lemnos_linux_sys::confined::check_name(part)?;
                down = true;
                path.push(part);
                last = Some(part);
            }
            if last != Some(name) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            Dir::open_path(&path)
        }
        _ => Err(io::Error::from_raw_os_error(libc::ENOENT)),
    }
}
