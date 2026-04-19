use super::*;
use crate::DeviceDescriptor;

#[test]
fn standard_request_reports_interface_and_name() {
    let request = StandardRequest::Gpio(GpioRequest::Read);
    assert_eq!(request.interface(), InterfaceKind::Gpio);
    assert_eq!(request.name(), "gpio.read");
}

#[test]
fn request_validation_rejects_invalid_lengths() {
    let request = StandardRequest::I2c(I2cRequest::Read { length: 0 });
    let err = request
        .validate()
        .expect_err("zero-length read should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn pwm_validation_rejects_zero_periods() {
    let request = StandardRequest::Pwm(PwmRequest::SetPeriod { period_ns: 0 });
    let err = request.validate().expect_err("zero period should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn pwm_validation_rejects_duty_cycles_larger_than_period() {
    let request = StandardRequest::Pwm(PwmRequest::Configure(PwmConfiguration {
        period_ns: 1_000,
        duty_cycle_ns: 1_001,
        enabled: true,
        polarity: PwmPolarity::Normal,
    }));
    let err = request
        .validate()
        .expect_err("oversized duty cycle should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn spi_validation_rejects_zero_frequency() {
    let request = StandardRequest::Spi(SpiRequest::Configure(SpiConfiguration {
        mode: SpiMode::Mode0,
        max_frequency_hz: Some(0),
        bits_per_word: Some(8),
        bit_order: SpiBitOrder::MsbFirst,
    }));
    let err = request.validate().expect_err("zero frequency should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn spi_validation_rejects_zero_bits_per_word() {
    let request = StandardRequest::Spi(SpiRequest::Configure(SpiConfiguration {
        mode: SpiMode::Mode0,
        max_frequency_hz: Some(1_000_000),
        bits_per_word: Some(0),
        bit_order: SpiBitOrder::MsbFirst,
    }));
    let err = request
        .validate()
        .expect_err("zero bits per word should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn uart_validation_rejects_zero_baud_rate() {
    let request = StandardRequest::Uart(UartRequest::Configure(UartConfiguration {
        baud_rate: 0,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    }));
    let err = request.validate().expect_err("zero baud rate should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn usb_validation_rejects_zero_endpoint() {
    let request = StandardRequest::Usb(UsbRequest::BulkRead {
        endpoint: 0,
        length: 8,
        timeout_ms: None,
    });
    let err = request.validate().expect_err("endpoint zero should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn usb_validation_rejects_empty_control_read_buffer() {
    let request = StandardRequest::Usb(UsbRequest::Control(UsbControlTransfer {
        setup: UsbControlSetup {
            direction: UsbDirection::In,
            request_type: UsbRequestType::Vendor,
            recipient: UsbRecipient::Device,
            request: 0x01,
            value: 0,
            index: 0,
        },
        data: Vec::new(),
        timeout_ms: Some(100),
    }));
    let err = request
        .validate()
        .expect_err("empty control read buffer should fail");
    assert!(matches!(err, CoreError::InvalidRequest { .. }));
}

#[test]
fn device_request_validation_checks_interface() {
    let descriptor =
        DeviceDescriptor::new("gpiochip0-line1", InterfaceKind::Gpio).expect("descriptor");
    let request = DeviceRequest::new(
        DeviceId::new("gpiochip0-line1").expect("device id"),
        InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::Read { length: 4 })),
    );

    let err = request
        .validate_for(&descriptor)
        .expect_err("mismatched request should fail");
    assert!(matches!(err, CoreError::RequestInterfaceMismatch { .. }));
}
