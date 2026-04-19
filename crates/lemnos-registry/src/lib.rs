#![forbid(unsafe_code)]

mod candidate;
mod driver_id;
mod error;
mod registry;
mod report;

pub use candidate::DriverCandidate;
pub use driver_id::DriverId;
pub use error::{RegistryError, RegistryResult};
pub use registry::DriverRegistry;
pub use report::{DriverCandidateSummary, DriverMatchReport};

#[cfg(test)]
mod tests;
