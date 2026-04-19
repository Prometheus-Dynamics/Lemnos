#![forbid(unsafe_code)]

mod capability;
mod configured;
mod descriptor;
mod error;
mod event;
mod ids;
mod interface;
mod issue;
mod request;
mod state;
mod time;
mod value;

mod request_serde {
    #[cfg(feature = "serde")]
    pub use serde::{Deserialize, Serialize};
}

pub use capability::{CapabilityAccess, CapabilityDescriptor};
pub use configured::{
    ConfiguredDeviceModel, ConfiguredGpioSignal, ConfiguredGpioSignalBinding,
    ConfiguredGpioSignalTarget, ConfiguredI2cEndpoint, ConfiguredSpiEndpoint,
};
pub use descriptor::{
    DeviceAddress, DeviceControlSurface, DeviceDescriptor, DeviceDescriptorBuilder, DeviceKind,
    DeviceLink, DeviceRelation, MatchHints,
};
pub use error::{CoreError, CoreResult};
pub use event::{DeviceEvent, InventoryEvent, LemnosEvent, StateEvent};
pub use ids::{CapabilityId, DeviceId, InteractionId, IssueCode, LocalDeviceId};
pub use interface::InterfaceKind;
pub use issue::{DeviceIssue, IssueCategory, IssueSeverity};
pub use request::{
    CustomInteractionRequest, CustomInteractionResponse, DeviceRequest, DeviceResponse, GpioBias,
    GpioDirection, GpioDrive, GpioEdge, GpioLevel, GpioLineConfiguration, GpioRequest,
    GpioResponse, I2cOperation, I2cRequest, I2cResponse, I2cTransactionResult, InteractionRequest,
    InteractionResponse, PwmConfiguration, PwmPolarity, PwmRequest, PwmResponse, SpiBitOrder,
    SpiConfiguration, SpiMode, SpiRequest, SpiResponse, StandardRequest, StandardResponse,
    UartConfiguration, UartDataBits, UartFlowControl, UartParity, UartRequest, UartResponse,
    UartStopBits, UsbControlSetup, UsbControlTransfer, UsbDirection, UsbInterruptTransfer,
    UsbRecipient, UsbRequest, UsbRequestType, UsbResponse,
};
pub use state::{
    Availability, DeviceHealth, DeviceLifecycleState, DeviceStateSnapshot, OperationRecord,
    OperationStatus,
};
pub use time::TimestampMs;
pub use value::{Value, ValueKind, ValueMap};
