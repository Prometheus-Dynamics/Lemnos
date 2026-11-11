//! Driver packages for this service: off unless a directory is configured.
//!
//! With no directory nothing here runs: no file is looked at and nothing is loaded. With one,
//! every package in it is loaded beside the built-in drivers, in signed mode unless open mode is
//! asked for by name. A built-in driver's name always wins: a package with the same name is
//! refused. Nothing a package does or fails to do stops the service or another package.

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

/// Loads the configured packages and adds their drivers to `drivers`. Returns what there is to
/// say about it, one line each: every package loaded, every file refused and why, and anything
/// that kept the directory from being used at all.
pub fn add_packages(drivers: &mut Vec<DriverEntry>, config: &PackageConfig) -> Vec<String> {
    let Some(dir) = &config.dir else {
        return Vec::new();
    };
    let mode = if config.open {
        Mode::Open
    } else {
        let Some(keys) = &config.keys else {
            return vec![format!(
                "packages: {} not used: signed mode needs a key file",
                dir.display()
            )];
        };
        match TrustedKeys::from_file(keys) {
            Ok(keys) => Mode::Signed(keys),
            Err(error) => {
                return vec![format!(
                    "packages: {} not used: {}: {error}",
                    dir.display(),
                    keys.display()
                )];
            }
        }
    };
    let mut loader = Loader::new(mode);
    loader.reserve(drivers.iter().map(|driver| driver.manifest().name));
    let mut said = Vec::new();
    if let Err(error) = loader.scan(dir) {
        said.push(format!("packages: {}: {error}", dir.display()));
    }
    for package in loader.packages() {
        said.push(format!(
            "package {}: driver {:?}",
            package.path().display(),
            package.name()
        ));
    }
    for refused in loader.refused() {
        said.push(format!(
            "package {} refused: {}",
            refused.path.display(),
            refused.why
        ));
    }
    drivers.extend(loader.entries());
    said
}
