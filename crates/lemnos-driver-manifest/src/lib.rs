#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod error;
mod interaction;
mod manifest;
mod matching;
mod validation;
mod version;

pub use error::{ManifestError, ManifestResult};
pub use interaction::{InteractionKind, InteractionManifest};
pub use manifest::{DriverManifest, DriverPriority};
pub use matching::{ManifestMatch, MatchCondition, MatchRule};
pub use version::DriverVersion;

#[cfg(test)]
mod tests;
