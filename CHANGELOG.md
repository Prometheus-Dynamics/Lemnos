# Changelog

All notable changes to this workspace should be documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

## [Unreleased]

### Added

- `AsyncLinuxHotplugWatcher` (`lemnos-linux` `tokio` feature, enabled by the facade's `tokio` feature) waits on the inotify descriptor through Tokio's reactor instead of timer polling. `LinuxHotplugWatcher` now implements `AsFd`/`AsRawFd`.
- `RuntimeBindPolicy` lets refreshes bind matching devices (by interface, kind, or resolved driver id) on their own. Set it with `LemnosBuilder::with_bind_policy` or `set_bind_policy`. The default binds nothing.
- `Runtime::device_status` and `DeviceStateSnapshot::status` summarize a device as `DeviceStatus` (`Available`, `Degraded`, `Faulted`, `Missing`) without binding it.
- Built-in `HwmonFanDriver` (`lemnos.pwm.hwmon-fan`) for Linux hwmon fans, with `fan.read`, `fan.set_duty`, `fan.set_pwm`, and `fan.set_mode` interactions and `duty_ratio`/`mode`/`rpm` telemetry.
- `MockHwmonFan` in `lemnos-mock` gives class-device drivers a fake hwmon fan without a real `/sys`.
- `ErrorKind` and a `kind()` method on every Lemnos error type classify failures (not found, busy, permission denied, timeout, and so on) through wrapped sources.
- `Value::to_label_string` and `Value::flatten_labels` render values as flat string labels.

### Changed

- `RuntimeRebindReport` also lists devices bound by the bind policy. Policy bind failures are recorded as `RuntimeFailureOperation::Bind`.
- `BuiltInDriverBundle::DRIVER_IDS` now has seven entries.
- The Linux hwmon probe no longer publishes live `fan.pwm`, `fan.pwm_mode`, or `fan.rpm` descriptor properties, so speed changes no longer mark the fan as changed on refresh. Read them from the bound driver's telemetry. Descriptors now carry a static `fan.has_tachometer` flag.

## [1.0.0] - 2026-04-19

- Standardized the workspace around a shared root layout, toolchain, lint policy, and CI shape.
- Added repo-level development and testing guides plus Docker-backed facade validation.
- Added `scripts/check-file-sizes.sh`, `scripts/ci.sh`, and `scripts/repo-clean.sh`.
- Aligned dependency policy around `thiserror` for library errors and `tracing` for instrumentation.
