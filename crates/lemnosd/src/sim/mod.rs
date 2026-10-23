//! `lemnosd --sim`: the board's devices over simulated buses.
//!
//! The service runs the same board definition with the same generic drivers
//! as on a board. Only the buses differ: the I2C chips are register models
//! (`chips`) on simulated buses (`bus`), their physics (`world`, `plant`) is scripted
//! (`script`), and the fan, the thermal zone and the LED frames are files in
//! a sysfs-shaped tree under the sim root (`sysfs`). GPIO lines are mock
//! lines: the switched ports load the supply, and scripted levels drive
//! inputs. Nothing here touches the host's hardware, so it runs on x86_64 in
//! a container.
//!
//! [`Sim::start`] builds the board and runs the plant on a thread, in
//! simulated time (the script's `speed`). Dropping the `Sim` stops it.
//! `docs/system-service.md` (Simulated board) has the configuration.

mod bus;
mod chips;
mod plant;
mod script;
mod sysfs;
mod world;

#[cfg(test)]
mod tests;

pub use script::{Script, ScriptError};

use crate::mock::{MockBuses, MockHardware};
use bus::{SimBus, SimBuses};
use chips::Chip;
use lemnos_board::{BoardDefinition, BusRef, Buses, ConfigValue, DeviceSpec, I2cSelector};
use lemnos_drivers_linux::SysRoot;
use plant::{Plant, Sysfs};
use script::GpioLevel;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The plant's tick, in real time: the chips' data is at most this stale.
const TICK: Duration = Duration::from_millis(2);

/// Why a simulated board cannot be started.
#[derive(Debug)]
pub enum SimError {
    /// The script is not usable.
    Script(ScriptError),
    /// The board definition asks for something the simulation does not model.
    Board(String),
    /// The sim root could not be written.
    Io(io::Error),
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Script(e) => write!(f, "sim script: {e}"),
            Self::Board(e) => write!(f, "sim board: {e}"),
            Self::Io(e) => write!(f, "sim root: {e}"),
        }
    }
}

impl std::error::Error for SimError {}

impl From<io::Error> for SimError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A running simulated board. Dropping it stops the plant.
pub struct Sim {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sim {
    /// Prepares `definition` for the simulated board and starts the plant.
    ///
    /// The board's ws2812 strips are written to files under `root/dev`, and
    /// its I2C selectors (`i2c:compatible=`, `i2c:name=`) are answered by
    /// adapters in `root/sys`. Returns the buses to build the service with.
    pub fn start(
        definition: &mut BoardDefinition,
        script: &Script,
        root: &Path,
    ) -> Result<(Self, Box<dyn Buses>), SimError> {
        let built = build(definition, script, root)?;
        let speed = built.plant.speed();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let mut plant = built.plant;
        let thread = std::thread::Builder::new()
            .name("lemnosd-sim".into())
            .spawn(move || {
                let start = Instant::now();
                while !thread_stop.load(Ordering::Relaxed) {
                    plant.step(start.elapsed().as_secs_f64() * speed);
                    std::thread::sleep(TICK);
                }
            })?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            built.buses,
        ))
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The parts of a simulated board, before its plant runs.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Built {
    pub(crate) plant: Plant,
    /// The mock hardware: GPIO lines, and the buses' sysfs root.
    pub(crate) hardware: MockHardware,
    pub(crate) buses: Box<dyn Buses>,
    /// The simulated I2C buses, by number (shared with the plant and the service).
    pub(crate) i2c: BTreeMap<u32, SimBus>,
    /// The sysfs root the buses read.
    pub(crate) sys: PathBuf,
}

/// Builds the simulated board: sysfs, chips, ports, the LED files and the
/// plant, which has taken its first step (so every register is set).
pub(crate) fn build(
    definition: &mut BoardDefinition,
    script: &Script,
    root: &Path,
) -> Result<Built, SimError> {
    let sys_dir = root.join("sys");
    let dev_dir = root.join("dev");
    fs::create_dir_all(&sys_dir)?;
    fs::create_dir_all(&dev_dir)?;
    let sys = SysRoot::new(&sys_dir);

    // The selectors name adapters the board does not number: each distinct
    // one gets an adapter with its name or device-tree node, numbered above
    // the board's own bus numbers.
    let numbered = definition
        .devices
        .iter()
        .filter_map(|d| match d.bus {
            Some(BusRef::I2c(bus)) => Some(bus),
            _ => None,
        })
        .max();
    let mut next_bus = numbered.map_or(16, |m| (m + 1).max(16));
    let mut adapters: Vec<(I2cSelector, u32)> = Vec::new();
    for spec in &definition.devices {
        let Some(BusRef::I2cMatch(selector)) = &spec.bus else {
            continue;
        };
        if adapters.iter().any(|(s, _)| s == selector) {
            continue;
        }
        let bus = next_bus;
        next_bus += 1;
        sysfs::write_adapter(
            &sys_dir,
            &sysfs::Adapter {
                bus,
                name: selector.name.clone(),
                compatible: selector.compatible.clone(),
                of: selector.of.clone(),
                node: selector.node.clone(),
            },
        )?;
        adapters.push((selector.clone(), bus));
    }

    let hardware = MockHardware::new();
    let mut i2c: BTreeMap<u32, SimBus> = BTreeMap::new();
    let mut chips: Vec<(String, Chip)> = Vec::new();
    let mut ports = Vec::new();
    let mut fan = None;
    let mut thermal = None;
    for spec in &mut definition.devices {
        match spec.driver.as_str() {
            "bmi088" => {
                let bus = bus_number(spec, &sys)?;
                let accel = device_address(spec, 0x18)?;
                let gyro = config_address(spec, "gyro_address", 0x68)?;
                let chip = Chip::Bmi088 {
                    bus: i2c.entry(bus).or_default().clone(),
                    accel,
                    gyro,
                }
                .attach();
                chips.push((spec.id.clone(), chip));
            }
            "bmm150" => {
                let bus = bus_number(spec, &sys)?;
                let address = device_address(spec, 0x10)?;
                let chip = Chip::Bmm150 {
                    bus: i2c.entry(bus).or_default().clone(),
                    address,
                }
                .attach();
                chips.push((spec.id.clone(), chip));
            }
            "ina238" => {
                let bus = bus_number(spec, &sys)?;
                let address = device_address(spec, 0x40)?;
                let micro = config_int(spec, "shunt_micro_ohms").ok_or_else(|| {
                    SimError::Board(format!(
                        "{}: the simulation needs shunt_micro_ohms",
                        spec.id
                    ))
                })?;
                let chip = Chip::Ina238 {
                    bus: i2c.entry(bus).or_default().clone(),
                    address,
                    shunt_ohms: micro as f64 * 1e-6,
                }
                .attach();
                chips.push((spec.id.clone(), chip));
            }
            "ws2812" => {
                // The LED frames are written to a file: the strip's frames
                // are read through the frame watch, and on disk for tools.
                let name = spec
                    .path
                    .as_deref()
                    .and_then(|p| Path::new(p).file_name())
                    .map_or_else(|| "leds0".into(), |n| n.to_string_lossy().into_owned());
                let file = dev_dir.join(name);
                fs::write(&file, [])?;
                spec.path = Some(file.display().to_string());
            }
            "gpio-power-switch" => {
                let (chip, line) = gpio_config(spec)?;
                ports.push(hardware.line(&chip, line));
            }
            "hwmon-fan" => {
                if fan.is_some() {
                    return Err(SimError::Board("the simulated board has one fan".into()));
                }
                if spec.path.is_some() {
                    return Err(SimError::Board(format!(
                        "{}: the simulated fan is found by match.name",
                        spec.id
                    )));
                }
                let name = spec.matches.get("name").map_or("pwmfan", String::as_str);
                fan = Some(sysfs::write_fan(&sys_dir, name)?);
            }
            "thermal-zone" => {
                if thermal.is_some() {
                    return Err(SimError::Board(
                        "the simulated board has one thermal zone".into(),
                    ));
                }
                let zone = spec
                    .matches
                    .get("type")
                    .map_or("cpu-thermal", String::as_str);
                thermal = Some(sysfs::write_thermal_zone(
                    &sys_dir,
                    zone,
                    (script.thermal.ambient_c * 1000.0) as i64,
                )?);
            }
            "ina226" | "ina260" | "vcm" => {
                return Err(SimError::Board(format!(
                    "{}: no simulated model for driver {:?} (simulated: bmi088, bmm150, ina238)",
                    spec.id, spec.driver
                )));
            }
            // gpio-output and gpio-input: mock lines, below.
            _ => {}
        }
    }

    let mut lines: Vec<(lemnos_hal::mock::MockLine, GpioLevel)> = Vec::new();
    for level in &script.gpio {
        lines.push((hardware.line(&level.chip, level.line), level.clone()));
    }
    let mut plant =
        Plant::new(script, chips, ports, lines, Sysfs { fan, thermal }).map_err(SimError::Board)?;
    plant.step(0.0);
    let buses = Box::new(SimBuses {
        i2c: i2c.clone(),
        rest: MockBuses::new(hardware.clone(), sys_dir.clone()),
    });
    Ok(Built {
        plant,
        hardware,
        buses,
        i2c,
        sys: sys_dir,
    })
}

/// The bus number a device's bus resolves to on the simulated tree.
fn bus_number(spec: &DeviceSpec, sys: &SysRoot) -> Result<u32, SimError> {
    match spec.bus.as_ref().and_then(|b| b.i2c_bus(sys)) {
        Some(Ok(bus)) => Ok(bus),
        Some(Err(reason)) => Err(SimError::Board(format!("{}: {reason}", spec.id))),
        None => Ok(0),
    }
}

/// The device's 7-bit address, or `default`.
fn device_address(spec: &DeviceSpec, default: u8) -> Result<u8, SimError> {
    match spec.address {
        Some(a) => u8::try_from(a)
            .map_err(|_| SimError::Board(format!("{}: address out of range", spec.id))),
        None => Ok(default),
    }
}

/// A 7-bit address from the device's config, or `default`.
fn config_address(spec: &DeviceSpec, key: &str, default: u8) -> Result<u8, SimError> {
    match config_int(spec, key) {
        Some(a) => {
            u8::try_from(a).map_err(|_| SimError::Board(format!("{}: {key} out of range", spec.id)))
        }
        None => Ok(default),
    }
}

fn config_int(spec: &DeviceSpec, key: &str) -> Option<i64> {
    spec.config.get(key).and_then(ConfigValue::as_i64)
}

/// The chip and offset of a GPIO device's line.
fn gpio_config(spec: &DeviceSpec) -> Result<(String, u32), SimError> {
    let chip = spec
        .config
        .get("chip")
        .and_then(ConfigValue::as_str)
        .ok_or_else(|| SimError::Board(format!("{}: needs config.chip", spec.id)))?;
    let line = config_int(spec, "line")
        .and_then(|l| u32::try_from(l).ok())
        .ok_or_else(|| SimError::Board(format!("{}: needs config.line", spec.id)))?;
    Ok((chip.to_string(), line))
}
