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
//! - a part this module never makes (a function other than `ffs.NAME`, `ncm.usb0` or `acm.GS0`, a
//!   configuration not named `c.N`): the tree is not the helper's. It is left alone and the setup
//!   fails with `EEXIST`, naming the part.

use std::io;

use lemnos_hal::usb_descriptors::{GadgetFunction, UsbDescriptors};
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
}

/// The directories a gadget is made in. Opened once where they can be, and again at the next setup
/// where they could not (configfs not mounted yet, the mount point's parent not there yet).
pub struct Trees {
    roots: PathsOf,
    verify_configfs: bool,
    configfs: Option<Dir>,
    mounts: Option<Dir>,
    udc_class: Option<Dir>,
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
        }
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
        let g = self.gadget(udc_name)?;
        if !g.udc_class.exists(&g.name) {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        let outcome = match g.gadgets.kind(&g.dir_name()) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Outcome::Made,
            Err(error) => return Err(error),
            Ok(Kind::Dir) => {
                let dir = g.gadgets.open_dir(&g.dir_name())?;
                match classify(&g, &dir, &plan, functions, system)? {
                    Found::Same => return Ok(Outcome::Adopted),
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
    /// # Errors
    ///
    /// The name is not a controller's, or the write fails (`ENOENT` when the gadget is not made).
    pub fn bind(&mut self, udc_name: &str) -> io::Result<()> {
        let g = self.gadget(udc_name)?;
        let dir = g.gadgets.open_dir(&g.dir_name())?;
        attribute(&dir, "UDC", &g.name)
    }

    /// Writes an empty line to the gadget's `UDC`. A gadget that is not there, or not bound, is
    /// already off.
    ///
    /// # Errors
    ///
    /// The name is not a controller's.
    pub fn unbind(&mut self, udc_name: &str) -> io::Result<()> {
        let g = self.gadget(udc_name)?;
        if let Ok(dir) = g.gadgets.open_dir(&g.dir_name()) {
            let _ = dir.write("UDC", b"\n");
        }
        Ok(())
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
            remove_functions(&dir, &g.function_name(), false);
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
    system: &dyn Mounts,
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
        if let GadgetFunction::Ncm {
            host,
            device,
            ifname,
        } = function
        {
            let ncm = function_root.open_dir(NCM_DIR)?;
            if !holds(&ncm, "host_addr", &mac_text(host))
                || !holds(&ncm, "dev_addr", &mac_text(device))
            {
                return different("the NCM addresses differ");
            }
            if let Some(name) = ifname {
                // An `ifname` the kernel has rewritten (`lemnos%d` became `lemnos0`) is the kernel's.
                let has = read_text(&ncm, "ifname").unwrap_or_default();
                let template = name.as_str();
                let stem = template.split('%').next().unwrap_or(template);
                if !has.starts_with(stem) {
                    return different("the NCM interface name differs");
                }
            }
        }
    }
    if functions.iter().any(|f| matches!(f, GadgetFunction::Ffs))
        && !g
            .mounts
            .open_handle(&g.name)
            .is_ok_and(|m| system.mounted(&m))
    {
        return different("the FunctionFS mount is not there");
    }
    Ok(Found::Same)
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
/// `reuse` the FunctionFS directory stays, with its mount.
fn remove_functions(gadget: &Dir, ffs: &str, reuse: bool) {
    let Ok(functions) = gadget.open_dir("functions") else {
        return;
    };
    let names: Vec<&str> = if reuse {
        vec![NCM_DIR, ACM_DIR]
    } else {
        vec![ffs, NCM_DIR, ACM_DIR]
    };
    for name in names {
        remove_dir_of(&functions, name);
    }
}

/// Makes the tree, in the order of the module docs of the gadget.
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
    if let Ok(dir) = g.gadgets.open_dir(&g.dir_name()) {
        let _ = dir.write("UDC", b"\n");
        dismantle(&dir);
        if !reuse && let Ok(mount) = g.mounts.open_handle(&g.name) {
            // `unmount` of a point that isn't mounted fails, which is the state a fresh image is in.
            let _ = system.unmount(&mount.proc_path());
        }
        remove_functions(&dir, &g.function_name(), reuse);
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
                    g.mounts.ensure_dir(&g.name, 0o755)?;
                    let mount = g.mounts.open_handle(&g.name)?;
                    let options =
                        format!("uid={},gid={},rmode=0500,fmode=0600", owner.uid, owner.gid);
                    system.functionfs(&g.name, &mount.proc_path(), &options)?;
                }
                links.push(g.function_name());
            }
            GadgetFunction::Ncm {
                host,
                device,
                ifname,
            } => {
                functions_dir.make_dir(NCM_DIR, 0o755)?;
                let dir = functions_dir.open_dir(NCM_DIR)?;
                attribute(&dir, "host_addr", &mac_text(host))?;
                attribute(&dir, "dev_addr", &mac_text(device))?;
                // The NCM interface's OS descriptor, a group configfs makes for the function.
                // Written raw: the store takes up to 8 bytes and would keep a newline. Best
                // effort: the link does not depend on it, and the harness checks it is there.
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
