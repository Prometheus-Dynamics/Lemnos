use super::*;
use crate::async_runtime::sync::{lock, read_lock, write_lock};

impl AsyncRuntime {
    pub async fn bind(&self, device_id: DeviceId) -> AsyncRuntimeResult<()> {
        let _started_at = std::time::Instant::now();
        let bind_lock = self.bind_lock(&device_id);
        let inner = Arc::clone(&self.inner);

        let result: RuntimeResult<bool> = {
            let device_id = device_id.clone();
            tokio::task::spawn_blocking(move || {
                let bind_guard = lock(&bind_lock);
                let _ = &bind_guard;

                let prepared = {
                    let runtime = read_lock(&inner);
                    runtime.ensure_running()?;
                    if runtime.is_bound(&device_id) {
                        return Ok(false);
                    }

                    let descriptor =
                        runtime
                            .inventory()
                            .get(&device_id)
                            .cloned()
                            .ok_or_else(|| RuntimeError::UnknownDevice {
                                device_id: device_id.clone(),
                            })?;
                    runtime.prepare_binding(&descriptor)?
                };

                let output = prepared.bind()?;

                let mut runtime = write_lock(&inner);
                if !runtime.is_running() {
                    close_prepared_binding_output(&device_id, output);
                    return Err(RuntimeError::NotRunning);
                }

                if runtime.is_bound(&device_id) {
                    close_prepared_binding_output(&device_id, output);
                    return Ok(false);
                }

                runtime.store_bound_device(device_id.clone(), output);
                Ok(true)
            })
            .await?
        };
        let (result, _failure) = self
            .run_blocking({
                let device_id = device_id.clone();
                move |runtime| {
                    runtime.complete_operation(
                        device_id.clone(),
                        crate::RuntimeFailureOperation::Bind,
                        &result,
                    );
                    if matches!(result, Ok(true)) {
                        runtime.mark_desired_binding(device_id.clone());
                    }
                    let failure = runtime.failure(&device_id).cloned();
                    (result, failure)
                }
            })
            .await?;
        match result {
            Ok(_stored_binding) => {
                runtime_info_async!(
                    device_id = ?device_id,
                    stored_binding = _stored_binding,
                    elapsed_ms = _started_at.elapsed().as_millis() as u64,
                    "async runtime device bound"
                );
                Ok(())
            }
            Err(error) => {
                runtime_warn_async!(
                    device_id = ?device_id,
                    elapsed_ms = _started_at.elapsed().as_millis() as u64,
                    category = ?_failure.as_ref().map(|failure| failure.category),
                    driver_id = ?_failure.as_ref().and_then(|failure| failure.driver_id.as_ref().map(|driver_id| driver_id.as_str())),
                    error = %error,
                    "async runtime device bind failed"
                );
                Err(AsyncRuntimeError::from(error))
            }
        }
    }

    pub async fn unbind(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        let inner = Arc::clone(&self.inner);
        Ok(tokio::task::spawn_blocking(move || {
            let (removed_anything, detached) = {
                let mut runtime = write_lock(&inner);
                let (removed_anything, _, _, _, _, detached) = runtime.unbind_detached(&device_id);
                (removed_anything, detached)
            };
            if let Some(detached) = detached {
                crate::runtime::close_detached_bindings(vec![detached]);
            }
            removed_anything
        })
        .await?)
    }

    pub async fn refresh_state(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<DeviceStateSnapshot>> {
        self.refresh_state_shared(device_id)
            .await
            .map(|state| state.as_deref().cloned())
    }

    pub async fn refresh_state_shared(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        enum PendingStateRefresh {
            Unbound(Option<Arc<DeviceStateSnapshot>>),
            Bound(crate::runtime::SharedBoundDevice),
        }

        let pending = self
            .run_runtime_result({
                let device_id = device_id.clone();
                move |runtime| {
                    runtime.ensure_running()?;
                    let Some(binding) = runtime.bound_device(&device_id) else {
                        runtime.clear_failure(&device_id);
                        runtime_debug_async!(
                            device_id = ?device_id,
                            "async runtime refresh_state skipped because device is not bound"
                        );
                        return Ok(PendingStateRefresh::Unbound(
                            runtime.shared_state(&device_id),
                        ));
                    };
                    Ok(PendingStateRefresh::Bound(binding))
                }
            })
            .await?;
        let binding = match pending {
            PendingStateRefresh::Unbound(state) => return Ok(state),
            PendingStateRefresh::Bound(binding) => binding,
        };

        let refresh_result = {
            let device_id = device_id.clone();
            tokio::task::spawn_blocking(move || {
                let mut bound = lock(&binding);
                bound.state().map_err(|source| RuntimeError::Driver {
                    device_id,
                    source: Box::new(source),
                })
            })
            .await?
        };

        self.run_runtime_result({
            let device_id = device_id.clone();
            move |runtime| {
                runtime.complete_operation(
                    device_id.clone(),
                    crate::RuntimeFailureOperation::RefreshState,
                    &refresh_result,
                );
                let snapshot = refresh_result?;
                if let Some(snapshot) = snapshot {
                    runtime.cache_state(snapshot);
                }
                Ok(runtime.shared_state(&device_id))
            }
        })
        .await
    }

    pub async fn request(&self, request: DeviceRequest) -> AsyncRuntimeResult<DeviceResponse> {
        let _started_at = std::time::Instant::now();
        let device_id = request.device_id.clone();
        let interaction_name = crate::runtime::interaction_name_owned(&request.interaction);
        let was_bound = self
            .run_read_blocking({
                let device_id = device_id.clone();
                move |runtime| runtime.is_bound(&device_id)
            })
            .await?;
        let bind_lock = (!was_bound).then(|| self.bind_lock(&device_id));
        let inner = Arc::clone(&self.inner);

        struct RequestDispatch {
            _attempted_auto_bind: bool,
            result: RuntimeResult<(
                lemnos_core::InteractionResponse,
                Option<DeviceStateSnapshot>,
                bool,
            )>,
        }

        let dispatch = {
            let device_id = device_id.clone();
            let _interaction_name_for_dispatch = interaction_name.clone();
            tokio::task::spawn_blocking(move || {
                let mut attempted_auto_bind = false;
                let bind_guard = bind_lock.as_ref().map(|bind_lock| lock(bind_lock));
                let _ = &bind_guard;
                let result = (|| -> RuntimeResult<_> {
                    let (binding, cache_state_on_request, auto_bound) = {
                        let runtime = read_lock(&inner);
                        runtime_debug_async!(
                            device_id = ?device_id,
                            interaction = %_interaction_name_for_dispatch,
                            auto_bind_on_request = runtime.config().auto_bind_on_request,
                            already_bound = runtime.is_bound(&device_id),
                            "async runtime request dispatch starting"
                        );

                        runtime.validated_request_device(&request)?;

                        if let Some(binding) = runtime.bound_device(&device_id) {
                            (binding, runtime.config().cache_state_on_request, false)
                        } else {
                            let auto_bind_on_request = runtime.config().auto_bind_on_request;
                            attempted_auto_bind = auto_bind_on_request;
                            drop(runtime);

                            if !auto_bind_on_request {
                                return Err(RuntimeError::DeviceNotBound {
                                    device_id: device_id.clone(),
                                });
                            }

                            let runtime = write_lock(&inner);
                            if let Some(binding) = runtime.bound_device(&device_id) {
                                (binding, runtime.config().cache_state_on_request, false)
                            } else {
                                let device = runtime.validated_request_device(&request)?;

                                let prepared = runtime.prepare_binding(&device)?;
                                let cache_state_on_request =
                                    runtime.config().cache_state_on_request;
                                drop(runtime);

                                let output = prepared.bind()?;

                                let mut runtime = write_lock(&inner);
                                if !runtime.is_running() {
                                    close_prepared_binding_output(&device_id, output);
                                    return Err(RuntimeError::NotRunning);
                                }

                                if runtime.bound_device(&device_id).is_none() {
                                    runtime.store_bound_device(device_id.clone(), output);
                                    let binding =
                                        runtime.bound_device(&device_id).ok_or_else(|| {
                                            RuntimeError::DeviceNotBound {
                                                device_id: device_id.clone(),
                                            }
                                        })?;
                                    (binding, cache_state_on_request, true)
                                } else {
                                    let binding = runtime.bound_device(&device_id);
                                    drop(runtime);
                                    close_prepared_binding_output(&device_id, output);
                                    let runtime = read_lock(&inner);
                                    let binding =
                                        binding.or_else(|| runtime.bound_device(&device_id));
                                    let binding =
                                        binding.ok_or_else(|| RuntimeError::DeviceNotBound {
                                            device_id: device_id.clone(),
                                        })?;
                                    (binding, cache_state_on_request, false)
                                }
                            }
                        }
                    };

                    let interaction = request.interaction.clone();
                    let mut bound = lock(&binding);
                    runtime_debug_async!(
                        device_id = ?device_id,
                        driver_id = bound.driver_id(),
                        "async runtime dispatching request to bound device"
                    );
                    let response =
                        bound
                            .execute(&interaction)
                            .map_err(|source| RuntimeError::Driver {
                                device_id: device_id.clone(),
                                source: Box::new(source),
                            })?;
                    let state = if cache_state_on_request {
                        bound.state().map_err(|source| RuntimeError::Driver {
                            device_id: device_id.clone(),
                            source: Box::new(source),
                        })?
                    } else {
                        None
                    };
                    Ok((response, state, auto_bound, attempted_auto_bind))
                })();
                match result {
                    Ok((response, state, auto_bound, attempted_auto_bind)) => RequestDispatch {
                        _attempted_auto_bind: attempted_auto_bind,
                        result: Ok((response, state, auto_bound)),
                    },
                    Err(error) => RequestDispatch {
                        _attempted_auto_bind: attempted_auto_bind,
                        result: Err(error),
                    },
                }
            })
            .await?
        };

        let (result, _failure) = self
            .run_blocking({
                let device_id = device_id.clone();
                move |runtime| {
                    runtime.complete_operation(
                        device_id.clone(),
                        crate::RuntimeFailureOperation::Request,
                        &dispatch.result,
                    );
                    let failure = runtime.failure(&device_id).cloned();
                    match dispatch.result {
                        Ok((interaction, state, auto_bound)) => {
                            if let Some(state) = state {
                                runtime.cache_state(state);
                            }
                            if auto_bound {
                                runtime.mark_desired_binding(device_id.clone());
                            }
                            (Ok((interaction, auto_bound)), failure)
                        }
                        Err(error) => (Err(error), failure),
                    }
                }
            })
            .await?;
        match result {
            Ok((interaction, _auto_bound)) => {
                let response = DeviceResponse::new(device_id.clone(), interaction);
                runtime_info_async!(
                    device_id = ?response.device_id,
                    interaction = %interaction_name,
                    auto_bound = _auto_bound,
                    elapsed_ms = _started_at.elapsed().as_millis() as u64,
                    "async runtime request completed"
                );
                Ok(response)
            }
            Err(error) => {
                runtime_warn_async!(
                    device_id = ?device_id,
                    category = ?_failure.as_ref().map(|failure| failure.category),
                    driver_id = ?_failure.as_ref().and_then(|failure| failure.driver_id.as_ref().map(|driver_id| driver_id.as_str())),
                    interaction = %interaction_name,
                    error = %error,
                    auto_bound = dispatch._attempted_auto_bind,
                    elapsed_ms = _started_at.elapsed().as_millis() as u64,
                    "async runtime request failed"
                );
                Err(AsyncRuntimeError::from(error))
            }
        }
    }

    pub async fn with_runtime<T, F>(&self, operation: F) -> AsyncRuntimeResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Runtime) -> RuntimeResult<T> + Send + 'static,
    {
        self.run_runtime_result(operation).await
    }

    pub(crate) async fn run_blocking<T, F>(&self, operation: F) -> AsyncRuntimeResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Runtime) -> T + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        Ok(tokio::task::spawn_blocking(move || {
            let mut runtime = write_lock(&inner);
            operation(&mut runtime)
        })
        .await?)
    }

    pub(crate) async fn run_read_blocking<T, F>(&self, operation: F) -> AsyncRuntimeResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Runtime) -> T + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        Ok(tokio::task::spawn_blocking(move || {
            let runtime = read_lock(&inner);
            operation(&runtime)
        })
        .await?)
    }

    pub(crate) async fn run_runtime_result<T, F>(&self, operation: F) -> AsyncRuntimeResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Runtime) -> RuntimeResult<T> + Send + 'static,
    {
        self.run_blocking(operation)
            .await?
            .map_err(AsyncRuntimeError::from)
    }
}

fn close_prepared_binding_output(
    _device_id: &DeviceId,
    output: crate::runtime::PreparedBindingOutput,
) {
    let mut bound = output.bound;
    if let Err(_error) = bound.close() {
        runtime_warn_async!(
            device_id = ?_device_id,
            error = %_error,
            "async runtime failed to close discarded prepared binding"
        );
    }
}
