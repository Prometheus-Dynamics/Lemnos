//! How a trial resolves, from the boot that starts the trial to the board's confirmation or
//! fallback: the start-up accounting ([`SystemSlots::resolve_at_start`]), the judgement a trial
//! waits on ([`SystemSlots::settle`]), and the confirmation both the board and a caller make
//! ([`SystemSlots::confirm_as`]). See `docs/slots.md`, "How a trial resolves".

use std::fs;
use std::io;
use std::time::Duration;

use lemnos_core::{BoardView, CallerId, Result, Status};

use crate::SystemSlots;
use crate::boot;
use crate::store::{self, Meta, valid_id};
use crate::trial::{self, Dev, Log, Resolved};

/// Who confirms a trial: the board itself, or a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Auto,
    Call(CallerId),
}

/// How often a waiting trial is looked at.
const LOOK_EVERY: u64 = 1_000_000_000;

/// A deadline restart tries to write its reason this many times, then restarts anyway: the
/// default has to come back. The first wait is one second, doubling, capped at 8 s, so the tries
/// come at 0, 1, 3, 7, 15 and 23 s.
const RESTART_TRIES: u32 = 6;
const RETRY_FIRST_NS: u64 = 1_000_000_000;
const RETRY_CAP_NS: u64 = 8_000_000_000;

/// A deadline restart waiting to write its reason: the reason, how many tries have been made,
/// and when the next is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveUp {
    reason: String,
    tries: u32,
    wait_ns: u64,
    next_ns: u64,
}

impl SystemSlots {
    /// Starts one boot's accounting for a pending trial, before anything else runs.
    ///
    /// In the trial slot, the daemon running means the attempt the loader took is given back, once
    /// per boot. In any other slot the boots since the last look that never reached the daemon
    /// are recorded as used, and a trial with no attempts left falls back, recorded as
    /// `resolved`.
    pub(crate) fn resolve_at_start(&mut self) {
        let state = self.state();
        if state.trial.is_empty() {
            Log::remove(&self.layout);
            return;
        }
        let trial = state.trial.clone();
        let running = self.running();
        let pending = self.record.pending().ok().flatten();
        let left = pending
            .as_ref()
            .filter(|(id, _)| *id == trial)
            .map_or(0, |(_, n)| *n);
        let mut log = Log::load(&self.layout)
            .filter(|log| log.slot == trial)
            .unwrap_or_else(|| Log {
                slot: trial.clone(),
                seen: self.policy.attempts,
                ..Log::default()
            });
        if running == trial {
            if log.boot != self.boot {
                let given_back = (left + 1).min(self.policy.attempts);
                if pending.is_some() {
                    let _ = self.record.set_attempts(given_back);
                }
                log.seen = given_back;
                log.add_attempt();
                log.note("daemon-reached-not-yet-confirmed");
                log.boot = self.boot.clone();
            }
            let _ = log.save(&self.layout);
            return;
        }
        // Another slot is running: the trial's boots since the last look never reached the daemon.
        let failed = log.seen.saturating_sub(left);
        for _ in 0..failed {
            log.add_attempt();
        }
        if failed > 0 {
            log.note("power-lost-before-daemon");
        }
        log.seen = left;
        if left > 0 {
            let _ = log.save(&self.layout);
            return;
        }
        let reason = if !log.reason.is_empty() {
            log.reason.clone()
        } else if let Some(reason) = boot::read_reason(&self.boot_node) {
            reason
        } else {
            self.trial_reason(&trial).unwrap_or_else(|| {
                format!(
                    "slot {trial} did not reach a running daemon in {} boots",
                    log.attempts.len()
                )
            })
        };
        log.note("fell-back");
        boot::clear_reason(&self.boot_node);
        let _ = self.record.clear_trial();
        self.end_trial_failed(&trial, &reason);
        Log::remove(&self.layout);
        let resolved = Resolved {
            slot: trial,
            outcome: "fell-back".into(),
            by: "auto".into(),
            caller: None,
            reason,
            time: store::now_utc(),
        };
        let _ = resolved.save(&self.layout);
    }

    /// The trial's slot's own reason for its last failure, when its `.meta` has one.
    fn trial_reason(&self, id: &str) -> Option<String> {
        let meta = Meta::load(&self.layout, id);
        (meta.failed() && !meta.get("FAIL_REASON").is_empty())
            .then(|| meta.get("FAIL_REASON").to_string())
    }

    /// A trial that didn't come up is out of the state, and its slot is marked failed (once).
    fn end_trial_failed(&mut self, trial: &str, reason: &str) {
        let mut state = self.state();
        state.trial.clear();
        state.state = "rolled-back".into();
        state.error = reason.to_string();
        let _ = self.save(&state);
        let mut meta = Meta::load(&self.layout, trial);
        if !meta.failed() {
            let failures = meta.get("FAILURES").parse::<u32>().unwrap_or(0) + 1;
            meta.set("FAILED", "yes");
            meta.set("FAILURES", &failures.to_string());
            meta.set("FAIL_REASON", reason);
            meta.set("FAIL_TIME", &store::now_utc());
            if self.layout.installed(trial) {
                let _ = meta.save(&self.layout, trial);
            }
        }
    }

    /// The board as the host shows it now. Read only while a trial waits, or until this boot's
    /// baseline is written; reading allocates, and only then.
    pub(crate) fn watch(&mut self, view: &dyn BoardView) {
        if !self.wants_view() {
            return;
        }
        let mut devices = Vec::new();
        view.each(&mut |device| devices.push(Dev::from(device)));
        self.seen = devices;
        self.looked = true;
    }

    fn wants_view(&self) -> bool {
        !self.state().trial.is_empty() || !self.snapshot_written
    }

    /// The device baseline of slot `id`: the devices that were running and healthy in it.
    fn baseline(&self, id: &str) -> Vec<(String, bool)> {
        if id.is_empty() {
            return Vec::new();
        }
        fs::read_to_string(self.layout.marker(&format!("{id}.devices")))
            .map(|text| trial::parse_baseline(&text))
            .unwrap_or_default()
    }

    fn write_baseline(&self, id: &str) -> io::Result<()> {
        store::replace(
            &self.layout.marker(&format!("{id}.devices")),
            trial::baseline_of(&self.seen).as_bytes(),
        )
    }

    /// Called on each tick: judges a waiting trial, records a good boot's baseline, and returns
    /// when to look again (`None` when there is nothing left to look at).
    pub(crate) fn settle(&mut self, now_ns: u64) -> Option<u64> {
        if self.giving_up.is_some() {
            return self.finish_give_up(now_ns);
        }
        let started = *self.started_ns.get_or_insert(now_ns);
        let elapsed = Duration::from_nanos(now_ns.saturating_sub(started));
        let state = self.state();
        let running = self.running();
        if state.trial.is_empty() {
            if self.snapshot_written {
                return None;
            }
            if elapsed < self.policy.uptime || !self.looked || !valid_id(&running) {
                return Some(now_ns + LOOK_EVERY);
            }
            if self.write_baseline(&running).is_ok() {
                self.snapshot_written = true;
            }
            return None;
        }
        if running != state.trial || !self.policy.auto_confirm {
            return None;
        }
        let baseline = self.baseline(&state.default);
        let mut unmet = if self.looked {
            trial::regression(&baseline, &self.seen).or_else(|| {
                trial::source_unmet(
                    &self.sources,
                    &mut self.probes,
                    &self.seen,
                    self.started_wall,
                    now_ns,
                )
            })
        } else {
            Some("the board has not been looked at yet".to_string())
        };
        if unmet.is_none() && elapsed >= self.policy.uptime {
            match self.writes_work() {
                Ok(()) => {
                    let _ = self.confirm_as(By::Auto);
                    return None;
                }
                Err(why) => unmet = Some(why),
            }
        }
        // What holds the trial back is recorded as it changes, so `list` shows it while it waits.
        self.note_reason(unmet.clone().unwrap_or_default());
        if elapsed >= self.policy.deadline {
            let reason = unmet.unwrap_or_else(|| {
                format!(
                    "the daemon has not run for {} s",
                    self.policy.uptime.as_secs()
                )
            });
            if self.policy.reboot_at_deadline {
                return self.begin_give_up(reason, now_ns);
            }
        }
        Some(now_ns + LOOK_EVERY)
    }

    /// The slot actions a confirm needs: the state and the boot record can be written.
    fn writes_work(&mut self) -> std::result::Result<(), String> {
        let state = self.state();
        self.save(&state)
            .map_err(|_| "the slot state can't be written".to_string())?;
        if let Some((_, left)) = self
            .record
            .pending()
            .map_err(|error| format!("the boot record can't be read: {error}"))?
        {
            self.record
                .set_attempts(left)
                .map_err(|error| format!("the boot record can't be written: {error}"))?;
        }
        Ok(())
    }

    /// Records why the trial is still waiting, once per change (`""` once nothing holds it).
    fn note_reason(&mut self, reason: String) {
        let Some(mut log) = Log::load(&self.layout) else {
            return;
        };
        if log.reason != reason {
            log.reason = reason;
            let _ = log.save(&self.layout);
        }
    }

    /// The deadline passed with the trial unconfirmed: the reason is recorded, the trial's attempts
    /// are used up, and the board restarts into the default. See [`GiveUp`] for the writes.
    fn begin_give_up(&mut self, reason: String, now_ns: u64) -> Option<u64> {
        self.giving_up = Some(GiveUp {
            reason,
            tries: 0,
            wait_ns: RETRY_FIRST_NS,
            next_ns: now_ns,
        });
        self.finish_give_up(now_ns)
    }

    /// One step of a deadline restart: writes the reason once it is due, and restarts when it is
    /// written or the tries are used up. Returns when to come back, `None` once the restart went.
    fn finish_give_up(&mut self, now_ns: u64) -> Option<u64> {
        let (reason, next_ns) = self
            .giving_up
            .as_ref()
            .map(|give_up| (give_up.reason.clone(), give_up.next_ns))?;
        if now_ns < next_ns {
            return Some(next_ns);
        }
        let written = self.write_give_up(&reason);
        let give_up = self.giving_up.as_mut()?;
        give_up.tries += 1;
        let tries = give_up.tries;
        match written {
            Ok(warning) => {
                self.record_error = warning;
                self.giving_up = None;
                self.restart_after_give_up(&reason);
                None
            }
            Err(error) => {
                self.record_error = Some(error.clone());
                if tries >= RESTART_TRIES {
                    // The reason could not be written in time: the default comes back all the
                    // same, the reason kept in the boot partition if that could be written.
                    self.giving_up = None;
                    self.restart_after_give_up(&reason);
                    return None;
                }
                give_up.next_ns = now_ns + give_up.wait_ns;
                give_up.wait_ns = (give_up.wait_ns * 2).min(RETRY_CAP_NS);
                Some(give_up.next_ns)
            }
        }
    }

    /// The writes a deadline restart needs: the reason and the attempts used up, in the state
    /// (as the trial's log), with the reason also on the boot partition. Fails with the reason
    /// when the state could not hold it, so the caller tries again; a boot partition copy that
    /// could not be written is returned as a warning instead.
    fn write_give_up(&mut self, reason: &str) -> std::result::Result<Option<String>, String> {
        let boot_copy = boot::write_reason(&self.boot_node, reason);
        let state = self.state();
        let mut log = Log::load(&self.layout).unwrap_or_else(|| Log {
            slot: state.trial.clone(),
            ..Log::default()
        });
        // The boot record first: it is what brings the default back, whatever the state holds.
        self.record
            .set_attempts(0)
            .map_err(|error| format!("the boot record can't be written: {error}"))?;
        log.reason = reason.to_string();
        log.seen = 0;
        log.save(&self.layout)
            .map_err(|error| format!("the trial's record can't be written: {error}"))?;
        // The state holds the reason; a missing boot partition copy is reported, not retried.
        Ok(boot_copy
            .err()
            .map(|error| format!("the reason is not on the boot partition: {error}")))
    }

    /// Restarts the board into the default, after the reason has been kept as far as it could be.
    fn restart_after_give_up(&mut self, reason: &str) {
        if let Err(error) = self.record.restart() {
            let mut log = Log::load(&self.layout).unwrap_or_default();
            log.reason = format!("{reason}; the restart failed: {error}");
            let _ = log.save(&self.layout);
        }
    }

    /// Confirms what is running. A trial that is running ends as confirmed: it becomes the default
    /// (or stays a trial only, when the trial keeps the default), its attempts are cleared, the
    /// board's device baseline is written, and `resolved` says who confirmed it.
    pub(crate) fn confirm_as(&mut self, by: By) -> Result<()> {
        let running = self.running();
        if !valid_id(&running) {
            return Err(Status::NOT_FOUND);
        }
        let mut state = self.state();
        let mut housekeep = false;
        let mut resolved_here = false;
        if !state.trial.is_empty() && running == state.trial {
            let keeps = crate::store::read_line(&self.layout.marker("trial-keeps-default"));
            if keeps == running {
                // A trial only: it booted well, and the default stays for the next restart.
                state.trial.clear();
                state.staged.clear();
                self.record.clear_trial().map_err(|_| Status::IO)?;
            } else {
                self.record
                    .fallback_copy(&self.layout, &running)
                    .map_err(|_| Status::IO)?;
                self.record.set_default(&running).map_err(|_| Status::IO)?;
                self.record.clear_trial().map_err(|_| Status::IO)?;
                if state.default != running {
                    state.previous = core::mem::replace(&mut state.default, running.clone());
                }
                state.trial.clear();
                state.staged.clear();
                housekeep = true;
            }
            state.state = "confirmed".into();
            state.error.clear();
            resolved_here = true;
        } else if !state.trial.is_empty() {
            // The trial didn't come up as the running slot: the board went back.
            let trial = core::mem::take(&mut state.trial);
            let reason = format!("slot {trial} didn't confirm its boot; back on {running}");
            let _ = self.record.clear_trial();
            let mut meta = Meta::load(&self.layout, &trial);
            meta.set("GOOD", "no");
            if !meta.failed() {
                let failures = meta.get("FAILURES").parse::<u32>().unwrap_or(0) + 1;
                meta.set("FAILED", "yes");
                meta.set("FAILURES", &failures.to_string());
                meta.set("FAIL_REASON", &reason);
                meta.set("FAIL_TIME", &store::now_utc());
            }
            if self.layout.installed(&trial) {
                meta.save(&self.layout, &trial).map_err(|_| Status::IO)?;
            }
            state.state = "rolled-back".into();
            state.error = reason.clone();
            let _ = Resolved {
                slot: trial,
                outcome: "fell-back".into(),
                by: "call".into(),
                caller: self.caller_of(by),
                reason,
                time: store::now_utc(),
            }
            .save(&self.layout);
        } else if state.state == "rebooting" {
            state.state = "rolled-back".into();
        }
        if state.default.is_empty() {
            state.default = running.clone();
        }
        let mut meta = Meta::load(&self.layout, &running);
        let now = store::now_utc();
        meta.set("GOOD", "yes");
        meta.set("FAILED", "no");
        meta.set("CONFIRM_TIME", &now);
        meta.set("LAST_BOOT", &now);
        if self.layout.installed(&running) {
            meta.save(&self.layout, &running).map_err(|_| Status::IO)?;
        }
        let _ = store::replace(&self.layout.marker("fallbacks"), b"0\n");
        for marker in ["trial-started", "trial-init", "trial-keeps-default"] {
            let _ = fs::remove_file(self.layout.marker(marker));
        }
        self.save(&state)?;
        if resolved_here {
            Log::remove(&self.layout);
            boot::clear_reason(&self.boot_node);
            let _ = Resolved {
                slot: running.clone(),
                outcome: "confirmed".into(),
                by: match by {
                    By::Auto => "auto".into(),
                    By::Call(_) => "call".into(),
                },
                caller: self.caller_of(by),
                reason: String::new(),
                time: store::now_utc(),
            }
            .save(&self.layout);
        }
        if self.looked && self.write_baseline(&running).is_ok() {
            self.snapshot_written = true;
        }
        if housekeep {
            let keep_old = crate::store::read_line(&self.layout.marker("keep-old"));
            if keep_old != running {
                self.housekeep(&mut state)?;
            }
            let _ = fs::remove_file(self.layout.marker("keep-old"));
        }
        // Boot directories of slots no longer resident come off. Never fails the confirm.
        let _ = self.evict(&state, "");
        Ok(())
    }

    fn caller_of(&self, by: By) -> Option<u64> {
        match by {
            By::Auto => None,
            By::Call(caller) => Some(caller.0),
        }
    }
}
