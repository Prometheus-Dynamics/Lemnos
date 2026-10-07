use super::*;
use lemnos_device::MAX_CHANNELS;
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::mock::{MockDelay, MockI2c};
use lemnos_hal::{AddressWidth, ErrorKind, HalError};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const RAZE: &str = include_str!("../examples/raze.toml");

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-board-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn file(&self, path: &str, contents: &str) -> &Self {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{contents}\n")).unwrap();
        self
    }

    fn link(&self, link: &str, target: &str) -> &Self {
        let link = self.0.join(link);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(self.0.join(target), link).unwrap();
        self
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The Raze's I2C bus 1: BMI088 and BMM150 register files, plus an
/// INA238's 16-bit registers at 0x40.
struct MockBuses {
    sys: PathBuf,
}

/// Byte-register targets from `MockI2c`, and an INA238 at 0x40 with
/// big-endian 16-bit registers.
struct RazeBus {
    bytes: MockI2c,
    words: std::collections::BTreeMap<u8, u16>,
    pointer: u8,
}

impl embedded_hal::i2c::ErrorType for RazeBus {
    type Error = ErrorKind;
}

impl embedded_hal::i2c::I2c for RazeBus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal::i2c::Operation<'_>],
    ) -> Result<(), ErrorKind> {
        use embedded_hal::i2c::Operation;
        if address != 0x40 {
            return self.bytes.transaction(address, operations);
        }
        for op in operations {
            match op {
                Operation::Write(bytes) => {
                    self.pointer = bytes[0];
                    if bytes.len() >= 3 {
                        self.words
                            .insert(bytes[0], u16::from_be_bytes([bytes[1], bytes[2]]));
                    }
                }
                Operation::Read(buf) => {
                    let word = self.words.get(&self.pointer).copied().unwrap_or(0);
                    let be = word.to_be_bytes();
                    for (i, b) in buf.iter_mut().enumerate() {
                        *b = be.get(i).copied().unwrap_or(0);
                    }
                }
            }
        }
        Ok(())
    }
}

fn raze_bus() -> MockI2c {
    let mut bmm_trim = [0u8; 10];
    bmm_trim[0..2].copy_from_slice(&700i16.to_le_bytes());
    bmm_trim[2..4].copy_from_slice(&24000u16.to_le_bytes());
    bmm_trim[4..6].copy_from_slice(&6600u16.to_le_bytes());
    MockI2c::new()
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_registers(0x18, 0x12, &[0x00, 0x40, 0, 0, 0, 0])
        .with_target(0x10, AddressWidth::Bits8)
        .with_registers(0x10, 0x40, &[0x32])
        .with_registers(0x10, 0x68, &bmm_trim)
}

fn raze() -> RazeBus {
    RazeBus {
        bytes: raze_bus(),
        // Manufacturer "TI", device 0x2381, die temperature 30 °C.
        words: [(0x3e, 0x5449), (0x3f, 0x2381), (0x06, 30 * 128)].into(),
        pointer: 0,
    }
}

impl Buses for MockBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        if bus == 1 {
            Ok(DynI2c::new(raze()))
        } else {
            Err(BoardError::Device {
                device: format!("i2c-{bus}"),
                kind: ErrorKind::NotFound,
                reason: "no such bus".into(),
            })
        }
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.sys)
    }
}

#[test]
fn raze_definition_parses_validates_and_round_trips() {
    let board = BoardDefinition::from_toml_str(RAZE).unwrap();
    board.validate(&DriverRegistry::builtin()).unwrap();
    assert_eq!(board.board.id, "raze");
    assert_eq!(board.devices.len(), 7);
    let imu = board.device("imu").unwrap();
    assert_eq!(imu.bus, Some(BusRef::I2c(1)));
    assert_eq!(imu.address, Some(0x18));
    assert_eq!(imu.config["gyro_address"], ConfigValue::Integer(0x68));
    assert_eq!(board.device("fan").unwrap().matches["name"], "pwm-fan");
    let json = board.to_json_string();
    assert_eq!(BoardDefinition::from_json_str(&json).unwrap(), board);
}

#[test]
fn validation_reports_every_problem() {
    let mut board = BoardDefinition::from_toml_str(RAZE).unwrap();
    board.schema_version = 2;
    board.devices.push(DeviceSpec::new("imu", "bmi088"));
    board
        .devices
        .push(DeviceSpec::new("lights", "ws9999").on(BusRef::I2c(1)));
    board.devices.push(
        DeviceSpec::new("power2", "ina238")
            .on(BusRef::I2c(1))
            .with("shunt", ConfigValue::Integer(1)),
    );
    board.devices.push(
        DeviceSpec::new("fan2", "hwmon-fan")
            .on(BusRef::I2c(1))
            .with_backend(Backend::Userspace),
    );
    let Err(BoardError::Invalid(problems)) = board.validate(&DriverRegistry::builtin()) else {
        panic!("expected problems");
    };
    let all = problems.join("\n");
    for expected in [
        "schema_version 2",
        "\"imu\": duplicate id",
        "\"imu\": bmi088 needs `bus",
        "unknown driver \"ws9999\"",
        "unknown config key \"shunt\"",
        "hwmon-fan takes `path` or `match`",
        "hwmon-fan has no userspace driver",
    ] {
        assert!(all.contains(expected), "missing {expected:?} in\n{all}");
    }
    assert!(BoardDefinition::from_toml_str("format = 1").is_err());
    assert!(
        BoardDefinition::from_toml_str(&RAZE.replace("poll_ms = 10", "poll_msec = 10")).is_err()
    );
}

#[test]
fn userspace_drivers_build_from_the_definition() {
    let tree = Tree::new();
    let board = BoardDefinition::from_toml_str(RAZE).unwrap();
    let registry = DriverRegistry::builtin();
    let mut buses = MockBuses {
        sys: tree.0.clone(),
    };
    let mut buf = [0; MAX_CHANNELS];

    let mut imu = registry
        .build(board.device("imu").unwrap(), &mut buses)
        .unwrap();
    imu.init(&mut MockDelay::new()).unwrap();
    imu.read(&mut buf).unwrap();
    assert_eq!(imu.info().model, "BMI088");
    // Half of ±6 g.
    assert_eq!(buf[0], 29_419);

    let mut mag = registry
        .build(board.device("magnetometer").unwrap(), &mut buses)
        .unwrap();
    mag.init(&mut MockDelay::new()).unwrap();
    assert_eq!(mag.info().channels.len(), 3);

    let mut power = registry
        .build(board.device("power").unwrap(), &mut buses)
        .unwrap();
    power.init(&mut MockDelay::new()).unwrap();
    power.read(&mut buf).unwrap();
    assert_eq!(power.info().model, "INA238");
    assert_eq!(buf[4], 30_000);

    let fan = registry.build(board.device("fan").unwrap(), &mut buses);
    assert_eq!(fan.unwrap_err().kind(), ErrorKind::NotFound);
    let elsewhere = board.device("imu").unwrap().clone().on(BusRef::I2c(7));
    assert_eq!(
        registry.build(&elsewhere, &mut buses).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn kernel_drivers_win_when_bound() {
    let tree = Tree::new();
    let dev = "devices/platform/soc/i2c-1/1-0040";
    tree.file(&format!("{dev}/hwmon/hwmon4/name"), "ina238")
        .file(&format!("{dev}/hwmon/hwmon4/in1_input"), "12000")
        .file(&format!("{dev}/hwmon/hwmon4/in0_input"), "10")
        .file(&format!("{dev}/hwmon/hwmon4/curr1_input"), "1500")
        .file(&format!("{dev}/hwmon/hwmon4/power1_input"), "18000000")
        .file(&format!("{dev}/hwmon/hwmon4/temp1_input"), "31000")
        .link("class/hwmon/hwmon4", &format!("{dev}/hwmon/hwmon4"))
        .file("class/hwmon/hwmon2/name", "pwm-fan")
        .file("class/hwmon/hwmon2/pwm1", "100")
        .file("class/hwmon/hwmon2/pwm1_enable", "2")
        .file("class/thermal/thermal_zone0/type", "cpu-thermal")
        .file("class/thermal/thermal_zone0/temp", "51000");
    let board = BoardDefinition::from_toml_str(RAZE).unwrap();
    let registry = DriverRegistry::builtin();
    let mut buses = MockBuses {
        sys: tree.0.clone(),
    };
    let mut buf = [0; MAX_CHANNELS];

    // `auto` finds the bound ina238 hwmon device at 1-0040.
    let mut power = registry
        .build(board.device("power").unwrap(), &mut buses)
        .unwrap();
    power.init(&mut MockDelay::new()).unwrap();
    power.read(&mut buf).unwrap();
    assert_eq!(
        &buf[..5],
        &[12_000_000, 10_000_000, 1_500_000, 18_000_000, 31_000]
    );

    // `kernel` without a bound driver fails; `userspace` ignores the tree.
    let imu = board
        .device("imu")
        .unwrap()
        .clone()
        .with_backend(Backend::Kernel);
    assert_eq!(
        registry.build(&imu, &mut buses).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    let mut userspace = board
        .device("power")
        .unwrap()
        .clone()
        .with_backend(Backend::Userspace);
    userspace.id = "power-userspace".into();
    let mut power = registry.build(&userspace, &mut buses).unwrap();
    power.init(&mut MockDelay::new()).unwrap();
    power.read(&mut buf).unwrap();
    assert_ne!(buf[4], 31_000);

    let mut fan = registry
        .build(board.device("fan").unwrap(), &mut buses)
        .unwrap();
    assert!(fan.is_sensor() && fan.is_control());
    fan.read(&mut buf).unwrap();
    assert_eq!(&buf[1..3], &[392, 2]);
    let mut zone = registry
        .build(board.device("cpu-thermal").unwrap(), &mut buses)
        .unwrap();
    zone.read(&mut buf).unwrap();
    assert_eq!(buf[0], 51_000);
}
