use crate::{
    CoreError, CoreResult, DeviceDescriptor, DeviceId, InteractionId, InterfaceKind, Value,
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

mod gpio;
mod i2c;
mod pwm;
mod spi;
mod uart;
mod usb;

#[cfg(test)]
mod tests;

pub use gpio::{
    GpioBias, GpioDirection, GpioDrive, GpioEdge, GpioLevel, GpioLineConfiguration, GpioRequest,
    GpioResponse,
};
pub use i2c::{I2cOperation, I2cRequest, I2cResponse, I2cTransactionResult};
pub use pwm::{PwmConfiguration, PwmPolarity, PwmRequest, PwmResponse};
pub use spi::{SpiBitOrder, SpiConfiguration, SpiMode, SpiRequest, SpiResponse};
pub use uart::{
    UartConfiguration, UartDataBits, UartFlowControl, UartParity, UartRequest, UartResponse,
    UartStopBits,
};
pub use usb::{
    UsbControlSetup, UsbControlTransfer, UsbDirection, UsbInterruptTransfer, UsbRecipient,
    UsbRequest, UsbRequestType, UsbResponse,
};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomInteractionRequest {
    pub id: InteractionId,
    pub input: Option<Value>,
}

impl CustomInteractionRequest {
    pub fn new<I>(id: I) -> CoreResult<Self>
    where
        I: TryInto<InteractionId, Error = CoreError>,
    {
        Ok(Self::from_id(id.try_into()?))
    }

    pub fn from_id(id: InteractionId) -> Self {
        Self { id, input: None }
    }

    pub fn with_input(mut self, input: impl Into<Value>) -> Self {
        self.input = Some(input.into());
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomInteractionResponse {
    pub id: InteractionId,
    pub output: Option<Value>,
}

impl CustomInteractionResponse {
    pub fn new(id: InteractionId) -> Self {
        Self { id, output: None }
    }

    pub fn with_output(mut self, output: impl Into<Value>) -> Self {
        self.output = Some(output.into());
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StandardRequest {
    Gpio(GpioRequest),
    Pwm(PwmRequest),
    I2c(I2cRequest),
    Spi(SpiRequest),
    Uart(UartRequest),
    Usb(UsbRequest),
}

impl StandardRequest {
    pub const fn interface(&self) -> InterfaceKind {
        match self {
            Self::Gpio(_) => InterfaceKind::Gpio,
            Self::Pwm(_) => InterfaceKind::Pwm,
            Self::I2c(_) => InterfaceKind::I2c,
            Self::Spi(_) => InterfaceKind::Spi,
            Self::Uart(_) => InterfaceKind::Uart,
            Self::Usb(_) => InterfaceKind::Usb,
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::Gpio(request) => request.name(),
            Self::Pwm(request) => request.name(),
            Self::I2c(request) => request.name(),
            Self::Spi(request) => request.name(),
            Self::Uart(request) => request.name(),
            Self::Usb(request) => request.name(),
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        match self {
            Self::Pwm(PwmRequest::Configure(configuration)) if configuration.period_ns == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "PWM period must be greater than zero".into(),
                })
            }
            Self::Pwm(PwmRequest::Configure(configuration))
                if configuration.duty_cycle_ns > configuration.period_ns =>
            {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "PWM duty cycle must not exceed the period".into(),
                })
            }
            Self::Pwm(PwmRequest::SetPeriod { period_ns }) if *period_ns == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "PWM period must be greater than zero".into(),
                })
            }
            Self::I2c(I2cRequest::Read { length }) if *length == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "read length must be greater than zero".into(),
                })
            }
            Self::I2c(I2cRequest::Write { bytes }) if bytes.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "write payload must not be empty".into(),
                })
            }
            Self::I2c(I2cRequest::WriteRead { read_length, .. }) if *read_length == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "read length must be greater than zero".into(),
                })
            }
            Self::I2c(I2cRequest::Transaction { operations }) if operations.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "transaction operations must not be empty".into(),
                })
            }
            Self::Spi(SpiRequest::Transfer { write }) if write.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "transfer payload must not be empty".into(),
                })
            }
            Self::Spi(SpiRequest::Configure(configuration))
                if configuration.max_frequency_hz == Some(0) =>
            {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "SPI max frequency must be greater than zero".into(),
                })
            }
            Self::Spi(SpiRequest::Configure(configuration))
                if configuration.bits_per_word == Some(0) =>
            {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "SPI bits per word must be greater than zero".into(),
                })
            }
            Self::Spi(SpiRequest::Write { bytes }) if bytes.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "write payload must not be empty".into(),
                })
            }
            Self::Uart(UartRequest::Read { max_bytes }) if *max_bytes == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "max_bytes must be greater than zero".into(),
                })
            }
            Self::Uart(UartRequest::Configure(configuration)) if configuration.baud_rate == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "UART baud rate must be greater than zero".into(),
                })
            }
            Self::Uart(UartRequest::Write { bytes }) if bytes.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "write payload must not be empty".into(),
                })
            }
            Self::Usb(UsbRequest::BulkRead { length, .. }) if *length == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "bulk read length must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::BulkRead { endpoint, .. }) if *endpoint == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "bulk read endpoint must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::BulkWrite { bytes, .. }) if bytes.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "bulk write payload must not be empty".into(),
                })
            }
            Self::Usb(UsbRequest::BulkWrite { endpoint, .. }) if *endpoint == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "bulk write endpoint must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::InterruptRead { length, .. }) if *length == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "interrupt read length must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::InterruptRead { endpoint, .. }) if *endpoint == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "interrupt read endpoint must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::InterruptWrite(transfer)) if transfer.bytes.is_empty() => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "interrupt write payload must not be empty".into(),
                })
            }
            Self::Usb(UsbRequest::InterruptWrite(transfer)) if transfer.endpoint == 0 => {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "interrupt write endpoint must be greater than zero".into(),
                })
            }
            Self::Usb(UsbRequest::Control(transfer))
                if transfer.setup.direction == UsbDirection::In && transfer.data.is_empty() =>
            {
                Err(CoreError::InvalidRequest {
                    request: self.name(),
                    reason: "control read buffer must not be empty".into(),
                })
            }
            _ => Ok(()),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StandardResponse {
    Gpio(GpioResponse),
    Pwm(PwmResponse),
    I2c(I2cResponse),
    Spi(SpiResponse),
    Uart(UartResponse),
    Usb(UsbResponse),
}

impl StandardResponse {
    pub const fn interface(&self) -> InterfaceKind {
        match self {
            Self::Gpio(_) => InterfaceKind::Gpio,
            Self::Pwm(_) => InterfaceKind::Pwm,
            Self::I2c(_) => InterfaceKind::I2c,
            Self::Spi(_) => InterfaceKind::Spi,
            Self::Uart(_) => InterfaceKind::Uart,
            Self::Usb(_) => InterfaceKind::Usb,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionRequest {
    Standard(StandardRequest),
    Custom(CustomInteractionRequest),
}

impl InteractionRequest {
    pub fn validate(&self) -> CoreResult<()> {
        match self {
            Self::Standard(request) => request.validate(),
            Self::Custom(_) => Ok(()),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionResponse {
    Standard(StandardResponse),
    Custom(CustomInteractionResponse),
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRequest {
    pub device_id: DeviceId,
    pub interaction: InteractionRequest,
}

impl DeviceRequest {
    pub fn new(device_id: DeviceId, interaction: InteractionRequest) -> Self {
        Self {
            device_id,
            interaction,
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        self.interaction.validate()
    }

    pub fn validate_for(&self, device: &DeviceDescriptor) -> CoreResult<()> {
        self.validate()?;
        if let InteractionRequest::Standard(request) = &self.interaction
            && request.interface() != device.interface
        {
            return Err(CoreError::RequestInterfaceMismatch {
                request: request.name(),
                interface: device.interface,
            });
        }
        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceResponse {
    pub device_id: DeviceId,
    pub interaction: InteractionResponse,
}

impl DeviceResponse {
    pub fn new(device_id: DeviceId, interaction: InteractionResponse) -> Self {
        Self {
            device_id,
            interaction,
        }
    }
}
