//! The service: one thread, one `poll` loop over the socket, the clients and
//! the next device deadline.

use crate::calibration;
use crate::clients::Client;
use crate::devices::{Placement, Slot, Subscription, WHOLE_DEVICE, channel_mask};
use crate::fusion::{self, Fusion};
use crate::light::{FrameWatch, Light, LightIntent, MAX_LEDS, next_frame_due};
use crate::looks::LookTable;
use crate::notify::Notifier;
use crate::schedule::{self, Due, Next};
use crate::state;
use crate::update::UpdateWatcher;
use crate::workers::{Done, Job, Workers};
use lemnos_board::{BoardDefinition, BoardError, Buses, DriverRegistry};
use lemnos_device::{BoxedDevice, DeviceClass, DeviceStatus};
use lemnos_hal::ErrorKind;
use lemnos_ipc::{
    Event, LedShow, LightFrame, LightInfo, LooksOp, Message, RawReading, Refusal, Request, VERSION,
};
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
/// Whether a client may change looks and presets: Atlas, the Orion bridge's
/// callers (`orion:<requested_by>`), and `lemnos-ctl` run by an operator. The
/// socket's permissions are the real boundary; this names who the service
/// expects to make the changes.
fn may_change_looks(client: &str) -> bool {
    client == "atlas" || client == "lemnos-ctl" || client.starts_with("orion:")
}

/// The fastest a light's frames are watched (`Request::WatchFrames`).
const MAX_WATCH_FPS: u16 = 60;
/// How often calibration is checked for saves (a save waits for its minute).
const CALIBRATION_POLL_MS: u64 = 1_000;
/// How long before the fusion device (whose IMU or magnetometer is missing or
/// not calibrated yet) is tried again.
const FUSION_RETRY_MS: u64 = 1_000;
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
    /// The directory of saved settings (`LEMNOSD_STATE_DIR`); `None` keeps
    /// none (a power switch's `persist` is then ignored).
    pub state_dir: Option<PathBuf>,
    /// The directory the devices' calibrations are saved in (see
    /// `calibration.rs`). Defaults to `LEMNOSD_CALIBRATION_DIR`.
    pub calibration_dir: PathBuf,
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
            state_dir: None,
            calibration_dir: calibration::default_dir(),
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

#[path = "service_fusion.rs"]
mod service_fusion;

#[path = "service_calibration.rs"]
mod service_calibration;

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
    /// The first frame written to a ring has been logged (boot timing).
    first_frame_logged: bool,
    booting_until: Option<u64>,
    notifier: Notifier,
    next_watchdog_ms: u64,
    pollfds: Vec<PollFd>,
    /// The sensors the scheduler looks at (reused between passes).
    due: Vec<Due>,
    /// The named looks (built-in, board, look files).
    looks: LookTable,
    next_looks_ms: u64,
    /// The bus threads that read the sensors on each bus.
    workers: Workers,
    /// Where settings persist (`None`: nowhere).
    state_dir: Option<PathBuf>,
    /// The orientation fusion devices built so far (see `fusion.rs`).
    fusions: Vec<Fusion>,
    calibration_dir: PathBuf,
    next_calibration_ms: u64,
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
        let mut specs = config.board.devices.clone();
        if let Some(dir) = &config.state_dir {
            for spec in &mut specs {
                state::apply_saved_power(spec, dir);
            }
        }
        let mut looks = LookTable::new(
            &config.board,
            config.looks_dir.as_deref(),
            config.looks_override_dir.as_deref(),
        );
        if let Some(dir) = &config.state_dir {
            looks.use_presets(crate::presets::PresetDir::new(dir));
        }
        let now_ms = boottime_us() / 1000;
        let mut service = Self {
            board: config.board.board.id.clone(),
            definition,
            raw: crate::raw::RawState::default(),
            registry: config.registry,
            buses,
            slots: specs.into_iter().map(Slot::new).collect(),
            lights: Vec::new(),
            clients: Vec::new(),
            listener,
            socket: config.socket,
            next_client: 1,
            owners: Vec::new(),
            update: config.update_status.map(UpdateWatcher::new),
            next_update_ms: 0,
            first_frame_logged: false,
            booting_until: config.booting_ms.map(|ms| now_ms + ms),
            notifier: Notifier::none(),
            next_watchdog_ms: 0,
            pollfds: Vec::new(),
            due: Vec::new(),
            looks,
            next_looks_ms: 0,
            workers: Workers::new()?,
            state_dir: config.state_dir.clone(),
            fusions: Vec::new(),
            calibration_dir: config.calibration_dir,
            next_calibration_ms: 0,
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

    // The service logs to stderr (the journal); a power switch's status
    // changes (a fault tripping, a recovery) are worth reading there.
    #[allow(clippy::print_stderr)]
    fn status_event(&mut self, index: usize, status: DeviceStatus) {
        let slot = &self.slots[index];
        if slot
            .info
            .is_some_and(|i| i.class == DeviceClass::PowerSwitch)
        {
            eprintln!("lemnosd: {}: {status:?} {}", slot.id(), slot.reason);
        }
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
            if self.slots[index].spec.driver == fusion::DRIVER {
                self.build_fusion(index, now_ms);
                continue;
            }
            let was_present = self.slots[index].present;
            let changed =
                self.slots[index].build(&self.registry, &mut *self.buses, &mut SleepDelay, now_ms);
            if let Some(status) = changed {
                self.status_event(index, status);
            }
            if !was_present && self.slots[index].present {
                calibration::restore(&mut self.slots[index], &self.calibration_dir);
            }
            // A sensor on a bus is read by that bus's thread.
            if self.slots[index].placement == Placement::Worker && self.slots[index].lane.is_none()
            {
                let bus = self.slots[index]
                    .spec
                    .bus
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                match self.workers.lane(&bus) {
                    Ok(lane) => self.slots[index].lane = Some(lane),
                    // No thread to be had: the service thread reads it.
                    Err(_) => self.slots[index].placement = Placement::Inline,
                }
            }
            if self.slots[index].is_light() && !self.lights.iter().any(|l| l.slot == index) {
                let slot = &self.slots[index];
                let count = slot.device.as_ref().map_or(0, |d| d.pixel_count());
                let defaults = lemnos_board::light_defaults(&slot.spec).unwrap_or_default();
                let mut light = Light::new(index, count, defaults);
                light.gravity = lemnos_board::gravity_config(&slot.spec).ok();
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
        self.sync_fusion();
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

    /// Reads the inline sensors (those the service thread reads) that are
    /// due, in the order `schedule` picks, until none is due or the next one
    /// has to wait (see `next_deadline`).
    fn poll_devices(&mut self) {
        loop {
            self.due.clear();
            for (index, slot) in self.slots.iter().enumerate() {
                if slot.is_sensor() && slot.placement == Placement::Inline {
                    self.due.push(Due {
                        index,
                        deadline_us: slot.next_read_us,
                        // Never due when the sensor is not read.
                        period_us: slot.period_us().unwrap_or(u64::MAX),
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

    /// Reads one inline sensor on its schedule, reports a status change, and
    /// sends fresh values to the subscribers that are due.
    fn read_sensor(&mut self, index: usize) {
        let was_read = self.slots[index].read_us;
        if let Some(status) = self.slots[index].read_scheduled() {
            self.status_event(index, status);
        }
        let read_us = self.slots[index].read_us;
        if read_us != was_read {
            let row = self.slots[index].values;
            self.feed_fusion(index, &[row], read_us, 0);
            let now_us = self.now_us();
            self.deliver(index, read_us, now_us);
        }
    }

    /// Sends the reading at `read_us` to each subscriber whose next
    /// deadline it meets, and moves those deadlines on.
    fn deliver(&mut self, index: usize, read_us: u64, now_us: u64) {
        let due: Vec<(u32, u64)> = self.slots[index]
            .subscriptions
            .iter_mut()
            .filter(|s| s.next_us <= read_us)
            .map(|s| {
                s.next_us = schedule::advance(s.next_us, u64::from(s.period_ms) * 1000, now_us);
                (s.client, s.mask)
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let device = self.slots[index].id().to_string();
        let status = self.slots[index].status;
        let timestamp_us = self.slots[index].read_us;
        let sends: Vec<(usize, Message)> = self
            .clients
            .iter()
            .enumerate()
            .filter_map(|(ci, c)| {
                let mask = client_mask(&due, c.id)?;
                let message = Message::Reading(RawReading {
                    device: device.clone(),
                    timestamp_us,
                    status,
                    values: self.slots[index].channel_values_for(mask),
                });
                Some((ci, message))
            })
            .collect();
        for (ci, message) in sends {
            self.clients[ci].send(&message);
        }
    }

    /// Takes the reads that finished on the bus threads, then starts the
    /// ones that are due on each idle bus.
    fn collect_reads(&mut self) {
        while let Some(done) = self.workers.take_done() {
            self.complete(done);
        }
        self.dispatch_reads();
    }

    /// Applies a finished read: status, values, the next deadline, the
    /// subscribers and the one-shot readers waiting on it.
    fn complete(&mut self, done: Done) {
        let index = done.index;
        let now_us = self.now_us();
        let ok = done.result.is_ok();
        let count = if ok { done.count } else { 0 };
        let slot = &mut self.slots[index];
        slot.busy = false;
        slot.device = Some(done.device);
        slot.calibration_cache = slot
            .device
            .as_ref()
            .and_then(BoxedDevice::calibration_status);
        if count > 0 {
            slot.values = done.samples[count - 1];
        }
        let period_us = slot
            .device
            .as_ref()
            .and_then(lemnos_device::BoxedDevice::sample_period_us)
            .map_or(0, u64::from);
        // A buffered sensor with nothing new in its buffer has no new reading:
        // the last one stays the latest (its status still changes).
        let empty_batch = ok && count == 0 && slot.batched;
        let (was_fresh, was_us) = (slot.fresh, slot.read_us);
        let status = slot.finish_read(
            done.started_us,
            done.cost_us,
            done.result.map(|_| ()),
            &done.why,
        );
        if empty_batch {
            slot.fresh = was_fresh;
            slot.read_us = was_us;
        }
        if let Some(dispatched) = slot.dispatched_us.take() {
            // The grid moves on from the deadline this read was for. A
            // subscriber that arrived meanwhile set `next_read_us` to now.
            slot.next_read_us = match slot.period_us() {
                Some(period_us) => {
                    schedule::advance(dispatched, period_us, now_us).min(slot.next_read_us)
                }
                None => u64::MAX,
            };
        }
        let waiters = std::mem::take(&mut slot.waiters);
        if let Some(status) = status {
            self.status_event(index, status);
        }
        if count > 0 {
            self.deliver_batch(index, &done.samples[..count], done.started_us, period_us);
            self.feed_fusion(index, &done.samples[..count], done.started_us, period_us);
        }
        // Nothing new in the buffer, but a one-shot reader or a subscriber is
        // owed a reading: read the device now, on this thread.
        let mut ok = ok;
        let owed = !waiters.is_empty() || !self.slots[index].subscriptions.is_empty();
        if empty_batch && owed {
            if let Some(status) = self.slots[index].read() {
                self.status_event(index, status);
            }
            ok = self.slots[index].fresh;
            if ok {
                let read_us = self.slots[index].read_us;
                let row = self.slots[index].values;
                self.feed_fusion(index, &[row], read_us, 0);
                self.deliver(index, read_us, now_us);
            }
        }
        self.answer(index, &waiters, ok);
    }

    /// Sends a batch's samples to the subscribers that are due for them. The
    /// samples are `period_us` apart and the last was taken at `read_us`; each
    /// is stamped with its own time, and a subscriber's deadlines move on from
    /// those times, not from when the batch arrived.
    fn deliver_batch(
        &mut self,
        index: usize,
        samples: &[[i32; lemnos_device::MAX_CHANNELS]],
        read_us: u64,
        period_us: u64,
    ) {
        let channels = self.slots[index].info.map_or(0, |i| i.channels.len());
        let device = self.slots[index].id().to_string();
        let status = self.slots[index].status;
        let last = samples.len() as u64 - 1;
        for (i, values) in samples.iter().enumerate() {
            let at_us = read_us.saturating_sub((last - i as u64) * period_us);
            // A sample within half a sample period of a deadline is due for it:
            // a request in whole milliseconds (2 ms for a 400 Hz stream) then
            // gets every sample, not one in three.
            let slack = period_us / 2;
            let mut due: Vec<(u32, u64)> = Vec::new();
            for sub in self.slots[index].subscriptions.iter_mut() {
                if sub.next_us <= at_us + slack {
                    sub.next_us =
                        schedule::advance(sub.next_us, u64::from(sub.period_ms) * 1000, at_us);
                    due.push((sub.client, sub.mask));
                }
            }
            if due.is_empty() {
                continue;
            }
            let sends: Vec<(u32, Message)> = self
                .clients
                .iter()
                .filter_map(|c| {
                    let mask = client_mask(&due, c.id)?;
                    let message = Message::Reading(RawReading {
                        device: device.clone(),
                        timestamp_us: at_us,
                        status,
                        values: mask_values(&values[..channels], mask),
                    });
                    Some((c.id, message))
                })
                .collect();
            for (id, message) in sends {
                if let Some(client) = self.clients.iter_mut().find(|c| c.id == id) {
                    client.send(&message);
                }
            }
        }
    }

    /// Sends a one-shot reader its reading, or why there is none.
    fn answer(&mut self, index: usize, waiters: &[u32], ok: bool) {
        if waiters.contains(&GRAVITY_WAITER) {
            self.apply_gravity(index);
        }
        if waiters.is_empty() {
            return;
        }
        let message = if ok && self.slots[index].fresh {
            self.reading(index)
        } else {
            Message::Reply {
                id: 0,
                result: Err(Refusal::Device(
                    self.slots[index].error.unwrap_or(ErrorKind::Unavailable),
                )),
            }
        };
        for client in self.clients.iter_mut().filter(|c| waiters.contains(&c.id)) {
            client.send(&message);
        }
    }

    /// On each idle bus, starts the next read: a one-shot read a client waits
    /// for first, else the sensor the schedule picks among that bus's sensors.
    /// Sensors on other buses do not count, so a slow read on one bus never
    /// holds back a due read on another.
    fn dispatch_reads(&mut self) {
        let now_us = self.now_us();
        for lane in 0..self.workers.lanes() {
            if self.workers.is_busy(lane) {
                continue;
            }
            let waiting = self.slots.iter().position(|s| {
                s.lane == Some(lane) && s.present && !s.busy && !s.waiters.is_empty()
            });
            if let Some(index) = waiting {
                self.dispatch(index, false);
                continue;
            }
            self.due.clear();
            for (index, slot) in self.slots.iter().enumerate() {
                if slot.lane == Some(lane) && slot.present && !slot.busy {
                    self.due.push(Due {
                        index,
                        deadline_us: slot.next_read_us,
                        period_us: slot.period_us().unwrap_or(u64::MAX),
                        cost_us: slot.read_cost_us,
                    });
                }
            }
            if let Next::Run(index) = schedule::choose(&self.due, now_us) {
                self.dispatch(index, true);
            }
        }
    }

    /// Sends sensor `index`'s device to its bus thread for one read.
    /// `scheduled`: the read is the schedule's (it moves the grid on).
    fn dispatch(&mut self, index: usize, scheduled: bool) {
        let now_us = self.now_us();
        let slot = &mut self.slots[index];
        let Some(lane) = slot.lane else {
            return;
        };
        let Some(mut device) = slot.device.take() else {
            return;
        };
        // The channels this read needs (a one-shot reader's read takes all).
        let mask = slot.read_mask(now_us);
        device.select_channels(mask);
        slot.values_mask = mask;
        let job = Job { index, device };
        slot.busy = true;
        if scheduled {
            // Until the read completes, the schedule does not fire it again.
            slot.dispatched_us = Some(slot.next_read_us);
            slot.next_read_us = u64::MAX;
        }
        if let Err(job) = self.workers.submit(lane, job) {
            // The bus thread is gone: the device stays here.
            let slot = &mut self.slots[index];
            slot.device = Some(job.device);
            slot.busy = false;
            if let Some(deadline) = slot.dispatched_us.take() {
                slot.next_read_us = deadline;
            }
        }
    }

    /// Waits for the reads in flight (at shutdown), applying each.
    fn drain_reads(&mut self) {
        while self.workers.any_busy() {
            let Some(done) = self.workers.wait_done() else {
                return;
            };
            self.complete(done);
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
        if slot.present && !slot.is_sensor() {
            return Err(Refusal::Unsupported);
        }
        slot.subscriptions.push(Subscription {
            client,
            period_ms,
            next_us: now_us,
            mask: WHOLE_DEVICE,
        });
        // Read on the new schedule from now.
        slot.next_read_us = slot.next_read_us.min(now_us);
        let read_ms = u32::try_from(slot.read_cost_us.div_ceil(1000)).unwrap_or(u32::MAX);
        // The device is never read faster than its cap or one read takes.
        Ok(period_ms.max(slot.cap_ms()).max(read_ms))
    }

    /// Starts (or, with 0, ends) this client's readings of some channels of
    /// `device` (see `Request::SubscribeChannels`). Each selection is its own
    /// subscription, at its own period; the device reads what the due ones need.
    fn subscribe_channels(
        &mut self,
        ci: usize,
        device: &str,
        channels: &[String],
        period_ms: u32,
    ) -> Result<u32, Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let client = self.clients[ci].id;
        let now_us = self.now_us();
        let slot = &mut self.slots[index];
        if period_ms > 0 && slot.present && !slot.is_sensor() {
            return Err(Refusal::Unsupported);
        }
        let mask = channel_mask(slot.info, channels).ok_or(Refusal::UnknownChannel)?;
        slot.subscriptions
            .retain(|s| !(s.client == client && s.mask == mask));
        if period_ms == 0 {
            return Ok(0);
        }
        slot.subscriptions.push(Subscription {
            client,
            period_ms,
            next_us: now_us,
            mask,
        });
        slot.next_read_us = slot.next_read_us.min(now_us);
        let read_ms = u32::try_from(slot.read_cost_us.div_ceil(1000)).unwrap_or(u32::MAX);
        Ok(period_ms.max(slot.cap_ms()).max(read_ms))
    }

    fn poll_update(&mut self, now_ms: u64) {
        if now_ms < self.next_update_ms {
            return;
        }
        self.next_update_ms = now_ms + UPDATE_POLL_MS;
        let Some(change) = self.update.as_mut().and_then(|u| u.poll(now_ms)) else {
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
            if self.lights[li].look_changed {
                self.lights[li].look_changed = false;
                self.start_gravity(li);
            }
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
            // A new look goes out at once; animation frames on a fixed
            // FRAME_MS cadence, however often the loop wakes.
            if !light.animator.is_pending() && now_ms < light.next_render_ms {
                continue;
            }
            light.next_render_ms = next_frame_due(light.next_render_ms, now_ms);
            let slot = light.slot;
            let mut wrote = false;
            if let Some(frame) = light.animator.render(now_ms)
                && let Some(device) = self.slots[slot].device.as_mut()
            {
                // The frame the ring shows, for frame watches.
                let n = frame.len().min(MAX_LEDS);
                light.current[..n].copy_from_slice(&frame[..n]);
                light.has_frame = true;
                match device.show(frame) {
                    Ok(()) => wrote = true,
                    Err(kind) => {
                        let status = DeviceStatus::after_error(kind);
                        if let Some(status) = self.slots[slot].set_status(status, Some(kind)) {
                            self.status_event(slot, status);
                        }
                    }
                }
            }
            if wrote && !self.first_frame_logged {
                self.first_frame_logged = true;
                log_first_frame();
            }
        }
    }

    /// A new look starts: a falling sparkle wants the ring's bottom. It
    /// starts at the default (`default_down`, or the middle of the ring), and
    /// one read of the IMU moves it to where gravity says, through the same
    /// one-shot reads a client gets. Spawns after that fall toward it.
    fn start_gravity(&mut self, li: usize) {
        let falls = self.lights[li].animator.look().wants_bottom();
        let light = &mut self.lights[li];
        light.waiting_gravity = false;
        let Some(gravity) = light.gravity.clone() else {
            return;
        };
        light.animator.set_bottom(gravity.default_milli());
        if !falls {
            return;
        }
        let Some(device) = gravity.device.clone() else {
            return;
        };
        let Some(index) = self.slot_of(&device) else {
            return;
        };
        if !self.slots[index].is_sensor() {
            return;
        }
        if self.slots[index].placement == Placement::Worker {
            self.slots[index].waiters.push(GRAVITY_WAITER);
            self.lights[li].waiting_gravity = true;
            self.dispatch_reads();
        } else {
            if let Some(status) = self.slots[index].read_full() {
                self.status_event(index, status);
            }
            self.apply_gravity(index);
        }
    }

    /// Moves the bottom of every light waiting on the IMU in slot `index` to
    /// where its reading says down is (or leaves it at the default).
    fn apply_gravity(&mut self, index: usize) {
        let accel = acceleration(&self.slots[index]);
        let id = self.slots[index].id().to_string();
        for light in self.lights.iter_mut() {
            let waits = light.waiting_gravity
                && light.gravity.as_ref().and_then(|g| g.device.as_deref()) == Some(id.as_str());
            if !waits {
                continue;
            }
            light.waiting_gravity = false;
            let Some(gravity) = light.gravity.as_ref() else {
                continue;
            };
            let bottom = accel.and_then(|a| gravity.bottom(a));
            light
                .animator
                .set_bottom(bottom.map_or(gravity.default_milli(), |b| b.led_milli));
        }
    }

    /// Starts (or with `fps` 0, ends) client `ci`'s watch of a light's
    /// frames. The granted rate is `fps`, at most [`MAX_WATCH_FPS`].
    fn watch_frames(
        &mut self,
        ci: usize,
        device: &str,
        fps: u16,
        now_ms: u64,
    ) -> Result<u16, Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let li = self
            .lights
            .iter()
            .position(|l| l.slot == index)
            .ok_or(Refusal::Unsupported)?;
        let client = self.clients[ci].id;
        let watchers = &mut self.lights[li].watchers;
        watchers.retain(|w| w.client != client);
        if fps == 0 {
            return Ok(0);
        }
        let fps = fps.min(MAX_WATCH_FPS);
        watchers.push(FrameWatch {
            client,
            period_ms: (1_000 / u64::from(fps)).max(1),
            next_ms: now_ms,
            sent: None,
            info_sent: None,
            seq: 0,
        });
        Ok(fps)
    }

    /// Sends the watchers of each light what changed: its description when
    /// it changes, and its frame when the frame differs from the last one sent
    /// and the watcher's rate allows. Costs nothing for a light nobody watches.
    fn send_frames(&mut self, now_ms: u64) {
        let mut outgoing: Vec<(u32, Message)> = Vec::new();
        for li in 0..self.lights.len() {
            if self.lights[li].watchers.is_empty() {
                continue;
            }
            let info = self.light_info(li);
            let device = self.slots[self.lights[li].slot].id().to_string();
            let n = self.lights[li].count.min(MAX_LEDS);
            let light = &mut self.lights[li];
            let current = light.current;
            for w in &mut light.watchers {
                if w.info_sent.as_ref() != Some(&info) {
                    w.info_sent = Some(info.clone());
                    outgoing.push((w.client, Message::LightInfo(info.clone())));
                }
                if !differs(w, &current, n) || now_ms < w.next_ms {
                    continue;
                }
                let frame = current;
                w.sent = Some(frame);
                w.next_ms = now_ms + w.period_ms;
                w.seq = w.seq.wrapping_add(1);
                let pixels = frame[..n]
                    .iter()
                    .map(|c| {
                        u32::from(c.w) << 24
                            | u32::from(c.r) << 16
                            | u32::from(c.g) << 8
                            | u32::from(c.b)
                    })
                    .collect();
                outgoing.push((
                    w.client,
                    Message::Frame(LightFrame {
                        device: device.clone(),
                        seq: w.seq,
                        pixels,
                    }),
                ));
            }
        }
        for (client, message) in outgoing {
            if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
                c.send(&message);
            }
        }
    }

    /// A light's geometry and what it shows now.
    fn light_info(&self, li: usize) -> LightInfo {
        let slot = self.lights[li].slot;
        let spec = &self.slots[slot].spec;
        let (offset, clockwise) = lemnos_board::light_geometry(spec).unwrap_or((0, true));
        let (owner, layer, look) = match self.lights[li].shown_intent() {
            Some((owner, layer, intent)) => {
                let look = match intent.show {
                    Show::Look { name, .. } => name.as_str().to_string(),
                    _ => String::new(),
                };
                (self.owner_name(owner), layer.name().to_string(), look)
            }
            None => (String::new(), String::new(), String::new()),
        };
        LightInfo {
            device: self.slots[slot].id().to_string(),
            count: u16::try_from(self.lights[li].count).unwrap_or(u16::MAX),
            offset,
            clockwise,
            look,
            layer,
            owner,
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
                        // A one-shot read is served from the last reading
                        // while it is no older than the device's cap.
                        let now_us = self.now_us();
                        let slot = &self.slots[index];
                        // A reading of only some channels (a subscription's) is
                        // not the whole device's: a one-shot read reads them all.
                        let stale = !slot.fresh
                            || slot.values_mask & slot.all_channels() != slot.all_channels()
                            || now_us.saturating_sub(slot.read_us)
                                >= u64::from(slot.cap_ms()) * 1000;
                        if stale && slot.present && slot.placement == Placement::Worker {
                            // The bus thread reads it; the reply comes with the reading.
                            let id = self.clients[ci].id;
                            self.slots[index].waiters.push(id);
                            self.dispatch_reads();
                            return;
                        }
                        if stale
                            && self.slots[index].is_sensor()
                            && let Some(status) = self.slots[index].read_full()
                        {
                            self.status_event(index, status);
                        }
                        if stale && self.slots[index].fresh {
                            let (read_us, row) =
                                (self.slots[index].read_us, self.slots[index].values);
                            self.feed_fusion(index, &[row], read_us, 0);
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
            Request::SubscribeChannels {
                id,
                device,
                channels,
                period_ms,
            } => {
                let result = self.subscribe_channels(ci, &device, &channels, period_ms);
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
            Request::WatchFrames { id, device, fps } => {
                let result = self.watch_frames(ci, &device, fps, now_ms);
                self.clients[ci].send(&Message::Reply {
                    id,
                    result: result.map(f64::from),
                });
            }
            Request::Calibration {
                id,
                device,
                command,
            } => {
                let result = self.calibration(ci, &device, command);
                self.clients[ci].send(&Message::Reply {
                    id,
                    result: result.map(|()| 0.0),
                });
            }
            Request::CalibrationStatus { id, device } => {
                let message = match self.calibration_status(&device) {
                    Ok(status) => Message::CalibrationStatus { id, device, status },
                    Err(refusal) => Message::Reply {
                        id,
                        result: Err(refusal),
                    },
                };
                self.clients[ci].send(&message);
            }
            Request::Looks { id, op } => {
                // Preset changes and deletions are Atlas's, Orion's or an
                // operator's (`lemnos-ctl`); other clients may only read.
                let changes = matches!(
                    op,
                    LooksOp::PresetApply(_)
                        | LooksOp::PresetSave { .. }
                        | LooksOp::PresetDelete(_)
                        | LooksOp::Delete(_)
                );
                let name = self.clients[ci].name.clone();
                let result = if changes && !may_change_looks(&name) {
                    Err(format!(
                        "client {name:?} may not change looks (writers: atlas, orion:*, lemnos-ctl)"
                    ))
                } else {
                    self.looks_op(op)
                };
                self.clients[ci].send(&Message::Text { id, result });
            }
        }
    }

    /// One looks operation (`Request::Looks`); the answer is its text or its
    /// refusal.
    fn looks_op(&mut self, op: LooksOp) -> Result<String, String> {
        match op {
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
            LooksOp::PresetList => Ok(self.looks.preset_list()),
            LooksOp::PresetShow(name) => self.looks.preset_show(&name),
            LooksOp::PresetApply(name) => {
                let applied = self.looks.preset_apply(&name);
                self.invalidate_lights();
                applied
            }
            LooksOp::PresetSave { name, text } => {
                let saved = self.looks.preset_save(&name, &text);
                self.invalidate_lights();
                saved
            }
            LooksOp::PresetDelete(name) => {
                let deleted = self.looks.preset_delete(&name);
                self.invalidate_lights();
                deleted
            }
            LooksOp::Delete(name) => {
                let deleted = self.looks.look_delete(&name);
                self.invalidate_lights();
                deleted
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
        // A power switch's write is latched: it is the setting, not a
        // client's lease, so it does not end with the writer's connection.
        let latched = self.slots[index]
            .info
            .is_some_and(|i| i.class == DeviceClass::PowerSwitch);
        if !latched {
            self.slots[index].note_write(
                ci,
                &crate::clients::Requester {
                    id: wid,
                    name: &wname,
                    keep: wkeep,
                },
            );
        }
        if self.slots[index].before_write() {
            self.save_fan_state();
        }
        let device_ref = self.slots[index]
            .device
            .as_mut()
            .ok_or(Refusal::Device(ErrorKind::Unavailable))?;
        match device_ref.set(ci, raw as i32) {
            Ok(applied) => {
                if ci == 0 && latched {
                    // The switch's reading is what it was just set to.
                    self.slots[index].values[0] = applied;
                    self.persist_power(index, applied == 1);
                }
                Ok(crate::scaled(applied, exponent))
            }
            Err(kind) => {
                let status = DeviceStatus::after_error(kind);
                if let Some(status) = self.slots[index].set_status(status, Some(kind)) {
                    self.status_event(index, status);
                }
                Err(Refusal::Device(kind))
            }
        }
    }

    /// Saves a persisting power switch's state (see [`state`]).
    #[allow(clippy::print_stderr)]
    fn persist_power(&self, index: usize, on: bool) {
        let slot = &self.slots[index];
        let Some(dir) = self.state_dir.as_ref() else {
            return;
        };
        if !state::power_persists(&slot.spec) {
            return;
        }
        if let Err(error) = state::save_power(dir, slot.id(), on) {
            eprintln!("lemnosd: {}: saving its state: {error}", slot.id());
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
            if !slot.present {
                next = next.min(slot.next_build_ms * 1000);
            } else if slot.is_sensor() && !slot.busy && slot.next_read_us > now_us {
                // A sensor whose read is in flight wakes the loop when it
                // completes (the wake pair), not at a deadline.
                next = next.min(slot.next_read_us);
            }
        }
        for light in &self.lights {
            if let Some(at) = light.next_ms(now_us / 1000) {
                next = next.min(at * 1000);
            }
            // A frame that changed while its watcher's rate held it back goes
            // out at the watcher's next time.
            for w in &light.watchers {
                if differs(w, &light.current, light.count.min(MAX_LEDS)) {
                    next = next.min(w.next_ms * 1000);
                }
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
        self.collect_reads();
        self.poll_devices();
        self.poll_update(now);
        self.poll_looks(now);
        self.poll_calibration(now);
        self.render_lights(now);
        self.send_frames(now);
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
        self.pollfds
            .push(PollFd::new(self.workers.wake_fd(), POLLIN));
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
        self.workers.drain_wake();
        self.collect_reads();

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
        // A new subscription may be due now (and the fusion's inputs follow it).
        self.sync_fusion();
        self.poll_devices();
        // A request may have changed a light: render without waiting.
        self.render_lights(self.now_ms());
        self.send_frames(self.now_ms());
        self.send_edges();
        self.drop_closed();
        self.sync_fusion();
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
            for light in &mut self.lights {
                light.watchers.retain(|w| w.client != client.id);
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
        self.drain_reads();
        let now = self.now_ms();
        for slot in self.slots.iter_mut() {
            calibration::persist(slot, &self.calibration_dir, now, false);
        }
        self.raw.release_all();
        for slot in &mut self.slots {
            slot.restore();
        }
        self.power_on_exit();
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

    /// Each power switch goes to its `on_exit` state (`keep`: as it is).
    fn power_on_exit(&mut self) {
        for slot in &mut self.slots {
            if !slot
                .info
                .is_some_and(|i| i.class == DeviceClass::PowerSwitch)
            {
                continue;
            }
            let level = match state::power_on_exit(&slot.spec) {
                "on" => 1,
                "off" => 0,
                _ => continue,
            };
            if let Some(device) = slot.device.as_mut() {
                let _ = device.set(0, level);
            }
        }
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

/// The channels a client wants from a delivery: the union of the masks of its
/// due subscriptions, `None` if it has none due.
fn client_mask(due: &[(u32, u64)], client: u32) -> Option<u64> {
    let mut found = None;
    for (_, mask) in due.iter().filter(|(c, _)| *c == client) {
        found = Some(found.unwrap_or(0u64) | mask);
    }
    found
}

/// `values` with no value for the channels outside `mask`.
fn mask_values(values: &[i32], mask: u64) -> Vec<i32> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if i < 64 && mask & (1 << i) != 0 {
                *v
            } else {
                lemnos_device::NO_VALUE
            }
        })
        .collect()
}

/// Whether the frame `current` differs from the last one watcher `w` was sent,
/// over the first `n` LEDs.
fn differs(w: &FrameWatch, current: &[lemnos_light::Rgbw; MAX_LEDS], n: usize) -> bool {
    w.sent.as_ref().is_none_or(|sent| sent[..n] != current[..n])
}

/// The one-shot waiter a falling sparkle's gravity read is queued under (no
/// client has this id).
const GRAVITY_WAITER: u32 = u32::MAX;

/// The acceleration (m/s^2, the device's own axes) of an IMU, from its
/// last reading; `None` when a channel has no value.
fn acceleration(slot: &Slot) -> Option<[f64; 3]> {
    let info = slot.info?;
    let mut out = [0.0; 3];
    for (axis, name) in ["acceleration.x", "acceleration.y", "acceleration.z"]
        .iter()
        .enumerate()
    {
        let index = info.channels.iter().position(|c| c.name == *name)?;
        let raw = *slot.values.get(index)?;
        if raw == lemnos_device::NO_VALUE {
            return None;
        }
        out[axis] = f64::from(raw) * 10f64.powi(i32::from(info.channels[index].exponent));
    }
    Some(out)
}

/// Logs, once per start, when the first ring frame was written: seconds since
/// the kernel started (`/proc/uptime`), for the boot's timing.
#[allow(clippy::print_stderr)]
fn log_first_frame() {
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| "?".into());
    eprintln!("lemnosd: first ring frame written at {uptime} s since boot");
}
