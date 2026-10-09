//! The service: one thread, one `poll` loop over the socket, the clients and
//! the next device deadline.

use crate::clients::Client;
use crate::devices::{Slot, Subscription};
use crate::light::{Light, LightIntent, MAX_LEDS};
use crate::notify::Notifier;
use crate::update::UpdateWatcher;
use lemnos_board::{BoardDefinition, BoardError, Buses, DriverRegistry};
use lemnos_device::DeviceStatus;
use lemnos_hal::ErrorKind;
use lemnos_ipc::{Event, Message, RawReading, Refusal, Request, VERSION};
use lemnos_light::{Layer, Show, SystemState};
use lemnos_linux_sys::poll::{POLLIN, POLLOUT, PollFd, poll_many};
use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often the updater's status files are read.
const UPDATE_POLL_MS: u64 = 500;

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
}

impl ServiceConfig {
    pub fn new(board: BoardDefinition, socket: impl Into<PathBuf>) -> Self {
        Self {
            board,
            registry: DriverRegistry::builtin(),
            socket: socket.into(),
            update_status: None,
            booting_ms: None,
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
pub struct Service {
    board: String,
    registry: DriverRegistry,
    buses: Box<dyn Buses>,
    slots: Vec<Slot>,
    lights: Vec<Light>,
    clients: Vec<Client>,
    listener: UnixListener,
    socket: PathBuf,
    start: Instant,
    next_client: u32,
    owners: Vec<(String, u32)>,
    update: Option<UpdateWatcher>,
    next_update_ms: u64,
    booting_until: Option<u64>,
    notifier: Notifier,
    next_watchdog_ms: u64,
    pollfds: Vec<PollFd>,
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
        let mut service = Self {
            board: config.board.board.id.clone(),
            registry: config.registry,
            buses,
            slots: config.board.devices.into_iter().map(Slot::new).collect(),
            lights: Vec::new(),
            clients: Vec::new(),
            listener,
            socket: config.socket,
            start: Instant::now(),
            next_client: 1,
            owners: Vec::new(),
            update: config.update_status.map(UpdateWatcher::new),
            next_update_ms: 0,
            booting_until: config.booting_ms,
            notifier: Notifier::none(),
            next_watchdog_ms: 0,
            pollfds: Vec::new(),
        };
        service.build_devices(0);
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

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    fn broadcast(&mut self, message: &Message) {
        for client in self.clients.iter_mut().filter(|c| c.greeted) {
            client.send(message);
        }
    }

    fn status_event(&mut self, index: usize, status: DeviceStatus) {
        let event = Event::Status {
            device: self.slots[index].id().to_string(),
            status,
            error: self.slots[index].error,
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

    fn poll_devices(&mut self, now_ms: u64) {
        let now_us = self.now_us();
        for index in 0..self.slots.len() {
            let was_read = self.slots[index].read_us;
            if let Some(status) = self.slots[index].poll(now_ms, now_us) {
                self.status_event(index, status);
            }
            if self.slots[index].read_us == was_read {
                continue;
            }
            // Fresh values: send them to the subscribers that are due.
            let due: Vec<u32> = self.slots[index]
                .subscriptions
                .iter_mut()
                .filter(|s| s.next_ms <= now_ms)
                .map(|s| {
                    s.next_ms = now_ms + u64::from(s.period_ms);
                    s.client
                })
                .collect();
            if due.is_empty() {
                continue;
            }
            let message = self.reading(index);
            for client in self.clients.iter_mut().filter(|c| due.contains(&c.id)) {
                client.send(&message);
            }
        }
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

    fn render_lights(&mut self, now_ms: u64) {
        for li in 0..self.lights.len() {
            if let Some(winner) = self.lights[li].arbitrate(now_ms) {
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
                ..
            } => {
                let owner = self.owner_for(&client);
                let id = {
                    let c = &mut self.clients[ci];
                    c.name = client;
                    c.priority = priority;
                    c.keep = keep;
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
                        if !self.slots[index].fresh && self.slots[index].is_sensor() {
                            let now_us = self.now_us();
                            if let Some(status) = self.slots[index].read(now_us) {
                                self.status_event(index, status);
                            }
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
            Request::Subscribe { device, period_ms } => {
                let Some(index) = self.slot_of(&device) else {
                    self.clients[ci].send(&Message::Reply {
                        id: 0,
                        result: Err(Refusal::UnknownDevice),
                    });
                    return;
                };
                let client = self.clients[ci].id;
                let slot = &mut self.slots[index];
                slot.subscriptions.retain(|s| s.client != client);
                if period_ms > 0 {
                    slot.subscriptions.push(Subscription {
                        client,
                        period_ms,
                        next_ms: now_ms,
                    });
                    // Read on the new schedule from now.
                    slot.next_read_ms = slot.next_read_ms.min(now_ms);
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
                if matches!(request.show, lemnos_ipc::LedShow::Status(_)) {
                    // A client's status ends the booting spinner.
                    self.booting_until = None;
                    self.lights[li].clear_service(Layer::System);
                }
                self.lights[li].request(owner, priority, &request, now_ms);
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

    /// When the loop must wake next.
    fn next_deadline(&self, now_ms: u64) -> u64 {
        let mut next = now_ms + 1_000;
        for slot in &self.slots {
            if slot.device.is_none() {
                next = next.min(slot.next_build_ms);
            } else if slot.is_sensor() {
                next = next.min(slot.next_read_ms);
            }
        }
        for light in &self.lights {
            if let Some(at) = light.next_ms(now_ms) {
                next = next.min(at);
            }
        }
        if self.update.is_some() {
            next = next.min(self.next_update_ms);
        }
        if self.notifier.watchdog_interval().is_some() {
            next = next.min(self.next_watchdog_ms);
        }
        next.max(now_ms)
    }

    /// One pass: devices, lights, then up to `max_wait` waiting for clients
    /// (or `extra`, such as a signal descriptor). Returns whether `extra`
    /// became readable.
    pub fn step(&mut self, max_wait: Duration, extra: Option<BorrowedFd<'_>>) -> io::Result<bool> {
        let now = self.now_ms();
        self.build_devices(now);
        self.poll_devices(now);
        self.poll_update(now);
        self.render_lights(now);
        if let Some(interval) = self.notifier.watchdog_interval()
            && now >= self.next_watchdog_ms
        {
            self.notifier.ping();
            self.next_watchdog_ms = now + interval.as_millis() as u64;
        }

        let wait = Duration::from_millis(self.next_deadline(now).saturating_sub(self.now_ms()))
            .min(max_wait);
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
        // A request may have changed a light: render without waiting.
        self.render_lights(self.now_ms());
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
    /// is restarting, the rebooting look), the socket removed.
    pub fn shutdown(&mut self, rebooting: bool) {
        self.notifier.stopping();
        for slot in &mut self.slots {
            slot.restore();
        }
        let now = self.now_ms();
        for li in 0..self.lights.len() {
            let light = &mut self.lights[li];
            let slot = light.slot;
            let frame: [lemnos_light::Rgbw; MAX_LEDS] = if rebooting {
                let (look, _) =
                    LightIntent::new(Show::System(SystemState::Rebooting)).resolve(&light.defaults);
                light.animator.set(look, lemnos_light::Transition::CUT, now);
                let mut frame = [lemnos_light::Rgbw::OFF; MAX_LEDS];
                if let Some(rendered) = light.animator.render(now) {
                    frame[..rendered.len()].copy_from_slice(rendered);
                }
                frame
            } else {
                [lemnos_light::Rgbw::OFF; MAX_LEDS]
            };
            let count = light.count;
            if let Some(device) = self.slots[slot].device.as_mut() {
                let _ = device.show(&frame[..count]);
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}
