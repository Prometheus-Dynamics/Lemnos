//! Driver names a board definition can use, and how each builds its device.

use crate::schema::{Backend, BusRef, ConfigValue, DeviceSpec};
use crate::{BoardError, Buses, DynI2c};
use lemnos_device::{BoxedDevice, DeviceClass};
use lemnos_drivers_linux::{HwmonFan, I2cLocation, KernelDevice, ThermalZone};
use lemnos_hal::ErrorKind;
use std::path::PathBuf;

/// How a driver's devices are reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    /// On an I2C bus (`bus` required, `address` optional).
    I2c,
    /// A kernel class device found by `path` or `match` attributes.
    Platform,
}

/// Builds a device from its spec.
pub type Build = fn(&DeviceSpec, &mut dyn Buses) -> Result<BoxedDevice, BoardError>;

/// One driver name.
#[derive(Debug, Clone, Copy)]
pub struct DriverEntry {
    /// The name in `driver = "..."`.
    pub name: &'static str,
    pub summary: &'static str,
    pub class: DeviceClass,
    pub interface: Interface,
    /// The I2C address when the spec gives none.
    pub default_address: Option<u16>,
    /// The `config` keys it accepts.
    pub config_keys: &'static [&'static str],
    /// The `match` keys it accepts.
    pub match_keys: &'static [&'static str],
    /// The `config` keys that take one of a fixed set of strings, with the
    /// strings each accepts. `validate` checks these, so a bad value is
    /// reported with the key and the allowed values, not only when the
    /// driver binds.
    pub config_choices: &'static [(&'static str, &'static [&'static str])],
    /// Whether `backend = "kernel"` (and so `auto`) can use a kernel driver.
    pub kernel: bool,
    /// Whether it has a userspace driver.
    pub userspace: bool,
    pub build: Build,
}

impl DriverEntry {
    /// Problems with `spec` for this driver.
    pub fn check(&self, spec: &DeviceSpec) -> Vec<String> {
        let mut problems = Vec::new();
        match self.interface {
            Interface::I2c => {
                if !spec.bus.as_ref().is_some_and(BusRef::is_i2c) {
                    problems.push(format!(
                        "{} needs `bus = \"i2c-<n>\"` or `bus = \"i2c:<key>=<value>\"`",
                        self.name
                    ));
                }
                if spec.address.is_none() && self.default_address.is_none() {
                    problems.push(format!("{} needs an `address`", self.name));
                }
                if spec.address.is_some_and(|a| a > 0x7f) {
                    problems.push("address must be a 7-bit address".into());
                }
            }
            Interface::Platform => {
                if spec.bus.is_some() || spec.address.is_some() {
                    problems.push(format!("{} takes `path` or `match`, not a bus", self.name));
                }
            }
        }
        if spec.backend == Backend::Kernel && !self.kernel {
            problems.push(format!("{} has no kernel backend", self.name));
        }
        if spec.backend == Backend::Userspace && !self.userspace {
            problems.push(format!("{} has no userspace driver", self.name));
        }
        for (key, allowed) in self.config_choices {
            let Some(value) = spec.config.get(*key) else {
                continue;
            };
            if !value.as_str().is_some_and(|text| allowed.contains(&text)) {
                let given = value
                    .as_str()
                    .map_or_else(|| format!("{value:?}"), |text| format!("{text:?}"));
                problems.push(format!(
                    "config {key:?} is {given}, must be one of: {}",
                    allowed.join(", ")
                ));
            }
        }
        for key in spec.config.keys() {
            if !self.config_keys.contains(&key.as_str()) {
                problems.push(format!(
                    "unknown config key {key:?} (accepted: {})",
                    self.config_keys.join(", ")
                ));
            }
        }
        for key in spec.matches.keys() {
            if !self.match_keys.contains(&key.as_str()) {
                problems.push(format!("unknown match key {key:?}"));
            }
        }
        problems
    }
}

/// The drivers a host can build. [`DriverRegistry::builtin`] has every
/// driver in this workspace; hosts can register more.
#[derive(Debug, Clone)]
pub struct DriverRegistry {
    entries: Vec<DriverEntry>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl DriverRegistry {
    /// No drivers.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The built-in drivers: `bmi088`, `bmm150`, `ina226`, `ina238`,
    /// `ina260`, `vcm`, `hwmon-fan`, `thermal-zone`, `ws2812`,
    /// `gpio-output`, `gpio-input`.
    pub fn builtin() -> Self {
        Self {
            entries: BUILTIN.to_vec(),
        }
    }

    /// Adds or replaces a driver.
    pub fn register(&mut self, entry: DriverEntry) {
        self.entries.retain(|e| e.name != entry.name);
        self.entries.push(entry);
    }

    pub fn get(&self, name: &str) -> Option<&DriverEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn entries(&self) -> &[DriverEntry] {
        &self.entries
    }

    /// Checks and builds the device `spec` describes.
    pub fn build(
        &self,
        spec: &DeviceSpec,
        buses: &mut dyn Buses,
    ) -> Result<BoxedDevice, BoardError> {
        let entry = self.get(&spec.driver).ok_or_else(|| {
            BoardError::device(
                &spec.id,
                ErrorKind::Configuration,
                format!("unknown driver {:?}", spec.driver),
            )
        })?;
        let problems = entry.check(spec);
        if !problems.is_empty() {
            return Err(BoardError::device(
                &spec.id,
                ErrorKind::Configuration,
                problems.join("; "),
            ));
        }
        (entry.build)(spec, buses)
    }
}

// --- settings ---------------------------------------------------------------

fn bad(spec: &DeviceSpec, reason: impl Into<String>) -> BoardError {
    BoardError::device(&spec.id, ErrorKind::Configuration, reason)
}

fn choice<T: Copy>(
    spec: &DeviceSpec,
    key: &str,
    options: &[(&str, T)],
    default: T,
) -> Result<T, BoardError> {
    match spec.config.get(key) {
        None => Ok(default),
        Some(value) => {
            let text = value.as_str().unwrap_or_default();
            options
                .iter()
                .find(|(name, _)| *name == text)
                .map(|(_, v)| *v)
                .ok_or_else(|| {
                    let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
                    bad(spec, format!("{key} must be one of {}", names.join(", ")))
                })
        }
    }
}

fn integer(spec: &DeviceSpec, key: &str) -> Result<Option<i64>, BoardError> {
    spec.config
        .get(key)
        .map(|v| {
            v.as_i64()
                .ok_or_else(|| bad(spec, format!("{key} must be an integer")))
        })
        .transpose()
}

fn number(spec: &DeviceSpec, key: &str) -> Result<Option<f64>, BoardError> {
    spec.config
        .get(key)
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| bad(spec, format!("{key} must be a number")))
        })
        .transpose()
}

/// The device's I2C bus number (a selector is resolved under the buses'
/// sysfs root) and address.
fn i2c(
    spec: &DeviceSpec,
    entry_default: Option<u16>,
    buses: &dyn Buses,
) -> Result<(u32, u16), BoardError> {
    let bus = match spec.bus.as_ref().and_then(|b| b.i2c_bus(&buses.sys())) {
        Some(Ok(bus)) => bus,
        Some(Err(reason)) => return Err(BoardError::device(&spec.id, ErrorKind::NotFound, reason)),
        None => 0,
    };
    Ok((bus, spec.address.or(entry_default).unwrap_or_default()))
}

fn open(spec: &DeviceSpec, buses: &mut dyn Buses, bus: u32) -> Result<DynI2c, BoardError> {
    buses.i2c(bus).map_err(|e| match e {
        BoardError::Device { kind, reason, .. } => BoardError::device(&spec.id, kind, reason),
        other => other,
    })
}

/// The kernel-backed device for `spec` if its backend allows one and the
/// kernel driver is bound; an error if the backend demands one and it is
/// missing.
fn kernel_device(
    spec: &DeviceSpec,
    buses: &dyn Buses,
    info: &'static lemnos_device::DeviceInfo,
    binding: &'static lemnos_device::kernel::KernelBinding,
    location: Option<I2cLocation<'_>>,
) -> Result<Option<BoxedDevice>, BoardError> {
    if spec.backend == Backend::Userspace {
        return Ok(None);
    }
    let found = KernelDevice::find(info, binding, &buses.sys(), location)
        .map_err(|e| BoardError::device(&spec.id, ErrorKind::Failed, e.to_string()))?;
    match (found, spec.backend) {
        (Some(device), _) => Ok(Some(BoxedDevice::sensor(device))),
        (None, Backend::Kernel) => Err(BoardError::device(
            &spec.id,
            ErrorKind::NotFound,
            format!("no bound kernel driver for {}", info.model),
        )),
        (None, _) => Ok(None),
    }
}

// --- built-in drivers -------------------------------------------------------

const BUILTIN: &[DriverEntry] = &[
    DriverEntry {
        name: "bmi088",
        summary: "Bosch BMI088 IMU (accelerometer + gyroscope)",
        class: DeviceClass::Imu,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_bmi088::ACCEL_ADDRESS as u16),
        config_keys: &[
            "gyro_address",
            "accel_range",
            "accel_rate",
            "gyro_range",
            "gyro_rate",
            "fifo",
        ],
        match_keys: &[],
        config_choices: BMI088_CHOICES,
        kernel: true,
        userspace: true,
        build: build_bmi088,
    },
    DriverEntry {
        name: "bmm150",
        summary: "Bosch BMM150 magnetometer",
        class: DeviceClass::Magnetometer,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_bmm150::DEFAULT_ADDRESS as u16),
        config_keys: &["preset", "data_rate"],
        match_keys: &[],
        config_choices: BMM150_CHOICES,
        kernel: true,
        userspace: true,
        build: build_bmm150,
    },
    DriverEntry {
        name: "ina226",
        summary: "TI INA226 power monitor",
        class: DeviceClass::PowerMonitor,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_ina2xx::DEFAULT_ADDRESS as u16),
        config_keys: INA_KEYS,
        match_keys: &[],
        config_choices: &[],
        kernel: true,
        userspace: true,
        build: build_ina226,
    },
    DriverEntry {
        name: "ina238",
        summary: "TI INA238 power monitor",
        class: DeviceClass::PowerMonitor,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_ina2xx::DEFAULT_ADDRESS as u16),
        config_keys: INA_KEYS,
        match_keys: &[],
        config_choices: &[],
        kernel: true,
        userspace: true,
        build: build_ina238,
    },
    DriverEntry {
        name: "ina260",
        summary: "TI INA260 power monitor (integrated shunt)",
        class: DeviceClass::PowerMonitor,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_ina2xx::DEFAULT_ADDRESS as u16),
        config_keys: &[],
        match_keys: &[],
        config_choices: &[],
        kernel: true,
        userspace: true,
        build: build_ina260,
    },
    DriverEntry {
        name: "vcm",
        summary: "Camera focus voice-coil motor (DW9714, DW9807, DW9817, AK7375)",
        class: DeviceClass::Lens,
        interface: Interface::I2c,
        default_address: Some(lemnos_drivers_vcm::DEFAULT_ADDRESS as u16),
        config_keys: &["chip"],
        match_keys: &[],
        config_choices: &[],
        kernel: false,
        userspace: true,
        build: build_vcm,
    },
    DriverEntry {
        name: "hwmon-fan",
        summary: "Linux hwmon fan (pwm-fan and similar)",
        class: DeviceClass::Fan,
        interface: Interface::Platform,
        default_address: None,
        // `restore_mode`: for chips with an automatic mode, the
        // `pwm1_enable` value that hands the fan back when a host stops
        // (default 2). `pwm-fan` fans go back through their cooling device.
        config_keys: &["restore_mode"],
        match_keys: &["name"],
        config_choices: &[],
        kernel: true,
        userspace: false,
        build: build_hwmon_fan,
    },
    DriverEntry {
        name: "thermal-zone",
        summary: "Linux thermal zone",
        class: DeviceClass::Temperature,
        interface: Interface::Platform,
        default_address: None,
        config_keys: &[],
        match_keys: &["type"],
        config_choices: &[],
        kernel: true,
        userspace: false,
        build: build_thermal_zone,
    },
    DriverEntry {
        name: "ws2812",
        summary: "WS2812/SK6812 LED strip or ring (Raspberry Pi RP1 ws2812-pio device)",
        class: DeviceClass::Light,
        interface: Interface::Platform,
        default_address: None,
        config_keys: crate::light::LIGHT_KEYS,
        match_keys: &[],
        config_choices: LIGHT_CHOICES,
        kernel: true,
        userspace: false,
        build: build_ws2812,
    },
    DriverEntry {
        name: "gpio-output",
        summary: "GPIO output line",
        class: DeviceClass::Gpio,
        interface: Interface::Platform,
        default_address: None,
        config_keys: &["chip", "line", "active_low", "initial"],
        match_keys: &[],
        config_choices: &[],
        kernel: false,
        userspace: true,
        build: build_gpio_output,
    },
    DriverEntry {
        name: "gpio-input",
        summary: "GPIO input line",
        class: DeviceClass::Gpio,
        interface: Interface::Platform,
        default_address: None,
        config_keys: &["chip", "line", "active_low"],
        match_keys: &[],
        config_choices: &[],
        kernel: false,
        userspace: true,
        build: build_gpio_input,
    },
];

const BMI088_CHOICES: &[(&str, &[&str])] = &[
    ("accel_range", &["3g", "6g", "12g", "24g"]),
    (
        "accel_rate",
        &[
            "12.5hz", "25hz", "50hz", "100hz", "200hz", "400hz", "800hz", "1600hz",
        ],
    ),
    (
        "gyro_range",
        &["125dps", "250dps", "500dps", "1000dps", "2000dps"],
    ),
    (
        "gyro_rate",
        &[
            "2000hz-532",
            "2000hz-230",
            "1000hz-116",
            "400hz-47",
            "200hz-23",
            "100hz-12",
            "200hz-64",
            "100hz-32",
        ],
    ),
];

const BMM150_CHOICES: &[(&str, &[&str])] = &[
    (
        "preset",
        &["low-power", "regular", "enhanced", "high-accuracy"],
    ),
    (
        "data_rate",
        &["2hz", "6hz", "8hz", "10hz", "15hz", "20hz", "25hz", "30hz"],
    ),
];

/// The ws2812 light's effect keys (see `light.rs`).
const LIGHT_CHOICES: &[(&str, &[&str])] = &[
    ("status_effect", &["solid", "blink", "breathe", "chase"]),
    ("error_effect", &["solid", "blink", "breathe", "chase"]),
    ("locate_effect", &["solid", "blink", "breathe", "chase"]),
];

const INA_KEYS: &[&str] = &[
    "shunt_micro_ohms",
    "shunt_ohms",
    "max_current_micro_amps",
    "max_current_amps",
];

fn build_bmi088(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    use lemnos_drivers_bmi088::{AccelRange, AccelRate, Bmi088, Config, GyroRange, GyroRate};
    let (bus, accel) = i2c(
        spec,
        Some(lemnos_drivers_bmi088::ACCEL_ADDRESS.into()),
        &*buses,
    )?;
    let gyro = integer(spec, "gyro_address")?
        .map(|a| u16::try_from(a).map_err(|_| bad(spec, "gyro_address must be a 7-bit address")))
        .transpose()?
        .unwrap_or(lemnos_drivers_bmi088::GYRO_ADDRESS.into());
    let location = I2cLocation {
        bus,
        addresses: &[accel, gyro],
    };
    if let Some(device) = kernel_device(
        spec,
        buses,
        &lemnos_drivers_bmi088::INFO,
        &lemnos_drivers_bmi088::KERNEL,
        Some(location),
    )? {
        return Ok(device);
    }
    let config = Config {
        accel_range: choice(
            spec,
            "accel_range",
            &[
                ("3g", AccelRange::G3),
                ("6g", AccelRange::G6),
                ("12g", AccelRange::G12),
                ("24g", AccelRange::G24),
            ],
            AccelRange::G6,
        )?,
        accel_rate: choice(
            spec,
            "accel_rate",
            &[
                ("12.5hz", AccelRate::Hz12_5),
                ("25hz", AccelRate::Hz25),
                ("50hz", AccelRate::Hz50),
                ("100hz", AccelRate::Hz100),
                ("200hz", AccelRate::Hz200),
                ("400hz", AccelRate::Hz400),
                ("800hz", AccelRate::Hz800),
                ("1600hz", AccelRate::Hz1600),
            ],
            AccelRate::Hz100,
        )?,
        gyro_range: choice(
            spec,
            "gyro_range",
            &[
                ("125dps", GyroRange::Dps125),
                ("250dps", GyroRange::Dps250),
                ("500dps", GyroRange::Dps500),
                ("1000dps", GyroRange::Dps1000),
                ("2000dps", GyroRange::Dps2000),
            ],
            GyroRange::Dps2000,
        )?,
        gyro_rate: choice(
            spec,
            "gyro_rate",
            &[
                ("2000hz-532", GyroRate::Hz2000Bw532),
                ("2000hz-230", GyroRate::Hz2000Bw230),
                ("1000hz-116", GyroRate::Hz1000Bw116),
                ("400hz-47", GyroRate::Hz400Bw47),
                ("200hz-23", GyroRate::Hz200Bw23),
                ("100hz-12", GyroRate::Hz100Bw12),
                ("200hz-64", GyroRate::Hz200Bw64),
                ("100hz-32", GyroRate::Hz100Bw32),
            ],
            GyroRate::Hz2000Bw532,
        )?,
    };
    let bus = open(spec, buses, bus)?;
    let address = |a: u16| u8::try_from(a).map_err(|_| bad(spec, "address out of range"));
    // The FIFOs are opt-in (`fifo = true`): a batch read returns every sample
    // since the last read, but at the chip's output rate, so it costs more bus
    // time than polling unless the output rate matches what is subscribed.
    let fifo = spec
        .config
        .get("fifo")
        .and_then(ConfigValue::as_bool)
        .unwrap_or(false);
    let mut imu = Bmi088::with_addresses(bus, address(accel)?, address(gyro)?).with_config(config);
    if fifo {
        imu = imu.with_fifo();
    }
    Ok(BoxedDevice::sensor(imu))
}

fn build_bmm150(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    use lemnos_drivers_bmm150::{Bmm150, Config, DataRate, Preset};
    let (bus, address) = i2c(
        spec,
        Some(lemnos_drivers_bmm150::DEFAULT_ADDRESS.into()),
        &*buses,
    )?;
    if let Some(device) = kernel_device(
        spec,
        buses,
        &lemnos_drivers_bmm150::INFO,
        &lemnos_drivers_bmm150::KERNEL,
        Some(I2cLocation {
            bus,
            addresses: &[address],
        }),
    )? {
        return Ok(device);
    }
    let config = Config {
        preset: choice(
            spec,
            "preset",
            &[
                ("low-power", Preset::LowPower),
                ("regular", Preset::Regular),
                ("enhanced", Preset::Enhanced),
                ("high-accuracy", Preset::HighAccuracy),
            ],
            Preset::Regular,
        )?,
        data_rate: choice(
            spec,
            "data_rate",
            &[
                ("2hz", DataRate::Hz2),
                ("6hz", DataRate::Hz6),
                ("8hz", DataRate::Hz8),
                ("10hz", DataRate::Hz10),
                ("15hz", DataRate::Hz15),
                ("20hz", DataRate::Hz20),
                ("25hz", DataRate::Hz25),
                ("30hz", DataRate::Hz30),
            ],
            DataRate::Hz10,
        )?,
    };
    let address = u8::try_from(address).map_err(|_| bad(spec, "address out of range"))?;
    let bus = open(spec, buses, bus)?;
    Ok(BoxedDevice::sensor(
        Bmm150::new(bus, address).with_config(config),
    ))
}

fn build_ina(
    spec: &DeviceSpec,
    buses: &mut dyn Buses,
    model: lemnos_drivers_ina2xx::Model,
) -> Result<BoxedDevice, BoardError> {
    use lemnos_drivers_ina2xx::{Config, Ina, Model};
    let (bus, address) = i2c(
        spec,
        Some(lemnos_drivers_ina2xx::DEFAULT_ADDRESS.into()),
        &*buses,
    )?;
    if let Some(device) = kernel_device(
        spec,
        buses,
        model.info(),
        model.kernel(),
        Some(I2cLocation {
            bus,
            addresses: &[address],
        }),
    )? {
        return Ok(device);
    }
    let micro = |integer_key: &str, unit_key: &str| -> Result<Option<u32>, BoardError> {
        if let Some(v) = integer(spec, integer_key)? {
            return u32::try_from(v)
                .map(Some)
                .map_err(|_| bad(spec, format!("{integer_key} is out of range")));
        }
        Ok(number(spec, unit_key)?
            .map(|v| (v * 1e6).round().clamp(0.0, f64::from(u32::MAX)) as u32))
    };
    let config = if model == Model::Ina260 {
        Config::from_micro(2_000, 1_000_000)
    } else {
        let shunt = micro("shunt_micro_ohms", "shunt_ohms")?
            .ok_or_else(|| bad(spec, "needs shunt_micro_ohms (or shunt_ohms)"))?;
        let max = micro("max_current_micro_amps", "max_current_amps")?
            .ok_or_else(|| bad(spec, "needs max_current_micro_amps (or max_current_amps)"))?;
        Config::from_micro(shunt, max)
    };
    let address = u8::try_from(address).map_err(|_| bad(spec, "address out of range"))?;
    let bus = open(spec, buses, bus)?;
    Ina::new(bus, address, model, config)
        .map(BoxedDevice::sensor)
        .map_err(|e| bad(spec, e.to_string()))
}

fn build_ina226(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    build_ina(spec, buses, lemnos_drivers_ina2xx::Model::Ina226)
}

fn build_ina238(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    build_ina(spec, buses, lemnos_drivers_ina2xx::Model::Ina238)
}

fn build_ina260(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    build_ina(spec, buses, lemnos_drivers_ina2xx::Model::Ina260)
}

fn build_vcm(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    use lemnos_drivers_vcm::{Vcm, VcmChip};
    let chip = match spec.config.get("chip").and_then(ConfigValue::as_str) {
        Some(name) => VcmChip::from_name(name)
            .ok_or_else(|| bad(spec, format!("unknown VCM chip {name:?}")))?,
        None => {
            return Err(bad(
                spec,
                "needs config.chip (dw9714, dw9807, dw9817, ak7375)",
            ));
        }
    };
    let (bus, address) = i2c(
        spec,
        Some(lemnos_drivers_vcm::DEFAULT_ADDRESS.into()),
        &*buses,
    )?;
    let address = u8::try_from(address).map_err(|_| bad(spec, "address out of range"))?;
    let bus = open(spec, buses, bus)?;
    Vcm::new(bus, address, chip.format())
        .map(BoxedDevice::control)
        .map_err(|e| bad(spec, e.to_string()))
}

fn not_found(spec: &DeviceSpec, what: &str) -> BoardError {
    BoardError::device(&spec.id, ErrorKind::NotFound, format!("no {what} matches"))
}

fn build_hwmon_fan(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    let fan = match &spec.path {
        Some(path) => HwmonFan::new(PathBuf::from(path)),
        None => HwmonFan::find(
            &buses.sys().hwmon(),
            spec.matches.get("name").map(String::as_str),
        )
        .map_err(|e| BoardError::device(&spec.id, ErrorKind::Failed, e.to_string()))?
        .ok_or_else(|| not_found(spec, "hwmon fan"))?,
    };
    Ok(BoxedDevice::both(fan))
}

fn build_thermal_zone(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    let zone = match (&spec.path, spec.matches.get("type")) {
        (Some(path), _) => ThermalZone::new(PathBuf::from(path)),
        (None, Some(zone_type)) => ThermalZone::find(&buses.sys().thermal(), zone_type)
            .map_err(|e| BoardError::device(&spec.id, ErrorKind::Failed, e.to_string()))?
            .ok_or_else(|| not_found(spec, "thermal zone"))?,
        (None, None) => return Err(bad(spec, "needs `path` or `match.type`")),
    };
    Ok(BoxedDevice::sensor(zone))
}

fn build_ws2812(spec: &DeviceSpec, _buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    let config = crate::light::strip_config(spec)?;
    // Check the look settings now, so a bad colour fails validation-time builds.
    crate::light::light_defaults(spec)?;
    let path = spec.path.clone().unwrap_or_else(|| "/dev/leds0".into());
    Ok(BoxedDevice::light(lemnos_drivers_linux::Ws2812Pio::new(
        path, config,
    )))
}

fn gpio_ref(spec: &DeviceSpec) -> Result<crate::GpioRef, BoardError> {
    let chip = spec
        .config
        .get("chip")
        .and_then(ConfigValue::as_str)
        .ok_or_else(|| bad(spec, "needs config.chip (gpiochipN or a chip label)"))?
        .to_string();
    let line = integer(spec, "line")?
        .and_then(|l| u32::try_from(l).ok())
        .ok_or_else(|| bad(spec, "needs config.line (the offset on the chip)"))?;
    let active_low = spec
        .config
        .get("active_low")
        .and_then(ConfigValue::as_bool)
        .unwrap_or(false);
    Ok(crate::GpioRef {
        chip,
        line,
        active_low,
    })
}

fn build_gpio_output(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    let line = gpio_ref(spec)?;
    let initial = spec
        .config
        .get("initial")
        .and_then(ConfigValue::as_bool)
        .unwrap_or(false);
    let pin = buses.gpio_output(&line, initial)?;
    Ok(BoxedDevice::both(lemnos_device::gpio::OutputLine::new(
        pin, initial,
    )))
}

fn build_gpio_input(spec: &DeviceSpec, buses: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    let line = gpio_ref(spec)?;
    let pin = buses.gpio_input(&line)?;
    Ok(BoxedDevice::sensor(lemnos_device::gpio::InputLine::new(
        pin,
    )))
}
