//! Driver packages: a driver as a compiled file the service loads after the image is built,
//! instead of a crate compiled into it.
//!
//! A package is a shared object that exports one symbol, a
//! [`PackageDescriptor`](lemnos_abi::package::PackageDescriptor). A driver crate becomes one with
//! `lemnos_core::export_package!(TheDriver)`. This crate finds packages, checks them, loads them
//! and hands the host a [`DriverEntry`] for each. From there a package's driver is run by the
//! same code as a built-in one: the same lane, panic boundary, call deadline, backoff, rebind,
//! health and events. `docs/writing-a-driver.md` shows a whole package.
//!
//! # What may be loaded
//!
//! Lemnos loads no vendor library and no vendor runtime, here or anywhere. The dynamic loader is
//! used for one thing: Lemnos's own driver packages. A file is handed to the loader only after
//! its bytes show it is a whole shared object for this machine that exports the package entry,
//! with the package magic and a version and layout hash this host was built with
//! ([`PACKAGE_VERSION`](lemnos_abi::package::PACKAGE_VERSION)). A file that is anything else is
//! refused with nothing of it run or mapped, initialisers included. After loading, the same
//! fields are checked again in memory, and only then is the package's `register` called. The
//! loader ([`lemnos_abi::load`]) can look up the package entry and nothing else.
//!
//! # Signed and open
//!
//! [`Mode`] is the one switch.
//!
//! - [`Mode::Open`]: any package in the directory runs. Give it a driver and it runs.
//! - [`Mode::Signed`]: a package runs only if the file `<package>.sig` beside it holds an
//!   Ed25519 signature (64 raw bytes) of the whole package file by one of the
//!   [`TrustedKeys`]. The keys come from the service's configuration, never from a package.
//!   No signature, a wrong one, or no trusted key at all: refused.
//!
//! In both modes the file is read once into sealed memory ([`lemnos_abi::load::seal`]), which
//! nothing can change afterwards. The signature is checked over the bytes read back from that
//! memory, the format checks run on the same bytes, and the loader maps that same memory. The
//! path is never opened a second time, so there is no moment between the check and the load in
//! which the file could be swapped.
//!
//! # What this does not do
//!
//! A package runs inside the service's process. Its panics and hangs are contained exactly as a
//! built-in driver's are. Memory corruption is not and cannot be: a package that writes where it
//! should not can break anything in the service. Signed mode says who built a package, not that
//! it is correct.
//!
//! A package is never unloaded. Its descriptor, tables and strings are used for as long as the
//! process lives, and a driver from it may be bound again at any time. A package refused after
//! it was loaded (its registration failed, or its driver's name was taken) stays mapped and is
//! never called again.
//!
//! What a package's driver can be given today: other devices, and I2C buses. Any other kind of
//! bus is `NOT_SUPPORTED` when it binds.

mod adapter;
mod elf;
mod sign;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use lemnos_abi::AbiVersion;
use lemnos_abi::load::{LoadError, load, seal};
use lemnos_abi::package::{DriverTable, Out, PACKAGE_MAGIC, PACKAGE_VERSION, PackageDriver, Slice};
use lemnos_core::{Manifest, Status};
use lemnos_host::DriverEntry;

pub use sign::{KEY_LEN, KeyError, SIGNATURE_LEN, TrustedKeys};

/// The largest file taken for a package. A driver is nowhere near it.
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

/// What a package file's name ends with. A scan looks at nothing else.
pub const PACKAGE_SUFFIX: &str = ".so";
/// What is added to a package file's name to name its signature.
pub const SIGNATURE_SUFFIX: &str = ".sig";

/// Which packages run.
#[derive(Debug, Clone)]
pub enum Mode {
    /// Any package.
    Open,
    /// Only packages signed by one of these keys.
    Signed(TrustedKeys),
}

/// Why a file was not loaded as a package. One file's refusal never stops another's load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The file, or in signed mode its signature, could not be read.
    Unreadable(String),
    /// Larger than [`MAX_PACKAGE_BYTES`].
    TooLarge,
    /// Signed mode, and there is no signature file beside it.
    Unsigned,
    /// Signed mode, and the signature is not a trusted key's signature of these bytes.
    BadSignature,
    /// Not a Lemnos driver package, and why: not a shared object, one for another machine, one
    /// with no package entry, or one whose entry lacks the package magic.
    NotAPackage(&'static str),
    /// A shared object whose tables point past the end of the file.
    Truncated,
    /// A package built for another version of the package boundary.
    AbiMismatch {
        package: (u16, u16),
        host: (u16, u16),
    },
    /// A package of this version whose types are laid out differently from this host's.
    LayoutMismatch,
    /// The dynamic loader refused it, in its words.
    LoadFailed(String),
    /// Its `register` panicked.
    RegistrationPanicked,
    /// Its `register` failed with this status.
    RegistrationFailed(Status),
    /// What it registered is not a usable driver, and why.
    BadManifest(&'static str),
    /// Its driver has the name of a built-in driver or of a package loaded before it.
    DuplicateName(String),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "can't be read: {why}"),
            Self::TooLarge => write!(f, "larger than {MAX_PACKAGE_BYTES} bytes"),
            Self::Unsigned => write!(f, "unsigned: signed mode, and it has no signature file"),
            Self::BadSignature => {
                write!(
                    f,
                    "bad signature: not signed by a trusted key, or changed since"
                )
            }
            Self::NotAPackage(why) => write!(f, "not a driver package: {why}"),
            Self::Truncated => write!(f, "truncated: the file ends before its own tables do"),
            Self::AbiMismatch { package, host } => write!(
                f,
                "ABI mismatch: built for package ABI {}.{}, this host has {}.{}",
                package.0, package.1, host.0, host.1
            ),
            Self::LayoutMismatch => {
                write!(
                    f,
                    "layout mismatch: its types are laid out differently from this host's"
                )
            }
            Self::LoadFailed(why) => write!(f, "the dynamic loader refused it: {why}"),
            Self::RegistrationPanicked => write!(f, "its registration panicked"),
            Self::RegistrationFailed(status) => write!(f, "its registration failed: {status:?}"),
            Self::BadManifest(why) => write!(f, "bad manifest: {why}"),
            Self::DuplicateName(name) => write!(f, "duplicate driver name {name:?}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A loaded package: one driver the host can start.
#[derive(Clone)]
pub struct Package {
    path: PathBuf,
    manifest: &'static Manifest,
    entry: DriverEntry,
}

impl Package {
    /// The file it was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its driver's name, which a board file names it by.
    pub fn name(&self) -> &'static str {
        self.manifest.name
    }

    pub fn manifest(&self) -> &'static Manifest {
        self.manifest
    }

    /// What to give the host, beside the built-in drivers' entries.
    pub fn entry(&self) -> DriverEntry {
        self.entry
    }
}

impl core::fmt::Debug for Package {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Package")
            .field("path", &self.path)
            .field("name", &self.manifest.name)
            .finish()
    }
}

/// A file a scan refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub path: PathBuf,
    pub why: Refusal,
}

/// Loads packages, by path or by scanning a directory, and keeps what it loaded.
///
/// A driver's name is taken once. Names given to [`Loader::reserve`] (the built-in drivers')
/// always win: a package with one of them is refused. Between packages the first loaded wins,
/// and a scan loads in order of file name.
#[derive(Debug)]
pub struct Loader {
    mode: Mode,
    names: BTreeSet<String>,
    packages: Vec<Package>,
    refused: Vec<Refused>,
}

impl Loader {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            names: BTreeSet::new(),
            packages: Vec::new(),
            refused: Vec::new(),
        }
    }

    /// Driver names no package may take: the built-in drivers'.
    pub fn reserve<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        self.names.extend(names.into_iter().map(str::to_owned));
    }

    /// Loads the package at `path`.
    pub fn load(&mut self, path: &Path) -> Result<&Package, Refusal> {
        let package = open(path, &self.mode)?;
        if !self.names.insert(package.name().to_owned()) {
            return Err(Refusal::DuplicateName(package.name().to_owned()));
        }
        self.packages.push(package);
        Ok(&self.packages[self.packages.len() - 1])
    }

    /// Loads every file in `dir` whose name ends in [`PACKAGE_SUFFIX`], in order of name, not
    /// looking into directories inside it. A file that is refused is recorded in
    /// [`Loader::refused`] and the scan goes on. The error is the directory's own: it can't be
    /// read.
    pub fn scan(&mut self, dir: &Path) -> io::Result<()> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let named = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(PACKAGE_SUFFIX));
            if named && path.is_file() {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            if let Err(why) = self.load(&path) {
                self.refused.push(Refused { path, why });
            }
        }
        Ok(())
    }

    /// Every package loaded so far.
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    /// Every file a scan refused so far, and why.
    pub fn refused(&self) -> &[Refused] {
        &self.refused
    }

    /// The loaded packages' entries, to give the host beside the built-in drivers'.
    pub fn entries(&self) -> Vec<DriverEntry> {
        self.packages.iter().map(Package::entry).collect()
    }
}

/// How many things (devices and buses) packages' drivers hold right now, across the process.
/// A driver gives back what it holds when the host lets go of it.
pub fn resources_held() -> usize {
    adapter::resources_held()
}

fn unreadable(error: io::Error) -> Refusal {
    Refusal::Unreadable(error.to_string())
}

/// Whether a descriptor's first fields are a package's that this host can run.
fn check(magic: u64, version: &AbiVersion) -> Result<(), Refusal> {
    if magic != PACKAGE_MAGIC {
        return Err(Refusal::NotAPackage("its entry lacks the package magic"));
    }
    let host = PACKAGE_VERSION;
    if (version.struct_size as usize) < size_of::<AbiVersion>()
        || version.major != host.major
        || version.minor > host.minor
    {
        return Err(Refusal::AbiMismatch {
            package: (version.major, version.minor),
            host: (host.major, host.minor),
        });
    }
    if version.layout != host.layout {
        return Err(Refusal::LayoutMismatch);
    }
    Ok(())
}

/// The signature file beside a package.
fn signature_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(SIGNATURE_SUFFIX);
    PathBuf::from(name)
}

fn open(path: &Path, mode: &Mode) -> Result<Package, Refusal> {
    // The one read of the file. From here on only the sealed copy is looked at.
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(unreadable)?
        .take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(Refusal::TooLarge);
    }
    let sealed = seal(&bytes).map_err(unreadable)?;
    let copy = File::from(sealed.try_clone().map_err(unreadable)?);
    copy.read_exact_at(&mut bytes, 0).map_err(unreadable)?;
    drop(copy);

    if let Mode::Signed(keys) = mode {
        let signature = match std::fs::read(signature_path(path)) {
            Ok(signature) => signature,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Refusal::Unsigned);
            }
            Err(error) => return Err(unreadable(error)),
        };
        if !keys.signed(&bytes, &signature) {
            return Err(Refusal::BadSignature);
        }
    }

    let head = elf::inspect(&bytes)?;
    check(head.magic, &head.version)?;
    drop(bytes);

    let descriptor = match load(sealed) {
        Ok(descriptor) => descriptor,
        Err(LoadError::Refused(why)) => return Err(Refusal::LoadFailed(why)),
        Err(LoadError::NoEntry) => return Err(Refusal::NotAPackage("it has no package entry")),
    };
    check(descriptor.magic, &descriptor.version)?;

    let mut drivers: Slice<'static, PackageDriver> = Slice::EMPTY;
    let status = (descriptor.register)(Out::new(&mut drivers));
    if status == Status::PANIC {
        return Err(Refusal::RegistrationPanicked);
    }
    if !status.is_ok() {
        return Err(Refusal::RegistrationFailed(status));
    }
    let [driver] = drivers.as_slice() else {
        return Err(Refusal::BadManifest(
            "it did not register exactly one driver",
        ));
    };
    if (driver.table.struct_size as usize) < size_of::<DriverTable>() {
        return Err(Refusal::LayoutMismatch);
    }
    let manifest =
        lemnos_core::package::manifest_in(&driver.manifest).map_err(Refusal::BadManifest)?;
    let manifest: &'static Manifest = Box::leak(Box::new(manifest));
    Ok(Package {
        path: path.to_owned(),
        manifest,
        entry: adapter::entry(driver, manifest),
    })
}
