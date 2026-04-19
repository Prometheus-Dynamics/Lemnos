mod kinds;
mod metadata;
mod model;

pub use kinds::{DeviceAddress, DeviceKind};
pub use metadata::{DeviceControlSurface, DeviceLink, DeviceRelation, MatchHints};
pub use model::{DeviceDescriptor, DeviceDescriptorBuilder};

#[cfg(test)]
mod tests;
