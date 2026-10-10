//! A real `lemnosd` over mock hardware, for client tests (feature `mock`).
//!
//! [`MockLemnosd`] runs the actual [`Service`](crate::Service) on a thread,
//! on a socket in a temporary directory, with in-memory I2C buses, GPIO
//! lines, PWM channels and SPI devices ([`MockHardware`]) and a fake sysfs
//! tree under the same directory. Arbitration, claims ending with the
//! connection and control restores behave exactly as on a board.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use lemnosd::mock::{MockHardware, MockLemnosd};
//!
//! let hardware = MockHardware::new();
//! hardware.i2c(1).clone(); // preset registers with `with_registers`
//! let service = MockLemnosd::start(
//!     r#"
//! format = "lemnos.board"
//! schema_version = 1
//! [board]
//! id = "test"
//! [[lines]]
//! name = "aux"
//! chip = "gpiochip0"
//! line = 4
//! safe = "low"
//! "#,
//!     hardware.clone(),
//! )?;
//! let client = lemnos_ipc::DeviceClient::connect(service.socket(), "helios");
//! # drop(client);
//! # Ok(())
//! # }
//! ```

use crate::{Service, ServiceConfig};
use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use lemnos_board::raw::{DynLine, DynPwm, DynSpi};
use lemnos_board::{
    BoardDefinition, BoardError, Buses, DynI2c, DynInputPin, DynOutputPin, GpioRef,
};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::ErrorKind;
use lemnos_hal::mock::{MockI2c, MockLine, MockPwm, MockRawSpi};
use lemnos_hal::raw::{LineConfig, RawLine};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

type Map<K, V> = Arc<Mutex<BTreeMap<K, V>>>;

fn get<K: Ord + Clone, V: Clone + Default>(map: &Map<K, V>, key: K) -> V {
    map.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_default()
        .clone()
}

/// The mock hardware behind a [`MockLemnosd`]. Every accessor returns a
/// shared handle (created on first use): preset it before or inspect it
/// after the service uses it.
#[derive(Debug, Clone, Default)]
pub struct MockHardware {
    i2c: Map<u32, MockI2c>,
    lines: Map<(String, u32), MockLine>,
    pwms: Map<(u32, u32), MockPwm>,
    spi: Map<(u32, u16), MockRawSpi>,
    names: Map<String, (String, u32)>,
    labels: Map<String, String>,
    missing_buses: Arc<Mutex<Vec<u32>>>,
}

impl MockHardware {
    pub fn new() -> Self {
        Self::default()
    }

    /// I2C bus `bus` (every bus exists unless [`remove_i2c`](Self::remove_i2c)).
    pub fn i2c(&self, bus: u32) -> MockI2c {
        get(&self.i2c, bus)
    }

    /// Makes I2C bus `bus` absent.
    pub fn remove_i2c(&self, bus: u32) {
        self.missing_buses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bus);
    }

    /// Line `offset` of chip `chip` (by its `gpiochipN` name).
    pub fn line(&self, chip: &str, offset: u32) -> MockLine {
        get(&self.lines, (chip.to_string(), offset))
    }

    /// Gives line `offset` of `chip` the kernel line name `name`.
    pub fn name_line(&self, name: &str, chip: &str, offset: u32) {
        self.names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string(), (chip.to_string(), offset));
    }

    /// Gives chip `chip` (`gpiochipN`) the label `label`.
    pub fn label_chip(&self, label: &str, chip: &str) {
        self.labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(label.to_string(), chip.to_string());
    }

    pub fn pwm(&self, chip: u32, channel: u32) -> MockPwm {
        get(&self.pwms, (chip, channel))
    }

    pub fn spi(&self, bus: u32, chip_select: u16) -> MockRawSpi {
        get(&self.spi, (bus, chip_select))
    }
}

/// The service's buses: the mock hardware, and the fake sysfs at `sys`.
struct MockBuses {
    hardware: MockHardware,
    sys: PathBuf,
}

impl Buses for MockBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        let missing = self
            .hardware
            .missing_buses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&bus);
        if missing {
            return Err(BoardError::device(
                &format!("i2c-{bus}"),
                ErrorKind::NotFound,
                "no such bus",
            ));
        }
        Ok(DynI2c::new(self.hardware.i2c(bus)))
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.sys)
    }

    fn line(
        &mut self,
        chip: &str,
        offset: u32,
        config: &LineConfig,
        _consumer: &str,
    ) -> Result<DynLine, BoardError> {
        let mut line = self.hardware.line(&self.line_chip_id(chip), offset);
        line.configure(config)
            .map_err(|kind| BoardError::device(&format!("{chip}:{offset}"), kind, "configure"))?;
        Ok(DynLine::new(line, None))
    }

    fn line_chip_id(&self, chip: &str) -> String {
        self.hardware
            .labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(chip)
            .cloned()
            .unwrap_or_else(|| chip.to_string())
    }

    fn find_line(&self, name: &str) -> Option<(String, u32)> {
        self.hardware
            .names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    fn pwm(&mut self, chip: u32, channel: u32) -> Result<DynPwm, BoardError> {
        Ok(DynPwm::new(self.hardware.pwm(chip, channel)))
    }

    fn gpio_output(&mut self, line: &GpioRef, initial: bool) -> Result<DynOutputPin, BoardError> {
        let mut config = LineConfig::output(initial);
        config.active_low = line.active_low;
        Ok(DynOutputPin::new(MockGpio(self.gpio(line, config)?)))
    }

    fn gpio_input(&mut self, line: &GpioRef) -> Result<DynInputPin, BoardError> {
        let mut config = LineConfig::input();
        config.active_low = line.active_low;
        Ok(DynInputPin::new(MockGpio(self.gpio(line, config)?)))
    }

    fn spi(&mut self, bus: u32, chip_select: u16) -> Result<DynSpi, BoardError> {
        Ok(DynSpi::new(self.hardware.spi(bus, chip_select)))
    }
}

impl MockBuses {
    /// Configures mock line `line` as `config` (the hardware's own line, so a
    /// test reads what the service drove).
    fn gpio(&self, line: &GpioRef, config: LineConfig) -> Result<MockLine, BoardError> {
        let mut pin = self
            .hardware
            .line(&self.line_chip_id(&line.chip), line.line);
        pin.configure(&config).map_err(|kind| {
            BoardError::device(&format!("{}:{}", line.chip, line.line), kind, "configure")
        })?;
        Ok(pin)
    }
}

/// A mock line as an embedded-hal pin, for the GPIO-backed devices.
struct MockGpio(MockLine);

impl ErrorType for MockGpio {
    type Error = ErrorKind;
}

impl OutputPin for MockGpio {
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        RawLine::set(&mut self.0, false)
    }
    fn set_high(&mut self) -> Result<(), ErrorKind> {
        RawLine::set(&mut self.0, true)
    }
}

impl InputPin for MockGpio {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        RawLine::get(&mut self.0)
    }
    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        RawLine::get(&mut self.0).map(|high| !high)
    }
}

/// A running service over [`MockHardware`]; stopped (as `systemctl stop`
/// would: claims released, fans handed back) when dropped.
pub struct MockLemnosd {
    root: PathBuf,
    socket: PathBuf,
    hardware: MockHardware,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Keep the directory when dropped (a restart reuses its state).
    keep_root: bool,
}

impl MockLemnosd {
    /// Starts a service for the board definition `board` (TOML; `{root}`
    /// stands for the service's directory, whose `sys/` is its sysfs root).
    pub fn start(board: &str, hardware: MockHardware) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnosd-mock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sys"))?;
        Self::start_in(root, board, hardware)
    }

    /// [`start`](Self::start) in a directory the caller prepared (for
    /// example with fake sysfs files under `root/sys`).
    pub fn start_in(root: PathBuf, board: &str, hardware: MockHardware) -> io::Result<Self> {
        let state = root.join("state");
        Self::start_with_state(root, board, hardware, Some(state))
    }

    /// [`start_in`](Self::start_in) with the service's state directory
    /// (`None`: none, so a power switch's `persist` is ignored).
    pub fn start_with_state(
        root: PathBuf,
        board: &str,
        hardware: MockHardware,
        state_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let text = board.replace("{root}", &root.display().to_string());
        let definition = BoardDefinition::from_toml_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let socket = root.join("lemnosd.sock");
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let buses = MockBuses {
            hardware: hardware.clone(),
            sys: root.join("sys"),
        };
        let (thread_stop, thread_socket) = (Arc::clone(&stop), socket.clone());
        let thread = std::thread::spawn(move || {
            let mut config = ServiceConfig::new(definition, thread_socket);
            config.state_dir = state_dir;
            match Service::new(config, Box::new(buses)) {
                Ok(mut service) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = service.run(&thread_stop);
                    service.shutdown(false);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            }
        });
        match ready_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(Self {
                root,
                socket,
                hardware,
                stop,
                thread: Some(thread),
                keep_root: false,
            }),
            Ok(Err(error)) => Err(io::Error::other(error)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service did not start",
            )),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The service's directory (`sys/` is its sysfs root).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn hardware(&self) -> &MockHardware {
        &self.hardware
    }

    /// Stops the service (claims released, fans handed back) and waits.
    pub fn stop(mut self) {
        self.shut_down();
    }

    /// Stops the service and keeps its directory (state files and the
    /// sysfs tree), for a restart over the same state.
    pub fn stop_keep_root(mut self) {
        self.shut_down();
        self.keep_root = true;
    }

    fn shut_down(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MockLemnosd {
    fn drop(&mut self) {
        self.shut_down();
        if !self.keep_root {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
