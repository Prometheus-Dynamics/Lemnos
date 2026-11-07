//! The service's fan controllers: each `[[composites]]` `fan-controller` is a
//! composite device (`lemnos_device::fan_device`) in its own slot. Its
//! inputs' channels feed it once a period, and it drives its fan's `duty`
//! control. Clients tune it with `Control::set` (kept in the state directory,
//! see `state::save_fan_tuning`). The fan's hand-back (set at its first bind)
//! is the failsafe; see `docs/composite-devices.md`, "Failsafe".

use super::Service;
use crate::devices::{Placement, Slot};
use crate::state;
use lemnos_board::fan_controller::{self, FAN_CONTROLLER, FanControllerSpec};
use lemnos_device::fan_control::{Health, MAX_INPUTS, Point, Settings};
use lemnos_device::fan_device::{
    CTL_ENABLED, CTL_FLOOR, CTL_HYSTERESIS, CTL_SLEW, FanDevice, PERIOD_MS,
};
use lemnos_device::{
    Channel, Composite, Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo,
    DeviceStatus, InputSpec, MAX_CHANNELS, NO_VALUE, Outputs, Quantity, Sample, Sensor,
};
use lemnos_drivers_linux::{CONTROL_DUTY, CONTROL_MODE, MODE_MANUAL};
use lemnos_hal::ErrorKind;
use lemnos_ipc::Refusal;
use std::fmt::Write as _;

/// After a failed write the controller hands the fan back and waits this long
/// before it takes the fan again (ms).
const RETRY_MS: u64 = 10_000;
/// A controller whose last step is older than this many periods has stopped
/// completing steps, and lemnosd stops pinging the watchdog (`fan_stepping`).
const STALL_PERIODS: u64 = 3;

/// One fan controller built from the board.
pub(crate) struct FanLoop {
    pub spec: FanControllerSpec,
    /// The controller's own slot: its channels and controls are listed there.
    pub slot: usize,
    pub device: FanDevice,
    /// The fan's slot.
    fan: usize,
    /// For each input, its slot and channel index.
    inputs: Vec<(usize, usize)>,
    next_ms: u64,
    /// Whether the fan has been put in manual mode and written to.
    taking: bool,
    /// After a failed write: when the controller may take the fan again.
    retry_at_ms: Option<u64>,
    /// When the controller last completed a step (ms, the step's clock).
    stepped_ms: Option<u64>,
}

/// The fan as the controller's output: the first write takes the fan (manual
/// mode), each write sets its duty, and a release hands it back.
struct FanOutput<'a> {
    slot: &'a mut Slot,
    taking: &'a mut bool,
}

impl Outputs for FanOutput<'_> {
    fn set(&mut self, _output: usize, value: i32) -> Result<(), ErrorKind> {
        let device = self.slot.device.as_mut().ok_or(ErrorKind::Unavailable)?;
        if !*self.taking {
            device.set(CONTROL_MODE, MODE_MANUAL)?;
            *self.taking = true;
        }
        device.set(CONTROL_DUTY, value).map(|_| ())
    }

    fn release(&mut self, _output: usize) {
        self.slot.restore();
        *self.taking = false;
    }
}

/// The controller's descriptor: its channels and controls, named after the
/// board's input roles. `lemnos-device`'s descriptors borrow `'static` data,
/// and the names come from the board, so they are leaked once per controller
/// when the service starts (a few hundred bytes each).
fn leak_info(spec: &FanControllerSpec, units: &[(Quantity, i8)]) -> &'static DeviceInfo {
    fn name(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }
    // The layout `lemnos_device::fan_device` documents.
    let mut channels = vec![
        Channel::new("demand", Quantity::Ratio, -3),
        Channel::new("duty", Quantity::Ratio, -3),
        Channel::new("source", Quantity::Mode, 0),
    ];
    // Hysteresis is in the first input's unit.
    let (unit, exponent) = units.first().copied().unwrap_or((Quantity::Ratio, -3));
    let mut controls = vec![
        ControlInfo::new("floor", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("hysteresis", unit, exponent, 0, i32::MAX),
        ControlInfo::new("slew", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("enabled", Quantity::Level, 0, 0, 1),
    ];
    for (input, &(quantity, exponent)) in spec.inputs.iter().zip(units) {
        channels.push(Channel::new(
            name(format!("input.{}", input.role)),
            quantity,
            exponent,
        ));
    }
    for (input, &(quantity, exponent)) in spec.inputs.iter().zip(units) {
        for point in 0..input.points.len() {
            controls.push(ControlInfo::new(
                name(format!("curve.{}.{point}.value", input.role)),
                quantity,
                exponent,
                i32::MIN,
                i32::MAX,
            ));
            controls.push(ControlInfo::new(
                name(format!("curve.{}.{point}.duty", input.role)),
                Quantity::Ratio,
                -3,
                0,
                1000,
            ));
        }
    }
    Box::leak(Box::new(DeviceInfo::new(
        DeviceClass::Fan,
        "fan-controller",
        Box::leak(channels.into_boxed_slice()),
        Box::leak(controls.into_boxed_slice()),
    )))
}

/// The tuning as `state` saves it: one line per setting, and one line per
/// input's curve (`curve <role> <value> <duty> ...`).
fn tuning_text(device: &FanDevice, spec: &FanControllerSpec) -> String {
    let Settings {
        floor,
        hysteresis,
        slew_per_s,
        ..
    } = device.settings();
    let mut text = format!(
        "floor {floor}\nhysteresis {hysteresis}\nslew {slew_per_s}\nenabled {}\n",
        i32::from(device.enabled())
    );
    for (i, input) in spec.inputs.iter().enumerate() {
        let _ = write!(text, "curve {}", input.role);
        for point in device.curve(i) {
            let _ = write!(text, " {} {}", point.value, point.duty);
        }
        text.push('\n');
    }
    text
}

/// Applies a saved tuning to `device`. A line the device or the board does
/// not accept is an error; the caller keeps the board's settings then.
fn apply_tuning(
    device: &mut FanDevice,
    spec: &FanControllerSpec,
    text: &str,
) -> Result<(), String> {
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else {
            continue;
        };
        if key == "curve" {
            let role = words.next().ok_or("curve: no role")?;
            let index = spec
                .inputs
                .iter()
                .position(|i| i.role == role)
                .ok_or_else(|| format!("curve {role:?}: no such input"))?;
            let numbers = words
                .map(|w| {
                    w.parse::<i32>()
                        .map_err(|_| format!("curve {role:?}: {w:?}"))
                })
                .collect::<Result<Vec<i32>, String>>()?;
            let (pairs, rest) = numbers.as_chunks::<2>();
            if !rest.is_empty() {
                return Err(format!("curve {role:?}: values and duties come in pairs"));
            }
            let points: Vec<Point> = pairs
                .iter()
                .map(|pair| Point {
                    value: pair[0],
                    duty: pair[1],
                })
                .collect();
            device
                .set_curve(index, &points)
                .map_err(|e| format!("curve {role:?}: {e:?}"))?;
            continue;
        }
        let index = match key {
            "floor" => CTL_FLOOR,
            "hysteresis" => CTL_HYSTERESIS,
            "slew" => CTL_SLEW,
            "enabled" => CTL_ENABLED,
            other => return Err(format!("unknown tuning {other:?}")),
        };
        let value: i32 = words
            .next()
            .and_then(|w| w.parse().ok())
            .ok_or_else(|| format!("{key}: no value"))?;
        device
            .set(index, value)
            .map_err(|e| format!("{key}: {e:?}"))?;
    }
    Ok(())
}

/// Maps a device refusal to the wire's.
fn refusal(error: DeviceError<ErrorKind>) -> Refusal {
    match error {
        DeviceError::Driver(kind) => Refusal::Device(kind),
        DeviceError::Unsupported => Refusal::UnknownControl,
        DeviceError::OutOfRange => Refusal::OutOfRange,
        DeviceError::BufferTooSmall => Refusal::Device(ErrorKind::InvalidInput),
    }
}

impl Service {
    /// Builds the fan controllers of the board: each one's slot (already in
    /// the table) takes its descriptor, and the input and fan slots they name
    /// must exist and carry the channels the board names. Saved tuning is
    /// applied over the board's settings.
    #[allow(clippy::print_stderr)]
    pub(super) fn build_fan_loops(&mut self) -> Result<Vec<FanLoop>, String> {
        let mut loops = Vec::new();
        let composites: Vec<_> = self
            .definition
            .composites
            .iter()
            .filter(|c| c.driver == FAN_CONTROLLER)
            .cloned()
            .collect();
        for composite in &composites {
            let spec = fan_controller::parse(&self.definition, composite)
                .map_err(|problems| problems.join("; "))?;
            let slot_of = |id: &str| self.slots.iter().position(|s| s.id() == id);
            let slot =
                slot_of(&spec.id).ok_or_else(|| format!("composite {:?}: no slot", spec.id))?;
            let fan = slot_of(&spec.fan)
                .ok_or_else(|| format!("composite {:?}: no fan device {:?}", spec.id, spec.fan))?;
            let mut inputs = Vec::new();
            let mut units = Vec::new();
            for input in &spec.inputs {
                let index = slot_of(&input.device).ok_or_else(|| {
                    format!("composite {:?}: no device {:?}", spec.id, input.device)
                })?;
                let (channel, unit) = self.slots[index]
                    .info
                    .and_then(|info| {
                        info.channels
                            .iter()
                            .position(|c| c.name == input.channel)
                            .map(|c| (c, (info.channels[c].quantity, info.channels[c].exponent)))
                    })
                    .ok_or_else(|| {
                        format!(
                            "composite {:?}: device {:?} has no channel {:?}",
                            spec.id, input.device, input.channel
                        )
                    })?;
                inputs.push((index, channel));
                units.push(unit);
            }
            let curves: Vec<(InputSpec, &[Point])> = spec
                .inputs
                .iter()
                .map(|input| {
                    (
                        InputSpec {
                            required: true,
                            max_age_ms: input.max_age_ms,
                        },
                        input.points.as_slice(),
                    )
                })
                .collect();
            let info = leak_info(&spec, &units);
            let mut device = FanDevice::new(info, spec.settings, &curves)
                .map_err(|e| format!("composite {:?}: {e:?}", spec.id))?;
            if let Some(dir) = &self.state_dir
                && let Some(text) = state::load_fan_tuning(dir, &spec.id)
                && let Err(why) = apply_tuning(&mut device, &spec, &text)
            {
                eprintln!("lemnosd: {}: saved tuning not used: {why}", spec.id);
            }

            let slot_state = &mut self.slots[slot];
            slot_state.info = Some(info);
            slot_state.placement = Placement::Composite;
            slot_state.present = true;
            // Its values come from its own steps, never from a read.
            slot_state.next_read_us = u64::MAX;
            slot_state.status = DeviceStatus::Available;

            loops.push(FanLoop {
                spec,
                slot,
                device,
                fan,
                inputs,
                next_ms: 0,
                taking: false,
                retry_at_ms: None,
                stepped_ms: None,
            });
        }
        Ok(loops)
    }

    /// Steps the fan controllers that are due and enabled.
    pub(super) fn fan_tick(&mut self, now_ms: u64) {
        for index in 0..self.fan_loops.len() {
            let fan_loop = &mut self.fan_loops[index];
            if now_ms < fan_loop.next_ms || !fan_loop.device.enabled() {
                continue;
            }
            fan_loop.next_ms = now_ms + u64::from(PERIOD_MS);
            self.fan_step(index, now_ms);
        }
    }

    /// Whether every enabled controller has completed a step lately. The
    /// watchdog is pinged only then (`docs/composite-devices.md`, "Failsafe").
    pub(super) fn fan_stepping(&self, now_ms: u64) -> bool {
        self.fan_loops.iter().all(|l| {
            !l.device.enabled()
                || l.stepped_ms.is_some_and(|at| {
                    now_ms < at.saturating_add(STALL_PERIODS * u64::from(PERIOD_MS))
                })
        })
    }

    fn fan_step(&mut self, index: usize, now_ms: u64) {
        let now_us = self.now_us();
        // On the stack: a step allocates nothing (a controller has at most MAX_INPUTS).
        let mut samples = [Sample::NONE; MAX_INPUTS];
        let inputs = &self.fan_loops[index].inputs;
        let n = inputs.len().min(MAX_INPUTS);
        for (sample, &(slot, channel)) in samples.iter_mut().zip(inputs) {
            let slot = &self.slots[slot];
            if slot.read_us != 0 && slot.values_mask & (1 << channel) != 0 {
                *sample = Sample {
                    value: slot.values[channel],
                    age_ms: (now_us.saturating_sub(slot.read_us) / 1000) as u32,
                    present: true,
                };
            }
        }
        let samples = &samples[..n];
        let fan_loop = &mut self.fan_loops[index];
        fan_loop.stepped_ms = Some(now_ms);
        if fan_loop.retry_at_ms.is_some_and(|at| now_ms < at) {
            return;
        }
        fan_loop.retry_at_ms = None;

        let fan = fan_loop.fan;
        if !fan_loop.taking && self.slots[fan].before_write() {
            self.save_fan_state();
        }
        let fan_loop = &mut self.fan_loops[index];
        let mut out = FanOutput {
            slot: &mut self.slots[fan],
            taking: &mut fan_loop.taking,
        };
        let result = fan_loop.device.step(now_ms, samples, &mut out);
        self.fan_publish(index, now_us);
        if result.is_err() {
            self.fan_fault(index, now_ms);
        }
    }

    /// Puts the controller's channels into its slot and tells subscribers.
    fn fan_publish(&mut self, index: usize, now_us: u64) {
        let slot_index = self.fan_loops[index].slot;
        let device = &mut self.fan_loops[index].device;
        let mut values = [NO_VALUE; MAX_CHANNELS];
        let count = device.info().channels.len();
        let _ = device.read(&mut values[..count]);
        let degraded = device.health() == Health::Degraded;
        let enabled = device.enabled();

        let status = if degraded {
            DeviceStatus::Degraded
        } else {
            DeviceStatus::Available
        };
        let reason = if !enabled {
            "disabled (enabled = 0): the kernel's curve drives the fan"
        } else if degraded {
            "a required input is missing or stale: the fan is at full duty"
        } else {
            ""
        };
        let slot = &mut self.slots[slot_index];
        slot.values = values;
        slot.values_mask = (1u64 << count) - 1;
        slot.read_us = now_us;
        if let Some(status) = slot.set_status(status, None) {
            self.status_event(slot_index, status);
        }
        self.slots[slot_index].reason = reason.to_string();
        self.deliver(slot_index, now_us, now_us);
    }

    /// A failed write: the fan goes back to the kernel and the controller
    /// waits before it takes the fan again.
    #[allow(clippy::print_stderr)]
    fn fan_fault(&mut self, index: usize, now_ms: u64) {
        let fan_loop = &mut self.fan_loops[index];
        let fan = fan_loop.fan;
        let id = fan_loop.spec.id.clone();
        fan_loop.taking = false;
        fan_loop.retry_at_ms = Some(now_ms + RETRY_MS);
        eprintln!("lemnosd: fan controller {id:?} failed to write its fan; handing it back");
        self.slots[fan].restore();
        let slot_index = self.fan_loops[index].slot;
        self.slots[slot_index].reason =
            "the fan write failed: handed back to the kernel, retrying".into();
        if let Some(status) = self.slots[slot_index].set_status(DeviceStatus::Faulted, None) {
            self.status_event(slot_index, status);
        }
    }

    /// A client's write to a fan controller's control (`Request::Set`). The
    /// tuning is the setting, not a lease, so it is kept after the writer
    /// leaves; it is saved to the state directory.
    #[allow(clippy::print_stderr)]
    pub(super) fn fan_set(&mut self, index: usize, ci: usize, raw: i32) -> Result<i32, Refusal> {
        let li = self
            .fan_loops
            .iter()
            .position(|l| l.slot == index)
            .ok_or(Refusal::Unsupported)?;
        let applied = self.fan_loops[li].device.set(ci, raw).map_err(refusal)?;
        if ci == CTL_ENABLED {
            if applied == 0 {
                self.fan_release(li);
            }
            let now_us = self.now_us();
            self.fan_publish(li, now_us);
        }
        if let Some(dir) = &self.state_dir {
            let fan_loop = &self.fan_loops[li];
            let text = tuning_text(&fan_loop.device, &fan_loop.spec);
            if let Err(error) = state::save_fan_tuning(dir, &fan_loop.spec.id, &text) {
                eprintln!("lemnosd: fan tuning {:?}: {error}", fan_loop.spec.id);
            }
        }
        Ok(applied)
    }

    /// A client's read of a fan controller's control.
    pub(super) fn fan_get(&mut self, index: usize, ci: usize) -> Result<i32, Refusal> {
        let li = self
            .fan_loops
            .iter()
            .position(|l| l.slot == index)
            .ok_or(Refusal::Unsupported)?;
        self.fan_loops[li].device.get(ci).map_err(refusal)
    }

    /// Hands the controller's fan back (the controller is disabled).
    fn fan_release(&mut self, index: usize) {
        let fan_loop = &mut self.fan_loops[index];
        let fan = fan_loop.fan;
        let mut out = FanOutput {
            slot: &mut self.slots[fan],
            taking: &mut fan_loop.taking,
        };
        fan_loop.device.release(&mut out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_board::fan_controller::FanInput;

    fn spec() -> FanControllerSpec {
        let points = vec![
            Point {
                value: 50_000,
                duty: 700,
            },
            Point {
                value: 75_000,
                duty: 1000,
            },
        ];
        FanControllerSpec {
            id: "fan-control".into(),
            settings: Settings {
                floor: 700,
                hysteresis: 3_000,
                slew_per_s: 50,
                kick_ms: 0,
            },
            inputs: vec![FanInput {
                role: "cpu".into(),
                device: "cpu-thermal".into(),
                channel: "temperature".into(),
                points,
                max_age_ms: 3_000,
            }],
            fan: "fan".into(),
        }
    }

    fn device(spec: &FanControllerSpec) -> FanDevice {
        let units = [(Quantity::Temperature, -3)];
        let info = leak_info(spec, &units);
        let curves: Vec<(InputSpec, &[Point])> = spec
            .inputs
            .iter()
            .map(|i| {
                (
                    InputSpec {
                        required: true,
                        max_age_ms: i.max_age_ms,
                    },
                    i.points.as_slice(),
                )
            })
            .collect();
        FanDevice::new(info, spec.settings, &curves).unwrap()
    }

    #[test]
    fn a_tuning_text_applies_back_to_the_same_device() {
        let spec = spec();
        let mut tuned = device(&spec);
        tuned.set(CTL_FLOOR, 800).unwrap();
        tuned.set(CTL_ENABLED, 0).unwrap();
        tuned
            .set_curve(
                0,
                &[
                    Point {
                        value: 40_000,
                        duty: 500,
                    },
                    Point {
                        value: 80_000,
                        duty: 1000,
                    },
                ],
            )
            .unwrap();
        let text = tuning_text(&tuned, &spec);
        let mut fresh = device(&spec);
        apply_tuning(&mut fresh, &spec, &text).unwrap();
        assert_eq!(tuning_text(&fresh, &spec), text);
        assert!(!fresh.enabled());
        assert_eq!(fresh.settings().floor, 800);
    }

    #[test]
    fn a_tuning_that_does_not_fit_the_board_is_refused() {
        let spec = spec();
        let mut d = device(&spec);
        assert!(apply_tuning(&mut d, &spec, "curve nope 1 2\n").is_err());
        assert!(apply_tuning(&mut d, &spec, "curve cpu 1 2 3\n").is_err());
        assert!(apply_tuning(&mut d, &spec, "bogus 1\n").is_err());
        assert!(apply_tuning(&mut d, &spec, "floor 2000\n").is_err());
    }

    #[test]
    fn the_descriptor_lists_channels_and_controls_in_the_device_order() {
        let spec = spec();
        let info = leak_info(&spec, &[(Quantity::Temperature, -3)]);
        let names: Vec<&str> = info.controls.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "floor",
                "hysteresis",
                "slew",
                "enabled",
                "curve.cpu.0.value",
                "curve.cpu.0.duty",
                "curve.cpu.1.value",
                "curve.cpu.1.duty"
            ]
        );
        assert_eq!(info.channels[3].name, "input.cpu");
    }
}
