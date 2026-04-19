use lemnos_bus::{BusError, BusResult, SessionMetadata, SessionState};
use lemnos_core::DeviceId;

pub(crate) fn permission_denied(
    device_id: &DeviceId,
    operation: &'static str,
    reason: impl Into<String>,
) -> BusError {
    BusError::PermissionDenied {
        device_id: device_id.clone(),
        operation,
        reason: reason.into(),
    }
}

pub(crate) fn ensure_open(
    metadata: &SessionMetadata,
    device_id: &DeviceId,
    interface: &'static str,
    operation: &'static str,
) -> BusResult<()> {
    if metadata.state == SessionState::Closed {
        return Err(BusError::SessionUnavailable {
            device_id: device_id.clone(),
            reason: format!("cannot perform '{operation}' on a closed {interface} session"),
        });
    }
    Ok(())
}

pub(crate) fn run_call<T, Transport>(
    metadata: &mut SessionMetadata,
    transport: &mut Transport,
    call: impl FnOnce(&mut Transport) -> BusResult<T>,
) -> BusResult<T> {
    metadata.begin_call();
    let result = call(transport);
    metadata.finish_call(&result);
    result
}
