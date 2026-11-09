use std::collections::BTreeMap;

use super::rules::{Due, Next, advance, choose};

/// How fast a device can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevicePace {
    /// The device makes a new value this often (0: whenever it's read).
    pub sample_period_us: u64,
    /// Samples it can buffer. 0 means it holds only the latest.
    pub buffered_samples: u32,
}

/// What a subscriber actually got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    pub id: u64,
    pub period_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NoPeriod,
    NoValues,
    NoDevice,
}

/// One read to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadPlan {
    pub device: u32,
    /// Which values to read. A driver may read less when bits are clear.
    pub mask: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read(ReadPlan),
    WaitUntil(u64),
    Idle,
}

struct Subscription {
    device: u32,
    mask: u64,
    period_us: u64,
    only_changes: bool,
    /// What was last delivered, reused so filtering allocates nothing.
    last: Vec<u8>,
    delivered: bool,
}

#[derive(Default)]
struct DeviceSchedule {
    pace: DevicePace,
    period_us: u64,
    mask: u64,
    deadline_us: u64,
    cost_us: u64,
    /// Extra values wanted by a one-shot read, for the next read only.
    once_mask: u64,
    once: bool,
}

/// Decides what is read when.
#[derive(Default)]
pub struct Scheduler {
    next_id: u64,
    subscriptions: BTreeMap<u64, Subscription>,
    devices: BTreeMap<u32, DeviceSchedule>,
    due: Vec<Due>,
}

/// The period to read at: never faster than the device makes values or than
/// one read takes. A buffering device is read in batches, a whole number of
/// samples apart, and never so rarely that its buffer overflows.
fn fit(requested_us: u64, pace: DevicePace, cost_us: u64) -> u64 {
    let period = requested_us.max(cost_us);
    let sample = pace.sample_period_us;
    if sample == 0 {
        return period;
    }
    if pace.buffered_samples == 0 {
        return period.max(sample);
    }
    let samples = (period / sample).clamp(1, u64::from(pace.buffered_samples));
    samples * sample
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers what a device can do. Needed before anyone subscribes to it.
    pub fn add_device(&mut self, device: u32, pace: DevicePace) {
        self.devices.entry(device).or_default().pace = pace;
    }

    pub fn remove_device(&mut self, device: u32) {
        self.devices.remove(&device);
        self.subscriptions.retain(|_, s| s.device != device);
    }

    /// Asks for a device's values, and says what was granted.
    pub fn subscribe(
        &mut self,
        device: u32,
        mask: u64,
        requested_us: u64,
        only_changes: bool,
        now_us: u64,
    ) -> Result<Grant, Refusal> {
        if requested_us == 0 {
            return Err(Refusal::NoPeriod);
        }
        if mask == 0 {
            return Err(Refusal::NoValues);
        }
        let schedule = self.devices.get(&device).ok_or(Refusal::NoDevice)?;
        let period_us = fit(requested_us, schedule.pace, schedule.cost_us);
        self.next_id += 1;
        let id = self.next_id;
        self.subscriptions.insert(
            id,
            Subscription {
                device,
                mask,
                period_us,
                only_changes,
                last: Vec::new(),
                delivered: false,
            },
        );
        self.recompute(device, now_us);
        Ok(Grant { id, period_us })
    }

    pub fn unsubscribe(&mut self, id: u64, now_us: u64) {
        if let Some(gone) = self.subscriptions.remove(&id) {
            self.recompute(gone.device, now_us);
        }
    }

    fn recompute(&mut self, device: u32, now_us: u64) {
        let (mut period, mut mask) = (0, 0);
        for subscription in self.subscriptions.values() {
            if subscription.device == device {
                mask |= subscription.mask;
                period = match period {
                    0 => subscription.period_us,
                    p => p.min(subscription.period_us),
                };
            }
        }
        if let Some(schedule) = self.devices.get_mut(&device) {
            if schedule.period_us == 0 && period != 0 {
                schedule.deadline_us = now_us;
            }
            schedule.period_us = period;
            schedule.mask = mask;
        }
    }

    /// Reads a device once, as soon as it can go, with these values added to
    /// whatever subscriptions already want.
    pub fn read_once(&mut self, device: u32, mask: u64, now_us: u64) -> Result<(), Refusal> {
        let schedule = self.devices.get_mut(&device).ok_or(Refusal::NoDevice)?;
        schedule.once = true;
        schedule.once_mask |= mask;
        schedule.deadline_us = schedule.deadline_us.min(now_us);
        Ok(())
    }

    /// What to do at `now_us`.
    pub fn next(&mut self, now_us: u64) -> Action {
        self.due.clear();
        let mut earliest = None::<u64>;
        for (device, schedule) in &self.devices {
            if schedule.period_us == 0 && !schedule.once {
                continue;
            }
            self.due.push(Due {
                index: *device as usize,
                deadline_us: schedule.deadline_us,
                period_us: schedule.period_us.max(1),
                cost_us: schedule.cost_us,
            });
            earliest = Some(earliest.map_or(schedule.deadline_us, |e| e.min(schedule.deadline_us)));
        }
        match choose(&self.due, now_us) {
            Next::Run(index) => {
                let device = index as u32;
                let schedule = &self.devices[&device];
                Action::Read(ReadPlan {
                    device,
                    mask: schedule.mask | schedule.once_mask,
                })
            }
            Next::WaitUntil(at) => Action::WaitUntil(at),
            Next::Idle => earliest.map_or(Action::Idle, Action::WaitUntil),
        }
    }

    /// Records a finished read, and moves the device to its next deadline.
    pub fn finished(&mut self, device: u32, started_us: u64, ended_us: u64) {
        let Some(schedule) = self.devices.get_mut(&device) else {
            return;
        };
        schedule.cost_us = ended_us.saturating_sub(started_us);
        schedule.once = false;
        schedule.once_mask = 0;
        if schedule.period_us != 0 {
            schedule.deadline_us = advance(schedule.deadline_us, schedule.period_us, ended_us);
        }
    }

    /// Whether this subscription should be given `values`. With
    /// `only_changes`, a repeat is dropped.
    pub fn should_deliver(&mut self, id: u64, values: &[u8]) -> bool {
        let Some(subscription) = self.subscriptions.get_mut(&id) else {
            return false;
        };
        if subscription.only_changes && subscription.delivered && subscription.last == values {
            return false;
        }
        subscription.delivered = true;
        subscription.last.clear();
        subscription.last.extend_from_slice(values);
        true
    }

    /// Subscribers of `device`, with the values each asked for.
    pub fn subscribers(&self, device: u32) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.subscriptions
            .iter()
            .filter(move |(_, s)| s.device == device)
            .map(|(id, s)| (*id, s.mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLLED: DevicePace = DevicePace {
        sample_period_us: 2_500,
        buffered_samples: 0,
    };
    const BUFFERED: DevicePace = DevicePace {
        sample_period_us: 2_500,
        buffered_samples: 16,
    };

    fn with_device(pace: DevicePace) -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.add_device(1, pace);
        scheduler
    }

    #[test]
    fn nothing_is_read_until_someone_asks() {
        let mut scheduler = with_device(POLLED);
        assert_eq!(scheduler.next(1_000_000), Action::Idle);
    }

    #[test]
    fn a_subscribe_says_what_it_got_and_why_it_was_refused() {
        let mut scheduler = with_device(POLLED);
        assert_eq!(
            scheduler.subscribe(1, 1, 0, false, 0),
            Err(Refusal::NoPeriod)
        );
        assert_eq!(
            scheduler.subscribe(1, 0, 10, false, 0),
            Err(Refusal::NoValues)
        );
        assert_eq!(
            scheduler.subscribe(9, 1, 10, false, 0),
            Err(Refusal::NoDevice)
        );
        let grant = scheduler.subscribe(1, 1, 10_000, false, 0).unwrap();
        assert_eq!(grant.period_us, 10_000);
    }

    #[test]
    fn a_polled_device_clamps_to_its_own_period() {
        let mut scheduler = with_device(POLLED);
        let grant = scheduler.subscribe(1, 1, 100, false, 0).unwrap();
        assert_eq!(grant.period_us, 2_500);
    }

    #[test]
    fn the_grant_is_at_least_the_read_time() {
        let mut scheduler = with_device(DevicePace::default());
        let first = scheduler.subscribe(1, 1, 100, false, 0).unwrap();
        assert_eq!(first.period_us, 100);
        scheduler.finished(1, 0, 900);
        let second = scheduler.subscribe(1, 1, 100, false, 900).unwrap();
        assert_eq!(second.period_us, 900);
    }

    #[test]
    fn a_buffered_device_is_read_a_whole_number_of_samples_apart() {
        let mut scheduler = with_device(BUFFERED);
        let grant = scheduler.subscribe(1, 1, 11_000, false, 0).unwrap();
        assert_eq!(grant.period_us, 10_000);
    }

    #[test]
    fn a_buffered_device_is_never_left_to_overflow() {
        let mut scheduler = with_device(BUFFERED);
        let grant = scheduler.subscribe(1, 1, 10_000_000, false, 0).unwrap();
        assert_eq!(grant.period_us, 16 * 2_500);
    }

    #[test]
    fn a_buffered_device_is_read_at_least_a_sample_apart() {
        let mut scheduler = with_device(BUFFERED);
        let grant = scheduler.subscribe(1, 1, 1, false, 0).unwrap();
        assert_eq!(grant.period_us, 2_500);
    }

    #[test]
    fn subscribers_combine_to_the_fastest_period_and_all_their_values() {
        let mut scheduler = with_device(DevicePace::default());
        let slow = scheduler.subscribe(1, 0b01, 10_000, false, 0).unwrap();
        scheduler.subscribe(1, 0b10, 5_000, false, 0).unwrap();
        assert_eq!(
            scheduler.next(0),
            Action::Read(ReadPlan {
                device: 1,
                mask: 0b11
            })
        );
        scheduler.finished(1, 0, 10);
        assert_eq!(scheduler.next(10), Action::WaitUntil(5_000));

        scheduler.unsubscribe(slow.id, 10);
        assert_eq!(
            scheduler.next(5_000),
            Action::Read(ReadPlan {
                device: 1,
                mask: 0b10
            })
        );
    }

    #[test]
    fn the_last_unsubscribe_stops_the_reads() {
        let mut scheduler = with_device(DevicePace::default());
        let grant = scheduler.subscribe(1, 1, 1_000, false, 0).unwrap();
        scheduler.unsubscribe(grant.id, 0);
        assert_eq!(scheduler.next(10_000), Action::Idle);
    }

    #[test]
    fn a_one_shot_during_a_masked_subscription_gets_every_value() {
        let mut scheduler = with_device(DevicePace::default());
        scheduler.subscribe(1, 0b001, 1_000_000, false, 0).unwrap();
        scheduler.finished(1, 0, 10);

        scheduler.read_once(1, 0b111, 20).unwrap();
        assert_eq!(
            scheduler.next(20),
            Action::Read(ReadPlan {
                device: 1,
                mask: 0b111
            })
        );
        scheduler.finished(1, 20, 30);

        // The next scheduled read is back to what the subscription asked for.
        assert_eq!(
            scheduler.next(2_000_000),
            Action::Read(ReadPlan {
                device: 1,
                mask: 0b001
            })
        );
    }

    #[test]
    fn a_one_shot_works_with_no_subscribers() {
        let mut scheduler = with_device(DevicePace::default());
        scheduler.read_once(1, 1, 5).unwrap();
        assert!(matches!(scheduler.next(5), Action::Read(_)));
        scheduler.finished(1, 5, 6);
        assert_eq!(scheduler.next(100), Action::Idle);
    }

    #[test]
    fn the_repeat_filter_drops_only_repeats() {
        let mut scheduler = with_device(DevicePace::default());
        let grant = scheduler.subscribe(1, 1, 1_000, true, 0).unwrap();
        assert!(scheduler.should_deliver(grant.id, &[1, 2]));
        assert!(!scheduler.should_deliver(grant.id, &[1, 2]));
        assert!(scheduler.should_deliver(grant.id, &[1, 3]));
    }

    #[test]
    fn without_the_filter_repeats_are_delivered() {
        let mut scheduler = with_device(DevicePace::default());
        let grant = scheduler.subscribe(1, 1, 1_000, false, 0).unwrap();
        assert!(scheduler.should_deliver(grant.id, &[1]));
        assert!(scheduler.should_deliver(grant.id, &[1]));
    }

    #[test]
    fn a_slow_device_does_not_delay_a_fast_one() {
        let mut scheduler = Scheduler::new();
        scheduler.add_device(1, DevicePace::default());
        scheduler.add_device(2, DevicePace::default());
        scheduler.subscribe(1, 1, 1_000, false, 0).unwrap();
        scheduler.subscribe(2, 1, 100_000, false, 0).unwrap();
        scheduler.finished(1, 0, 100);
        scheduler.finished(2, 100, 2_100);
        // The slow one costs 2 ms and the fast one is due at 1 ms, so the
        // fast one goes first when both are due.
        assert!(matches!(
            scheduler.next(100_000),
            Action::Read(ReadPlan { device: 1, .. })
        ));
    }

    #[test]
    fn removing_a_device_removes_its_subscriptions() {
        let mut scheduler = with_device(DevicePace::default());
        let grant = scheduler.subscribe(1, 1, 1_000, false, 0).unwrap();
        scheduler.remove_device(1);
        assert_eq!(scheduler.subscribers(1).count(), 0);
        assert!(!scheduler.should_deliver(grant.id, &[1]));
    }
}
