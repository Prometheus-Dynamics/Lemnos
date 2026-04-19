use crate::{DriverCandidate, DriverId, DriverMatchReport, RegistryError, RegistryResult};
use lemnos_core::{DeviceDescriptor, DeviceId};
use lemnos_driver_manifest::DriverManifest;
use lemnos_driver_manifest::ManifestResult;
use lemnos_driver_sdk::Driver;
use lemnos_driver_sdk::DriverMatch;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Stores driver trait objects only at the runtime composition boundary.
///
/// Matching and binding dispatch cross the trait-object boundary once per
/// candidate driver. Inner transport/session work remains on concrete types
/// after a driver has been selected and bound.
#[derive(Default)]
pub struct DriverRegistry {
    drivers: Vec<Arc<dyn Driver>>,
    preferred_drivers: BTreeMap<DeviceId, DriverId>,
}

struct RankedDriver<'a> {
    driver: &'a Arc<dyn Driver>,
    manifest: Cow<'static, DriverManifest>,
    match_result: DriverMatch,
}

impl RankedDriver<'_> {
    fn driver_id(&self) -> &str {
        self.driver.id()
    }

    fn is_supported(&self) -> bool {
        self.match_result.is_supported()
    }

    fn into_candidate(self) -> DriverCandidate {
        DriverCandidate {
            driver: Arc::clone(self.driver),
            driver_id: self.driver_id().to_string(),
            manifest: self.manifest,
            match_result: self.match_result,
        }
    }
}

impl DriverRegistry {
    pub fn register<D>(&mut self, driver: D) -> RegistryResult<()>
    where
        D: Driver + 'static,
    {
        self.register_boxed(Box::new(driver))
    }

    pub fn register_boxed(&mut self, driver: Box<dyn Driver>) -> RegistryResult<()> {
        let driver = Arc::<dyn Driver>::from(driver);
        if self
            .drivers
            .iter()
            .any(|existing| existing.id() == driver.id())
        {
            return Err(RegistryError::DuplicateDriverId {
                driver_id: DriverId::from(driver.id()),
            });
        }

        validate_driver(driver.as_ref()).map_err(|source| RegistryError::InvalidManifest {
            driver_id: DriverId::from(driver.id()),
            source: Box::new(source),
        })?;

        self.drivers.push(driver);
        Ok(())
    }

    pub fn prefer_driver_for_device(
        &mut self,
        device_id: impl Into<DeviceId>,
        driver_id: impl Into<DriverId>,
    ) -> RegistryResult<()> {
        let device_id = device_id.into();
        let driver_id = driver_id.into();

        if self.driver(driver_id.as_str()).is_none() {
            return Err(RegistryError::UnknownPreferredDriver { driver_id });
        }

        self.preferred_drivers.insert(device_id, driver_id);
        Ok(())
    }

    pub fn clear_preferred_driver_for_device(&mut self, device_id: &DeviceId) -> Option<DriverId> {
        self.preferred_drivers.remove(device_id)
    }

    pub fn preferred_driver_for_device(&self, device_id: &DeviceId) -> Option<&DriverId> {
        self.preferred_drivers.get(device_id)
    }

    pub fn driver(&self, id: impl AsRef<str>) -> Option<&dyn Driver> {
        let id = id.as_ref();
        self.drivers
            .iter()
            .find(|driver| driver.id() == id)
            .map(|driver| driver.as_ref())
    }

    pub fn candidates_for(&self, device: &DeviceDescriptor) -> Vec<DriverCandidate> {
        self.ranked_candidates(device)
            .into_iter()
            .filter(RankedDriver::is_supported)
            .map(RankedDriver::into_candidate)
            .collect()
    }

    pub fn match_report(&self, device: &DeviceDescriptor) -> DriverMatchReport {
        DriverMatchReport::from_ranked_candidates(
            device.id.clone(),
            self.preferred_drivers.get(&device.id).cloned(),
            self.ranked_candidates(device)
                .into_iter()
                .map(RankedDriver::into_candidate)
                .collect(),
        )
    }

    fn ranked_candidates(&self, device: &DeviceDescriptor) -> Vec<RankedDriver<'_>> {
        let mut candidates = self
            .drivers
            .iter()
            .map(|driver| RankedDriver {
                driver,
                manifest: driver.manifest_ref(),
                match_result: driver.matches(device),
            })
            .collect::<Vec<_>>();

        candidates.sort_by(|left, right| {
            right
                .is_supported()
                .cmp(&left.is_supported())
                .then_with(|| right.match_result.compare_rank(&left.match_result))
                .then_with(|| left.driver_id().cmp(right.driver_id()))
        });
        candidates
    }

    pub fn best_match(&self, device: &DeviceDescriptor) -> Option<DriverCandidate> {
        self.ranked_candidates(device)
            .into_iter()
            .find(RankedDriver::is_supported)
            .map(RankedDriver::into_candidate)
    }

    pub fn resolve(&self, device: &DeviceDescriptor) -> RegistryResult<DriverCandidate> {
        let candidates = self
            .ranked_candidates(device)
            .into_iter()
            .filter(RankedDriver::is_supported)
            .collect::<Vec<_>>();
        if let Some(preferred_driver_id) = self.preferred_drivers.get(&device.id) {
            if let Some(index) = candidates
                .iter()
                .position(|candidate| candidate.driver_id() == preferred_driver_id.as_str())
            {
                return candidates
                    .into_iter()
                    .nth(index)
                    .map(RankedDriver::into_candidate)
                    .ok_or_else(|| RegistryError::PreferredDriverDidNotMatch {
                        device_id: device.id.clone(),
                        driver_id: preferred_driver_id.clone(),
                    });
            }

            return Err(RegistryError::PreferredDriverDidNotMatch {
                device_id: device.id.clone(),
                driver_id: preferred_driver_id.clone(),
            });
        }

        let mut candidates = candidates.into_iter();
        let Some(best) = candidates.next() else {
            return Err(RegistryError::NoMatchingDriver {
                device_id: device.id.clone(),
            });
        };

        let conflicts = candidates
            .take_while(|candidate| {
                candidate.match_result.score == best.match_result.score
                    && candidate.match_result.level == best.match_result.level
            })
            .map(|candidate| DriverId::from(candidate.driver_id()))
            .collect::<Vec<_>>();

        if conflicts.is_empty() {
            return Ok(best.into_candidate());
        }

        let mut driver_ids = vec![DriverId::from(best.driver_id())];
        driver_ids.extend(conflicts);
        Err(RegistryError::ConflictingMatches {
            device_id: device.id.clone(),
            driver_ids,
        })
    }

    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}

fn validate_driver(driver: &dyn Driver) -> ManifestResult<()> {
    let manifest = driver.manifest_ref();
    manifest.validate()?;
    Ok(())
}
