use crate::{DriverCandidate, DriverId};
use lemnos_core::DeviceId;
use lemnos_driver_manifest::DriverPriority;
use lemnos_driver_sdk::DriverMatchLevel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverCandidateSummary {
    pub driver_id: DriverId,
    pub driver_summary: String,
    pub supported: bool,
    pub priority: DriverPriority,
    pub level: DriverMatchLevel,
    pub score: u32,
    pub reasons: Vec<String>,
    pub matched_rule: Option<usize>,
}

impl DriverCandidateSummary {
    pub fn from_candidate(candidate: &DriverCandidate) -> Self {
        Self {
            driver_id: DriverId::from(candidate.driver_id.as_str()),
            driver_summary: candidate.manifest.summary.clone(),
            supported: candidate.is_supported(),
            priority: candidate.manifest.priority,
            level: candidate.match_result.level,
            score: candidate.match_result.score,
            reasons: candidate.match_result.reasons.clone(),
            matched_rule: candidate.match_result.matched_rule,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverMatchReport {
    pub device_id: DeviceId,
    pub preferred_driver_id: Option<DriverId>,
    pub supported: Vec<DriverCandidateSummary>,
    pub rejected: Vec<DriverCandidateSummary>,
}

impl DriverMatchReport {
    pub(crate) fn from_ranked_candidates(
        device_id: DeviceId,
        preferred_driver_id: Option<DriverId>,
        ranked: Vec<DriverCandidate>,
    ) -> Self {
        let mut supported = Vec::new();
        let mut rejected = Vec::new();

        for candidate in ranked {
            let summary = DriverCandidateSummary::from_candidate(&candidate);
            if summary.supported {
                supported.push(summary);
            } else {
                rejected.push(summary);
            }
        }

        Self {
            device_id,
            preferred_driver_id,
            supported,
            rejected,
        }
    }

    pub fn best(&self) -> Option<&DriverCandidateSummary> {
        self.supported.first()
    }

    pub fn conflicting_top_matches(&self) -> Vec<&DriverCandidateSummary> {
        let Some(best) = self.best() else {
            return Vec::new();
        };

        self.supported
            .iter()
            .take_while(|candidate| candidate.score == best.score && candidate.level == best.level)
            .collect()
    }

    pub fn preferred(&self) -> Option<&DriverCandidateSummary> {
        let preferred_driver_id = self.preferred_driver_id.as_ref()?;
        self.supported
            .iter()
            .chain(self.rejected.iter())
            .find(|candidate| &candidate.driver_id == preferred_driver_id)
    }
}
