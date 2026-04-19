use crate::{DeviceAddress, DeviceKind, InterfaceKind};
use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    #[error("{kind} cannot be empty")]
    EmptyIdentifier { kind: &'static str },
    #[error("{kind} '{value}' contains invalid character '{invalid}'")]
    InvalidIdentifierCharacter {
        kind: &'static str,
        value: String,
        invalid: char,
    },
    #[error("{kind} '{value}' must start with an ASCII alphanumeric character")]
    InvalidIdentifierStart { kind: &'static str, value: String },
    #[error("device kind '{kind}' does not belong to interface '{interface}'")]
    KindInterfaceMismatch {
        kind: DeviceKind,
        interface: InterfaceKind,
    },
    #[error("device address '{address}' does not belong to interface '{interface}'")]
    AddressInterfaceMismatch {
        address: DeviceAddress,
        interface: InterfaceKind,
    },
    #[error("request '{request}' does not belong to interface '{interface}'")]
    RequestInterfaceMismatch {
        request: &'static str,
        interface: InterfaceKind,
    },
    #[error("request '{request}' is invalid: {reason}")]
    InvalidRequest {
        request: &'static str,
        reason: String,
    },
}
