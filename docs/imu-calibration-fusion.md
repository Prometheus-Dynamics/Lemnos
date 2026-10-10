# IMU and magnetometer calibration, and orientation fusion

Status: designed here, implemented in phases. Builds on [compact-model.md](compact-model.md),
[board-definition.md](board-definition.md) and [system-service.md](system-service.md).

The goal: a board's IMU and magnetometer give **raw** and **calibrated** numbers with no
user action, and a **fused orientation** (quaternion, roll, pitch, yaw, gravity, linear
acceleration) is available whenever something asks for it. Calibration needs no stillness
(it is estimated while the board moves); Atlas can force a guided, more precise routine.

## Layers

```
lemnos-fusion      no_std, no alloc: filters, ellipsoid fits, calibrators, routines, words
   ▲                 (f32; no device-model or bus knowledge)
   │
lemnos-device      calibration contract: CalibrationStatus, CalibrationCommand, persisted words
   ▲                 (Sensor default methods; DynSensor / BoxedDevice forms)
   │
lemnos-drivers-bmi088, lemnos-drivers-bmm150
                   the drivers run the calibrators in their read path and expose raw + cal channels
   ▲
   │
lemnosd            hosts the fusion device (driver "fusion"), persists calibration per device,
                   routes samples IMU -> fusion, serves calibration over IPC / lemnos-ctl
```

The driver owns calibration: it estimates, applies and exposes it. lemnosd only starts and
stops routines, persists the applied words, and feeds the fusion device.

## Channels

Existing channel names and indices are kept; calibrated channels are appended. A client that
subscribes `acceleration.*` or `angular_rate.*` gets the same values as before (the prefix match
needs a `.` after the name, so `acceleration_cal.*` is not included).

| Device (`driver`) | Index | Channel | Quantity, unit | Meaning |
|---|---|---|---|---|
| `imu` (`bmi088`) | 0-2 | `acceleration.{x,y,z}` | m/s² (exp -3) | raw, unchanged |
| | 3-5 | `angular_rate.{x,y,z}` | rad/s (exp -6) | raw, unchanged |
| | 6-8 | `acceleration_cal.{x,y,z}` | m/s² (exp -3) | offset and scale applied |
| | 9-11 | `angular_rate_cal.{x,y,z}` | rad/s (exp -6) | zero-rate offset removed |
| `magnetometer` (`bmm150`) | 0-2 | `magnetic_field.{x,y,z}` | T (exp -9) | raw, unchanged |
| | 3-5 | `magnetic_field_cal.{x,y,z}` | T (exp -9) | hard- and soft-iron applied |
| `orientation` (`fusion`) | 0-3 | `quaternion.{w,x,y,z}` | ratio (exp -6) | body orientation, world from sensor |
| | 4-6 | `roll`, `pitch`, `yaw` | rad (exp -6) | yaw is 0 at magnetic north only with a magnetometer (see below) |
| | 7-9 | `gravity.{x,y,z}` | m/s² (exp -3) | gravity in the body frame |
| | 10-12 | `linear_acceleration.{x,y,z}` | m/s² (exp -3) | calibrated acceleration minus gravity |
| | 13 | `magnetic_disturbance` | level | 1 while the magnetometer is ignored (disturbed or not calibrated) |
| | 14 | `confidence.imu` | ratio (exp -3) | the IMU's accelerometer and gyro confidence (the lower of the two) |
| | 15 | `confidence.magnetometer` | ratio (exp -3) | the magnetometer's confidence |

`NO_VALUE` marks a channel with no value (a fusion output before its first sample, or a
calibrated channel with the calibration feature off).

Quaternions use the Hamilton convention, scalar first, and rotate **body-frame** vectors into the
**world** frame. World is east-north-up: `yaw` is 0 pointing magnetic north when the 9-axis
mode has a calibrated magnetometer (`declination_deg` shifts it to true north), and it is only
relative with the 6-axis mode.

## lemnos-fusion (no_std crate, no allocation)

Plain `f32` arithmetic (`[f32; 3]` vectors, `Quat`, `[[f32; 3]; 3]` matrices). Every
estimator is a plain struct with fixed-size state and no heap.

### Orientation filter

- `Algorithm::Mahony` (default) and `Algorithm::Madgwick` (gradient descent, `beta`).
- `Mode::SixAxis` (accelerometer and gyroscope) and `Mode::NineAxis` (adds the magnetometer).
- Mahony's integral term estimates the **gyroscope bias online**, from the attitude error.
  This needs no stillness: it converges while the board moves against gravity (roll and pitch).
  Its estimate is `Output::gyro_bias`.
- **Variable dt:** each update takes its `dt` from the sample timestamps (`t_us: u64`, boot
  clock). `dt` is clamped to `(0, 0.1]` s. A longer gap (no samples for over 0.5 s) only
  re-anchors the clock: the attitude is kept and the gyro is not integrated across the gap.
- **Accelerometer trust:** the accelerometer's correction is weighted by how close `|a|` is to
  gravity: full weight within ±10 %, zero beyond ±50 %, linear between. Dynamic motion therefore
  does not pull the attitude.
- **Magnetic disturbance:** the magnetometer is ignored for an update when
  - it is not trusted (the caller passes `trusted = false` when its confidence is low, which is
    the case before any calibration), or
  - `|B|` differs from the calibrated field magnitude by more than 15 %, or
  - its dip angle (the angle of the field below horizontal, from the accelerometer's gravity
    direction) differs from the learned or configured reference by more than 10°.
  The flag is `Output::magnetic_disturbance`. Magnetometer data older than 0.5 s counts as
  absent (the output then comes from the six-axis update).
- **Mounting:** `OrientationConfig::mount` is a fixed rotation from the sensor frame to the
  robot (body) frame, given as roll, pitch, yaw in degrees (intrinsic ZYX). The output
  orientation is `q_world_body = q_world_sensor ⊗ q_sensor_body⁻¹`; `gravity` and
  `linear_acceleration` are reported in the body frame too.
- `Output::yaw` is the body's heading (ZYX Tait-Bryan). Without the magnetometer the yaw is
  relative: the gyroscope's z-axis bias is not observable from gravity, so it drifts unless a
  zero-rate estimate (from the calibrator, below) has removed it. Only the 9-axis mode gives a
  yaw referenced to the world.

### Ellipsoid calibration (accelerometer and magnetometer)

Both sensors are fitted with the same model: a sensor's samples lie on an ellipsoid
`(x - c)ᵀ Q (x - c) = 1`, so `corrected = r̄ · Q^½ (x - c)` maps them to a sphere of radius `r̄`.
This covers offset, per-axis scale and cross-axis (soft iron) at once.

- `EllipsoidFit`: accumulates the 9×9 normal equations of the linear fit `xᵀAx + bᵀx = 1`, with
  an exponential forgetting factor (so an old field fades), and occupancy counts over the
  sphere's 26 direction cells (the 3×3×3 cube's cells other than the centre, classified from the
  sample's direction relative to the current centre). `coverage()` is the share of cells that
  hold at least `min_per_cell` samples.
- `fit()` solves the normal equations (with a small ridge term for conditioning, on samples
  normalised by a reference magnitude), recovers `c`, `Q`, and `Q^½` by a Jacobi
  eigendecomposition of the 3×3 matrix, and returns the `Ellipsoid`, its residual (RMS of
  `|corrected| - r̄`, relative to `r̄`) and its coverage.
- `Ellipsoid::IDENTITY` is no correction. The applied ellipsoid is the last one accepted.

### Automatic calibration (no stillness)

`ImuCalibrator` (accelerometer, gyroscope) and `MagCalibrator` run on every sample the driver
reads, with bounded cost per sample.

- **Gyroscope zero-rate:** a window of about 1 s (or 100 samples) with a gyro standard deviation
  under 0.5 °/s and `|a|` within 3 % of gravity yields a candidate offset (the window mean). The
  candidate blends into the applied offset with weight `min(0.2, samples_in_window / 500)`, so
  one still window never moves it far. Confidence rises with the still time accumulated
  (full at 60 s). The fusion's integral term refines the residual while moving; neither is
  required.
- **Accelerometer:** quasi-static samples (`|ω| < 0.05 rad/s` and `|a|` within 5 % of gravity,
  held for the window above) are fed to an `EllipsoidFit` with reference `g = 9.80665 m/s²`.
  The fit is applied when its coverage is at least 0.6, its residual at most 2 %, and it has at
  least 300 samples. Application is rate-limited: each accepted fit moves the applied correction
  at most 25 % of the way (blended per parameter), and a fit whose centre moves by more than
  0.5 g is rejected as an outlier.
- **Magnetometer:** every sample feeds a `MagCalibrator` (an `EllipsoidFit` in µT, forgetting
  factor 0.999 per sample, so about 1000 samples are remembered). Applied under the same
  thresholds, and rate-limited the same way. A raw field outside 15-80 µT is not accepted (it is
  not an Earth field).
- **Confidence** of a part, 0 to 1000 permille: the minimum of three scores, each 1 at or above
  its threshold and falling linearly to 0: coverage (threshold 0.6), residual (threshold 2 %),
  and samples (threshold 300). The part is `active` once its confidence is at least 500.
- The applied calibration changes `CalibrationStatus::revision`; lemnosd persists it.

### Forced routines (Atlas)

`Routine` is a state machine over the same estimators. It collects **candidates**, never the
applied calibration.

| Routine | Procedure | Done when | Result |
|---|---|---|---|
| `AccelSix` | Hold still on each face. A face counts when its gravity direction is within 20° of ±X, ±Y or ±Z and it is quasi-static for 1 s. | 6 faces with 100 samples each, and the fit passes its thresholds | accelerometer candidate |
| `MagRotate` | Rotate through as many orientations as possible. | coverage ≥ 0.9, residual ≤ 3 %, ≥ 500 samples | magnetometer candidate |
| `GyroHold` | Hold still. | 5 s of still windows | gyroscope candidate |

Each routine has a timeout (`AccelSix` 180 s, `MagRotate` 180 s, `GyroHold` 30 s). A timeout, or
a fit that misses its thresholds, ends with `failed = true` and no candidate. `Stop` ends a
routine and keeps any finished candidate. `Apply` makes the candidate applied (and the candidate
goes into the applied state with confidence set to the candidate's); `Discard` drops it;
`Reset` returns every part to factory state.

### Words (persistence)

`words::encode` and `words::decode` write and read the applied state as `i32` words (at most
`MAX_CALIBRATION_WORDS = 64`). Word 0 is the layout version (`1`); word 1 is the part kind (the
IMU and the magnetometer have different layouts, so a word set of one kind does not load into
the other). Values are fixed-point: offsets in µm/s², µrad/s or nT, matrix entries ×10⁶,
confidences in permille, sample counts exact. A layout of another version is refused; the device
then runs with factory calibration and lemnosd logs why.

### Public API (the contract other crates code against)

```rust
pub type Vec3 = [f32; 3];
pub const STANDARD_GRAVITY: f32 = 9.80665;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quat { pub w: f32, pub x: f32, pub y: f32, pub z: f32 }
impl Quat { pub const IDENTITY: Quat; pub fn from_euler_deg(roll: f32, pitch: f32, yaw: f32) -> Quat; pub fn mul(self, o: Quat) -> Quat; pub fn conj(self) -> Quat; pub fn rotate(self, v: Vec3) -> Vec3; pub fn euler(self) -> (f32, f32, f32) /* roll, pitch, yaw */ }

pub enum Algorithm { Mahony, Madgwick }
pub enum Mode { SixAxis, NineAxis }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrientationConfig {
    pub algorithm: Algorithm,          // Mahony
    pub mode: Mode,                    // SixAxis
    pub kp: f32,                       // 1.0 (Mahony proportional gain)
    pub ki: f32,                       // 0.01 (Mahony integral gain)
    pub beta: f32,                     // 0.1 (Madgwick)
    pub mount: Quat,                   // sensor -> body; Quat::IDENTITY default
    pub declination_rad: f32,          // 0.0
    pub dip_rad: Option<f32>,          // None: learned
}
impl Default for OrientationConfig {..}

pub struct Orientation { /* fixed state */ }
impl Orientation {
    pub fn new(config: OrientationConfig) -> Self;
    pub fn reset(&mut self);
    /// Calibrated inputs: gyro rad/s, accel m/s² (body frame of the sensor).
    pub fn update_imu(&mut self, t_us: u64, gyro: Vec3, accel: Vec3);
    /// Calibrated field in µT; `trusted` is false while the magnetometer's calibration is not applied.
    pub fn update_mag(&mut self, t_us: u64, field_ut: Vec3, trusted: bool);
    pub fn output(&self) -> Output;
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Output {
    pub valid: bool,                   // at least one IMU update
    pub quaternion: Quat,              // world <- body (mount applied)
    pub roll: f32, pub pitch: f32, pub yaw: f32,   // rad
    pub gravity: Vec3,                 // m/s², body frame
    pub linear_acceleration: Vec3,     // m/s², body frame
    pub gyro_bias: Vec3,               // rad/s, the integral term's estimate (sensor frame)
    pub magnetic_disturbance: bool,    // mag ignored now (disturbed, untrusted, or absent)
}

// Calibration
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ellipsoid { pub center: Vec3, pub matrix: [[f32; 3]; 3], pub radius: f32 }
impl Ellipsoid { pub const IDENTITY: Ellipsoid; pub fn apply(&self, raw: Vec3) -> Vec3; }
pub struct EllipsoidFit { /* fixed state */ }
impl EllipsoidFit { pub fn new(reference: f32, forgetting: f32, min_per_cell: u16) -> Self; pub fn add(&mut self, sample: Vec3); pub fn samples(&self) -> u32; pub fn coverage(&self) -> f32; pub fn fit(&self) -> Option<Fit>; pub fn clear(&mut self); }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit { pub ellipsoid: Ellipsoid, pub residual: f32, pub coverage: f32, pub samples: u32 }

pub struct ImuCalibrator { /* accel + gyro estimators, applied state */ }
impl ImuCalibrator {
    pub fn new() -> Self;
    /// One sample (raw counts already converted to SI units).
    pub fn push(&mut self, accel: Vec3, gyro: Vec3) -> Changed;   // Changed: bool-like flag: applied state changed
    pub fn accel(&self) -> Ellipsoid;  pub fn gyro_bias(&self) -> Vec3;
    pub fn status(&self) -> PartStatus (accel), PartStatus (gyro)   // confidence/coverage/residual/samples/active, all permille
    pub fn reset(&mut self);
    pub fn words(&self, out: &mut [i32]) -> usize; pub fn load(&mut self, words: &[i32]) -> Result<(), WordsError>;
    // Forced routines:
    pub fn start(&mut self, routine: Routine); pub fn stop(&mut self); pub fn apply(&mut self) -> bool; pub fn discard(&mut self);
    pub fn running(&self) -> Option<Routine>; pub fn progress_permille(&self) -> u16; pub fn candidate(&self) -> bool; pub fn failed(&self) -> bool;
}
pub struct MagCalibrator { /* ... */ }
impl MagCalibrator { pub fn new() -> Self; pub fn push(&mut self, field_ut: Vec3) -> Changed; pub fn field(&self) -> Ellipsoid; pub fn status(&self) -> PartStatus; pub fn reset(&mut self); pub fn words(&self, out: &mut [i32]) -> usize; pub fn load(&mut self, words: &[i32]) -> Result<(), WordsError>; /* routines as ImuCalibrator, Routine::MagRotate */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum Routine { AccelSix, MagRotate, GyroHold }
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PartStatus { pub samples: u32, pub confidence: u16, pub coverage: u16, pub residual: u16, pub active: bool }
pub struct WordsError;  // wrong version or kind
pub mod words { pub const VERSION: i32 = 1; pub const KIND_IMU: i32 = 1; pub const KIND_MAG: i32 = 2; }
```

The driver crates copy `PartStatus`/`Routine` into the device-model types (`lemnos_device::CalibrationPart`,
`CalibrationRoutine`); `lemnos-fusion` does not depend on `lemnos-device`.

## Drivers

`lemnos-drivers-bmi088` and `lemnos-drivers-bmm150` depend on `lemnos-fusion` (feature `float`;
without it the calibrated channels are `NO_VALUE` and no estimator is built). Both keep their
raw path and their existing bus code. Their `INFO` channels are the table above.

- The IMU's `read` and `read_batch` run the `ImuCalibrator` on every sample they return, including
  samples a client does not ask for, and fill the `_cal` channels from the applied calibration.
- The magnetometer's `read` runs the `MagCalibrator` and fills `magnetic_field_cal`.
- `Sensor::calibration_status`, `calibration_command`, `calibration_words` and `load_calibration`
  are implemented with the words layout above.

Calibration accrues only while the device is read. An IMU that nobody subscribes to is not read
(`idle` IMUs are never read by default), so its calibration waits for a reader. The fusion
device's `always` option keeps the IMU and magnetometer read.

## lemnosd

### The fusion device

`driver = "fusion"` in `board.toml`, class `orientation`, placement **composite** (not a bus
device; lemnosd builds it itself, because it needs the IMU and magnetometer slots). The registry
keeps a `fusion` entry so the board validates; `DriverRegistry::build` for it returns
`Unsupported` (only lemnosd hosts it).

```toml
[[devices]]
id = "orientation"
driver = "fusion"
poll_ms = 10                      # the fastest it reports (its subscriptions' cap)
config = { imu = "imu", mag = "magnetometer", mode = "9axis", algorithm = "mahony",
           kp = 1.0, ki = 0.01, mount_roll_deg = 0, mount_pitch_deg = 0, mount_yaw_deg = 90,
           declination_deg = 0.0, always = false }
```

| Key | Default | Meaning |
|---|---|---|
| `imu` | (required) | the IMU device id; `acceleration_cal.*` and `angular_rate_cal.*` are used |
| `mag` | none | the magnetometer id; required for `mode = "9axis"` |
| `mode` | `6axis` | `6axis` or `9axis` |
| `algorithm` | `mahony` | `mahony` or `madgwick` |
| `kp`, `ki` | 1.0, 0.01 | Mahony gains (`beta` for Madgwick, default 0.1) |
| `mount_roll_deg`, `mount_pitch_deg`, `mount_yaw_deg` | 0 | sensor-to-robot mounting (ZYX intrinsic) |
| `declination_deg` | 0 | magnetic to true north |
| `dip_deg` | learned | the reference dip for disturbance rejection |
| `always` | false | keep the IMU and magnetometer read while the fusion device exists |

Flat `mount_*` keys are used, not a nested table, because the registry's config values are
scalars.

### On demand

A fusion device is **idle** when nothing subscribes to it and `always` is false. Then lemnosd
adds no reads: the IMU and the magnetometer are read only for their own subscribers, as before.
When a client subscribes to the fusion device, lemnosd adds an internal subscription (a reserved
client id, not a socket client) to the IMU at the fusion's period, with the calibrated channels as
the mask, and to the magnetometer at its own rate (100 ms). When the last subscriber leaves, the
internal subscriptions end. The IMU is then idle again.

Samples flow from the bus workers to the filter on the service thread: each IMU batch goes through
`deliver_batch`'s timestamps (the batch's samples are `sample_period_us` apart, the last at the
read time), so the filter sees every sample at its own time. A magnetometer sample is fed with
its read time. The fusion device's subscribers then get the latest output at their period, as a
sensor's do.

### Calibration in lemnosd

- `calibration.rs` (lemnosd) owns the files. A device's calibration is in
  `/var/lib/lemnos/calibration/<device>.toml` (overridden by `LEMNOSD_CALIBRATION_DIR`, for tests),
  written atomically (temp file, then rename):

```toml
format = "lemnos.calibration"
schema_version = 1
device = "imu"
driver = "bmi088"
revision = 42
words = [1, 1, 3, 1000000, ...]
```

- Loaded when the device is built, after `init`. A file of another format or version is ignored
  with a reason in the device's status.
- Saved when the applied calibration's `revision` changes, at most once a minute, and on `apply`
  and at shutdown.

### IPC and lemnos-ctl

New requests (`lemnos-ipc`), answered with a reply or a calibration status message:

- `Calibration { id, device, command }` with `command` one of `start:<routine>`, `stop`,
  `apply`, `discard`, `reset`.
- `CalibrationStatus { id, device }` returns the status (the revision, running routine, progress,
  candidate, failed, the three parts).

`lemnos-ctl calibration show <device>` (the words, decoded as a summary), `status <device>`,
`reset <device>`, `start <device> <accel-six|mag-rotate|gyro-hold>`, `stop <device>`,
`apply <device>`, `discard <device>`. The `writers` policy applies to these commands as it does to
control writes. A fusion device's `status` shows its confidences and its disturbance.

Atlas reaches the same operations through lemnosd's socket. Orion (`lemnos-orion`) exposing them
as actions is a follow-up; the bridge's pinned Orion revision would need the same change.

## Tests

- `lemnos-fusion`: filter convergence (static tilt, gravity direction within 0.5°); rotation at a
  known rate with a gyro bias (the bias is estimated, roll and pitch stay bounded); 9-axis yaw drift
  bounded over 60 s at rest, 6-axis yaw relative; mag ellipsoid recovery from synthetic hard and
  soft iron with partial coverage (offset within 2 %, radius within 2 %); accelerometer sphere
  recovery from six faces and from tumbling samples; disturbance rejection (a field 30 % off is
  ignored, the yaw does not jump); variable `dt` (jittered timestamps give the same result within
  0.5°); words round trip; no_std build for `thumbv7em-none-eabihf`.
- Drivers: raw channels unchanged; calibrated channels match the applied calibration; FIFO
  batches run the calibrator on every sample; the routine state machine on synthetic samples
  (routine completes and produces a candidate only after `Apply`; timeout fails); words round trip
  and version rejection.
- lemnosd: a fusion device with mock sensors: the IMU and magnetometer are idle until a subscriber
  arrives, then stream at the fusion rate, and go idle when it leaves; `always = true` keeps them
  read; calibration words persist across a restart (temporary directory).

## Measurements

Measured on the Raze (CM5) only; host timings are not reported. The plan: `lemnosd`'s CPU with the
fusion device subscribed at 100 Hz, and orientation sanity (the gravity vector against the board's
pose; yaw over 60 s at rest). Results are recorded in the commit that adds them.
