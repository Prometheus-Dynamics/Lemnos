# Composite devices

Status: **the fan controller is built as a composite device** and run on board #1 under the
lemnosd lock (see "Built" below). The `Composite` trait is in `lemnos-device`, with the fan
controller as its first implementation. Its listing, live tuning and persisted tuning are built.
The other uses below are still design, and so are `override` and `lemnos-ctl fan reset`.
[system-service.md](system-service.md) leaves room for it in `lemnosd`. Builds on
[compact-model.md](compact-model.md) and [board-definition.md](board-definition.md).

## Built

- **The control law** (`crates/lemnos-device/src/fan_control.rs`, `no_std`, no allocation): a
  curve per input (piecewise linear, clamped; `check_curve` refuses unordered, out-of-range or
  too many points), the demand as the maximum over inputs, the floor, hysteresis (a peak is held
  until the value has fallen the band), the slew limit on the way down (rising is immediate),
  the spin-up kick (only with floor 0), and staleness (a missing or old required input forces
  full duty and `Health::Degraded`). Host-tested (9 tests).
- **The board entry** (`crates/lemnos-board/src/fan_controller.rs`): a top-level
  `[[composites]]` table, separate from `[[devices]]` because the controller has no hardware
  driver. Validation reports every problem at once: unknown driver, duplicate id, unknown config
  key, input not `<device>.<channel>`, input device missing, curve invalid, outputs not exactly
  `{ fan = <device> }`, and a fan whose `writers` do not admit `lemnosd.fan`.
- **The composite device** (`crates/lemnos-device/src/composite.rs`, `fan_device.rs`):
  the `Composite` trait (`no_std`) and `FanDevice`, which is the controller as a device:
  `Sensor` (channels), `Control` (tuning) and `Composite` (inputs, outputs, `step`, `release`).
  Its layout is fixed (`fan_device` documents the channel and control order).
- **The host** (`crates/lemnosd/src/service_fan.rs`): the controller is a slot in the device
  table, so `lemnos-ctl list` shows it with its status (`degraded` and a reason while a
  required input is missing or stale, `faulted` after a failed write, `available` otherwise)
  and its channels and controls. Once per second per enabled controller it reads each input's
  latest channel value and its age, steps the device, and writes the fan's `duty` (output 0).
  On its first step it sets `pwm_mode` to manual and records the fan's hand-back
  (`before_write`), so `ExecStopPost` and a clean stop return the fan to the kernel. A failed
  write hands the fan back and waits 10 s before taking it again.
- **Live tuning and persistence:** a client's `set` on a controller control is applied at once
  and saved to `<LEMNOSD_STATE_DIR>/fan/<id>.tuning` (text, one line per setting and one per
  curve); a saved tuning is applied over the board's at start. A curve keeps its point count
  (set by the board) and stays ascending: a point that would cross its neighbour is refused.
  Tuning writes are settings, not a client's lease, so they survive the writer.
- **Watchdog:** lemnosd pings `WATCHDOG=1` only while every enabled controller has completed a
  step in the last three periods (`fan_stepping`).
- **Units in the listing:** the input's channels carry the input's unit; the curve controls
  carry the input's quantity and exponent, so `lemnos-ctl set fan-control curve.cpu.0.duty 0.9`
  takes a fraction. Hysteresis takes the first input's unit.

```toml
[[composites]]
id = "fan-control"
driver = "fan-controller"
config = { input_cpu = "cpu-thermal.temperature", output_fan = "fan", floor = 300, hysteresis = 3000, slew = 50, kick_ms = 0, max_age_ms = 3000, curve_cpu = [[30000, 300], [60000, 600], [75000, 1000]] }
```

The entry is a device table, so it reuses the device deserializer. `input_<role>` is `<device>.<channel>`,
`output_fan` is the fan device, and `curve_<role>` is the input's curve; the scalars are the other keys.

Units: duties are per-mille; `curve_<role>` values are in the input's unit (millidegrees for a
thermal zone), so `50000` is 50 °C. An `input_<role>` without its `curve_<role>` uses the default
curve `[[30000, 300], [60000, 600], [75000, 1000]]`.

**Not built yet:** `override` (a fixed duty with a timeout), `lemnos-ctl fan reset` (clearing
the saved tuning; delete `<state>/fan/<id>.tuning` and restart until then), a composite in the
runtime (it is a `lemnosd` device; the runtime has no inter-device data path), the other uses
below, and `kick_ms` as a control (it stays a board setting).

**Deviations from the trait sketch above, as built:** `inputs()` returns `InputSpec { required,
max_age_ms }` and `outputs()` returns `OutputSpec { per_mille }`: the roles and the units are the
host's (the board's `input_<role>`), not the device's, and `Outputs::set` takes the output's
index. `Outputs::set` and `release` take the output's index and return `ErrorKind`, and
`Composite::step` returns `DeviceError<ErrorKind>`, so a failed write reaches the host as an
error. `Sample` is the one in `lemnos_device::composite`. The `[[composites]]` keys are the ones
in the example below, not the `inputs`/`outputs`/`curve.cpu` keys of the first sketch.

## What a composite device is

A composite (virtual) device is a device-model device whose channels and controls are
computed from other devices instead of read from hardware:

- **inputs** are channels of other devices (a thermal zone's `temperature`, a power
  monitor's `power`, an IMU's `angular_rate.x`);
- **outputs** are controls of other devices it drives (a fan's `duty`), if any;
- its **own channels** report what it computed (a target duty, an orientation, a power
  headroom), and its **own controls** are its tuning (curve points, gains, a floor).

To clients it is an ordinary L1 device: it has a `DeviceInfo` (class `Fan`,
`Orientation`, `PowerMonitor`, ...), it is read with `Sensor::read` and tuned with
`Control::set`, and it shows up in `lemnosd`, in the runtime and in `lemnos-lite` tables
like any other device. What is new is that a host must feed it inputs and carry out its
outputs on a schedule.

## Model

One more L1 trait, `no_std` like the rest of `lemnos-device`:

```rust
/// A device computed from other devices' channels.
pub trait Composite: Device {
    /// The input channels it reads, by role ("cpu", "power"); resolved by the
    /// host to (device, channel) pairs when it is built.
    fn inputs(&self) -> &'static [InputSpec];
    /// The output controls it drives, by role ("fan").
    fn outputs(&self) -> &'static [OutputSpec];
    /// How often the host calls `step`.
    fn period_ms(&self) -> u32;

    /// Computes one step. `inputs[i]` is the latest value of input `i` and
    /// its age; `NO_VALUE` when the input has never been read or failed.
    /// Writes to outputs go through `out`, which applies the host's write
    /// policy and records failures.
    fn step(&mut self, now_ms: u64, inputs: &[Sample], out: &mut dyn Outputs)
        -> Result<(), DeviceError<Self::Error>>;

    /// Called when the host stops the composite (shutdown, reconfiguration,
    /// a fault it cannot recover from): hand outputs back to their failsafe.
    fn release(&mut self, out: &mut dyn Outputs);
}

pub struct Sample { pub value: i32, pub age_ms: u32 }
```

- `InputSpec { role, quantity, required, max_age_ms }` and `OutputSpec { role, quantity }`
  are static data, like channels. The quantity lets the host check that the configured
  input (`cpu-thermal.temperature`) has the expected quantity and convert exponents.
- The host owns scheduling: it reads each input device at least as often as the composite
  needs, keeps the latest sample and its age, calls `step` every `period_ms`, and calls
  `release` on every path that stops the composite.
- `Outputs::set(role, control, value)` is the only way a composite touches another device.
  The host applies the same write policy as for clients, with the composite as a named
  writer (`lemnosd.fan`), so a board definition can say who may drive the fan.

A composite is configured in the board definition like any device, with two more keys:

```toml
[[devices]]
id = "fan-control"
driver = "fan-controller"
inputs = { cpu = "cpu-thermal.temperature", board = "power.power" }
outputs = { fan = "fan" }
config = { floor = 0.70, hysteresis = 3.0, curve.cpu = [[50.0, 0.70], [65.0, 0.85], [75.0, 1.0]] }
```

`inputs` and `outputs` map roles to `<device>.<channel>` and `<device>`; validation checks
that the referenced devices exist, the quantities match, and outputs are controls of a
device that lists the composite in its `writers`.

## The fan controller

The motivating case: replace the kernel's pwm-fan curve with one that knows more (several
temperatures, board power, workload), can be tuned live, and keeps a high floor (the example board
wants at least 70 %).

### Inputs

Any number of channels with a temperature, power or workload quantity, each with its own
curve:

- thermal zones (`cpu-thermal.temperature`), die temperatures (`power.die_temperature`,
  IMU temperature);
- power (`power.power`): heat that has not reached a sensor yet;
- workload: a `system-load` device (CPU utilisation from `/proc/stat`, a Linux L1 device in
  `lemnos-drivers-linux`), or a hint a client sets (a client raising "vision pipeline
  running").

Each input's curve maps its value to a duty; the controller's demand is the **maximum** over
inputs, so the hottest source wins and adding inputs can only make cooling more aggressive.

### Curve, floor, hysteresis

- **Curve:** piecewise-linear points `(value, duty)` per input, interpolated, clamped at
  both ends.
- **Floor and ceiling:** `duty = clamp(demand, floor, 1.0)`. The floor is a hard minimum (70
  % on the example board); the fan never stops while the controller runs.
- **Hysteresis:** the curve is evaluated on the rising value; when an input falls, it is
  evaluated at `value + hysteresis` until it has fallen by the band, so the fan does not
  hunt around a breakpoint.
- **Slew limit:** the duty changes by at most `slew` per second downwards (default 5 %/s)
  and without limit upwards: cooling up is immediate, cooling down is gentle and quiet.
- **Spin-up:** starting from a stopped fan (only when the floor is 0), command 100 % for
  `kick_ms` before the target, so the fan starts reliably at low duties.
- **Staleness:** a required input older than its `max_age_ms` (or `NO_VALUE`) forces the
  duty to 100 % and marks the device `degraded`; it never lowers cooling because a sensor
  disappeared.

### Live tuning

The controller's controls are its parameters: `floor`, `hysteresis`, `slew` (per-mille per
second), `enabled` (0 hands the fan back to the kernel and stops the controller until it is set
to 1), and the curve points (`curve.<role>.<n>.value` in the input's unit, `curve.<role>.<n>.duty`
per-mille). They are set like any control, and the write policy applies. Each change is saved in
the state directory (see "Built"), so a restart keeps it. Its channels report `demand` (the curve
result), `duty` (what was written), `source` (which input won) and `input.<role>` (the values it
used).

### Output

To a hwmon fan (`hwmon-fan`): it writes `pwm_mode = 1` (manual) when it starts, then
`duty` every step. Also possible: a raw sysfs PWM channel, or a fan on a microcontroller
reached through `lemnos-lite` and the optional bridge.

### Failsafe

The kernel's pwm-fan curve stays configured (the device tree's cooling maps) and is the
fallback. The controller only ever *borrows* the fan:

1. **Taking control** is writing `pwm1`. **Giving it back** is the fan hand-back
   (`FanRestore`, see [system-service.md](system-service.md#fan-hand-back)). On the
   example board's `pwm-fan` (measured on the BCM2712 board) there is no automatic `pwm1_enable` mode: the
   hand-back restores the bind-time `pwm1_enable` (1), restores the cooling device's
   governor state from before the controller's first write, and makes the bound zones'
   governor re-evaluate. Chips with an automatic mode get `restore_mode`
   (default 2).
2. **Clean stop** (SIGTERM, reconfiguration, `enabled = false`): `release` restores the
   mode before the process exits.
3. **Controller fault** (an output write fails, required inputs stale beyond a grace
   period): restore the kernel mode and mark the device `faulted`; retry taking control
   only after the inputs recover for a while.
4. **Panic:** a panic hook restores the mode before aborting.
5. **Crash, `kill -9`, missed watchdog:** no code of the process runs, so systemd restores
   the mode: `lemnosd.service` has
   `ExecStopPost=+/usr/bin/lemnos-ctl fan restore --all` (from the plans `lemnosd`
   recorded at bind), which systemd runs after the main process exits for any reason,
   including SIGKILL and the watchdog's SIGABRT. `Restart=always` then brings the
   controller back.
6. **Watchdog:** `lemnosd` sends `WATCHDOG=1` only while the control loop keeps completing
   steps (inputs fresh, writes succeeding). A hung loop stops the pings; systemd kills the
   service, `ExecStopPost` restores the kernel curve, and the unit restarts.
7. **Firmware:** independently of all of the above, the CPU firmware throttles at 85 °C,
   and the device tree's critical trip shuts the board down. The controller's curves must
   reach 100 % well below that (75 °C on the example board), so throttling is never the cooling
   policy.

The result: at any moment either the controller is alive and stepping, or the kernel's
curve is in charge, or the firmware throttles. There is no state in which nobody drives the
fan.

## Other uses

- **Fused IMU orientation.** Inputs: the BMI088's acceleration and angular rate, the
  BMM150's field. A Madgwick or Mahony filter at the IMU's rate produces an `Orientation`
  device with `quaternion.{w,x,y,z}` (and `roll`, `pitch`, `yaw` in rad). Controls: the
  filter gain, magnetometer enable, hard- and soft-iron calibration. No outputs. Clients
  (a pose estimator, a vision pipeline's camera transforms) read one device instead of
  fusing in each application.
- **Board power budget.** Inputs: the INA238's `power` and per-rail estimates; output
  channels `budget`, `headroom`, and an event when headroom goes negative. Outputs could
  cap a load's draw (limit the fan's ceiling, ask the camera service for a lower frame rate through a
  client hint), or only report.
- **Status aggregation.** Inputs: several devices' statuses; output: one LED intent (the
  worst status wins), so the status LED reflects the board without each application
  arbitrating.
- **Derived sensors.** Unit conversions, averages, a "case temperature" estimated from
  several sensors.

## Where it would run

- **`lemnosd`** (primary): composites are devices in its table; its poll loop already
  reads inputs on schedules and owns the write policy and the systemd watchdog. See
  [system-service.md](system-service.md).
- **`lemnos-lite`**: the trait is `no_std`, so a microcontroller can run the same fan
  controller against its own sensors; the table gains a `step` pass.
- **The runtime (L3):** possible, but the runtime has no inter-device data path today; a
  composite would need read access to other bound devices' latest states. Not needed while
  `lemnosd` hosts the hardware.

## Decided

- Built: the fan controller first, as the proposal says.
- `max` over the inputs (as proposed).
- Tuned parameters persist in `lemnosd`'s state directory, one text file per controller
  (`<state>/fan/<id>.tuning`), not in an overrides file. The board definition is never changed.

## Board test (round 4, board #1)

Under `/tmp/lemnos-r3-lemnosd.lock`, with the unit stopped and a temporary `lemnosd` (the
service's own build, the example board plus the `[[composites]]` entry, state in `/tmp`):

- Listing: `fan-control  fan  fan-controller  degraded` at the first step (no input read yet,
  so full duty), then `available`, with the ten controls and four channels.
- Live: setting `curve.cpu.0.duty` to 0.9 (the floor is 0.7, `pwm1` 179) put `pwm1` at 230 within
  three seconds; a point set out of order is refused (`value out of range`).
- Restart: the 0.9 point was back after a stop and a start, with the same `pwm1` 230 as before.
- Restore: `pwm1` 179, `pwm1_enable` 1 and `cur_state` 0 before and after, from the stop helper.

