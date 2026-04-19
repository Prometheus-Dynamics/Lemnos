use std::borrow::Cow;
use std::sync::Arc;

use lemnos_driver_manifest::DriverManifest;
use lemnos_driver_sdk::{Driver, DriverMatch};

#[derive(Clone)]
pub struct DriverCandidate {
    pub driver: Arc<dyn Driver>,
    pub driver_id: String,
    pub manifest: Cow<'static, DriverManifest>,
    pub match_result: DriverMatch,
}

impl DriverCandidate {
    pub fn is_supported(&self) -> bool {
        self.match_result.is_supported()
    }
}
