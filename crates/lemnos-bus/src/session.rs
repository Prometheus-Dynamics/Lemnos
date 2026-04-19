use crate::BusResult;
use lemnos_core::{DeviceDescriptor, InterfaceKind, TimestampMs};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionAccess {
    SharedReadOnly,
    Shared,
    Exclusive,
    ExclusiveController,
}

impl SessionAccess {
    pub const fn can_read(self) -> bool {
        true
    }

    pub const fn can_write(self) -> bool {
        !matches!(self, Self::SharedReadOnly)
    }

    pub const fn is_exclusive(self) -> bool {
        matches!(self, Self::Exclusive | Self::ExclusiveController)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum SessionState {
    #[default]
    Open,
    Idle,
    Busy,
    Stale,
    Faulted,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub backend_name: String,
    pub access: SessionAccess,
    pub state: SessionState,
    pub opened_at: Option<TimestampMs>,
    pub last_active_at: Option<TimestampMs>,
}

impl SessionMetadata {
    pub fn new(backend_name: impl Into<String>, access: SessionAccess) -> Self {
        let now = current_timestamp_ms();
        Self {
            backend_name: backend_name.into(),
            access,
            state: SessionState::Open,
            opened_at: Some(now),
            last_active_at: Some(now),
        }
    }

    pub fn with_state(mut self, state: SessionState) -> Self {
        self.state = state;
        self
    }

    pub fn with_opened_at(mut self, opened_at: TimestampMs) -> Self {
        self.opened_at = Some(opened_at);
        self
    }

    pub fn with_last_active_at(mut self, last_active_at: TimestampMs) -> Self {
        self.last_active_at = Some(last_active_at);
        self
    }

    pub fn mark_idle(&mut self) {
        self.touch_now();
        self.state = SessionState::Idle;
    }

    pub fn mark_busy(&mut self) {
        self.touch_now();
        self.state = SessionState::Busy;
    }

    pub fn mark_stale(&mut self) {
        self.touch_now();
        self.state = SessionState::Stale;
    }

    pub fn mark_faulted(&mut self) {
        self.touch_now();
        self.state = SessionState::Faulted;
    }

    pub fn mark_closed(&mut self) {
        self.touch_now();
        self.state = SessionState::Closed;
    }

    pub fn begin_call(&mut self) {
        self.ensure_opened_at();
        self.mark_busy();
    }

    pub fn finish_call<T>(&mut self, result: &BusResult<T>) {
        if result.is_ok() {
            self.mark_idle();
        } else {
            self.mark_faulted();
        }
    }

    pub fn touch_now(&mut self) {
        let now = current_timestamp_ms();
        self.ensure_opened_at_with(now);
        self.last_active_at = Some(now);
    }

    fn ensure_opened_at(&mut self) {
        self.ensure_opened_at_with(current_timestamp_ms());
    }

    fn ensure_opened_at_with(&mut self, now: TimestampMs) {
        if self.opened_at.is_none() {
            self.opened_at = Some(now);
        }
    }
}

fn current_timestamp_ms() -> TimestampMs {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    TimestampMs::new(u64::try_from(millis).unwrap_or(u64::MAX))
}

pub trait BusSession: Send + Sync {
    fn interface(&self) -> InterfaceKind;
    fn device(&self) -> &DeviceDescriptor;
    fn metadata(&self) -> &SessionMetadata;
    fn close(&mut self) -> BusResult<()>;
}

pub trait StreamSession: BusSession {
    type Event: Send + Sync + Clone + PartialEq + Eq + 'static;

    fn poll_events(
        &mut self,
        max_events: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<Self::Event>>;
}
