#[cfg(feature = "serde")]
use crate::request_serde::*;
use std::time::Duration;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsbDirection {
    In,
    Out,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsbRequestType {
    Standard,
    Class,
    Vendor,
    Reserved,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsbRecipient {
    Device,
    Interface,
    Endpoint,
    Other,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbControlSetup {
    pub direction: UsbDirection,
    pub request_type: UsbRequestType,
    pub recipient: UsbRecipient,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbControlTransfer {
    pub setup: UsbControlSetup,
    pub data: Vec<u8>,
    pub timeout_ms: Option<u32>,
}

impl UsbControlTransfer {
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_ms
            .map(|timeout_ms| Duration::from_millis(timeout_ms as u64))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(timeout_to_millis_u32(timeout));
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterruptTransfer {
    pub endpoint: u8,
    pub bytes: Vec<u8>,
    pub timeout_ms: Option<u32>,
}

impl UsbInterruptTransfer {
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_ms
            .map(|timeout_ms| Duration::from_millis(timeout_ms as u64))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(timeout_to_millis_u32(timeout));
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbRequest {
    Control(UsbControlTransfer),
    BulkRead {
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    },
    BulkWrite {
        endpoint: u8,
        bytes: Vec<u8>,
        timeout_ms: Option<u32>,
    },
    InterruptRead {
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    },
    InterruptWrite(UsbInterruptTransfer),
    ClaimInterface {
        interface_number: u8,
        alternate_setting: Option<u8>,
    },
    ReleaseInterface {
        interface_number: u8,
    },
}

impl UsbRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Control(_) => "usb.control_transfer",
            Self::BulkRead { .. } => "usb.bulk_read",
            Self::BulkWrite { .. } => "usb.bulk_write",
            Self::InterruptRead { .. } => "usb.interrupt_read",
            Self::InterruptWrite(_) => "usb.interrupt_write",
            Self::ClaimInterface { .. } => "usb.claim_interface",
            Self::ReleaseInterface { .. } => "usb.release_interface",
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        match self {
            Self::Control(transfer) => transfer.timeout(),
            Self::BulkRead { timeout_ms, .. }
            | Self::BulkWrite { timeout_ms, .. }
            | Self::InterruptRead { timeout_ms, .. } => {
                timeout_ms.map(|timeout_ms| Duration::from_millis(timeout_ms as u64))
            }
            Self::InterruptWrite(transfer) => transfer.timeout(),
            Self::ClaimInterface { .. } | Self::ReleaseInterface { .. } => None,
        }
    }
}

fn timeout_to_millis_u32(timeout: Duration) -> u32 {
    timeout.as_millis().try_into().unwrap_or(u32::MAX)
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbResponse {
    Bytes(Vec<u8>),
    InterfaceClaimed {
        interface_number: u8,
        alternate_setting: Option<u8>,
    },
    InterfaceReleased {
        interface_number: u8,
    },
    Applied,
}
