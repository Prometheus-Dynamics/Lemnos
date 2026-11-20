//! A real lemnosd for a consumer's tests.
//!
//! [`StandIn`] runs the service in-process: the same `lemnosd::Service`, the same built-in drivers
//! the daemon ships (`lemnosd::builtin::drivers`), the same socket and wire. Only the hardware is
//! simulated: a [`SimBoard`] whose lines, PWM channels, buses and streams the test makes and keeps
//! a hand on, and a fake sysfs tree ([`FakeSys`]) for the drivers that read `/sys`. A consumer
//! connects to [`StandIn::socket`] with its own client, exactly as to a board's daemon.
//!
//! Because it is the real service, it refuses what the real one refuses: a claim another client
//! holds is `BUSY`, a board device's line is not raw, a fan override ends when its client goes.
//! Nothing here answers for the service.
//!
//! Reading back what the hardware was told is the simulated parts' own: `MockLine::level` and
//! `config`, `MockPwm::current`, `SimSpi`'s transactions, and [`FanFiles`] for a fake hwmon fan.
//! A light's colours are read through the service, with the light's `current` method.
//!
//! [`StandIn::restart`] stops the service and starts it again on the same board, socket and
//! simulated parts, as a lemnosd restart on a board: what the hardware holds stays, what the
//! service held (claims, overrides, subscriptions) is gone, and clients reconnect.
//!
//! ```no_run
//! use lemnos_standin::{SimBoard, StandIn};
//!
//! let sim = SimBoard::new();
//! let line = sim.add_line("aux");
//! let board = "[[devices]]\nid = \"aux\"\ndriver = \"raw-bus\"\nuses = { line = \"aux\" }\n";
//! let standin = StandIn::start(board, sim).unwrap();
//! standin.wait_running(&["aux"]).unwrap();
//! // Connect to standin.socket() with lemnos-client, as to a board's daemon.
//! # let _ = line;
//! ```

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::{Client, DeviceState};
use lemnos_host::DriverEntry;
use lemnos_host::board::parse;
use lemnosd::{Config, Service};

pub use lemnos_sim::{SimBoard, mock};

/// How long [`StandIn::wait_running`] waits by default.
pub const READY_WAIT: Duration = Duration::from_secs(20);

/// Why a stand-in didn't start or a wait ran out.
#[derive(Debug)]
pub enum Error {
    /// The board file text didn't parse.
    Board(String),
    /// The service refused to start.
    Start(String),
    /// The directory or a file in it couldn't be made.
    Io(std::io::Error),
    /// A device wasn't running when the wait ran out: its name and what the service said of it.
    NotRunning { device: String, state: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(why) => write!(f, "the board file doesn't parse: {why}"),
            Self::Start(why) => write!(f, "the service didn't start: {why}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::NotRunning { device, state } => write!(f, "{device} isn't running: {state}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// A running service over a simulated board, in its own directory (removed when it drops).
pub struct StandIn {
    service: Option<Service>,
    dir: PathBuf,
    socket: PathBuf,
    board: String,
    sim: SimBoard,
    extra: Vec<fn() -> DriverEntry>,
    /// Whether the directory is the stand-in's own, removed when it drops.
    owns_dir: bool,
}

impl StandIn {
    /// Starts the service on `devices` (the board file's device, line and stream entries; the
    /// header is added) over `sim`, in a new directory under the temp dir.
    pub fn start(devices: &str, sim: SimBoard) -> Result<Self, Error> {
        Self::start_with(devices, sim, Vec::new())
    }

    /// [`StandIn::start`], with drivers of the test's own beside the built-in ones (a test
    /// double for a device the consumer itself serves, say). Each is a function making its entry,
    /// so a restart has them again.
    pub fn start_with(
        devices: &str,
        sim: SimBoard,
        extra: Vec<fn() -> DriverEntry>,
    ) -> Result<Self, Error> {
        let dir = Self::new_dir()?;
        Self::begin(dir, true, devices, sim, extra)
    }

    /// [`StandIn::start`] in `dir`, which the caller made and keeps: the socket goes there, and
    /// so can a fake sysfs tree ([`FakeSys::new`]) the board names before the service starts. The
    /// stand-in doesn't remove it.
    pub fn start_in(dir: &Path, devices: &str, sim: SimBoard) -> Result<Self, Error> {
        Self::begin(dir.to_path_buf(), false, devices, sim, Vec::new())
    }

    /// A new, empty directory under the temp dir, named for this process; the caller removes it.
    pub fn new_dir() -> Result<PathBuf, Error> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "lemnos-standin-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        Ok(dir)
    }

    fn begin(
        dir: PathBuf,
        owns_dir: bool,
        devices: &str,
        sim: SimBoard,
        extra: Vec<fn() -> DriverEntry>,
    ) -> Result<Self, Error> {
        let mut standin = Self {
            service: None,
            socket: dir.join("lemnosd.sock"),
            dir,
            board: format!(
                "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"standin\"\n{devices}"
            ),
            sim,
            extra,
            owns_dir,
        };
        standin.run()?;
        Ok(standin)
    }

    fn run(&mut self) -> Result<(), Error> {
        let board = parse(&self.board).map_err(|e| Error::Board(e.to_string()))?;
        let mut drivers = lemnosd::builtin::drivers();
        drivers.extend(self.extra.iter().map(|make| make()));
        let config = Config::new(
            self.socket.clone(),
            "lemnos-standin".to_string(),
            board,
            drivers,
            Box::new(self.sim.clone()),
        );
        let service = Service::start(config).map_err(|e| Error::Start(e.to_string()))?;
        self.service = Some(service);
        Ok(())
    }

    /// The service's socket: connect a client here.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The stand-in's directory, where its socket is. Removed when the stand-in drops, unless the
    /// caller gave it ([`StandIn::start_in`]).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The simulated board, whose parts the test reads back and drives.
    pub fn sim(&self) -> &SimBoard {
        &self.sim
    }

    /// Waits until every device in `devices` is running, for at most [`READY_WAIT`].
    pub fn wait_running(&self, devices: &[&str]) -> Result<(), Error> {
        self.wait_running_for(devices, READY_WAIT)
    }

    /// [`StandIn::wait_running`], waiting at most `wait`.
    pub fn wait_running_for(&self, devices: &[&str], wait: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + wait;
        loop {
            let waiting = self.not_running(devices);
            let Some((device, state)) = waiting else {
                return Ok(());
            };
            if Instant::now() >= deadline {
                return Err(Error::NotRunning { device, state });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The first of `devices` that isn't running, with what the service says of it.
    fn not_running(&self, devices: &[&str]) -> Option<(String, String)> {
        let list = Client::connect(&self.socket, "lemnos-standin").and_then(|c| c.list());
        let list = match list {
            Ok(list) => list,
            Err(error) => {
                let first = devices.first().copied().unwrap_or("the service");
                return Some((first.to_string(), format!("no answer: {error}")));
            }
        };
        devices
            .iter()
            .find_map(|name| match list.iter().find(|d| d.name == *name) {
                None => Some((name.to_string(), "not on the board".to_string())),
                Some(d) if d.state != DeviceState::Running => {
                    Some((name.to_string(), format!("{:?}: {}", d.state, d.reason)))
                }
                Some(_) => None,
            })
    }

    /// Stops the service. The simulated parts keep their state; clients see the socket close.
    pub fn stop(&mut self) {
        if let Some(service) = self.service.take() {
            service.stop();
        }
    }

    /// Whether the service is running (between [`StandIn::stop`] and [`StandIn::restart`] it
    /// isn't).
    pub fn is_running(&self) -> bool {
        self.service.is_some()
    }

    /// Stops the service, if it runs, and starts it again on the same board, socket and simulated
    /// parts: a lemnosd restart.
    pub fn restart(&mut self) -> Result<(), Error> {
        self.stop();
        self.run()
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop();
        if self.owns_dir {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// A fake sysfs tree, laid out as the kernel shows it, for the drivers that read `/sys`
/// (`hwmon-fan` and `fan-control` with `sys_root`, `thermal-zone` with `path`).
pub struct FakeSys {
    root: PathBuf,
}

impl FakeSys {
    /// A tree at `<dir>/sys`. Point a fan's `sys_root` at [`FakeSys::root`].
    pub fn new(dir: &Path) -> Result<Self, Error> {
        let root = dir.join("sys");
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The tree's root: a `sys_root` setting.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// An hwmon fan at `class/hwmon/<hwmon>`, its `name` attribute `name`, at `pwm1` duty
    /// (0 to 255) in mode `pwm1_enable` (1 manual, 2 automatic), as the kernel left it.
    pub fn hwmon_fan(
        &self,
        hwmon: &str,
        name: &str,
        pwm1: u8,
        pwm1_enable: u8,
    ) -> Result<FanFiles, Error> {
        let dir = self.root.join("class/hwmon").join(hwmon);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("name"), format!("{name}\n"))?;
        fs::write(dir.join("pwm1"), format!("{pwm1}\n"))?;
        fs::write(dir.join("pwm1_enable"), format!("{pwm1_enable}\n"))?;
        fs::write(dir.join("fan1_input"), "0\n")?;
        Ok(FanFiles { dir })
    }

    /// A thermal zone at `class/thermal/thermal_zone<n>` of type `kind`, at `millidegrees`.
    /// Its directory is a `thermal-zone` device's `path`.
    pub fn thermal_zone(&self, n: u32, kind: &str, millidegrees: i32) -> Result<Zone, Error> {
        let dir = self.root.join(format!("class/thermal/thermal_zone{n}"));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("type"), format!("{kind}\n"))?;
        let zone = Zone { dir };
        zone.set(millidegrees)?;
        Ok(zone)
    }
}

/// A fake hwmon fan's files: what the fan was told, and its speed to set.
#[derive(Debug, Clone)]
pub struct FanFiles {
    dir: PathBuf,
}

impl FanFiles {
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// The duty the fan was last given (0 to 255), `None` when the file doesn't hold a number.
    pub fn pwm1(&self) -> Option<u8> {
        self.read("pwm1").parse().ok()
    }

    /// The fan's mode: 1 manual, 2 automatic.
    pub fn pwm1_enable(&self) -> Option<u8> {
        self.read("pwm1_enable").parse().ok()
    }

    /// Sets the speed the fan reports, in revolutions per minute.
    pub fn set_rpm(&self, rpm: u32) -> Result<(), Error> {
        Ok(fs::write(self.dir.join("fan1_input"), format!("{rpm}\n"))?)
    }

    /// The fan's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A fake thermal zone.
#[derive(Debug, Clone)]
pub struct Zone {
    dir: PathBuf,
}

impl Zone {
    /// Sets the temperature, in millidegrees Celsius.
    pub fn set(&self, millidegrees: i32) -> Result<(), Error> {
        Ok(fs::write(
            self.dir.join("temp"),
            format!("{millidegrees}\n"),
        )?)
    }

    /// The zone's directory: a `thermal-zone` device's `path`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
