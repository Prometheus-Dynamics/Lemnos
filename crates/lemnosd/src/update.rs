//! Shows the device package's updates on the status light, from the files
//! the updater already writes (no updater changes):
//!
//! - `/run/pd-device/update.json`: `{"state": "...", "progress": N, ...}`,
//!   rewritten on every state change;
//! - `/run/pd-device/update/progress`: the image copy's progress (0-1000)
//!   while staging, which `update status` maps to 100-950.

use lemnos_light::{Phase, SystemState};
use std::path::{Path, PathBuf};

/// How long `staged` (written, waiting for a restart) is shown.
pub(crate) const STAGED_MS: u64 = 5_000;
/// How long a failed update or a rollback is shown.
pub(crate) const FAILED_MS: u64 = 60_000;

/// What the updater's state means for the light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UpdateView {
    pub state: SystemState,
    /// Show it for this long (from when it was first seen), else while the
    /// updater stays in this state.
    pub hold_ms: Option<u64>,
}

pub(crate) struct UpdateWatcher {
    status: PathBuf,
    progress: PathBuf,
    /// The last state and error text seen, so a held state shows once.
    seen: Option<(String, String)>,
}

impl UpdateWatcher {
    /// Watches `status` (`update.json`); the copy progress is
    /// `<status dir>/update/progress`.
    pub fn new(status: impl Into<PathBuf>) -> Self {
        let status = status.into();
        let progress = status
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join("update/progress");
        Self {
            status,
            progress,
            seen: None,
        }
    }

    /// Reads the files. `Some(view)` when the light should change: a new
    /// view, or `None` inside when the update is over.
    pub fn poll(&mut self) -> Option<Option<UpdateView>> {
        let text = std::fs::read_to_string(&self.status).unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let state = json
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("idle")
            .to_string();
        let error = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let progress = std::fs::read_to_string(&self.progress)
            .ok()
            .and_then(|p| p.trim().parse::<u32>().ok())
            .map(|copied| (100 + copied.min(1000) * 85 / 100) as u16);
        let key = (state.clone(), error);
        let changed = self.seen.as_ref() != Some(&key);
        let view = match state.as_str() {
            "staging" => Some(UpdateView {
                state: match progress {
                    Some(progress) => SystemState::Updating {
                        progress: Some(progress),
                        phase: Phase::Writing,
                    },
                    None => SystemState::Updating {
                        progress: None,
                        phase: Phase::Verifying,
                    },
                },
                hold_ms: None,
            }),
            "staged" => Some(UpdateView {
                state: SystemState::Updating {
                    progress: Some(1000),
                    phase: Phase::Staged,
                },
                hold_ms: Some(STAGED_MS),
            }),
            "trying" => Some(UpdateView {
                state: SystemState::Booting,
                hold_ms: None,
            }),
            "rolled-back" => Some(UpdateView {
                state: SystemState::RolledBack,
                hold_ms: Some(FAILED_MS),
            }),
            "error" => Some(UpdateView {
                state: SystemState::UpdateFailed,
                hold_ms: Some(FAILED_MS),
            }),
            _ => None,
        };
        self.seen = Some(key);
        match view {
            // Progress moves within `staging`: report every poll.
            Some(view) if changed || view.hold_ms.is_none() => Some(Some(view)),
            Some(_) => None,
            None if changed => Some(None),
            None => None,
        }
    }
}
