//! The service's fan controllers: each `[[composites]]` `fan-controller` reads
//! its inputs' channels and drives its fan's `duty` control, through the
//! control law in `lemnos_device::fan_control`. The fan's hand-back (set at its
//! first bind) is the failsafe; see `docs/composite-devices.md`, "Failsafe".

use super::Service;
use lemnos_board::fan_controller::{self, FAN_CONTROLLER, FanControllerSpec};
use lemnos_device::fan_control::{FanController, Health, Input, Sample};
use lemnos_drivers_linux::{CONTROL_DUTY, CONTROL_MODE, MODE_MANUAL};

/// How often a controller steps (ms).
const STEP_MS: u64 = 1_000;
/// After a failed write the controller hands the fan back and waits this long
/// before it takes the fan again (ms).
const RETRY_MS: u64 = 10_000;

/// One fan controller built from the board.
pub(crate) struct FanLoop {
    pub spec: FanControllerSpec,
    controller: FanController,
    /// The fan's slot.
    fan: usize,
    /// For each input, its slot and channel index.
    inputs: Vec<(usize, usize)>,
    next_ms: u64,
    /// Whether the fan has been put in manual mode and written to.
    taking: bool,
    /// After a failed write: when the controller may take the fan again.
    retry_at_ms: Option<u64>,
    health: Health,
}

/// The control law's inputs of a fan controller.
fn control_inputs(spec: &FanControllerSpec) -> Vec<Input<'_>> {
    spec.inputs
        .iter()
        .map(|input| Input {
            points: &input.points,
            required: true,
            max_age_ms: input.max_age_ms,
        })
        .collect()
}

impl Service {
    /// Builds the fan controllers of the board: the input and fan slots they
    /// name must exist and carry the channels the board names.
    pub(super) fn build_fan_loops(&mut self) -> Result<Vec<FanLoop>, String> {
        let mut loops = Vec::new();
        for composite in &self.definition.composites {
            if composite.driver != FAN_CONTROLLER {
                continue;
            }
            let spec = fan_controller::parse(&self.definition, composite)
                .map_err(|problems| problems.join("; "))?;
            let slot_of = |id: &str| self.slots.iter().position(|s| s.id() == id);
            let fan = slot_of(&spec.fan)
                .ok_or_else(|| format!("composite {:?}: no fan device {:?}", spec.id, spec.fan))?;
            let mut inputs = Vec::new();
            for input in &spec.inputs {
                let index = slot_of(&input.device).ok_or_else(|| {
                    format!("composite {:?}: no device {:?}", spec.id, input.device)
                })?;
                let channel = self.slots[index]
                    .info
                    .and_then(|info| info.channels.iter().position(|c| c.name == input.channel))
                    .ok_or_else(|| {
                        format!(
                            "composite {:?}: device {:?} has no channel {:?}",
                            spec.id, input.device, input.channel
                        )
                    })?;
                inputs.push((index, channel));
            }
            let controller = FanController::new(spec.settings);
            loops.push(FanLoop {
                spec,
                controller,
                fan,
                inputs,
                next_ms: 0,
                taking: false,
                retry_at_ms: None,
                health: Health::Running,
            });
        }
        Ok(loops)
    }

    /// Steps the fan controllers that are due.
    pub(super) fn fan_tick(&mut self, now_ms: u64) {
        for index in 0..self.fan_loops.len() {
            if now_ms < self.fan_loops[index].next_ms {
                continue;
            }
            self.fan_loops[index].next_ms = now_ms + STEP_MS;
            self.fan_step(index, now_ms);
        }
    }

    #[allow(clippy::print_stderr)]
    fn fan_step(&mut self, index: usize, now_ms: u64) {
        let now_us = self.now_us();
        let samples: Vec<Sample> = self.fan_loops[index]
            .inputs
            .iter()
            .map(|&(slot, channel)| {
                let slot = &self.slots[slot];
                let read = slot.read_us != 0 && slot.values_mask & (1 << channel) != 0;
                if read {
                    Sample {
                        value: slot.values[channel],
                        age_ms: (now_us.saturating_sub(slot.read_us) / 1000) as u32,
                        present: true,
                    }
                } else {
                    Sample::NONE
                }
            })
            .collect();
        let fan_loop = &mut self.fan_loops[index];
        if fan_loop.retry_at_ms.is_some_and(|at| now_ms < at) {
            return;
        }
        fan_loop.retry_at_ms = None;
        let inputs = control_inputs(&fan_loop.spec);
        let decision = fan_loop.controller.step(now_ms, &inputs, &samples);
        let fan = fan_loop.fan;
        if decision.health != fan_loop.health {
            eprintln!(
                "lemnosd: fan controller {:?} is {:?}",
                fan_loop.spec.id, decision.health
            );
            fan_loop.health = decision.health;
        }

        if !self.fan_loops[index].taking {
            if self.slots[fan].before_write() {
                self.save_fan_state();
            }
            let mode = self.slots[fan]
                .device
                .as_mut()
                .map(|d| d.set(CONTROL_MODE, MODE_MANUAL));
            if !matches!(mode, Some(Ok(_))) {
                self.fan_fault(index, now_ms);
                return;
            }
            self.fan_loops[index].taking = true;
        }
        let written = self.slots[fan]
            .device
            .as_mut()
            .map(|d| d.set(CONTROL_DUTY, decision.duty));
        if !matches!(written, Some(Ok(_))) {
            self.fan_fault(index, now_ms);
        }
    }

    /// A failed write: the fan goes back to the kernel and the controller
    /// waits before it takes the fan again.
    #[allow(clippy::print_stderr)]
    fn fan_fault(&mut self, index: usize, now_ms: u64) {
        let fan = self.fan_loops[index].fan;
        let id = self.fan_loops[index].spec.id.clone();
        eprintln!("lemnosd: fan controller {id:?} failed to write its fan; handing it back");
        self.slots[fan].restore();
        let fan_loop = &mut self.fan_loops[index];
        fan_loop.taking = false;
        fan_loop.retry_at_ms = Some(now_ms + RETRY_MS);
    }
}
