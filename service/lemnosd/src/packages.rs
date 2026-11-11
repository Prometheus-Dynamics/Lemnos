//! Driver packages for this service: off unless a directory is configured.
//!
//! With no directory nothing here runs: no file is looked at and nothing is loaded. With one,
//! every package in it is loaded beside the built-in drivers, in signed mode unless open mode is
//! asked for by name. A built-in driver's name always wins: a package with the same name is
//! refused. Nothing a package does or fails to do stops the service or another package.
//!
//! What became of each file is kept as a [`PackageReport`]: the service prints it when it
//! starts and answers a client that asks with it (`lemnos-ctl packages`).
//!
//! A service linked statically against its C library can load no package: it has no dynamic
//! loader. It says so once, looks at no file, and runs its built-in drivers as ever.

use std::path::PathBuf;

use lemnos_host::DriverEntry;
use lemnos_package::{Loader, Mode, TrustedKeys};

/// Where packages come from and which may run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageConfig {
    /// The directory to scan. `None`, the default, is off.
    pub dir: Option<PathBuf>,
    /// The file of trusted public keys, for signed mode: one key per line as 64 hex digits.
    pub keys: Option<PathBuf>,
    /// Open mode: run any package in the directory. Off, the default, is signed mode.
    pub open: bool,
}

/// Which packages a service runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PackageMode {
    /// None: no directory is configured.
    #[default]
    Off,
    /// Only packages signed by a trusted key.
    Signed,
    /// Any package in the directory.
    Open,
}

/// One package file and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    pub path: PathBuf,
    /// The name of the driver it was loaded as. `None` for a file that was refused.
    pub driver: Option<String>,
    /// Why it was refused. Empty for one that was loaded.
    pub reason: String,
}

/// What became of the packages a service was configured with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageReport {
    pub mode: PackageMode,
    /// Why the directory could not be used at all, or was only partly read. Empty otherwise.
    pub note: String,
    /// Every file looked at, in order of name: the loaded ones, then the refused ones.
    pub files: Vec<PackageFile>,
}

impl PackageReport {
    /// What there is to say, one line each: anything that kept the directory from being used,
    /// every package loaded, and every file refused and why.
    pub fn lines(&self) -> Vec<String> {
        let mut said = Vec::new();
        if !self.note.is_empty() {
            said.push(format!("packages: {}", self.note));
        }
        for file in &self.files {
            said.push(match &file.driver {
                Some(driver) => format!("package {}: driver {driver:?}", file.path.display()),
                None => format!("package {} refused: {}", file.path.display(), file.reason),
            });
        }
        said
    }
}

/// Loads the configured packages and adds their drivers to `drivers`. Returns what became of
/// them.
pub fn add_packages(drivers: &mut Vec<DriverEntry>, config: &PackageConfig) -> PackageReport {
    add_packages_if(drivers, config, lemnos_package::can_load())
}

/// [`add_packages`], for a program that can load shared objects (`can_load`) or one that can't.
pub fn add_packages_if(
    drivers: &mut Vec<DriverEntry>,
    config: &PackageConfig,
    can_load: bool,
) -> PackageReport {
    let Some(dir) = &config.dir else {
        return PackageReport::default();
    };
    let mut report = PackageReport {
        mode: if config.open {
            PackageMode::Open
        } else {
            PackageMode::Signed
        },
        ..PackageReport::default()
    };
    if !can_load {
        report.note = format!(
            "{} not used: this lemnosd is statically linked and can't load driver packages; \
             packages need a dynamically linked build",
            dir.display()
        );
        return report;
    }
    let mode = if config.open {
        Mode::Open
    } else {
        let Some(keys) = &config.keys else {
            report.note = format!("{} not used: signed mode needs a key file", dir.display());
            return report;
        };
        match TrustedKeys::from_file(keys) {
            Ok(keys) => Mode::Signed(keys),
            Err(error) => {
                report.note = format!("{} not used: {}: {error}", dir.display(), keys.display());
                return report;
            }
        }
    };
    let mut loader = Loader::new(mode);
    loader.reserve(drivers.iter().map(|driver| driver.manifest().name));
    if let Err(error) = loader.scan(dir) {
        report.note = format!("{}: {error}", dir.display());
    }
    for package in loader.packages() {
        report.files.push(PackageFile {
            path: package.path().to_owned(),
            driver: Some(package.name().to_owned()),
            reason: String::new(),
        });
    }
    for refused in loader.refused() {
        report.files.push(PackageFile {
            path: refused.path.clone(),
            driver: None,
            reason: refused.why.to_string(),
        });
    }
    drivers.extend(loader.entries());
    report
}
