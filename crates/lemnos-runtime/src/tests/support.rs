#[path = "../../../../testing/support/linux_test_root.rs"]
mod linux_test_root;

use lemnos_core::{
    GpioDirection, GpioLevel, GpioLineConfiguration, PwmConfiguration, PwmPolarity, SpiBitOrder,
    SpiConfiguration, SpiMode, UartConfiguration, UartDataBits, UartFlowControl, UartParity,
    UartStopBits, UsbControlSetup, UsbControlTransfer, UsbDirection, UsbRecipient, UsbRequestType,
};
use lemnos_linux::LinuxPaths;
use lemnos_mock::MockUsbDevice;
use linux_test_root::TempLinuxTestRoot;
use std::path::Path;
use std::sync::Mutex;

pub(super) static PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(super) struct TestRoot {
    pub(super) root: TempLinuxTestRoot,
}

impl TestRoot {
    pub(super) fn new() -> Self {
        Self {
            root: TempLinuxTestRoot::new("lemnos-runtime-tests"),
        }
    }

    pub(super) fn paths(&self) -> LinuxPaths {
        LinuxPaths::new()
            .with_sys_class_root(self.root.root().join("sys/class"))
            .with_sys_bus_root(self.root.root().join("sys/bus"))
            .with_dev_root(self.root.root().join("dev"))
    }

    pub(super) fn create_dir(&self, relative: impl AsRef<Path>) {
        self.root.create_dir(relative);
    }

    pub(super) fn write(&self, relative: impl AsRef<Path>, contents: &str) {
        self.root.write(relative, contents);
    }

    pub(super) fn touch(&self, relative: impl AsRef<Path>) {
        self.root.touch(relative);
    }
}

pub(super) fn create_linux_watch_roots(root: &TestRoot) {
    root.create_dir("sys/class/gpio");
    root.create_dir("sys/class/pwm");
    root.create_dir("sys/class/i2c-dev");
    root.create_dir("sys/class/tty");
    root.create_dir("sys/bus/i2c/devices");
    root.create_dir("sys/bus/spi/devices");
    root.create_dir("sys/bus/usb/devices");
}

pub(super) fn output_config() -> GpioLineConfiguration {
    GpioLineConfiguration {
        direction: GpioDirection::Output,
        active_low: false,
        bias: None,
        drive: None,
        edge: None,
        debounce_us: None,
        initial_level: Some(GpioLevel::Low),
    }
}

pub(super) fn pwm_config() -> PwmConfiguration {
    PwmConfiguration {
        period_ns: 20_000_000,
        duty_cycle_ns: 5_000_000,
        enabled: false,
        polarity: PwmPolarity::Normal,
    }
}

pub(super) fn spi_config() -> SpiConfiguration {
    SpiConfiguration {
        mode: SpiMode::Mode0,
        max_frequency_hz: Some(2_000_000),
        bits_per_word: Some(8),
        bit_order: SpiBitOrder::MsbFirst,
    }
}

pub(super) fn uart_config() -> UartConfiguration {
    UartConfiguration {
        baud_rate: 115_200,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    }
}

pub(super) fn usb_vendor_request_for_interface(interface_number: u16) -> UsbControlTransfer {
    UsbControlTransfer {
        setup: UsbControlSetup {
            direction: UsbDirection::In,
            request_type: UsbRequestType::Vendor,
            recipient: UsbRecipient::Interface,
            request: 0x01,
            value: 0,
            index: interface_number,
        },
        data: vec![0; 4],
        timeout_ms: Some(100),
    }
}

pub(super) fn usb_vendor_request() -> UsbControlTransfer {
    usb_vendor_request_for_interface(0)
}

pub(super) fn mock_usb_device() -> MockUsbDevice {
    MockUsbDevice::new(1, [2])
        .with_vendor_product(0x1209, 0x0001)
        .with_interface(0)
        .with_control_response(usb_vendor_request(), [0x10, 0x20, 0x30, 0x40])
        .with_bulk_in_response(0x81, [0xAA, 0xBB, 0xCC])
}

pub(super) fn mock_usb_composite_device() -> MockUsbDevice {
    MockUsbDevice::new(1, [4])
        .with_vendor_product(0x1209, 0x0003)
        .with_interface(0)
        .with_interface(1)
        .with_control_response(usb_vendor_request(), [0x10, 0x20, 0x30, 0x40])
        .with_control_response(
            usb_vendor_request_for_interface(1),
            [0x41, 0x42, 0x43, 0x44],
        )
}

pub(super) struct HostI2cFixture {
    pub(super) bus: u32,
    pub(super) address: u16,
    pub(super) write: Vec<u8>,
    pub(super) expected_read: Vec<u8>,
}

impl HostI2cFixture {
    pub(super) fn from_env() -> Result<Self, String> {
        Ok(Self {
            bus: parse_required_u32_env("LEMNOS_TEST_I2C_BUS")?,
            address: parse_required_u16_env("LEMNOS_TEST_I2C_ADDRESS")?,
            write: parse_required_hex_bytes_env("LEMNOS_TEST_I2C_WRITE_HEX")?,
            expected_read: parse_required_hex_bytes_env("LEMNOS_TEST_I2C_EXPECT_READ_HEX")?,
        })
    }
}

pub(super) struct HostGpioFixture {
    pub(super) chip_name: String,
    pub(super) offset: u32,
    pub(super) expected_level: Option<GpioLevel>,
}

impl HostGpioFixture {
    pub(super) fn from_env() -> Result<Self, String> {
        Ok(Self {
            chip_name: required_env("LEMNOS_TEST_GPIO_CHIP")?,
            offset: parse_required_u32_env("LEMNOS_TEST_GPIO_OFFSET")?,
            expected_level: optional_env("LEMNOS_TEST_GPIO_EXPECT_LEVEL")
                .map(|raw| parse_gpio_level("LEMNOS_TEST_GPIO_EXPECT_LEVEL", &raw))
                .transpose()?,
        })
    }
}

pub(super) struct HostSpiFixture {
    pub(super) bus: u32,
    pub(super) chip_select: u16,
    pub(super) write: Vec<u8>,
    pub(super) expected_read: Vec<u8>,
    pub(super) configuration: Option<SpiConfiguration>,
}

impl HostSpiFixture {
    pub(super) fn from_env() -> Result<Self, String> {
        let configuration = optional_spi_configuration_from_env()?;
        Ok(Self {
            bus: parse_required_u32_env("LEMNOS_TEST_SPI_BUS")?,
            chip_select: parse_required_u16_env("LEMNOS_TEST_SPI_CHIP_SELECT")?,
            write: parse_required_hex_bytes_env("LEMNOS_TEST_SPI_TRANSFER_HEX")?,
            expected_read: parse_required_hex_bytes_env("LEMNOS_TEST_SPI_EXPECT_READ_HEX")?,
            configuration,
        })
    }
}

pub(super) struct HostUsbFixture {
    pub(super) bus: u16,
    pub(super) ports: Vec<u8>,
    pub(super) interface_number: u8,
}

impl HostUsbFixture {
    pub(super) fn from_env() -> Result<Self, String> {
        Ok(Self {
            bus: parse_required_u16_env("LEMNOS_TEST_USB_BUS")?,
            ports: parse_required_u8_list_env("LEMNOS_TEST_USB_PORTS")?,
            interface_number: parse_required_u8_env("LEMNOS_TEST_USB_INTERFACE")?,
        })
    }
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("set {name} to run this host-backed test"))
        .and_then(|value| {
            if value.is_empty() {
                Err(format!("{name} must not be empty"))
            } else {
                Ok(value)
            }
        })
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_required_u32_env(name: &str) -> Result<u32, String> {
    let raw = required_env(name)?;
    parse_u64_value(name, &raw).and_then(|value| {
        u32::try_from(value).map_err(|_| format!("{name} value '{raw}' does not fit into u32"))
    })
}

fn parse_required_u16_env(name: &str) -> Result<u16, String> {
    let raw = required_env(name)?;
    parse_u64_value(name, &raw).and_then(|value| {
        u16::try_from(value).map_err(|_| format!("{name} value '{raw}' does not fit into u16"))
    })
}

fn parse_required_u8_env(name: &str) -> Result<u8, String> {
    let raw = required_env(name)?;
    parse_u64_value(name, &raw).and_then(|value| {
        u8::try_from(value).map_err(|_| format!("{name} value '{raw}' does not fit into u8"))
    })
}

fn parse_required_u8_list_env(name: &str) -> Result<Vec<u8>, String> {
    let raw = required_env(name)?;
    let values = raw
        .split(['.', ',', ':', '/', '-', ' ', '\t'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            parse_u64_value(name, part).and_then(|value| {
                u8::try_from(value)
                    .map_err(|_| format!("{name} entry '{part}' does not fit into u8"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if values.is_empty() {
        Err(format!("{name} must contain at least one USB port number"))
    } else {
        Ok(values)
    }
}

fn parse_required_hex_bytes_env(name: &str) -> Result<Vec<u8>, String> {
    let raw = required_env(name)?;
    parse_hex_bytes(name, &raw)
}

fn parse_gpio_level(name: &str, raw: &str) -> Result<GpioLevel, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "0" | "low" => Ok(GpioLevel::Low),
        "1" | "high" => Ok(GpioLevel::High),
        _ => Err(format!(
            "{name} must be one of 0, 1, low, or high; got '{raw}'"
        )),
    }
}

fn parse_u64_value(name: &str, raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
            .map_err(|error| format!("failed to parse {name} value '{raw}' as hex: {error}"))
    } else {
        trimmed
            .parse::<u64>()
            .map_err(|error| format!("failed to parse {name} value '{raw}' as decimal: {error}"))
    }
}

fn parse_hex_bytes(name: &str, raw: &str) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{name} must contain at least one byte"));
    }

    let has_separators = trimmed
        .chars()
        .any(|character| matches!(character, ',' | ':' | '-' | '_' | ' ' | '\t' | '\n' | '\r'));
    let tokens = if has_separators {
        trimmed
            .split(|character: char| {
                matches!(character, ',' | ':' | '-' | '_' | ' ' | '\t' | '\n' | '\r')
            })
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        let compact = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        if !compact.len().is_multiple_of(2) {
            return Err(format!(
                "{name} hex string '{raw}' must contain an even number of digits"
            ));
        }
        compact
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect::<Vec<_>>()
    };

    if tokens.is_empty() {
        return Err(format!("{name} must contain at least one byte"));
    }

    tokens
        .into_iter()
        .map(|token| {
            let normalized = token
                .strip_prefix("0x")
                .or_else(|| token.strip_prefix("0X"))
                .unwrap_or(token.as_str());
            u8::from_str_radix(normalized, 16)
                .map_err(|error| format!("failed to parse {name} byte '{token}' as hex: {error}"))
        })
        .collect()
}

fn optional_spi_configuration_from_env() -> Result<Option<SpiConfiguration>, String> {
    let mode = optional_env("LEMNOS_TEST_SPI_MODE")
        .map(|raw| parse_spi_mode("LEMNOS_TEST_SPI_MODE", &raw))
        .transpose()?;
    let max_frequency_hz = optional_env("LEMNOS_TEST_SPI_MAX_HZ")
        .map(|raw| parse_u64_value("LEMNOS_TEST_SPI_MAX_HZ", &raw))
        .transpose()?
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                format!("LEMNOS_TEST_SPI_MAX_HZ value '{value}' does not fit into u32")
            })
        })
        .transpose()?;
    let bits_per_word = optional_env("LEMNOS_TEST_SPI_BITS_PER_WORD")
        .map(|raw| parse_u64_value("LEMNOS_TEST_SPI_BITS_PER_WORD", &raw))
        .transpose()?
        .map(|value| {
            u8::try_from(value).map_err(|_| {
                format!("LEMNOS_TEST_SPI_BITS_PER_WORD value '{value}' does not fit into u8")
            })
        })
        .transpose()?;
    let bit_order = optional_env("LEMNOS_TEST_SPI_BIT_ORDER")
        .map(|raw| parse_spi_bit_order("LEMNOS_TEST_SPI_BIT_ORDER", &raw))
        .transpose()?;

    if mode.is_none()
        && max_frequency_hz.is_none()
        && bits_per_word.is_none()
        && bit_order.is_none()
    {
        Ok(None)
    } else {
        Ok(Some(SpiConfiguration {
            mode: mode.unwrap_or(SpiMode::Mode0),
            max_frequency_hz,
            bits_per_word,
            bit_order: bit_order.unwrap_or(SpiBitOrder::MsbFirst),
        }))
    }
}

fn parse_spi_mode(name: &str, raw: &str) -> Result<SpiMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "0" | "mode0" => Ok(SpiMode::Mode0),
        "1" | "mode1" => Ok(SpiMode::Mode1),
        "2" | "mode2" => Ok(SpiMode::Mode2),
        "3" | "mode3" => Ok(SpiMode::Mode3),
        _ => Err(format!(
            "{name} must be one of 0, 1, 2, 3, mode0, mode1, mode2, or mode3"
        )),
    }
}

fn parse_spi_bit_order(name: &str, raw: &str) -> Result<SpiBitOrder, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "msb" | "msb-first" | "msbfirst" => Ok(SpiBitOrder::MsbFirst),
        "lsb" | "lsb-first" | "lsbfirst" => Ok(SpiBitOrder::LsbFirst),
        _ => Err(format!(
            "{name} must be one of msb, msb-first, lsb, or lsb-first"
        )),
    }
}
