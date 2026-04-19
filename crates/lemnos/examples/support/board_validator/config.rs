use lemnos::prelude::GpioLevel;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioReadTarget {
    pub chip_name: String,
    pub offset: u32,
    pub expected_level: Option<GpioLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterfaceTarget {
    pub bus: u16,
    pub ports: Vec<u8>,
    pub interface_number: u8,
    pub alternate_setting: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedTarget {
    pub name: String,
    pub expect_trigger: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanTarget {
    pub hwmon_name: String,
    pub set_pwm: Option<u64>,
    pub set_mode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiTarget {
    pub bus: u32,
    pub chip_select: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bmm150Target {
    pub bus: u32,
    pub address: u16,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bmi055Target {
    pub bus: u32,
    pub accel_address: u16,
    pub gyro_address: u16,
    pub label: String,
    pub accel_interrupt: Option<GpioReadTarget>,
    pub gyro_interrupt: Option<GpioReadTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSensorKind {
    Ina226,
    Ina238,
    Ina260,
}

impl PowerSensorKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ina226" => Ok(Self::Ina226),
            "ina238" => Ok(Self::Ina238),
            "ina260" => Ok(Self::Ina260),
            _ => Err(format!(
                "unsupported POWER_KIND '{value}'; expected ina226, ina238, or ina260"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PowerTarget {
    pub kind: PowerSensorKind,
    pub bus: u32,
    pub address: u16,
    pub label: String,
    pub shunt_resistance_ohms: f64,
    pub max_current_a: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatorConfig {
    pub config_path: Option<PathBuf>,
    pub board_name: String,
    pub gpio_reads: Vec<GpioReadTarget>,
    pub leds: Vec<LedTarget>,
    pub fans: Vec<FanTarget>,
    pub spis: Vec<SpiTarget>,
    pub uart_ports: Vec<String>,
    pub usb_target: Option<UsbInterfaceTarget>,
    pub bmm150: Option<Bmm150Target>,
    pub bmi055: Option<Bmi055Target>,
    pub power: Option<PowerTarget>,
}

impl ValidatorConfig {
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut config_path = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "-h" | "--help" => return Err(Self::usage().to_string()),
                "--config" => {
                    let Some(path) = args.get(index + 1) else {
                        return Err("--config requires a path".into());
                    };
                    config_path = Some(PathBuf::from(path));
                    index += 2;
                }
                value if value.starts_with('-') => {
                    return Err(format!("unrecognized option '{value}'"));
                }
                value => {
                    config_path = Some(PathBuf::from(value));
                    index += 1;
                }
            }
        }

        let sources = ConfigSources::new(config_path.clone())?;
        let board_name = sources
            .get("BOARD_NAME")
            .unwrap_or_else(|| "lemnos-target".to_string());

        Ok(Self {
            config_path,
            board_name,
            gpio_reads: parse_gpio_targets(
                sources
                    .get("GPIO_READ_LINES")
                    .as_deref()
                    .unwrap_or_default(),
            )?,
            leds: parse_led_targets(sources.get("LED_TARGETS").as_deref().unwrap_or_default())?,
            fans: parse_fan_targets(sources.get("FAN_TARGETS").as_deref().unwrap_or_default())?,
            spis: parse_spi_targets(sources.get("SPI_TARGETS").as_deref().unwrap_or_default())?,
            uart_ports: parse_string_list(sources.get("UART_PORTS").as_deref().unwrap_or_default()),
            usb_target: parse_optional_usb_target(
                sources.get("USB_TARGET").as_deref(),
                sources.get("USB_ALT_SETTING").as_deref(),
            )?,
            bmm150: parse_bmm150(&sources)?,
            bmi055: parse_bmi055(&sources)?,
            power: parse_power(&sources)?,
        })
    }

    pub fn usage() -> &'static str {
        "usage: cargo run -p lemnos --example linux_device_validator -- [--config <path>|<path>]"
    }
}

struct ConfigSources {
    file_values: BTreeMap<String, String>,
}

impl ConfigSources {
    fn new(path: Option<PathBuf>) -> Result<Self, String> {
        let file_values = match path {
            Some(path) => parse_kv_file(&path)?,
            None => BTreeMap::new(),
        };
        Ok(Self { file_values })
    }

    fn get(&self, key: &str) -> Option<String> {
        env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| self.file_values.get(key).cloned())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
}

fn parse_kv_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read config file {}: {error}", path.display()))?;
    let mut values = BTreeMap::new();
    for (line_number, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "invalid config line {} in {}: expected KEY=VALUE",
                line_number + 1,
                path.display()
            ));
        };

        values.insert(key.trim().to_string(), value.trim().to_string());
    }

    Ok(values)
}

fn parse_bmm150(sources: &ConfigSources) -> Result<Option<Bmm150Target>, String> {
    let Some(bus) = sources.get("BMM150_BUS") else {
        return Ok(None);
    };
    let address = require(sources, "BMM150_ADDRESS")?;
    Ok(Some(Bmm150Target {
        bus: parse_u32(&bus)?,
        address: parse_u16(&address)?,
        label: sources
            .get("BMM150_LABEL")
            .unwrap_or_else(|| "bmm150".to_string()),
    }))
}

fn parse_bmi055(sources: &ConfigSources) -> Result<Option<Bmi055Target>, String> {
    let Some(bus) = get_first(sources, &["BMI088_BUS", "BMI055_BUS"]) else {
        return Ok(None);
    };

    let accel_interrupt = parse_optional_gpio_target(
        get_first(sources, &["BMI088_ACCEL_INT", "BMI055_ACCEL_INT"]).as_deref(),
    )?;
    let gyro_interrupt = parse_optional_gpio_target(
        get_first(sources, &["BMI088_GYRO_INT", "BMI055_GYRO_INT"]).as_deref(),
    )?;

    Ok(Some(Bmi055Target {
        bus: parse_u32(&bus)?,
        accel_address: parse_u16(&require_any(
            sources,
            &["BMI088_ACCEL_ADDRESS", "BMI055_ACCEL_ADDRESS"],
        )?)?,
        gyro_address: parse_u16(&require_any(
            sources,
            &["BMI088_GYRO_ADDRESS", "BMI055_GYRO_ADDRESS"],
        )?)?,
        label: get_first(sources, &["BMI088_LABEL", "BMI055_LABEL"])
            .unwrap_or_else(|| "bmi-imu".to_string()),
        accel_interrupt,
        gyro_interrupt,
    }))
}

fn parse_power(sources: &ConfigSources) -> Result<Option<PowerTarget>, String> {
    let Some(kind) = sources.get("POWER_KIND") else {
        return Ok(None);
    };
    let kind = PowerSensorKind::parse(&kind)?;
    let shunt_resistance_ohms = sources
        .get("POWER_SHUNT_RESISTANCE_OHMS")
        .as_deref()
        .map(parse_f64)
        .transpose()?
        .unwrap_or(0.0);
    let max_current_a = sources
        .get("POWER_MAX_CURRENT_A")
        .as_deref()
        .map(parse_f64)
        .transpose()?
        .unwrap_or(0.0);

    if matches!(kind, PowerSensorKind::Ina226)
        && (shunt_resistance_ohms <= 0.0 || max_current_a <= 0.0)
    {
        return Err(
            "POWER_KIND=ina226 requires POWER_SHUNT_RESISTANCE_OHMS and POWER_MAX_CURRENT_A".into(),
        );
    }

    if matches!(kind, PowerSensorKind::Ina238) && shunt_resistance_ohms <= 0.0 {
        return Err("POWER_KIND=ina238 requires POWER_SHUNT_RESISTANCE_OHMS".into());
    }

    Ok(Some(PowerTarget {
        kind,
        bus: parse_u32(&require(sources, "POWER_BUS")?)?,
        address: parse_u16(&require(sources, "POWER_ADDRESS")?)?,
        label: sources
            .get("POWER_LABEL")
            .unwrap_or_else(|| "power".to_string()),
        shunt_resistance_ohms,
        max_current_a,
    }))
}

fn require(sources: &ConfigSources, key: &str) -> Result<String, String> {
    sources
        .get(key)
        .ok_or_else(|| format!("missing required config key {key}"))
}

fn require_any(sources: &ConfigSources, keys: &[&str]) -> Result<String, String> {
    get_first(sources, keys).ok_or_else(|| {
        format!(
            "missing required config key; expected one of {}",
            keys.join(", ")
        )
    })
}

fn get_first(sources: &ConfigSources, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| sources.get(key))
}

fn parse_string_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_gpio_targets(value: &str) -> Result<Vec<GpioReadTarget>, String> {
    parse_string_list(value)
        .into_iter()
        .map(|entry| parse_gpio_target(&entry))
        .collect()
}

fn parse_led_targets(value: &str) -> Result<Vec<LedTarget>, String> {
    parse_string_list(value)
        .into_iter()
        .map(|entry| {
            let (name, expect_trigger) = match entry.split_once('=') {
                Some((name, trigger)) => (
                    name.trim().to_string(),
                    Some(trigger.trim().to_string()).filter(|value| !value.is_empty()),
                ),
                None => (entry.trim().to_string(), None),
            };
            if name.is_empty() {
                return Err("invalid LED target; expected led-name or led-name=trigger".into());
            }
            Ok(LedTarget {
                name,
                expect_trigger,
            })
        })
        .collect()
}

fn parse_fan_targets(value: &str) -> Result<Vec<FanTarget>, String> {
    parse_string_list(value)
        .into_iter()
        .map(|entry| {
            let mut parts = entry.split(':').map(str::trim);
            let Some(hwmon_name) = parts.next() else {
                return Err("invalid FAN target; expected hwmon-name[:pwm[:mode]]".into());
            };
            if hwmon_name.is_empty() {
                return Err("invalid FAN target; expected non-empty hwmon name".into());
            }
            let set_pwm = parts
                .next()
                .filter(|value| !value.is_empty())
                .map(parse_u64)
                .transpose()?;
            let set_mode = parts
                .next()
                .filter(|value| !value.is_empty())
                .map(parse_u64)
                .transpose()?;
            if parts.next().is_some() {
                return Err(format!(
                    "invalid FAN target '{entry}'; expected hwmon-name[:pwm[:mode]]"
                ));
            }
            Ok(FanTarget {
                hwmon_name: hwmon_name.to_string(),
                set_pwm,
                set_mode,
            })
        })
        .collect()
}

fn parse_spi_targets(value: &str) -> Result<Vec<SpiTarget>, String> {
    parse_string_list(value)
        .into_iter()
        .map(|entry| {
            let Some((bus, chip_select)) = entry.split_once(':') else {
                return Err(format!(
                    "invalid SPI target '{entry}'; expected bus:chip-select"
                ));
            };
            Ok(SpiTarget {
                bus: parse_u32(bus)?,
                chip_select: parse_u16(chip_select)?,
            })
        })
        .collect()
}

fn parse_optional_gpio_target(value: Option<&str>) -> Result<Option<GpioReadTarget>, String> {
    value.map(parse_gpio_target).transpose()
}

fn parse_gpio_target(value: &str) -> Result<GpioReadTarget, String> {
    let (target, expected_level) = match value.split_once('=') {
        Some((target, level)) => (target.trim(), Some(parse_gpio_level(level.trim())?)),
        None => (value.trim(), None),
    };
    let Some((chip_name, offset)) = target.rsplit_once(':') else {
        return Err(format!(
            "invalid GPIO target '{value}'; expected gpiochipN:offset or gpiochipN:offset=level"
        ));
    };
    Ok(GpioReadTarget {
        chip_name: chip_name.to_string(),
        offset: parse_u32(offset)?,
        expected_level,
    })
}

fn parse_optional_usb_target(
    value: Option<&str>,
    alternate_setting: Option<&str>,
) -> Result<Option<UsbInterfaceTarget>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut target = parse_usb_target(value)?;
    if let Some(alternate_setting) = alternate_setting {
        target.alternate_setting = Some(parse_u8(alternate_setting)?);
    }
    Ok(Some(target))
}

fn parse_usb_target(value: &str) -> Result<UsbInterfaceTarget, String> {
    let mut parts = value.split(':').map(str::trim);
    let Some(bus) = parts.next() else {
        return Err(format!(
            "invalid USB target '{value}'; expected bus:ports:interface"
        ));
    };
    let Some(ports) = parts.next() else {
        return Err(format!(
            "invalid USB target '{value}'; expected bus:ports:interface"
        ));
    };
    let Some(interface_number) = parts.next() else {
        return Err(format!(
            "invalid USB target '{value}'; expected bus:ports:interface"
        ));
    };
    if parts.next().is_some() {
        return Err(format!(
            "invalid USB target '{value}'; expected bus:ports:interface"
        ));
    }

    let ports = ports
        .split('.')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse_u8)
        .collect::<Result<Vec<_>, _>>()?;
    if ports.is_empty() {
        return Err(format!(
            "USB target '{value}' must include at least one port"
        ));
    }

    Ok(UsbInterfaceTarget {
        bus: parse_u16(bus)?,
        ports,
        interface_number: parse_u8(interface_number)?,
        alternate_setting: None,
    })
}

fn parse_gpio_level(value: &str) -> Result<GpioLevel, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" | "0" => Ok(GpioLevel::Low),
        "high" | "1" => Ok(GpioLevel::High),
        _ => Err(format!(
            "invalid GPIO level '{value}'; expected low/high or 0/1"
        )),
    }
}

fn parse_u8(value: &str) -> Result<u8, String> {
    u8::try_from(parse_u64(value)?).map_err(|_| format!("value '{value}' does not fit into u8"))
}

fn parse_u16(value: &str) -> Result<u16, String> {
    u16::try_from(parse_u64(value)?).map_err(|_| format!("value '{value}' does not fit into u16"))
}

fn parse_u32(value: &str) -> Result<u32, String> {
    u32::try_from(parse_u64(value)?).map_err(|_| format!("value '{value}' does not fit into u32"))
}

fn parse_u64(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
            .map_err(|error| format!("invalid hex integer '{value}': {error}"))
    } else {
        trimmed
            .parse::<u64>()
            .map_err(|error| format!("invalid integer '{value}': {error}"))
    }
}

fn parse_f64(value: &str) -> Result<f64, String> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|error| format!("invalid float '{value}': {error}"))
}
