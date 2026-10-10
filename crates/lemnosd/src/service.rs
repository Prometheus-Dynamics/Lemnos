//! The service: one thread, one `poll` loop over the socket, the clients and
//! the next device deadline.

use crate::clients::Client;
use crate::devices::{Slot, Subscription};
use crate::light::{Light, LightIntent, MAX_LEDS};
use crate::looks::LookTable;
use crate::notify::Notifier;
use crate::schedule::{self, Due, Next};
use crate::update::UpdateWatcher;
use lemnos_board::{BoardDefinition, BoardError, Buses, DriverRegistry};
use lemnos_device::DeviceStatus;
use lemnos_hal::ErrorKind;
use lemnos_ipc::{Event, LedShow, LooksOp, Message, RawReading, Refusal, Request, VERSION};
use lemnos_light::{Layer, Show, SystemState, valid_look_name};
use lemnos_linux_sys::poll::{POLLIN, POLLOUT, PollFd, poll_many};
use lemnos_linux_sys::time::boottime_us;
use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often the updater's status files are read.
const UPDATE_POLL_MS: u64 = 500;
/// How often the look files are checked for changes.
const LOOKS_POLL_MS: u64 = 1_000;
/// The reboot ember's fade on shutdown, and the most time a stop may spend
/// on it.
const SHUTDOWN_FADE_MS: u32 = 1_200;
const SHUTDOWN_CAP_MS: u64 = 1_500;

/// What the service needs.
pub struct ServiceConfig {
    pub board: BoardDefinition,
    pub registry: DriverRegistry,
    /// The socket clients connect to.
    pub socket: PathBuf,
    /// The device package's update status file (`LEMNOSD_UPDATE_STATUS`);
    /// `None` leaves updates off the light.
    pub update_status: Option<PathBuf>,
    /// Show the booting spinner for this long after start, or until a client
    /// sets a status (`None`: not at all).
    pub booting_ms: Option<u64>,
    /// The read-only directory of look files (`LEMNOSD_LOOKS_DIR`); `None`
    /// reads none.
    pub looks_dir: Option<PathBuf>,
    /// The writable directory of look files, read last and written by
    /// `looks save` (`LEMNOSD_LOOKS_OVERRIDE_DIR`); `None` refuses saves.
    pub looks_override_dir: Option<PathBuf>,
}

impl ServiceConfig {
    pub fn new(board: BoardDefinition, socket: impl Into<PathBuf>) -> Self {
        Self {
            board,
            registry: DriverRegistry::builtin(),
            socket: socket.into(),
            update_status: None,
            booting_ms: None,
            looks_dir: None,
            looks_override_dir: None,
        }
    }
}

/// The service failed to start.
#[derive(Debug)]
pub enum ServiceError {
    Board(BoardError),
    Io(io::Error),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(e) => e.fmt(f),
            Self::Io(e) => write!(f, "socket: {e}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<io::Error> for ServiceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

struct SleepDelay;

impl embedded_hal::delay::DelayNs for SleepDelay {
    fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(u64::from(ns)));
    }
}

/// `lemnosd`: hosts a board's devices and serves clients.
#[path = "service_raw.rs"]
mod service_raw;

pub struct Service {
    board: String,
    /// The whole definition (lines, PWM channels, raw policy).
    definition: lemnos_board::BoardDefinition,
    raw: crate::raw::RawState,
    registry: DriverRegistry,
    buses: Box<dyn Buses>,
    slots: Vec<Slot>,
    lights: Vec<Light>,
    clients: Vec<Client>,
    listener: UnixListener,
    socket: PathBuf,
    next_client: u32,
    owners: Vec<(String, u32)>,
    update: Option<UpdateWatcher>,
    next_update_ms: u64,
    booting_until: Option<u64>,
    notifier: Notifier,
    next_watchdog_ms: u64,
    pollfds: Vec<PollFd>,
    /// The sensors the scheduler looks at (reused between passes).
    due: Vec<Due>,
    /// The named looks (built-in, board, look files).
    looks: LookTable,
    next_looks_ms: u64,
}

impl Service {
    /// Validates the board, listens on the socket, and makes a first
    /// attempt at every device (devices that fail are retried later).
    pub fn new(config: ServiceConfig, buses: Box<dyn Buses>) -> Result<Self, ServiceError> {
        config
            .board
            .validate(&config.registry)
            .map_err(ServiceError::Board)?;
        if let Some(dir) = config.socket.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(&config.socket);
        let listener = UnixListener::bind(&config.socket)?;
        listener.set_nonblocking(true)?;
        let definition = config.board.clone();
        let looks = LookTable::new(
            &config.board,
            config.looks_dir.as_deref(),
            config.looks_override_dir.as_deref(),
        );
        let now_ms = boottime_us() / 1000;
        let mut service = Self {
            board: config.board.board.id.clone(),
            definition,
            raw: crate::raw::RawState::default(),
            registry: config.registry,
            buses,
            slots: config.board.devices.into_iter().map(Slot::new).collect(),
            lights: Vec::new(),
            clients: Vec::new(),
            listener,
            socket: config.socket,
            next_client: 1,
            owners: Vec::new(),
            update: config.update_status.map(UpdateWatcher::new),
            next_update_ms: 0,
            booting_until: config.booting_ms.map(|ms| now_ms + ms),
            notifier: Notifier::none(),
            next_watchdog_ms: 0,
            pollfds: Vec::new(),
            due: Vec::new(),
            looks,
            next_looks_ms: 0,
        };
        service.build_devices(now_ms);
        Ok(service)
    }

    /// Notifies systemd (`READY=1` now, the watchdog from the loop).
    pub fn with_systemd(mut self) -> Self {
        self.notifier = Notifier::from_env();
        self.notifier.status(&format!(
            "{} devices, {} available",
            self.slots.len(),
            self.slots
                .iter()
                .filter(|s| s.status == DeviceStatus::Available)
                .count()
        ));
        self.notifier.ready();
        self
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The fans to hand back to the kernel if the process dies.
    pub fn fan_restore_targets(&self) -> Vec<lemnos_drivers_linux::FanRestore> {
        self.slots.iter().filter_map(Slot::hand_back_plan).collect()
    }

    /// The clock everything is scheduled and stamped with: the boot clock,
    /// so times keep their meaning across a restart of the service.
    fn now_us(&self) -> u64 {
        boottime_us()
    }

    fn now_ms(&self) -> u64 {
        self.now_us() / 1000
    }

    fn broadcast(&mut self, message: &Message) {
        for client in self.clients.iter_mut().filter(|c| c.greeted && c.events) {
            client.send(message);
        }
    }

    fn status_event(&mut self, index: usize, status: DeviceStatus) {
        let event = Event::Status {
            device: self.slots[index].id().to_string(),
            status,
            error: self.slots[index].error,
            reason: self.slots[index].reason.clone(),
        };
        self.broadcast(&Message::Event(event));
    }

    fn build_devices(&mut self, now_ms: u64) {
        let fans_before = self.slots.iter().filter(|s| s.restore.is_some()).count();
        for index in 0..self.slots.len() {
            let changed =
                self.slots[index].build(&self.registry, &mut *self.buses, &mut SleepDelay, now_ms);
            if let Some(status) = changed {
                self.status_event(index, status);
            }
            if self.slots[index].is_light() && !self.lights.iter().any(|l| l.slot == index) {
                let slot = &self.slots[index];
                let count = slot.device.as_ref().map_or(0, |d| d.pixel_count());
                let defaults = lemnos_board::light_defaults(&slot.spec).unwrap_or_default();
                let mut light = Light::new(index, count, defaults);
                if let Some(until) = self.booting_until {
                    light.hold_service(
                        LightIntent::new(Show::System(SystemState::Booting)),
                        Some(until),
                    );
                }
                self.lights.push(light);
            }
        }
        // A fan was bound for the first time: record how to hand it back,
        // for the stop helper that runs after this process is gone.
        if self.slots.iter().filter(|s| s.restore.is_some()).count() != fans_before {
            self.save_fan_state();
        }
    }

    /// Writes the fans' hand-back plans for the stop helper.
    // The service logs to stderr (the journal).
    #[allow(clippy::print_stderr)]
    fn save_fan_state(&self) {
        let path = crate::fans::fan_state_path(&self.socket);
        if let Err(error) = crate::fans::write_fan_state(&path, &self.fan_restore_targets()) {
            eprintln!("lemnosd: {}: {error}", path.display());
        }
    }

    fn reading(&self, index: usize) -> Message {
        let slot = &self.slots[index];
        Message::Reading(RawReading {
            device: slot.id().to_string(),
            timestamp_us: slot.read_us,
            status: slot.status,
            values: slot.channel_values(),
        })
    }

    /// Reads the sensors that are due, in the order `schedule` picks, until
    /// none is due or the next one has to wait (see `next_deadline`).
    fn poll_devices(&mut self) {
        loop {
            self.due.clear();
            for (index, slot) in self.slots.iter().enumerate() {
                if slot.is_sensor() {
                    self.due.push(Due {
                        index,
                        deadline_us: slot.next_read_us,
                        period_us: slot.period_us(),
                        cost_us: slot.read_cost_us,
                    });
                }
            }
            match schedule::choose(&self.due, self.now_us()) {
                Next::Run(index) => self.read_sensor(index),
                Next::WaitUntil(_) | Next::Idle => return,
            }
        }
    }

    /// Reads one sensor on its schedule, reports a status change, and sends
    /// fresh values to the subscribers that are due.
    fn read_sensor(&mut self, index: usize) {
        let was_read = self.slots[index].read_us;
        if let Some(status) = self.slots[index].read_scheduled() {
            self.status_event(index, status);
        }
        let read_us = self.slots[index].read_us;
        if read_us == was_read {
            return;
        }
        let now_us = self.now_us();
        let due: Vec<u32> = self.slots[index]
            .subscriptions
            .iter_mut()
            .filter(|s| s.next_us <= read_us)
            .map(|s| {
                s.next_us = schedule::advance(s.next_us, u64::from(s.period_ms) * 1000, now_us);
                s.client
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let message = self.reading(index);
        for client in self.clients.iter_mut().filter(|c| due.contains(&c.id)) {
            client.send(&message);
        }
    }

    /// Starts (or, with 0, ends) this client's readings of `device`. Returns
    /// the granted period: the requested one, or the device's read time if
    /// that is longer. A sensor that is not built yet is accepted (it reads
    /// once it is).
    fn subscribe(&mut self, ci: usize, device: &str, period_ms: u32) -> Result<u32, Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let client = self.clients[ci].id;
        let now_us = self.now_us();
        let slot = &mut self.slots[index];
        slot.subscriptions.retain(|s| s.client != client);
        if period_ms == 0 {
            return Ok(0);
        }
        if slot.device.is_some() && !slot.is_sensor() {
            return Err(Refusal::Unsupported);
        }
        slot.subscriptions.push(Subscription {
            client,
            period_ms,
            next_us: now_us,
        });
        // Read on the new schedule from now.
        slot.next_read_us = slot.next_read_us.min(now_us);
        let read_ms = u32::try_from(slot.read_cost_us.div_ceil(1000)).unwrap_or(u32::MAX);
        Ok(period_ms.max(read_ms))
    }

    fn poll_update(&mut self, now_ms: u64) {
        if now_ms < self.next_update_ms {
            return;
        }
        self.next_update_ms = now_ms + UPDATE_POLL_MS;
        let Some(change) = self.update.as_mut().and_then(UpdateWatcher::poll) else {
            return;
        };
        let Some(light) = self.lights.first_mut() else {
            return;
        };
        match change {
            Some(view) => light.hold_service(
                LightIntent::new(Show::System(view.state)),
                view.hold_ms.map(|hold| now_ms + hold),
            ),
            None => light.clear_service(Layer::System),
        }
    }

    /// Re-reads the look files that changed, about once a second.
    #[allow(clippy::print_stderr)]
    fn poll_looks(&mut self, now_ms: u64) {
        if now_ms < self.next_looks_ms {
            return;
        }
        self.next_looks_ms = now_ms + LOOKS_POLL_MS;
        if self.looks.scan() {
            eprint!("lemnosd: looks changed: {}", self.looks.report());
            self.invalidate_lights();
        }
    }

    /// Re-reads every look file now (`SIGHUP`), and reports what loaded and
    /// what failed to the journal.
    #[allow(clippy::print_stderr)]
    pub fn reload_looks(&mut self) {
        let report = self.looks.reload();
        eprint!("lemnosd: looks: {report}");
        self.invalidate_lights();
    }

    /// Makes every light re-resolve what it shows (a look file changed).
    fn invalidate_lights(&mut self) {
        for light in &mut self.lights {
            light.invalidate();
        }
    }

    fn render_lights(&mut self, now_ms: u64) {
        for li in 0..self.lights.len() {
            let winner = {
                let looks = &self.looks;
                self.lights[li].arbitrate(now_ms, &|name| looks.get(name))
            };
            if let Some(winner) = winner {
                let owner = winner.map_or_else(String::new, |w| self.owner_name(w.owner));
                let layer = winner.map_or("", |w| w.layer.name()).to_string();
                let device = self.slots[self.lights[li].slot].id().to_string();
                self.broadcast(&Message::Event(Event::LedOwner {
                    device,
                    owner,
                    layer,
                }));
            }
            let light = &mut self.lights[li];
            // A new look goes out at once; animation frames at most every
            // FRAME_MS, however often the loop wakes.
            if !light.animator.is_pending() && now_ms < light.next_render_ms {
                continue;
            }
            light.next_render_ms = now_ms + u64::from(lemnos_light::FRAME_MS);
            let slot = light.slot;
            if let Some(frame) = light.animator.render(now_ms)
                && let Some(device) = self.slots[slot].device.as_mut()
                && let Err(kind) = device.show(frame)
            {
                let status = DeviceStatus::after_error(kind);
                if let Some(status) = self.slots[slot].set_status(status, Some(kind)) {
                    self.status_event(slot, status);
                }
            }
        }
    }

    fn owner_name(&self, owner: u32) -> String {
        if owner == crate::light::SERVICE_OWNER {
            return "lemnosd".into();
        }
        self.owners
            .iter()
            .find(|(_, id)| *id == owner)
            .map_or_else(String::new, |(name, _)| name.clone())
    }

    fn owner_for(&mut self, name: &str) -> u32 {
        if let Some((_, id)) = self.owners.iter().find(|(n, _)| n == name) {
            return *id;
        }
        let id = self.owners.len() as u32 + 1;
        self.owners.push((name.to_string(), id));
        id
    }

    fn slot_of(&self, device: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.id() == device)
    }

    fn handle(&mut self, ci: usize, request: Request, now_ms: u64) {
        match request {
            Request::Hello {
                client,
                priority,
                keep,
                events,
                ..
            } => {
                let owner = self.owner_for(&client);
                let id = {
                    let c = &mut self.clients[ci];
                    c.name = client;
                    c.priority = priority;
                    c.keep = keep;
                    c.events = events;
                    c.owner = owner;
                    c.greeted = true;
                    c.id
                };
                let board = self.board.clone();
                self.clients[ci].send(&Message::Welcome {
                    version: VERSION,
                    board,
                    client_id: id,
                });
            }
            Request::List => {
                let devices = self.slots.iter().map(Slot::describe).collect();
                self.clients[ci].send(&Message::Devices(devices));
            }
            Request::Read { device } => {
                let message = match self.slot_of(&device) {
                    None => Message::Reply {
                        id: 0,
                        result: Err(Refusal::UnknownDevice),
                    },
                    Some(index) => {
                        if !self.slots[index].fresh
                            && self.slots[index].is_sensor()
                            && let Some(status) = self.slots[index].read()
                        {
                            self.status_event(index, status);
                        }
                        if self.slots[index].fresh {
                            self.reading(index)
                        } else {
                            Message::Reply {
                                id: 0,
                                result: Err(Refusal::Device(
                                    self.slots[index].error.unwrap_or(ErrorKind::Unavailable),
                                )),
                            }
                        }
                    }
                };
                self.clients[ci].send(&message);
            }
            Request::Subscribe {
                id,
                device,
                period_ms,
            } => {
                let result = self.subscribe(ci, &device, period_ms);
                // Older clients (id 0) are answered only with a refusal.
                if id != 0 || result.is_err() {
                    self.clients[ci].send(&Message::Reply {
                        id,
                        result: result.map(f64::from),
                    });
                }
            }
            Request::Set {
                id,
                device,
                control,
                value,
            } => {
                let result = self.set(ci, &device, &control, value);
                if let Ok(applied) = result {
                    let by = self.clients[ci].name.clone();
                    self.broadcast(&Message::Event(Event::Control {
                        device,
                        control,
                        value: applied,
                        by,
                    }));
                }
                self.clients[ci].send(&Message::Reply { id, result });
            }
            Request::Get {
                id,
                device,
                control,
            } => {
                let result = self.get(&device, &control);
                self.clients[ci].send(&Message::Reply { id, result });
            }
            Request::Raw(request) => self.handle_raw(ci, request),
            Request::Restore {
                id,
                device,
                control,
            } => {
                let result = self.restore_control(ci, &device, &control).map(|()| 0.0);
                self.clients[ci].send(&Message::Reply { id, result });
            }
            Request::Release { id, device } => {
                let result = self.release(ci, &device);
                if result.is_ok() {
                    let by = self.clients[ci].name.clone();
                    self.broadcast(&Message::Event(Event::Control {
                        device,
                        control: "release".into(),
                        value: 0.0,
                        by,
                    }));
                }
                self.clients[ci].send(&Message::Reply {
                    id,
                    result: result.map(|()| 0.0),
                });
            }
            Request::Led(request) => {
                let li = if request.device.is_empty() {
                    (!self.lights.is_empty()).then_some(0)
                } else {
                    self.slot_of(&request.device)
                        .and_then(|slot| self.lights.iter().position(|l| l.slot == slot))
                };
                let Some(li) = li else {
                    return;
                };
                let (name, owner, priority) = {
                    let c = &self.clients[ci];
                    (c.name.clone(), c.owner, c.priority)
                };
                if !self.slots[self.lights[li].slot].allows(&name) {
                    return;
                }
                // A look must be one the service has (a built-in or a look
                // file), and an inline look a valid one; a refusal leaves
                // the light as it is.
                let refusal = match &request.show {
                    LedShow::Look { name, .. } if !valid_look_name(name) => {
                        Some(format!("{name:?} is not a valid look name"))
                    }
                    LedShow::Look { name, .. } if !self.looks.knows(name) => {
                        Some(format!("unknown look {name:?} (lemnos-ctl looks list)"))
                    }
                    LedShow::Inline { spec, .. } => spec
                        .validate()
                        .err()
                        .map(|reason| format!("invalid look: {reason}")),
                    _ => None,
                };
                if let Some(reason) = refusal {
                    if request.id != 0 {
                        self.clients[ci].send(&Message::Text {
                            id: request.id,
                            result: Err(reason),
                        });
                    }
                    return;
                }
                if matches!(request.show, LedShow::Status(_)) {
                    // A client's status ends the booting spinner.
                    self.booting_until = None;
                    self.lights[li].clear_service(Layer::System);
                }
                let accepted = self.lights[li].request(owner, priority, &request, now_ms);
                if request.id != 0 {
                    let result = if accepted {
                        Ok(String::new())
                    } else {
                        Err("every intent slot of the light is taken".to_string())
                    };
                    self.clients[ci].send(&Message::Text {
                        id: request.id,
                        result,
                    });
                }
            }
            Request::Looks { id, op } => {
                let result = match op {
                    LooksOp::List => Ok(self.looks.list()),
                    LooksOp::Show(name) => {
                        let defaults = self.lights.first().map(|l| l.defaults).unwrap_or_default();
                        self.looks.show(&name, &defaults)
                    }
                    LooksOp::Reload => {
                        let report = self.looks.reload();
                        self.invalidate_lights();
                        Ok(report)
                    }
                    LooksOp::Save { name, text } => {
                        let saved = self.looks.save(&name, &text);
                        if saved.is_ok() {
                            self.invalidate_lights();
                        }
                        saved
                    }
                };
                self.clients[ci].send(&Message::Text { id, result });
            }
        }
    }

    fn control(
        &self,
        device: &str,
        control: &str,
    ) -> Result<(usize, usize, i8, i32, i32), Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let slot = &self.slots[index];
        let info = slot.info.ok_or(Refusal::Device(
            slot.error.unwrap_or(ErrorKind::Unavailable),
        ))?;
        let ci = info.control_index(control).ok_or(Refusal::UnknownControl)?;
        let c = &info.controls[ci];
        Ok((index, ci, c.exponent, c.min, c.max))
    }

    fn set(
        &mut self,
        client: usize,
        device: &str,
        control: &str,
        value: f64,
    ) -> Result<f64, Refusal> {
        let (index, ci, exponent, min, max) = self.control(device, control)?;
        if !self.slots[index].allows(&self.clients[client].name) {
            return Err(Refusal::NotAllowed);
        }
        let raw = (value * 10f64.powi(-i32::from(exponent))).round();
        if !raw.is_finite() || raw < f64::from(min) || raw > f64::from(max) {
            return Err(Refusal::OutOfRange);
        }
        let (wid, wname, wkeep) = {
            let c = &self.clients[client];
            (c.id, c.name.clone(), c.keep)
        };
        self.slots[index].note_write(
            ci,
            &crate::clients::Requester {
                id: wid,
                name: &wname,
                keep: wkeep,
            },
        );
        if self.slots[index].before_write() {
            self.save_fan_state();
        }
        let device_ref = self.slots[index]
            .device
            .as_mut()
            .ok_or(Refusal::Device(ErrorKind::Unavailable))?;
        match device_ref.set(ci, raw as i32) {
            Ok(applied) => Ok(crate::scaled(applied, exponent)),
            Err(kind) => {
                let status = DeviceStatus::after_error(kind);
                if let Some(status) = self.slots[index].set_status(status, Some(kind)) {
                    self.status_event(index, status);
                }
                Err(Refusal::Device(kind))
            }
        }
    }

    /// Hands a fan back to the kernel's governor (`Request::Release`).
    fn release(&mut self, client: usize, device: &str) -> Result<(), Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        if !self.slots[index].allows(&self.clients[client].name) {
            return Err(Refusal::NotAllowed);
        }
        self.slots[index].release().map_err(Refusal::Device)?;
        self.save_fan_state();
        Ok(())
    }

    fn get(&mut self, device: &str, control: &str) -> Result<f64, Refusal> {
        let (index, ci, exponent, _, _) = self.control(device, control)?;
        let device_ref = self.slots[index]
            .device
            .as_mut()
            .ok_or(Refusal::Device(ErrorKind::Unavailable))?;
        device_ref
            .get(ci)
            .map(|raw| crate::scaled(raw, exponent))
            .map_err(Refusal::Device)
    }

    /// When the loop must wake next, in microseconds on the boot clock. A
    /// sensor whose deadline has passed is not waited for: it is either read
    /// now or deferred to another sensor's deadline, which is in the list.
    fn next_deadline(&self, now_us: u64) -> u64 {
        let mut next = now_us + 1_000_000;
        for slot in &self.slots {
            if slot.device.is_none() {
                next = next.min(slot.next_build_ms * 1000);
            } else if slot.is_sensor() && slot.next_read_us > now_us {
                next = next.min(slot.next_read_us);
            }
        }
        for light in &self.lights {
            if let Some(at) = light.next_ms(now_us / 1000) {
                next = next.min(at * 1000);
            }
        }
        if self.update.is_some() {
            next = next.min(self.next_update_ms * 1000);
        }
        if self.raw.polls_edges() {
            next = next.min(now_us + 20_000);
        }
        if self.notifier.watchdog_interval().is_some() {
            next = next.min(self.next_watchdog_ms * 1000);
        }
        next.max(now_us)
    }

    /// One pass: devices, lights, then up to `max_wait` waiting for clients
    /// (or `extra`, such as a signal descriptor). Returns whether `extra`
    /// became readable.
    pub fn step(&mut self, max_wait: Duration, extra: Option<BorrowedFd<'_>>) -> io::Result<bool> {
        let now = self.now_ms();
        self.build_devices(now);
        self.poll_devices();
        self.poll_update(now);
        self.poll_looks(now);
        self.render_lights(now);
        self.send_edges();
        if let Some(interval) = self.notifier.watchdog_interval()
            && now >= self.next_watchdog_ms
        {
            self.notifier.ping();
            self.next_watchdog_ms = now + interval.as_millis() as u64;
        }

        let now_us = self.now_us();
        let wait = Duration::from_micros(self.next_deadline(now_us) - now_us).min(max_wait);
        self.pollfds.clear();
        self.pollfds
            .push(PollFd::new(self.listener.as_fd(), POLLIN));
        for client in &self.clients {
            let events = if client.wants_write() {
                POLLIN | POLLOUT
            } else {
                POLLIN
            };
            self.pollfds
                .push(PollFd::new(client.stream.as_fd(), events));
        }
        for fd in self.raw.edge_fds() {
            self.pollfds.push(PollFd::from_raw(fd, POLLIN));
        }
        if let Some(fd) = extra {
            self.pollfds.push(PollFd::new(fd, POLLIN));
        }
        poll_many(&mut self.pollfds, Some(wait))?;
        let extra_ready = extra.is_some() && self.pollfds.last().is_some_and(|p| p.revents() != 0);

        // New clients.
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let id = self.next_client;
                    self.next_client = self.next_client.wrapping_add(1).max(1);
                    if let Ok(client) = Client::new(id, stream) {
                        self.clients.push(client);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // Requests.
        let now = self.now_ms();
        for ci in 0..self.clients.len() {
            match self.clients[ci].receive() {
                Ok(requests) => {
                    for request in requests {
                        self.handle(ci, request, now);
                    }
                }
                Err(_) => self.clients[ci].closed = true,
            }
            self.clients[ci].flush();
        }
        // A new subscription may be due now.
        self.poll_devices();
        // A request may have changed a light: render without waiting.
        self.render_lights(self.now_ms());
        self.send_edges();
        self.drop_closed();
        Ok(extra_ready)
    }

    fn drop_closed(&mut self) {
        let mut ci = 0;
        while ci < self.clients.len() {
            if !self.clients[ci].closed {
                ci += 1;
                continue;
            }
            let client = self.clients.remove(ci);
            for slot in &mut self.slots {
                slot.subscriptions.retain(|s| s.client != client.id);
            }
            if client.greeted {
                self.client_closed(&client);
            }
            let others = self.clients.iter().any(|c| c.owner == client.owner);
            if !client.keep && !others && client.greeted {
                for light in &mut self.lights {
                    light.forget(client.owner);
                }
            }
        }
    }

    /// Runs until `stop` is set.
    pub fn run(&mut self, stop: &AtomicBool) -> io::Result<()> {
        while !stop.load(Ordering::Relaxed) {
            self.step(Duration::from_millis(100), None)?;
        }
        Ok(())
    }

    /// Runs until `signals` becomes readable (a `SIGTERM`).
    pub fn run_until(&mut self, signals: BorrowedFd<'_>) -> io::Result<()> {
        loop {
            if self.step(Duration::from_secs(1), Some(signals))? {
                return Ok(());
            }
        }
    }

    /// Stops: fans back to the kernel, the light off (or, when the system
    /// is restarting, the rebooting ember, faded to over about 1.2 s), the
    /// socket removed.
    pub fn shutdown(&mut self, rebooting: bool) {
        self.notifier.stopping();
        self.raw.release_all();
        for slot in &mut self.slots {
            slot.restore();
        }
        if rebooting {
            self.fade_to_ember();
        } else {
            for light in &self.lights {
                let frame = [lemnos_light::Rgbw::OFF; MAX_LEDS];
                if let Some(device) = self.slots[light.slot].device.as_mut() {
                    let _ = device.show(&frame[..light.count]);
                }
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }

    /// Eases every light down to the reboot ember over
    /// [`SHUTDOWN_FADE_MS`], and holds it: the ring keeps the last frame
    /// while power is cycled, so it is a static look. Never longer than
    /// [`SHUTDOWN_CAP_MS`], so a stop is not delayed.
    fn fade_to_ember(&mut self) {
        let start = self.now_ms();
        let looks = &self.looks;
        for light in &mut self.lights {
            let (look, _) = LightIntent::new(Show::System(SystemState::Rebooting))
                .resolve(&light.defaults, &|name| looks.get(name));
            let transition =
                lemnos_light::Transition::new(SHUTDOWN_FADE_MS, lemnos_light::Easing::EaseInOut);
            light.animator.set(look, transition, start);
        }
        let slots = &mut self.slots;
        loop {
            let now = boottime_us() / 1000;
            let mut animating = false;
            for light in &mut self.lights {
                if let Some(frame) = light.animator.render(now)
                    && let Some(device) = slots[light.slot].device.as_mut()
                {
                    let _ = device.show(frame);
                }
                animating |= light.animator.is_animating(now);
            }
            if !animating || now.saturating_sub(start) >= SHUTDOWN_CAP_MS {
                break;
            }
            std::thread::sleep(Duration::from_millis(u64::from(lemnos_light::FRAME_MS)));
        }
    }
}
