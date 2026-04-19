use super::*;
use lemnos_core::DeviceDescriptor;
use std::sync::Arc;

pub(crate) struct PreparedBinding {
    descriptor: DeviceDescriptor,
    driver: Arc<dyn Driver>,
    backends: RuntimeBackends,
    cache_state_on_bind: bool,
}

pub(crate) struct PreparedBindingOutput {
    pub bound: Box<dyn BoundDevice>,
    pub initial_state: Option<DeviceStateSnapshot>,
}

pub(crate) struct DetachedBoundDevice {
    pub device_id: DeviceId,
    pub binding: SharedBoundDevice,
}

impl PreparedBinding {
    pub(crate) fn bind(self) -> RuntimeResult<PreparedBindingOutput> {
        let mut bound = self
            .driver
            .bind(&self.descriptor, &self.backends.bind_context())
            .map_err(|source| RuntimeError::Driver {
                device_id: self.descriptor.id.clone(),
                source: Box::new(source),
            })?;

        let initial_state = if self.cache_state_on_bind {
            bound.state().map_err(|source| RuntimeError::Driver {
                device_id: self.descriptor.id.clone(),
                source: Box::new(source),
            })?
        } else {
            None
        };

        Ok(PreparedBindingOutput {
            bound,
            initial_state,
        })
    }
}

impl Runtime {
    pub fn bind(&mut self, device_id: &DeviceId) -> RuntimeResult<()> {
        let started_at = std::time::Instant::now();
        let result = self.bind_device_by_id(device_id);
        self.complete_operation(device_id.clone(), RuntimeFailureOperation::Bind, &result);
        match &result {
            Ok(()) => {
                runtime_info!(
                    device_id = ?device_id,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "runtime device bound"
                );
            }
            Err(_error) => {
                let _failure = self.failures.get(device_id);
                runtime_warn!(
                    device_id = ?device_id,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    category = ?_failure.map(|failure| failure.category),
                    driver_id = ?_failure.and_then(|failure| failure.driver_id.as_ref().map(|driver_id| driver_id.as_str())),
                    error = %_error,
                    "runtime device bind failed"
                );
            }
        }
        #[cfg(not(feature = "tracing"))]
        let _ = &started_at;
        if result.is_ok() {
            self.desired_bindings.insert(device_id.clone());
        }
        result
    }

    pub fn unbind(&mut self, device_id: &DeviceId) -> bool {
        let started_at = std::time::Instant::now();
        let (
            removed_anything,
            _removed_binding,
            _removed_interest,
            _removed_state,
            _removed_failure,
            detached,
        ) = self.unbind_detached(device_id);
        if let Some(detached) = detached {
            close_detached_binding(detached);
        }
        if removed_anything {
            runtime_info!(
                device_id = ?device_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                removed_binding = _removed_binding,
                removed_interest = _removed_interest,
                removed_state = _removed_state,
                removed_failure = _removed_failure,
                "runtime device unbound"
            );
        }
        #[cfg(not(feature = "tracing"))]
        let _ = &started_at;
        removed_anything
    }

    pub fn refresh_state(
        &mut self,
        device_id: &DeviceId,
    ) -> RuntimeResult<Option<&DeviceStateSnapshot>> {
        let _ = self.refresh_state_shared(device_id)?;
        Ok(self.state(device_id))
    }

    pub fn refresh_state_shared(
        &mut self,
        device_id: &DeviceId,
    ) -> RuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        self.ensure_running()?;
        let Some(bound) = self.bound_device(device_id) else {
            self.failures.remove(device_id);
            runtime_debug!(
                device_id = ?device_id,
                "runtime refresh_state skipped because device is not bound"
            );
            return Ok(self.shared_state(device_id));
        };

        let snapshot = {
            let mut bound = lock_bound(&bound);
            bound.state().map_err(|source| RuntimeError::Driver {
                device_id: device_id.clone(),
                source: Box::new(source),
            })
        };
        self.complete_operation(
            device_id.clone(),
            RuntimeFailureOperation::RefreshState,
            &snapshot,
        );
        let snapshot = snapshot?;
        let cached_state = snapshot.is_some();
        if let Some(snapshot) = snapshot {
            self.cache_state(snapshot);
        }

        runtime_debug!(
            device_id = ?device_id,
            cached_state = cached_state,
            "runtime state refreshed"
        );
        #[cfg(not(feature = "tracing"))]
        let _ = cached_state;

        Ok(self.shared_state(device_id))
    }

    pub(super) fn bind_device_by_id(&mut self, device_id: &DeviceId) -> RuntimeResult<()> {
        self.ensure_running()?;
        if self.bindings.contains_key(device_id) {
            runtime_debug!(device_id = ?device_id, "runtime bind skipped because device is already bound");
            return Ok(());
        }

        let descriptor =
            self.inventory
                .get(device_id)
                .cloned()
                .ok_or_else(|| RuntimeError::UnknownDevice {
                    device_id: device_id.clone(),
                })?;
        self.bind_discovered_device(&descriptor)
    }

    pub(super) fn bind_discovered_device(
        &mut self,
        descriptor: &DeviceDescriptor,
    ) -> RuntimeResult<()> {
        self.ensure_running()?;
        if self.bindings.contains_key(&descriptor.id) {
            runtime_debug!(device_id = ?descriptor.id, "runtime bind skipped because device is already bound");
            return Ok(());
        }

        let prepared = self.prepare_binding(descriptor)?;
        let output = prepared.bind()?;
        self.store_bound_device(descriptor.id.clone(), output);
        Ok(())
    }

    pub(crate) fn prepare_binding(
        &self,
        descriptor: &DeviceDescriptor,
    ) -> RuntimeResult<PreparedBinding> {
        runtime_debug!(
            device_id = ?descriptor.id,
            interface = ?descriptor.interface,
            kind = ?descriptor.kind,
            driver_hint = ?descriptor.match_hints.driver_hint,
            "runtime resolving driver for bind"
        );
        let candidate = self.registry.resolve(descriptor)?;
        runtime_debug!(
            device_id = ?descriptor.id,
            driver_id = candidate.driver.id(),
            interface = ?descriptor.interface,
            kind = ?descriptor.kind,
            "runtime binding device"
        );
        Ok(PreparedBinding {
            descriptor: descriptor.clone(),
            driver: candidate.driver,
            backends: self.backends.clone(),
            cache_state_on_bind: self.config.cache_state_on_bind,
        })
    }

    pub(crate) fn store_bound_device(
        &mut self,
        device_id: DeviceId,
        output: PreparedBindingOutput,
    ) {
        if let Some(state) = output.initial_state {
            self.cache_state(state);
        }

        self.bindings
            .insert(device_id, Arc::new(Mutex::new(output.bound)));
    }

    pub(super) fn close_all_bound_devices(&mut self) {
        close_detached_bindings(self.take_all_bound_devices());
    }

    pub(crate) fn bound_device(&self, device_id: &DeviceId) -> Option<SharedBoundDevice> {
        self.bindings.get(device_id).map(Arc::clone)
    }

    pub(crate) fn shutdown_detached(&mut self) -> Vec<DetachedBoundDevice> {
        self.running = false;
        let detached = self.take_all_bound_devices();
        self.probe_inventory = ProbeInventoryIndex::default();
        self.desired_bindings.clear();
        self.states.clear();
        self.failures.clear();
        self.event_notifier.notify_changed();
        detached
    }

    pub(crate) fn unbind_detached(
        &mut self,
        device_id: &DeviceId,
    ) -> (bool, bool, bool, bool, bool, Option<DetachedBoundDevice>) {
        let detached = self.take_bound_device(device_id);
        let removed_binding = detached.is_some();
        let removed_interest = self.desired_bindings.remove(device_id);
        let removed_state = self.states.remove(device_id).is_some();
        let removed_failure = self.failures.remove(device_id).is_some();
        let removed_anything =
            removed_binding || removed_interest || removed_state || removed_failure;
        (
            removed_anything,
            removed_binding,
            removed_interest,
            removed_state,
            removed_failure,
            detached,
        )
    }

    pub(crate) fn detach_invalidated_bindings(
        &mut self,
        diff: &InventoryDiff,
        invalidated_bindings: &std::collections::BTreeSet<DeviceId>,
    ) -> Vec<DetachedBoundDevice> {
        let mut detached = Vec::new();
        for device_id in &diff.removed {
            if let Some(binding) = self.take_bound_device(device_id) {
                detached.push(binding);
            }
            self.states.remove(device_id);
            self.failures.remove(device_id);
        }

        for changed in &diff.changed {
            if !invalidated_bindings.contains(&changed.current.id) {
                continue;
            }

            if let Some(binding) = self.take_bound_device(&changed.current.id) {
                detached.push(binding);
            }
            self.states.remove(&changed.current.id);
            self.failures.remove(&changed.current.id);
        }
        detached
    }

    fn take_all_bound_devices(&mut self) -> Vec<DetachedBoundDevice> {
        std::mem::take(&mut self.bindings)
            .into_iter()
            .map(|(device_id, binding)| DetachedBoundDevice { device_id, binding })
            .collect()
    }

    fn take_bound_device(&mut self, device_id: &DeviceId) -> Option<DetachedBoundDevice> {
        self.bindings
            .remove(device_id)
            .map(|binding| DetachedBoundDevice {
                device_id: device_id.clone(),
                binding,
            })
    }
}

pub(crate) fn lock_bound(
    binding: &SharedBoundDevice,
) -> std::sync::MutexGuard<'_, Box<dyn BoundDevice>> {
    binding
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn close_detached_binding(detached: DetachedBoundDevice) {
    #[cfg(not(feature = "tracing"))]
    let _ = &detached.device_id;
    if let Err(error) = lock_bound(&detached.binding).close() {
        runtime_warn!(
            device_id = ?detached.device_id,
            error = %error,
            "runtime failed to close binding during teardown"
        );
        #[cfg(not(feature = "tracing"))]
        let _ = &error;
    }
}

pub(crate) fn close_detached_bindings(detached: Vec<DetachedBoundDevice>) {
    for detached in detached {
        close_detached_binding(detached);
    }
}
