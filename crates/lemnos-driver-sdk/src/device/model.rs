use crate::{DriverBindContext, DriverError, DriverMatch, DriverResult};
use lemnos_bus::BusSession;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, DeviceStateSnapshot, InteractionId, InteractionRequest,
    InteractionResponse, InterfaceKind,
};
use lemnos_driver_manifest::{DriverManifest, DriverPriority};
use std::borrow::Cow;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomInteraction {
    pub id: InteractionId,
    pub summary: String,
}

impl CustomInteraction {
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Result<Self, lemnos_core::CoreError> {
        Ok(Self {
            id: InteractionId::new(id)?,
            summary: summary.into(),
        })
    }
}

pub trait Driver: Send + Sync {
    fn id(&self) -> &str;
    fn interface(&self) -> InterfaceKind;

    /// Exposes driver metadata at the registry/bind composition boundary.
    ///
    /// Borrowed static manifests are preferred because registry matching and
    /// validation query this metadata repeatedly. Drivers that truly need
    /// per-instance manifests can still return `Cow::Owned(...)`.
    fn manifest_ref(&self) -> Cow<'static, DriverManifest>;

    fn manifest(&self) -> DriverManifest {
        self.manifest_ref().into_owned()
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        self.manifest_ref().match_device(device).into()
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        Err(DriverError::NotImplemented {
            driver_id: self.id().to_string(),
            action: format!("bind {}", device.id),
        })
    }
}

pub fn cached_manifest(
    cell: &'static OnceLock<DriverManifest>,
    build: impl FnOnce() -> DriverManifest,
) -> &'static DriverManifest {
    cell.get_or_init(build)
}

pub fn generic_driver_manifest(
    id: impl Into<String>,
    summary: impl Into<String>,
    interface: InterfaceKind,
    kinds: &[DeviceKind],
) -> DriverManifest {
    let mut manifest =
        DriverManifest::new(id, summary, vec![interface]).with_priority(DriverPriority::Generic);
    for kind in kinds {
        manifest = manifest.with_kind(*kind);
    }
    manifest
}

pub fn generic_driver_manifest_with_standard_interactions(
    id: impl Into<String>,
    summary: impl Into<String>,
    interface: InterfaceKind,
    kinds: &[DeviceKind],
    interactions: &[(&'static str, &'static str)],
) -> DriverManifest {
    let mut manifest = generic_driver_manifest(id, summary, interface, kinds);
    for (interaction_id, interaction_summary) in interactions {
        manifest = manifest.with_standard_interaction(*interaction_id, *interaction_summary);
    }
    manifest
}

pub trait BoundDevice: Send + Sync {
    fn device(&self) -> &DeviceDescriptor;
    fn driver_id(&self) -> &str;

    fn close(&mut self) -> DriverResult<()> {
        Ok(())
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(None)
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        Err(DriverError::UnsupportedAction {
            driver_id: self.driver_id().to_string(),
            device_id: self.device().id.clone(),
            action: interaction_name(request).into_owned(),
        })
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &[]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoopBoundDevice {
    pub driver_id: String,
    pub device: DeviceDescriptor,
    pub state: Option<DeviceStateSnapshot>,
    pub interactions: Vec<CustomInteraction>,
}

impl NoopBoundDevice {
    pub fn new(driver_id: impl Into<String>, device: DeviceDescriptor) -> Self {
        Self {
            driver_id: driver_id.into(),
            device,
            state: None,
            interactions: Vec::new(),
        }
    }

    pub fn with_state(mut self, state: DeviceStateSnapshot) -> Self {
        self.state = Some(state);
        self
    }

    pub fn with_interaction(mut self, interaction: CustomInteraction) -> Self {
        self.interactions.push(interaction);
        self
    }
}

impl BoundDevice for NoopBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(self.state.clone())
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        self.interactions.as_slice()
    }
}

pub fn interaction_name(request: &InteractionRequest) -> Cow<'_, str> {
    match request {
        InteractionRequest::Standard(request) => Cow::Borrowed(request.name()),
        InteractionRequest::Custom(request) => Cow::Borrowed(request.id.as_str()),
    }
}

pub fn validate_request_for_device(
    driver_id: &str,
    device: &DeviceDescriptor,
    request: &InteractionRequest,
) -> DriverResult<()> {
    request
        .validate()
        .map_err(|source| DriverError::InvalidRequest {
            driver_id: driver_id.to_string(),
            device_id: device.id.clone(),
            source,
        })
}

pub fn unsupported_action_error(
    driver_id: &str,
    device: &DeviceDescriptor,
    request: &InteractionRequest,
) -> DriverError {
    DriverError::UnsupportedAction {
        driver_id: driver_id.to_string(),
        device_id: device.id.clone(),
        action: interaction_name(request).into_owned(),
    }
}

pub fn bind_rejected_expected(
    driver_id: &str,
    device: &DeviceDescriptor,
    expected: impl Into<String>,
) -> DriverError {
    DriverError::BindRejected {
        driver_id: driver_id.to_string(),
        device_id: device.id.clone(),
        reason: format!("expected {}, found {}", expected.into(), device.kind),
    }
}

pub fn ensure_device_kind(
    driver_id: &str,
    device: &DeviceDescriptor,
    expected_kind: DeviceKind,
    expected_label: &'static str,
) -> DriverResult<()> {
    if device.kind == expected_kind {
        Ok(())
    } else {
        Err(bind_rejected_expected(driver_id, device, expected_label))
    }
}

pub fn ensure_device_kinds(
    driver_id: &str,
    device: &DeviceDescriptor,
    expected_kinds: &[DeviceKind],
    expected_label: &'static str,
) -> DriverResult<()> {
    if expected_kinds.contains(&device.kind) {
        Ok(())
    } else {
        Err(bind_rejected_expected(driver_id, device, expected_label))
    }
}

pub fn bind_with_session<S, B>(
    driver_id: &str,
    _device: &DeviceDescriptor,
    open_session: impl FnOnce() -> DriverResult<S>,
    build_bound: impl FnOnce(String, S) -> B,
) -> DriverResult<Box<dyn BoundDevice>>
where
    B: BoundDevice + 'static,
{
    let session = open_session()?;
    Ok(Box::new(build_bound(driver_id.to_string(), session)))
}

pub fn close_session<S>(driver_id: &str, session: &mut S) -> DriverResult<()>
where
    S: BusSession + ?Sized,
{
    session.close().map_err(|source| DriverError::Transport {
        driver_id: driver_id.to_string(),
        device_id: session.device().id.clone(),
        source,
    })
}

pub fn bind_session_for_kind<S, B>(
    driver_id: &str,
    device: &DeviceDescriptor,
    expected_kind: DeviceKind,
    expected_label: &'static str,
    open_session: impl FnOnce() -> DriverResult<S>,
    build_bound: impl FnOnce(String, S) -> B,
) -> DriverResult<Box<dyn BoundDevice>>
where
    B: BoundDevice + 'static,
{
    ensure_device_kind(driver_id, device, expected_kind, expected_label)?;
    bind_with_session(driver_id, device, open_session, build_bound)
}

pub fn bind_session_for_kinds<S, B>(
    driver_id: &str,
    device: &DeviceDescriptor,
    expected_kinds: &[DeviceKind],
    expected_label: &'static str,
    open_session: impl FnOnce() -> DriverResult<S>,
    build_bound: impl FnOnce(String, S) -> B,
) -> DriverResult<Box<dyn BoundDevice>>
where
    B: BoundDevice + 'static,
{
    ensure_device_kinds(driver_id, device, expected_kinds, expected_label)?;
    bind_with_session(driver_id, device, open_session, build_bound)
}
