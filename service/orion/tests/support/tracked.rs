//! The test driver whose callers are tracked: a caller's `take` is a hold that lasts while the
//! caller is not gone (see the bridge's lease in `calls.rs`). Shared by the bridge and the file tests.

use lemnos_core::{
    CallerId, Config as DriverConfig, JobId, JobState, Lifecycle, Reply, Result,
    Status as DriverStatus, contract, driver,
};

/// A device whose driver tracks its callers: `take` remembers the caller that asked, `held` counts
/// the callers it holds for, `who` says the caller's number, `hold_until_cancelled` is a long job
/// that runs until it is cancelled and reports no progress, and `count_until_cancelled` is one that
/// reports progress at every poll, `finish_when_cancelled` is a job that, cancelled, finishes as
/// done a moment later (its real outcome), and `ignore_cancel` is one that never finishes and
/// never reacts to its cancel (a device call that never returns). All are appended after the
/// others, so the contract only grows.
#[contract("tracked", version = 1)]
pub trait Tracked {
    #[method(1)]
    fn take(&mut self) -> Result<()>;
    #[method(2)]
    fn held(&mut self) -> Result<u32>;
    #[method(3)]
    fn who(&mut self) -> Result<u32>;
    #[method(4, job, stall = 60)]
    fn hold_until_cancelled(&mut self) -> Result<JobId>;
    #[method(5, job, stall = 60)]
    fn count_until_cancelled(&mut self) -> Result<JobId>;
    #[method(6, job, stall = 60)]
    fn finish_when_cancelled(&mut self) -> Result<JobId>;
    #[method(7, job, stall = 60)]
    fn ignore_cancel(&mut self) -> Result<JobId>;
}

#[driver("tracked", provides = [Tracked])]
pub struct TrackedDevice {
    calling: CallerId,
    holders: [Option<CallerId>; 8],
    job: Option<(JobId, bool)>,
    counting: Option<(JobId, bool)>,
    count: u64,
    finishing: Option<(JobId, bool)>,
    stubborn: Option<JobId>,
    next: u64,
}

impl Lifecycle for TrackedDevice {
    type Needs = TrackedDeviceNeeds;
    const TRACKS_CALLERS: bool = true;
    const JOBS: bool = true;
    // A counting job is polled a tenth of a second apart, so its progress reaches the host.
    const TICKS: bool = true;

    fn start(_: TrackedDeviceNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {
            calling: CallerId::LOCAL,
            holders: [None; 8],
            job: None,
            counting: None,
            count: 0,
            finishing: None,
            stubborn: None,
            next: 0,
        })
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.counting.is_some().then_some(now_ns + 100_000_000)
    }

    fn caller_gone(&mut self, caller: CallerId) {
        for holder in &mut self.holders {
            if *holder == Some(caller) {
                *holder = None;
            }
        }
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let _ = result;
        if self.stubborn == Some(job) {
            return JobState::Running { done: 0, total: 0 };
        }
        if let Some((id, cancelled)) = self.finishing
            && id == job
        {
            if cancelled {
                self.finishing = None;
                return JobState::Finished(DriverStatus::OK);
            }
            return JobState::Running { done: 0, total: 0 };
        }
        if let Some((id, cancelled)) = self.counting
            && id == job
        {
            if cancelled {
                self.counting = None;
                return JobState::Finished(DriverStatus::CANCELLED);
            }
            self.count += 1;
            return JobState::Running {
                done: self.count,
                total: 0,
            };
        }
        match self.job {
            Some((id, cancelled)) if id == job => {
                if cancelled {
                    self.job = None;
                    JobState::Finished(DriverStatus::CANCELLED)
                } else {
                    JobState::Running { done: 0, total: 0 }
                }
            }
            _ => JobState::Finished(DriverStatus::NOT_FOUND),
        }
    }

    fn job_cancel(&mut self, job: JobId) {
        if self.stubborn == Some(job) {
            return;
        }
        if let Some((id, cancelled)) = &mut self.finishing
            && *id == job
        {
            *cancelled = true;
        }
        if let Some((id, cancelled)) = &mut self.counting
            && *id == job
        {
            *cancelled = true;
        }
        if let Some((id, cancelled)) = &mut self.job
            && *id == job
        {
            *cancelled = true;
        }
    }
}

impl Tracked for TrackedDevice {
    fn take(&mut self) -> Result<()> {
        if self.holders.contains(&Some(self.calling)) {
            return Ok(());
        }
        if let Some(free) = self.holders.iter_mut().find(|h| h.is_none()) {
            *free = Some(self.calling);
        }
        Ok(())
    }

    fn held(&mut self) -> Result<u32> {
        Ok(self.holders.iter().flatten().count() as u32)
    }

    fn who(&mut self) -> Result<u32> {
        Ok(self.calling.0 as u32)
    }

    fn hold_until_cancelled(&mut self) -> Result<JobId> {
        if self.job.is_some() {
            return Err(DriverStatus::BUSY);
        }
        self.next += 1;
        let id = JobId(self.next);
        self.job = Some((id, false));
        Ok(id)
    }

    fn count_until_cancelled(&mut self) -> Result<JobId> {
        if self.counting.is_some() {
            return Err(DriverStatus::BUSY);
        }
        self.next += 1;
        let id = JobId(self.next);
        self.counting = Some((id, false));
        self.count = 0;
        Ok(id)
    }

    fn finish_when_cancelled(&mut self) -> Result<JobId> {
        if self.job.is_some() || self.counting.is_some() || self.finishing.is_some() {
            return Err(DriverStatus::BUSY);
        }
        self.next += 1;
        let id = JobId(self.next);
        self.finishing = Some((id, false));
        Ok(id)
    }

    fn ignore_cancel(&mut self) -> Result<JobId> {
        if self.job.is_some() || self.counting.is_some() || self.stubborn.is_some() {
            return Err(DriverStatus::BUSY);
        }
        self.next += 1;
        let id = JobId(self.next);
        self.stubborn = Some(id);
        Ok(id)
    }
}
