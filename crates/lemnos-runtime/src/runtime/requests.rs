use super::*;
use lemnos_core::DeviceDescriptor;
use std::borrow::Cow;

impl Runtime {
    pub(crate) fn validated_request_device(
        &self,
        request: &DeviceRequest,
    ) -> RuntimeResult<DeviceDescriptor> {
        self.ensure_running()?;
        let device = self
            .inventory
            .get(&request.device_id)
            .cloned()
            .ok_or_else(|| RuntimeError::UnknownDevice {
                device_id: request.device_id.clone(),
            })?;
        Self::validate_request_for_device_descriptor(request, &device)?;
        Ok(device)
    }

    pub(crate) fn validate_request_for_device_descriptor(
        request: &DeviceRequest,
        device: &DeviceDescriptor,
    ) -> RuntimeResult<()> {
        request
            .validate_for(device)
            .map_err(|source| RuntimeError::InvalidRequest {
                device_id: request.device_id.clone(),
                source: Box::new(source),
            })
    }

    pub(crate) fn prepare_request_binding(&mut self, request: &DeviceRequest) -> RuntimeResult<()> {
        let device = self.validated_request_device(request)?;

        if !self.bindings.contains_key(&request.device_id) {
            if !self.config.auto_bind_on_request {
                return Err(RuntimeError::DeviceNotBound {
                    device_id: request.device_id.clone(),
                });
            }
            self.bind_discovered_device(&device)?;
        }

        Ok(())
    }
    pub fn request(&mut self, request: DeviceRequest) -> RuntimeResult<DeviceResponse> {
        let started_at = std::time::Instant::now();
        let device_id = request.device_id.clone();
        let was_bound = self.bindings.contains_key(&device_id);
        let interaction_name = interaction_name_owned(&request.interaction);
        runtime_debug!(
            device_id = ?device_id,
            interaction = %interaction_name,
            auto_bind_on_request = self.config.auto_bind_on_request,
            already_bound = was_bound,
            "runtime request dispatch starting"
        );
        let result = self.request_inner(request);
        self.complete_operation(device_id.clone(), RuntimeFailureOperation::Request, &result);
        match &result {
            Ok(_response) => {
                runtime_info!(
                    device_id = ?_response.device_id,
                    interaction = %interaction_name,
                    auto_bound = !was_bound,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "runtime request completed"
                );
            }
            Err(_error) => {
                let _failure = self.failures.get(&device_id);
                runtime_warn!(
                    device_id = ?device_id,
                    category = ?_failure.map(|failure| failure.category),
                    driver_id = ?_failure.and_then(|failure| failure.driver_id.as_ref().map(|driver_id| driver_id.as_str())),
                    interaction = %interaction_name,
                    error = %_error,
                    auto_bound = !was_bound,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "runtime request failed"
                );
            }
        }
        #[cfg(not(feature = "tracing"))]
        let _ = (&started_at, &interaction_name);
        if result.is_ok()
            && !was_bound
            && let Ok(response) = &result
        {
            self.desired_bindings.insert(response.device_id.clone());
        }
        result
    }

    fn request_inner(&mut self, request: DeviceRequest) -> RuntimeResult<DeviceResponse> {
        self.prepare_request_binding(&request)?;

        let bound =
            self.bound_device(&request.device_id)
                .ok_or_else(|| RuntimeError::DeviceNotBound {
                    device_id: request.device_id.clone(),
                })?;
        let (interaction, cached_state) = {
            let mut bound = bound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime_debug!(
                device_id = ?request.device_id,
                driver_id = bound.driver_id(),
                "runtime dispatching request to bound device"
            );
            let interaction =
                bound
                    .execute(&request.interaction)
                    .map_err(|source| RuntimeError::Driver {
                        device_id: request.device_id.clone(),
                        source: Box::new(source),
                    })?;

            let cached_state = if self.config.cache_state_on_request {
                bound.state().map_err(|source| RuntimeError::Driver {
                    device_id: request.device_id.clone(),
                    source: Box::new(source),
                })?
            } else {
                None
            };
            (interaction, cached_state)
        };

        if let Some(state) = cached_state {
            self.cache_state(state);
        }

        Ok(DeviceResponse::new(request.device_id, interaction))
    }
}

pub(crate) fn interaction_name_owned(
    request: &lemnos_core::InteractionRequest,
) -> Cow<'static, str> {
    match request {
        lemnos_core::InteractionRequest::Standard(request) => Cow::Borrowed(request.name()),
        lemnos_core::InteractionRequest::Custom(request) => {
            Cow::Owned(request.id.as_str().to_string())
        }
    }
}
