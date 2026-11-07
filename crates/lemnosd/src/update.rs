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
pub(crate) const CONFIRMED_MS: u64 = 3_300;
/// On a trial boot the ember is held until the self-test begins: the status
/// file's `phase` turns `checking` (or `failed`) when the device package's
/// confirm service starts judging the trial. A status file without a `phase`
/// key (an older device package) gets this timed hand-over instead
/// (`LEMNOSD_TRIAL_EMBER_MS` overrides it).
pub(crate) const TRIAL_EMBER_MS: u64 = 4_000;
/// The longest the ember waits for `phase` before the sparkle shows anyway.
pub(crate) const TRIAL_EMBER_MAX_MS: u64 = 60_000;
/// The cross-fade from the ember into the trial's sparkle.
pub(crate) const TRIAL_CROSSFADE_MS: u32 = 1_000;

/// What the updater's state means for the light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UpdateView {
    pub state: SystemState,
    /// Show it for this long (from when it was first seen), else while the
    /// updater stays in this state.
    pub hold_ms: Option<u64>,
}

/// What the status file says that the light uses, kept from the last parse.
#[derive(Default)]
struct Status {
    state: String,
    error: String,
    /// The file has a `phase` key (a device package that reports the
    /// self-test's start).
    has_phase: bool,
    /// `phase` is `checking` or `failed`: the trial's self-test has begun.
    checking: bool,
}

impl Status {
    fn parse(bytes: &[u8]) -> Self {
        let json: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
        let text = |key: &str, default: &str| {
            json.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let phase = json.get("phase");
        Self {
            state: text("state", "idle"),
            error: text("error", ""),
            has_phase: phase.is_some(),
            checking: matches!(phase.and_then(|v| v.as_str()), Some("checking" | "failed")),
        }
    }
}

pub(crate) struct UpdateWatcher {
    status: PathBuf,
    progress: PathBuf,
    /// The status file's bytes as last read, and what they said: the file is
    /// re-parsed only when it changes (it lists every installed image, tens of
    /// kilobytes). The service polls when inotify reports a change in its
    /// directory (twice a second where it cannot watch).
    raw: Vec<u8>,
    next: Vec<u8>,
    parsed: Option<Status>,
    /// The last state and error text seen, so a held state shows once.
    seen: Option<(String, String)>,
    /// The ember of a trial boot is held until this time (ms on the service
    /// clock), then the trial's sparkle replaces it.
    ember_until: Option<u64>,
    trial_ember_ms: u64,
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
        let trial_ember_ms = std::env::var("LEMNOSD_TRIAL_EMBER_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(TRIAL_EMBER_MS);
        Self {
            status,
            progress,
            raw: Vec::new(),
            next: Vec::new(),
            parsed: None,
            seen: None,
            ember_until: None,
            trial_ember_ms,
        }
    }

    /// The directories whose changes [`Self::poll`] reads: the status
    /// file's, and the copy progress's.
    pub fn dirs(&self) -> [&Path; 2] {
        [
            self.status.parent().unwrap_or_else(|| Path::new("/")),
            self.progress.parent().unwrap_or_else(|| Path::new("/")),
        ]
    }

    /// When a poll is due with no file changed (ms on the service clock): the
    /// trial boot's ember hold ending.
    pub fn wake_at(&self) -> Option<u64> {
        self.ember_until
    }

    /// Reads the files. `Some(view)` when the light should change: a new
    /// view, or `None` inside when the update is over.
    pub fn poll(&mut self, now_ms: u64) -> Option<Option<UpdateView>> {
        self.next.clear();
        if let Ok(mut file) = std::fs::File::open(&self.status)
            && std::io::Read::read_to_end(&mut file, &mut self.next).is_err()
        {
            self.next.clear();
        }
        if self.parsed.is_none() || self.next != self.raw {
            std::mem::swap(&mut self.raw, &mut self.next);
            self.parsed = Some(Status::parse(&self.raw));
        }
        let progress =
            read_progress(&self.progress).map(|copied| (100 + copied.min(1000) * 85 / 100) as u16);
        let status = self.parsed.as_ref()?;
        let state = status.state.as_str();
        let seen = self.seen.as_ref();
        let changed = seen.is_none_or(|(s, e)| *s != status.state || *e != status.error);
        // The state seen last poll (`None`: this is the first look).
        let previous = seen.map(|(state, _)| state.as_str());
        let first = previous.is_none();
        // A trial boot (the status reads `trying`, or `rebooting`, when the
        // service starts) shows the ember first: the restart's look carries on
        // until the trial's self-test begins, then the sparkle fades in.
        let checking = status.checking;
        if first && matches!(state, "trying" | "rebooting") {
            // With a `phase` key the self-test's start ends the hold; the
            // timer is only a bound. Without one, it is the hand-over.
            let hold = if status.has_phase {
                TRIAL_EMBER_MAX_MS
            } else {
                self.trial_ember_ms
            };
            self.ember_until = Some(now_ms + hold);
        }
        if let Some(until) = self.ember_until {
            let trial = matches!(state, "trying" | "rebooting");
            if now_ms < until && trial && !checking {
                if changed {
                    self.seen = Some((status.state.clone(), status.error.clone()));
                }
                return if first {
                    Some(Some(UpdateView {
                        state: SystemState::Rebooting,
                        hold_ms: None,
                    }))
                } else {
                    None
                };
            }
            self.ember_until = None;
        }
        let view = match state {
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
        if changed {
            self.seen = Some((status.state.clone(), status.error.clone()));
        }
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

/// The image copy's progress (0-1000), read without allocating.
fn read_progress(path: &Path) -> Option<u32> {
    let mut buf = [0u8; 32];
    let mut file = std::fs::File::open(path).ok()?;
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    std::str::from_utf8(&buf[..n]).ok()?.trim().parse().ok()
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
        // The trial boot's ember first, then the trial's sparkle.
        assert_eq!(
            watcher.poll(0),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        );
        assert_eq!(watcher.poll(TRIAL_EMBER_MS - 1), None);
        assert_eq!(
            watcher.poll(TRIAL_EMBER_MS),
            Some(Some(UpdateView {
                state: SystemState::Booting,
                hold_ms: None,
            }))
        );
        set_state(&path, "confirmed");
        assert_eq!(
            watcher.poll(0),
            Some(Some(UpdateView {
                state: SystemState::Confirmed,
                hold_ms: Some(CONFIRMED_MS),
            }))
        );
        // Shown once: nothing more until the state changes again.
        assert_eq!(watcher.poll(0), None);
        assert_eq!(watcher.poll(0), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_start_with_an_old_confirmation_shows_nothing() {
        let (path, dir) = status_file("fresh");
        set_state(&path, "confirmed");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(watcher.poll(0), None);
        assert_eq!(watcher.poll(0), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_confirmation_that_was_not_from_a_trial_boot_shows_nothing() {
        let (path, dir) = status_file("idle-then-confirmed");
        set_state(&path, "idle");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(watcher.poll(0), None);
        set_state(&path, "confirmed");
        // Not from `trying`: no ripple, and the system layer is released.
        assert_eq!(watcher.poll(0), Some(None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reboot_the_updater_announces_shows_the_ember_while_it_lasts() {
        let (path, dir) = status_file("rebooting");
        set_state(&path, "staged");
        let mut watcher = UpdateWatcher::new(&path);
        assert!(matches!(
            watcher.poll(0),
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
            watcher.poll(0),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        );
        // Held: the view is re-stated while the updater stays in the state.
        assert!(matches!(
            watcher.poll(0),
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
        assert_eq!(watcher.poll(0), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_restart_at_start_holds_the_ember_until_the_trial_begins() {
        let (path, dir) = status_file("ember");
        set_state(&path, "rebooting");
        let mut watcher = UpdateWatcher::new(&path);
        // At start the ember shows at once, and holds through the trial's start.
        assert_eq!(
            watcher.poll(0),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        );
        assert_eq!(watcher.poll(TRIAL_EMBER_MS - 1), None);
        // Still `rebooting` when the hold is over: the ember stays.
        assert_eq!(
            watcher.poll(TRIAL_EMBER_MS),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None,
            }))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    fn set_trial_phase(path: &Path, phase: &str) {
        std::fs::write(path, format!(r#"{{"state": "trying", "phase": {phase}}}"#)).unwrap();
    }

    #[test]
    fn the_ember_holds_until_the_self_test_begins() {
        let (path, _dir) = status_file("phase");
        set_trial_phase(&path, "null");
        let mut watcher = UpdateWatcher::new(&path);
        assert_eq!(
            watcher.poll(0),
            Some(Some(UpdateView {
                state: SystemState::Rebooting,
                hold_ms: None
            }))
        );
        // Past the old timer but before the checks: still the ember.
        assert_eq!(watcher.poll(TRIAL_EMBER_MS + 1_000), None);
        set_trial_phase(&path, "\"checking\"");
        assert_eq!(
            watcher.poll(TRIAL_EMBER_MS + 2_000),
            Some(Some(UpdateView {
                state: SystemState::Booting,
                hold_ms: None
            }))
        );
    }

    #[test]
    fn a_trial_without_the_checks_starting_gets_the_sparkle_at_the_bound() {
        let (path, _dir) = status_file("phase-bound");
        set_trial_phase(&path, "null");
        let mut watcher = UpdateWatcher::new(&path);
        watcher.poll(0);
        assert_eq!(watcher.poll(TRIAL_EMBER_MAX_MS - 1), None);
        assert_eq!(
            watcher.poll(TRIAL_EMBER_MAX_MS),
            Some(Some(UpdateView {
                state: SystemState::Booting,
                hold_ms: None
            }))
        );
    }
}
