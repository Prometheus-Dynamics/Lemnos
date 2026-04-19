use lemnos_core::{InterfaceKind, TimestampMs, Value, ValueMap};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryContext {
    pub observed_at: Option<TimestampMs>,
    pub requested_interfaces: BTreeSet<InterfaceKind>,
    pub inline_probe_threshold: Option<usize>,
    pub max_parallel_probe_workers: Option<usize>,
    pub properties: ValueMap,
}

impl DiscoveryContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_observed_at(mut self, observed_at: TimestampMs) -> Self {
        self.observed_at = Some(observed_at);
        self
    }

    pub fn with_requested_interface(mut self, interface: InterfaceKind) -> Self {
        self.requested_interfaces.insert(interface);
        self
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    pub fn with_inline_probe_threshold(mut self, inline_probe_threshold: usize) -> Self {
        self.inline_probe_threshold = Some(inline_probe_threshold);
        self
    }

    pub fn with_max_parallel_probe_workers(mut self, max_parallel_probe_workers: usize) -> Self {
        self.max_parallel_probe_workers = Some(max_parallel_probe_workers);
        self
    }

    pub fn wants(&self, interface: InterfaceKind) -> bool {
        self.requested_interfaces.is_empty() || self.requested_interfaces.contains(&interface)
    }
}
