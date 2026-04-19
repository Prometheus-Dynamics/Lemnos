use crate::faults::{MockFaultRegistry, MockFaultScript};
use crate::gpio::{MockGpioLine, MockGpioLineState, MockGpioSession};
use crate::i2c::{MockI2cControllerSession, MockI2cDevice, MockI2cDeviceState, MockI2cSession};
use crate::pwm::{MockPwmChannel, MockPwmChannelState, MockPwmSession};
use crate::spi::{MockSpiDevice, MockSpiDeviceState, MockSpiSession};
use crate::uart::{MockUartPort, MockUartPortState, MockUartSession};
use crate::usb::{MockUsbDevice, MockUsbDeviceState, MockUsbSession};
use lemnos_bus::{
    BusBackend, BusError, BusResult, GpioBusBackend, GpioSession, I2cBusBackend,
    I2cControllerSession, I2cSession, PwmBusBackend, PwmSession, SessionAccess, SpiBusBackend,
    SpiSession, UartBusBackend, UartSession, UsbBusBackend, UsbSession,
};
use lemnos_core::{
    DeviceDescriptor, DeviceId, GpioLevel, GpioLineConfiguration, InterfaceKind, PwmConfiguration,
    SpiConfiguration, UartConfiguration, UsbControlTransfer,
};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryProbe, DiscoveryResult, InventorySnapshot, ProbeDiscovery,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

mod backends;
mod builder;
mod inventory;

pub(crate) const MOCK_BACKEND_NAME: &str = "mock-hardware";
const MOCK_INTERFACES: [InterfaceKind; 6] = [
    InterfaceKind::Gpio,
    InterfaceKind::Pwm,
    InterfaceKind::I2c,
    InterfaceKind::Spi,
    InterfaceKind::Uart,
    InterfaceKind::Usb,
];

#[derive(Default)]
pub(crate) struct MockHardwareState {
    pub gpio_lines: BTreeMap<DeviceId, MockGpioLineState>,
    pub pwm_channels: BTreeMap<DeviceId, MockPwmChannelState>,
    pub i2c_devices: BTreeMap<DeviceId, MockI2cDeviceState>,
    pub spi_devices: BTreeMap<DeviceId, MockSpiDeviceState>,
    pub uart_ports: BTreeMap<DeviceId, MockUartPortState>,
    pub usb_devices: BTreeMap<DeviceId, MockUsbDeviceState>,
    pub usb_descriptor_owners: BTreeMap<DeviceId, DeviceId>,
    pub faults: MockFaultRegistry,
}

#[derive(Default)]
pub struct MockHardwareBuilder {
    gpio_lines: Vec<MockGpioLine>,
    pwm_channels: Vec<MockPwmChannel>,
    i2c_devices: Vec<MockI2cDevice>,
    spi_devices: Vec<MockSpiDevice>,
    uart_ports: Vec<MockUartPort>,
    usb_devices: Vec<MockUsbDevice>,
}

#[derive(Clone, Default)]
pub struct MockHardware {
    pub(crate) state: Arc<Mutex<MockHardwareState>>,
}

pub(crate) fn take_injected_error(
    state: &Arc<Mutex<MockHardwareState>>,
    device_id: &DeviceId,
    operation: &'static str,
) -> BusResult<()> {
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(error) = state.faults.take(device_id, operation) {
        return Err(error);
    }
    Ok(())
}

fn build_state_map<Source, State>(
    items: Vec<Source>,
    into_state: impl FnMut(Source) -> State,
    state_id: impl Fn(&State) -> &DeviceId,
) -> BTreeMap<DeviceId, State> {
    items
        .into_iter()
        .map(into_state)
        .map(|state| (state_id(&state).clone(), state))
        .collect()
}

fn unsupported_device(device_id: &DeviceId) -> BusError {
    BusError::UnsupportedDevice {
        backend: MOCK_BACKEND_NAME.to_string(),
        device_id: device_id.clone(),
    }
}
