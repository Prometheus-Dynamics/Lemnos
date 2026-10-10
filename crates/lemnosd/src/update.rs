//! Shows the device package's updates on the status light, from the files
//! the updater already writes (no updater changes):
//!
//! - the status file `LEMNOSD_UPDATE_STATUS` names (for example
//!   `/run/board/update.json`): `{"state": "...", "progress": N, ...}`,
//!   rewritten on every state change;
//! - `update/progress` next to it: the image copy's progress (0-1000) while
//!   staging, which `update status` maps to 100-950.
//!
//! The path is set only through `LEMNOSD_UPDATE_STATUS` (the device
//! package's environment file); there is no built-in default.

use lemnos_light::{Phase, SystemState};
use std::path::{Path, PathBuf};

/// How long `staged` (written, waiting for a restart) is shown.
pub(crate) const STAGED_MS: u64 = 5_000;
/// How long a failed update or a rollback is shown.
pub(crate) const FAILED_MS: u64 = 60_000;
/// How long the confirmed celebration is held (its look runs about 3.2 s).
pub(crate) const CONFIRMED_MS: u64 = 3_200;

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
        // The state seen last poll (`None`: this is the first look).
        let previous = self.seen.as_ref().map(|(state, _)| state.as_str());
        let first = previous.is_none();
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
            // The updater writes this just before it restarts the system
            // (into a new version, or back): the ember, held while it lasts.
            "rebooting" => Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }),
            // Only the trial boot's own confirmation shows the ripple: an old
            // `confirmed` found at start-up shows nothing.
            "confirmed" if previous == Some("trying") => Some(UpdateView {
                state: SystemState::Confirmed,
                hold_ms: Some(CONFIRMED_MS),
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
            // Not on the first look: a start-up must not clear the boot
            // spinner the service shows until it is due.
            None if changed && !first => Some(None),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory with an update status file in it.
    fn status_file(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("lemnosd-update-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("update.json"), dir)
    }

    fn set_state(path: &Path, state: &str) {
        std::fs::write(path, format!(r#"{{"state": "{state}"}}"#)).unwrap();
    }

    #[test]
    fn a_trial_boot_that_is_confirmed_shows_the_ripple_once() {
        let (path, dir) = status_file("confirm");
        set_state(&path, "trying");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(
            watcher.poll(),
            Some(Some(UpdateView {
                state: SystemState::Booting,
                hold_ms: None,
            }))
        );
        set_state(&path, "confirmed");
        assert_eq!(
            watcher.poll(),
            Some(Some(UpdateView {
                state: SystemState::Confirmed,
                hold_ms: Some(CONFIRMED_MS),
            }))
        );
        // Shown once: nothing more until the state changes again.
        assert_eq!(watcher.poll(), None);
        assert_eq!(watcher.poll(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_start_with_an_old_confirmation_shows_nothing() {
        let (path, dir) = status_file("fresh");
        set_state(&path, "confirmed");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(watcher.poll(), None);
        assert_eq!(watcher.poll(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_confirmation_that_was_not_from_a_trial_boot_shows_nothing() {
        let (path, dir) = status_file("idle-then-confirmed");
        set_state(&path, "idle");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(watcher.poll(), None);
        set_state(&path, "confirmed");
        // Not from `trying`: no ripple, and the system layer is released.
        assert_eq!(watcher.poll(), Some(None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reboot_the_updater_announces_shows_the_ember_while_it_lasts() {
        let (path, dir) = status_file("rebooting");
        set_state(&path, "staged");
        let mut watcher = UpdateWatcher::new(&path);
        assert!(matches!(
            watcher.poll(),
            Some(Some(UpdateView {
                state: SystemState::Updating {
                    phase: Phase::Staged,
                    ..
                },
                hold_ms: Some(_),
            }))
        ));
        set_state(&path, "rebooting");
        assert_eq!(
            watcher.poll(),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        );
        // Held: the view is re-stated while the updater stays in the state.
        assert!(matches!(
            watcher.poll(),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_start_leaves_the_boot_spinner_alone() {
        let (path, dir) = status_file("idle-start");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(watcher.poll(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
