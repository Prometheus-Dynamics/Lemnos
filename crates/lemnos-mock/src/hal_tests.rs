//! Lemnos sessions seen through embedded-hal (`lemnos_bus::hal`), driving
//! `lemnos-hal` register maps.

use crate::{MockGpioLine, MockHardware, MockI2cDevice};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::i2c::{I2c, Operation};
use lemnos_bus::hal::{HalI2cBus, HalI2cDevice, HalPin};
use lemnos_bus::{GpioBusBackend, I2cBusBackend, SessionAccess};
use lemnos_core::{
    DeviceDescriptor, ErrorKind, GpioDirection, GpioLineConfiguration, InterfaceKind,
};
use lemnos_hal::{AddressWidth, HalError, I2cRegisters, RegWrite, RegisterBus};

#[test]
fn register_maps_run_on_device_sessions() {
    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(1, 0x50).with_bytes(0x10, [0xAA, 0xBB, 0xCC]))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::I2c)
        .expect("i2c device");
    let mut session = hardware
        .open_i2c(&device, SessionAccess::Exclusive)
        .expect("open i2c");

    let mut regs = I2cRegisters::new(
        HalI2cDevice::new(session.as_mut()),
        0x50,
        AddressWidth::Bits8,
    )
    .with_bursts(8);
    assert_eq!(regs.read16(0x10).unwrap(), 0xAABB);
    regs.write_sequence(&[RegWrite::byte(0x20, 1), RegWrite::byte(0x21, 2)])
        .unwrap();
    assert_eq!(hardware.i2c_bytes(&device.id, 0x20, 2), Some(vec![1, 2]));

    // Another address than the session's is refused.
    let mut other = I2cRegisters::new(
        HalI2cDevice::new(session.as_mut()),
        0x51,
        AddressWidth::Bits8,
    );
    assert_eq!(other.read8(0).unwrap_err().kind(), ErrorKind::Failed);
    let err = HalI2cDevice::new(session.as_mut())
        .write(0x51, &[0])
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[test]
fn controller_sessions_are_embedded_hal_buses() {
    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(4, 0x18).with_u8(0x00, 0x1E))
        .with_i2c_device(MockI2cDevice::new(4, 0x68).with_u8(0x00, 0x0F))
        .build();
    let owner = DeviceDescriptor::new("mock.bmi088", InterfaceKind::I2c).expect("owner");
    let mut controller = hardware
        .open_i2c_controller(&owner, 4, SessionAccess::ExclusiveController)
        .expect("open controller");
    let mut bus = HalI2cBus::new(controller.as_mut());
    let mut id = [0u8; 1];
    bus.write_read(0x18, &[0x00], &mut id).unwrap();
    assert_eq!(id, [0x1E]);
    let mut gyro = [0u8; 1];
    bus.transaction(
        0x68,
        &mut [Operation::Write(&[0x00]), Operation::Read(&mut gyro)],
    )
    .unwrap();
    assert_eq!(gyro, [0x0F]);
}

#[test]
fn gpio_sessions_are_pins() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 3).with_configuration(
            GpioLineConfiguration {
                direction: GpioDirection::Output,
                active_low: false,
                bias: None,
                drive: None,
                edge: None,
                debounce_us: None,
                initial_level: None,
            },
        ))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Gpio)
        .expect("gpio line");
    let mut session = hardware
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio");
    let mut pin = HalPin::new(session.as_mut());
    pin.set_high().unwrap();
    assert!(pin.is_high().unwrap());
    pin.set_low().unwrap();
    assert!(pin.is_low().unwrap());
}

#[test]
fn spi_sessions_are_devices() {
    use crate::MockSpiDevice;
    use lemnos_bus::SpiBusBackend;
    use lemnos_bus::hal::HalSpiDevice;
    use lemnos_hal::SpiRegisters;

    // A chip-id read: register 0x00 with the read flag, one dummy byte out.
    let hardware = MockHardware::builder()
        .with_spi_device(
            MockSpiDevice::new(0, 1).with_transfer_response([0x80, 0x00], [0xFF, 0x1E]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Spi)
        .expect("spi device");
    let mut session = hardware
        .open_spi(&device, SessionAccess::Exclusive)
        .expect("open spi");
    let mut regs = SpiRegisters::new(HalSpiDevice::new(session.as_mut()), AddressWidth::Bits8);
    assert_eq!(regs.read8(0x00).unwrap(), 0x1E);
}

#[test]
fn no_std_vcm_driver_runs_on_a_runtime_session() {
    use lemnos_drivers_vcm::Vcm;
    use lemnos_hal::mock::MockDelay;

    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(10, 0x0c))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::I2c)
        .expect("vcm");
    let mut session = hardware
        .open_i2c(&device, SessionAccess::Exclusive)
        .expect("open i2c");
    let mut lens = Vcm::dw9807(HalI2cDevice::new(session.as_mut()));
    lens.power_up(&mut MockDelay::new()).unwrap();
    lens.move_to(0x2a5).unwrap();
    // DW9807: control register 0x02 = 0 (on), position in 0x03-0x04.
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x02, 3),
        Some(vec![0x00, 0x02, 0xa5])
    );
}
