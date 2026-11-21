//! An instance of the simulated camera chain, in process: [`Sim`], made by [`SimBuilder`].
//!
//! [`Sim::builder`] starts `lemnosd`'s `Service` on a unix socket in a temporary directory, over a
//! simulated board that carries the reference board's sensor, power switch and receiver (no PiSP
//! blocks: the driver installs its simulated block once per process, so the front ends stay with
//! the process's own chain). The instance's state is its own: its sensor bus, clock, sensor-start
//! queue, receiver switches and lock-step frame log. Two instances in one process share none of
//! it, and their daemons' drivers find their own chain by the board's name, which the builder makes
//! unique.
//!
//! What a client does with the socket is what a board's client does: [`Sim::consumer`] opens the
//! sensor and the receiver and their `frames` stream, the way the chain proof does.
//!
//! Paced ([`Pace::Real`]) frames fall on the sensor's frame period, from the start of the receiver, on
//! the host's `CLOCK_MONOTONIC` or on a clock the test injects ([`ClockSource::Injected`],
//! [`Sim::advance_to`]). Lock-step ([`Pace::Lockstep`]) frames are made one per [`Consumer::next`],
//! with the controls that [`Sim::post`] names for each frame.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lemnos_client::{Client, FrameStream, HeldFrame, Next, RemoteCaller};
use lemnos_contract_camera_receiver::{CameraReceiver, Counters, Packing};
use lemnos_contract_camera_sensor::{CameraSensor, Controls};
use lemnos_core::{Bound, Status};
use lemnos_host::Options;
use lemnos_linux_stream::LinuxStreams;
use lemnosd::{Config, Service};

use crate::chain::{Chain, Control, Home, Meta, Pace, Registration, SensorFault};
use crate::scene::{self, Scene};
use crate::{Model, SimBuffers, chain_board, instance_drivers};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");

/// The largest exposure a control may ask for, in microseconds.
pub const MAX_EXPOSURE_US: u32 = 100_000;
/// The largest gain a control may ask for, in thousandths (16x).
pub const MAX_GAIN_MILLI: u32 = 16_000;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Where an instance's frame clock comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockSource {
    /// The daemon's tick: `CLOCK_MONOTONIC`.
    Monotonic,
    /// The test's clock: it moves only when [`Sim::advance_to`] moves it.
    Injected,
}

/// Where an instance's buffers come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// The host's dma-heap, when it has one that works. [`SimBuilder::start`] fails without one.
    DmaHeap,
    /// Sealed memfds, which can be read and written with `pread` (any host).
    Memfd,
}

/// Why a control was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The frame has been granted already: the control would land late.
    Late,
    /// The value is out of range.
    Value,
}

/// A builder for one instance. The defaults: paced on the host clock, dma-heap buffers if the host
/// has them (else memfds), the pattern scene, the reference sensor model.
#[derive(Clone)]
pub struct SimBuilder {
    pace: Pace,
    clock: ClockSource,
    memory: Memory,
    scene: Scene,
    model: Model,
    auto_off_after: Option<u32>,
}

impl Default for SimBuilder {
    fn default() -> Self {
        Self {
            pace: Pace::Real,
            clock: ClockSource::Monotonic,
            memory: Memory::Memfd,
            scene: Scene::Pattern,
            model: Model::B1,
            auto_off_after: None,
        }
    }
}

impl SimBuilder {
    /// How the receiver makes its frames.
    #[must_use]
    pub fn pace(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    /// Where the frame clock comes from.
    #[must_use]
    pub fn clock(mut self, clock: ClockSource) -> Self {
        self.clock = clock;
        self
    }

    /// Where the buffers come from.
    #[must_use]
    pub fn memory(mut self, memory: Memory) -> Self {
        self.memory = memory;
        self
    }

    /// What the frames show (lock-step frames; paced frames carry their identity's bytes).
    #[must_use]
    pub fn scene(mut self, scene: Scene) -> Self {
        self.scene = scene;
        self
    }

    /// The sensor model.
    #[must_use]
    pub fn model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// The receiver's `auto_off_after`: the failed attempts in a row before the host turns it off.
    #[must_use]
    pub fn auto_off_after(mut self, attempts: u32) -> Self {
        self.auto_off_after = Some(attempts);
        self
    }

    /// Starts the instance: the service on a socket in a new temporary directory.
    ///
    /// # Errors
    /// The buffers cannot be had (`Memory::DmaHeap` on a host without one), the directory cannot be
    /// made, or the service does not start.
    pub fn start(self) -> Result<Sim, String> {
        let buffers = match self.memory {
            Memory::Memfd => SimBuffers::memfd(),
            Memory::DmaHeap => SimBuffers::dma_heap()
                .ok_or_else(|| "this host has no usable dma-heap".to_string())?,
        };
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let name = format!("camera-sim-{}-{n}", std::process::id());
        let dir = std::env::temp_dir().join(format!("cs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let socket = dir.join("lemnosd.sock");

        let chain = Arc::new(Chain::instance(self.model));
        chain.set_pace(self.pace);
        chain.set_injected(self.clock == ClockSource::Injected);
        *chain.scene() = self.scene;
        {
            let mut lock = chain.lock();
            lock.exposure_us = scene::REFERENCE_EXPOSURE_US;
            lock.gain_milli = scene::REFERENCE_GAIN_MILLI;
        }

        let mut board = chain_board(REFERENCE_BOARD)?;
        board.board.id = name.clone();
        // The instance's board: the sensor, the power switch and the receiver. The receiver's link
        // is left out (its front end is not in an instance), and the back end's hardware with it.
        board.devices.retain(|d| {
            matches!(
                d.id.as_str(),
                "cam0-power" | "cam0-sensor" | "cam0-receiver"
            )
        });
        for device in &mut board.devices {
            if device.id == "cam0-receiver" {
                device.link = None;
                device.auto_off_after = self.auto_off_after;
            }
        }
        board.hardware.retain(|h| h.controller != "pisp-be");

        let registration: Registration = Home::register(&name, chain.clone());
        let lines = lemnos_sim::SimBoard::new();
        lines.add_line("gpio:pinctrl-rp1:34");
        let backend = crate::Board::on(Home::Own(chain.clone()), lines);
        let service = Service::start_with_buffers(
            Config {
                socket: socket.clone(),
                name,
                board,
                drivers: instance_drivers(),
                backend: Box::new(backend),
                options: Options::default(),
            },
            Arc::new(buffers),
        )
        .map_err(|e| e.to_string())?;
        Ok(Sim {
            chain,
            socket,
            dir,
            service: Some(service),
            _registration: registration,
        })
    }
}

/// A running instance. Dropping it stops its service and removes its directory.
pub struct Sim {
    chain: Arc<Chain>,
    socket: PathBuf,
    dir: PathBuf,
    service: Option<Service>,
    _registration: Registration,
}

impl Sim {
    /// A builder for an instance.
    #[must_use]
    pub fn builder() -> SimBuilder {
        SimBuilder::default()
    }

    /// The service's socket. Clients connect to it as they do to a board's daemon.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The sensor's frame period, from its registers (ns).
    #[must_use]
    pub fn period_ns(&self) -> u64 {
        self.chain.period_ns()
    }

    /// The clock the instance reads (ns): the injected one, or the instance's own sim time.
    #[must_use]
    pub fn now_ns(&self) -> u64 {
        self.chain.clock.now()
    }

    /// Moves the injected clock to `ns` (a time not later than now changes nothing). A paced
    /// receiver makes every frame that has fallen due by then, in one tick.
    pub fn advance_to(&self, ns: u64) {
        self.chain.clock.advance_to(ns);
    }

    /// Whether the sensor is streaming (its mode-select register is set).
    #[must_use]
    pub fn streaming(&self) -> bool {
        self.chain.with_world(|w| w.streaming)
    }

    /// Sets the sensor fault. [`SensorFault::Failed`] stays until cleared; [`SensorFault::Gone`]
    /// is spent by the next transaction to the sensor.
    pub fn sensor_fault(&self, fault: SensorFault) {
        self.chain.faults().sensor = Some(fault);
    }

    /// Clears a sensor fault that has not been spent.
    pub fn clear_sensor_fault(&self) {
        self.chain.faults().sensor = None;
    }

    /// The receiver's next `n` starts are refused with `IO`.
    pub fn refuse_receiver_starts(&self, n: u32) {
        self.chain.faults().refusals = n;
    }

    /// The receiver makes no frames while this is on, and raises no fault.
    pub fn receiver_silent(&self, on: bool) {
        self.chain.faults().silent = on;
    }

    /// The first frame the receiver did not make while silent, if it has been silent.
    #[must_use]
    pub fn first_unmade(&self) -> Option<u64> {
        self.chain.faults().first_unmade
    }

    /// The receiver is gone for good: it reports [`lemnos_core::Health::Gone`] with `reason`.
    pub fn receiver_gone(&self, reason: &'static str) {
        self.chain.faults().gone = Some(reason);
    }

    /// How many times the receiver has reported itself gone to the host.
    #[must_use]
    pub fn gone_reports(&self) -> u64 {
        self.chain.faults().gone_reports
    }

    /// The receiver's next tick ends its consumer (`fault_consumer`).
    pub fn fault_consumer(&self) {
        self.chain.faults().consumer = true;
    }

    /// Grants one frame: the receiver makes the next frame when a buffer is free. Lock-step only.
    /// [`Consumer::next`] grants for you; a test that drives the sensor itself uses this.
    ///
    /// # Errors
    /// The instance is not in lock-step.
    pub fn grant(&self) -> Result<u64, String> {
        if self.chain.pace() != Pace::Lockstep {
            return Err("grant needs the lock-step pace".to_string());
        }
        Ok(self.chain.grant())
    }

    /// Posts `control` for frame `frame`: it lands on that frame and holds from there on.
    ///
    /// # Errors
    /// [`Refusal::Late`] when the frame has been granted already; [`Refusal::Value`] when the value
    /// is out of range (an exposure of 1 to [`MAX_EXPOSURE_US`], a gain of 1x to [`MAX_GAIN_MILLI`]).
    pub fn post(&self, frame: u64, control: Control) -> Result<(), Refusal> {
        let mut lock = self.chain.lock();
        if frame < lock.granted {
            return Err(Refusal::Late);
        }
        match control {
            Control::Exposure(us) if !(1..=MAX_EXPOSURE_US).contains(&us) => {
                return Err(Refusal::Value);
            }
            Control::Gain(milli) if !(1000..=MAX_GAIN_MILLI).contains(&milli) => {
                return Err(Refusal::Value);
            }
            _ => {}
        }
        lock.pending.entry(frame).or_default().push(control);
        Ok(())
    }

    /// What frame `frame` was made with, if it has been made and is still in the log.
    #[must_use]
    pub fn meta(&self, frame: u64) -> Option<Meta> {
        self.chain.lock().meta.get(&frame).copied()
    }

    /// Opens the sensor and the receiver as a client of the socket, configures the receiver for the
    /// sensor's mode 0 and opens its `frames` stream. Nothing is started yet: [`Consumer::start`].
    ///
    /// # Errors
    /// A device is missing or refuses the set-up. The message names which.
    pub fn consumer(&self) -> Result<Consumer, String> {
        let client = Client::connect(&self.socket, "camera-sim").map_err(|e| e.to_string())?;
        client.set_timeout(Duration::from_secs(30));
        let list = client.list().map_err(|e| e.to_string())?;
        let index = |name: &str| {
            list.iter()
                .find(|d| d.name == name)
                .map(|d| d.index)
                .ok_or(format!("no device {name}"))
        };
        let receiver_index = index("cam0-receiver")?;
        let mut sensor = client
            .open::<dyn CameraSensor>("cam0-sensor")
            .map_err(|e| format!("cam0-sensor: {e}"))?;
        let mut receiver = client
            .open::<dyn CameraReceiver>("cam0-receiver")
            .map_err(|e| format!("cam0-receiver: {e}"))?;
        let _ = sensor.stop();
        sensor
            .select_mode(0)
            .map_err(|s| format!("select_mode: {s:?}"))?;
        let mode = sensor.mode(0).map_err(|s| format!("mode: {s:?}"))?;
        receiver
            .configure(mode.link, Packing::Packed)
            .map_err(|s| format!("receiver configure: {s:?}"))?;
        let frames = client
            .open_stream(receiver_index, "frames", Arc::new(LinuxStreams))
            .map_err(|e| format!("frames stream: {e}"))?;
        Ok(Consumer {
            chain: self.chain.clone(),
            client,
            sensor: Some(sensor),
            receiver: Some(receiver),
            frames,
        })
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            service.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One event of a lock-step consumer.
pub enum Event {
    /// A frame, with what it was made with. Give it back with [`Consumer::release`].
    Frame(SimFrame),
    /// The time passed with no frame: the grant is still outstanding.
    Idle,
    /// The receiver has stopped making frames (a fault, or silence); `since_ns` is when it was
    /// first seen to stop.
    Stalled { since_ns: u64 },
    /// The receiver is making frames again.
    Resumed,
    /// The stream ended.
    Ended,
}

/// A frame a consumer has.
pub struct SimFrame {
    pub id: u64,
    pub timestamp_ns: u64,
    pub meta: Meta,
    pub lease: HeldFrame,
}

/// A client's view of the instance: the sensor, the receiver and the receiver's frames.
pub struct Consumer {
    chain: Arc<Chain>,
    client: Client,
    sensor: Option<Bound<dyn CameraSensor, RemoteCaller>>,
    receiver: Option<Bound<dyn CameraReceiver, RemoteCaller>>,
    frames: FrameStream,
}

impl Consumer {
    /// Starts the receiver, then the sensor, as the chain proof does.
    ///
    /// # Errors
    /// The receiver or the sensor refuses its start: the status it gave.
    pub fn start(&mut self) -> Result<(), Status> {
        if let Some(receiver) = self.receiver.as_mut() {
            receiver.start()?;
        }
        if let Some(sensor) = self.sensor.as_mut() {
            sensor.start()?;
        }
        Ok(())
    }

    /// Advances exactly one frame in lock-step: grants it (unless a grant is still waiting for its
    /// buffer) and waits up to `timeout` for a frame.
    ///
    /// # Errors
    /// The instance is not in lock-step, or the stream is malformed.
    pub fn next(&mut self, timeout: Duration) -> Result<Event, String> {
        if self.chain.pace() != Pace::Lockstep {
            return Err("next needs the lock-step pace".to_string());
        }
        if !self.chain.grant_waiting() {
            self.chain.grant();
        }
        self.recv(timeout)
    }

    /// Waits up to `timeout` for the next frame the stream has, granting nothing. A paced instance
    /// is read this way.
    ///
    /// # Errors
    /// The stream is malformed.
    pub fn recv(&mut self, timeout: Duration) -> Result<Event, String> {
        let next = self
            .frames
            .next_or_end(timeout)
            .map_err(|e| e.to_string())?;
        Ok(match next {
            Next::Frame(lease) => {
                let id = lease.sequence();
                let meta = self.chain.lock().meta.get(&id).copied().unwrap_or(Meta {
                    exposure_us: 0,
                    analogue_gain_milli: 0,
                    applied: [None, None],
                    sensor_timestamp_ns: 0,
                });
                Event::Frame(SimFrame {
                    id,
                    timestamp_ns: lease.timestamp_ns(),
                    meta,
                    lease,
                })
            }
            Next::Stalled(_) => Event::Stalled {
                since_ns: self.chain.clock.now(),
            },
            Next::Resumed { .. } => Event::Resumed,
            Next::Ended(_) => Event::Ended,
            Next::TimedOut | Next::AtLimit | Next::JobEnded { .. } => Event::Idle,
        })
    }

    /// Gives a frame back to the stream.
    ///
    /// # Errors
    /// The stream refuses the release.
    pub fn release(&mut self, frame: SimFrame) -> Result<(), String> {
        self.frames.release(frame.lease).map_err(|e| e.to_string())
    }

    /// The bytes of a frame this consumer holds.
    #[must_use]
    pub fn bytes<'a>(&'a self, frame: &'a SimFrame) -> &'a [u8] {
        self.frames.bytes(&frame.lease)
    }

    /// The frames the stream skipped (a frame that came while the consumer held all it may).
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.frames.skipped()
    }

    /// Sets the sensor's frame length in lines (its vertical total): the frame period follows it.
    ///
    /// # Errors
    /// The sensor refuses the length.
    pub fn set_frame_length(&mut self, lines: u32) -> Result<(), Status> {
        match self.sensor.as_mut() {
            Some(sensor) => sensor.set_controls(Controls {
                frame_length: Some(lines),
                ..Controls::default()
            }),
            None => Err(Status::NOT_SUPPORTED),
        }
    }

    /// The receiver's counters: frames made, dropped (no buffer free) and errors.
    ///
    /// # Errors
    /// The receiver is not open, or refuses.
    pub fn counters(&mut self) -> Result<Counters, Status> {
        match self.receiver.as_mut() {
            Some(receiver) => receiver.counters(),
            None => Err(Status::NOT_SUPPORTED),
        }
    }

    /// The client, for the socket's other calls.
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }
}
