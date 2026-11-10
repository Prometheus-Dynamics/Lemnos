//! What a simulated board says when a test plugs something in or pulls it out.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{Hint, HintSource};

/// Hints kept for a reader that hasn't come. More than this and they are lost, as a kernel's
/// are when nobody reads them, and the reader is told only that something changed.
const KEPT: usize = 64;

#[derive(Default)]
struct Pending {
    hints: VecDeque<String>,
    overflowed: bool,
}

/// The hints of a [`SimBoard`](crate::SimBoard): one for each resource added or removed, in
/// order. Clones share them, and each hint is reported once.
#[derive(Clone, Default)]
pub struct SimHints(Arc<(Mutex<Pending>, Condvar)>);

impl SimHints {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, name: &str) {
        let mut pending = self.lock();
        if pending.hints.len() == KEPT {
            pending.hints.clear();
            pending.overflowed = true;
        }
        if !pending.overflowed {
            pending.hints.push_back(name.to_string());
        }
        self.0.1.notify_all();
    }

    fn report(mut pending: MutexGuard<'_, Pending>, report: &mut dyn FnMut(Hint<'_>)) -> usize {
        let hints = std::mem::take(&mut pending.hints);
        let overflowed = std::mem::take(&mut pending.overflowed);
        drop(pending);
        if overflowed {
            report(Hint::Any);
        }
        for name in &hints {
            report(Hint::Changed(name));
        }
        hints.len() + usize::from(overflowed)
    }

    /// Reports what is pending, without waiting, and returns how many hints that was.
    pub fn drain(&self, mut report: impl FnMut(Hint<'_>)) -> usize {
        Self::report(self.lock(), &mut report)
    }

    /// Drops what is pending, as when the hints never reach the host. Returns how many.
    pub fn lose(&self) -> usize {
        self.drain(|_| {})
    }
}

impl HintSource for SimHints {
    fn wait(&mut self, timeout: Duration, report: &mut dyn FnMut(Hint<'_>)) -> bool {
        let pending = self.lock();
        let (pending, _) = self
            .0
            .1
            .wait_timeout_while(pending, timeout, |pending| {
                pending.hints.is_empty() && !pending.overflowed
            })
            .unwrap_or_else(PoisonError::into_inner);
        Self::report(pending, report);
        true
    }
}
