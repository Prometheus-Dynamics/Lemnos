use super::Runtime;
use crate::{
    RuntimeEventCursor, RuntimeEventPoll, RuntimeEventRetentionStats, RuntimeEventSubscription,
};
use lemnos_core::{DeviceStateSnapshot, LemnosEvent, StateEvent};
use lemnos_discovery::InventoryDiff;
use std::mem;
use std::sync::Arc;

impl Runtime {
    pub fn events(&self) -> &[LemnosEvent] {
        self.events.as_slice()
    }

    pub fn subscribe(&self) -> RuntimeEventCursor {
        RuntimeEventCursor::from_index(self.event_tail_index())
    }

    pub fn subscribe_from_start(&self) -> RuntimeEventCursor {
        RuntimeEventCursor::from_index(self.event_base_index)
    }

    pub fn subscribe_blocking(&self) -> RuntimeEventSubscription {
        RuntimeEventSubscription::new(self.subscribe(), Arc::clone(&self.event_notifier))
    }

    pub fn subscribe_from_start_blocking(&self) -> RuntimeEventSubscription {
        RuntimeEventSubscription::new(
            self.subscribe_from_start(),
            Arc::clone(&self.event_notifier),
        )
    }

    pub fn poll_events<'a>(&'a self, cursor: &mut RuntimeEventCursor) -> &'a [LemnosEvent] {
        self.poll_events_with_status(cursor).events()
    }

    pub fn poll_events_with_status<'a>(
        &'a self,
        cursor: &mut RuntimeEventCursor,
    ) -> RuntimeEventPoll<'a> {
        let was_truncated = self.is_cursor_stale(cursor);
        let start_index = cursor
            .next_index()
            .max(self.event_base_index)
            .min(self.event_tail_index());
        let start_offset = start_index - self.event_base_index;
        cursor.advance_to(self.event_tail_index());
        RuntimeEventPoll::new(&self.events[start_offset..], was_truncated)
    }

    pub fn take_events(&mut self) -> Vec<LemnosEvent> {
        let events = mem::take(&mut self.events);
        self.event_base_index += events.len();
        self.retained_event_bytes = 0;
        if !events.is_empty() {
            self.event_notifier.notify_changed();
        }
        events
    }

    pub fn event_retention_stats(&self) -> RuntimeEventRetentionStats {
        RuntimeEventRetentionStats {
            event_base_index: self.event_base_index,
            event_tail_index: self.event_tail_index(),
            retained_events: self.events.len(),
            retained_event_bytes: self.retained_event_bytes,
            max_retained_events: self.config.max_retained_events,
            max_retained_event_bytes: self.config.max_retained_event_bytes,
        }
    }

    pub(crate) fn is_cursor_stale(&self, cursor: &RuntimeEventCursor) -> bool {
        cursor.next_index() < self.event_base_index
    }

    pub(crate) fn pending_event_count(&self, cursor: &RuntimeEventCursor) -> usize {
        self.event_tail_index()
            .saturating_sub(cursor.next_index().max(self.event_base_index))
    }

    pub(crate) fn cache_state(&mut self, state: DeviceStateSnapshot) {
        let state = Arc::new(state);
        let device_id = state.device_id.clone();
        self.push_retained_event(LemnosEvent::State(Box::new(StateEvent::Snapshot(
            Arc::clone(&state),
        ))));
        self.enforce_event_retention();
        self.notify_event_subscribers();
        self.states.insert(state.device_id.clone(), state);
        let retention = self.event_retention_stats();
        super::runtime_debug!(
            device_id = ?device_id,
            retained_events = retention.retained_events,
            retained_event_bytes = retention.retained_event_bytes,
            "runtime cached device state"
        );
        #[cfg(not(feature = "tracing"))]
        let _ = (&device_id, retention);
    }

    pub(super) fn record_inventory_events(&mut self, diff: &InventoryDiff) {
        let prior_tail = self.event_tail_index();
        for event in diff.events() {
            self.push_retained_event(LemnosEvent::Inventory(Box::new(event)));
        }
        self.enforce_event_retention();
        let new_tail = self.event_tail_index();
        if new_tail != prior_tail {
            let retention = self.event_retention_stats();
            self.notify_event_subscribers();
            super::runtime_debug!(
                added = diff.added.len(),
                removed = diff.removed.len(),
                changed = diff.changed.len(),
                retained_events = retention.retained_events,
                retained_event_bytes = retention.retained_event_bytes,
                "runtime recorded inventory events"
            );
            #[cfg(not(feature = "tracing"))]
            let _ = retention;
        }
    }

    fn event_tail_index(&self) -> usize {
        self.event_base_index + self.events.len()
    }

    fn push_retained_event(&mut self, event: LemnosEvent) {
        self.retained_event_bytes += super::retention::estimated_retained_event_bytes(&event);
        self.events.push(event);
    }

    pub(super) fn enforce_event_retention(&mut self) {
        let dropped_events = self
            .events
            .len()
            .saturating_sub(self.config.max_retained_events);
        #[cfg(feature = "tracing")]
        let mut dropped_bytes = 0;
        let mut drop_count = dropped_events;

        if let Some(limit) = self.config.max_retained_event_bytes {
            let mut retained_bytes = self.retained_event_bytes;
            for event in self.events.iter().skip(drop_count) {
                if retained_bytes <= limit {
                    break;
                }
                retained_bytes = retained_bytes
                    .saturating_sub(super::retention::estimated_retained_event_bytes(event));
                drop_count += 1;
            }
        }

        if drop_count == 0 {
            return;
        }

        for event in self.events.drain(0..drop_count) {
            let event_bytes = super::retention::estimated_retained_event_bytes(&event);
            #[cfg(feature = "tracing")]
            {
                dropped_bytes += event_bytes;
            }
            self.retained_event_bytes = self.retained_event_bytes.saturating_sub(event_bytes);
        }

        self.event_base_index += drop_count;
        self.event_notifier.notify_changed();
        #[cfg(feature = "tracing")]
        {
            super::runtime_debug!(
                dropped_events = drop_count,
                dropped_event_bytes = dropped_bytes,
                retained_events = self.events.len(),
                retained_event_bytes = self.retained_event_bytes,
                event_base_index = self.event_base_index,
                "runtime compacted retained events"
            );
        }
    }

    fn notify_event_subscribers(&self) {
        self.event_notifier.advance_to(self.event_tail_index());
    }
}
