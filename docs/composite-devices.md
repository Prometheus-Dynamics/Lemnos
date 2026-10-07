# Composite devices

Status: **design only, not implemented.** The user has not decided whether to build this;
nothing in the code depends on it. [system-service.md](system-service.md) leaves room for it
in `lemnosd`. Builds on [compact-model.md](compact-model.md) and
[board-definition.md](board-definition.md).

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
config = { floor = 0.70, hysteresis = 3.0, curve.cpu = [[50.0, 0.70], [65.0, 0.85], [75.0, 1.0]], restore_mode = 2 }
```

`inputs` and `outputs` map roles to `<device>.<channel>` and `<device>`; validation checks
that the referenced devices exist, the quantities match, and outputs are controls of a
device that lists the composite in its `writers`.

## The fan controller

The motivating case: replace the kernel's pwm-fan curve with one that knows more (several
temperatures, board power, workload), can be tuned live, and keeps a high floor (the Raze
wants at least 70 %).

### Inputs

Any number of channels with a temperature, power or workload quantity, each with its own
curve:

- thermal zones (`cpu-thermal.temperature`), die temperatures (`power.die_temperature`,
  IMU temperature);
- power (`power.power`): heat that has not reached a sensor yet;
- workload: a `system-load` device (CPU utilisation from `/proc/stat`, a Linux L1 device in
  `lemnos-drivers-linux`), or a hint a client sets (HeliOS raising "vision pipeline
  running").

Each input's curve maps its value to a duty; the controller's demand is the **maximum** over
inputs, so the hottest source wins and adding inputs can only make cooling more aggressive.

### Curve, floor, hysteresis

- **Curve:** piecewise-linear points `(value, duty)` per input, interpolated, clamped at
  both ends.
- **Floor and ceiling:** `duty = clamp(demand, floor, 1.0)`. The floor is a hard minimum (70
  % on the Raze); the fan never stops while the controller runs.
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

The controller's controls are its parameters: `floor`, `hysteresis`, `slew`, `enabled`,
`override` (a fixed duty for testing, with a timeout after which it reverts), and the curve
points (`curve.cpu.0.value`, `curve.cpu.0.duty`, ...). They are set like any control,
subject to the write policy. Tuned values live in memory; a client that wants them kept
writes them back into the board definition (or `lemnosd` keeps a small overrides file in
its state directory, `/var/lib/lemnosd/overrides.toml`, applied over the board definition
at start). Its channels report `demand` (the curve result), `duty` (what was written),
`input.<role>` (the values it used) and `source` (which input won).

### Output

To a hwmon fan (`hwmon-fan`): it writes `pwm_mode = 1` (manual) when it starts, then
`duty` every step. Also possible: a raw sysfs PWM channel, or a fan on a microcontroller
reached through `lemnos-lite` and the optional bridge.

### Failsafe

The kernel's pwm-fan curve stays configured (the device tree's cooling maps) and is the
fallback. The controller only ever *borrows* the fan:

1. **Taking control** is `pwm1_enable = 1`. **Giving it back** is `pwm1_enable = 2` (the
   kernel's automatic control) or the platform's equivalent, set per board as
   `restore_mode` because hwmon drivers differ (`HwmonFan::restore_automatic` writes 2).
   Verify on the CM5 kernel that 2 hands `pwm1` back to the thermal cooling device.
2. **Clean stop** (SIGTERM, reconfiguration, `enabled = false`): `release` restores the
   mode before the process exits.
3. **Controller fault** (an output write fails, required inputs stale beyond a grace
   period): restore the kernel mode and mark the device `faulted`; retry taking control
   only after the inputs recover for a while.
4. **Panic:** a panic hook restores the mode before aborting.
5. **Crash, `kill -9`, missed watchdog:** no code of the process runs, so systemd restores
   the mode: `lemnosd.service` has
   `ExecStopPost=/usr/bin/lemnos-ctl fan restore --all` (also a one-line `echo 2 >
   pwm1_enable` fallback), which systemd runs after the main process exits for any reason,
   including SIGKILL and the watchdog's SIGABRT. `Restart=always` then brings the
   controller back.
6. **Watchdog:** `lemnosd` sends `WATCHDOG=1` only while the control loop keeps completing
   steps (inputs fresh, writes succeeding). A hung loop stops the pings; systemd kills the
   service, `ExecStopPost` restores the kernel curve, and the unit restarts.
7. **Firmware:** independently of all of the above, the CPU firmware throttles at 85 °C,
   and the device tree's critical trip shuts the board down. The controller's curves must
   reach 100 % well below that (75 °C on the Raze), so throttling is never the cooling
   policy.

The result: at any moment either the controller is alive and stepping, or the kernel's
curve is in charge, or the firmware throttles. There is no state in which nobody drives the
fan.

## Other uses

- **Fused IMU orientation.** Inputs: the BMI088's acceleration and angular rate, the
  BMM150's field. A Madgwick or Mahony filter at the IMU's rate produces an `Orientation`
  device with `quaternion.{w,x,y,z}` (and `roll`, `pitch`, `yaw` in rad). Controls: the
  filter gain, magnetometer enable, hard- and soft-iron calibration. No outputs. Clients
  (HeliOS pose, PhotonVision's robot-to-camera transforms) read one device instead of
  fusing in each application.
- **Board power budget.** Inputs: the INA238's `power` and per-rail estimates; output
  channels `budget`, `headroom`, and an event when headroom goes negative. Outputs could
  cap a load's draw (limit the fan's ceiling, ask Styx for a lower frame rate through a
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

## To decide

- Whether to build it at all, and the fan controller first.
- `max` versus weighted combination of inputs (proposed: `max`).
- Whether tuned parameters persist in `lemnosd`'s state directory or only in the board
  definition (proposed: an overrides file, cleared by `lemnos-ctl fan reset`).
- The restore value for the Raze's fan driver, measured on the CM5.
