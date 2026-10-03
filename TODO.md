# TODO

Tracks Lemnos work driven by its integration into HeliOS
(`HeliOS-architecture-overhaul`, `backend/src/helios/peripherals`). See
[CHANGELOG.md](CHANGELOG.md) for the user-facing summary of finished work.

## Done

- [x] Async hotplug: `AsyncLinuxHotplugWatcher` (`lemnos-linux` `tokio`
      feature) waits on inotify through Tokio's reactor; `LinuxHotplugWatcher`
      implements `AsFd`/`AsRawFd`.
- [x] Bind policy: `RuntimeBindPolicy` binds matching devices on refresh
      (interface, kind, or driver id). Default binds nothing; failed binds are
      not retried until the device changes.
- [x] Unbound status: `Runtime::device_status` and
      `DeviceStateSnapshot::status` return `DeviceStatus` without binding.
- [x] Built-in `HwmonFanDriver` (`lemnos.pwm.hwmon-fan`) with `fan.read`,
      `fan.set_duty`, `fan.set_pwm`, and `fan.set_mode`. Replaces the HeliOS
      copy and drops its `period_ns = 255` PWM mapping.
- [x] `MockHwmonFan` in `lemnos-mock` for sysfs-free fan driver tests.
- [x] `ErrorKind` plus `kind()` on every Lemnos error type, delegating through
      wrapped sources.
- [x] `Value::to_label_string` and `Value::flatten_labels`.
- [x] Hwmon probe no longer publishes live `fan.pwm`/`fan.pwm_mode`/`fan.rpm`
      descriptor properties, so speed changes don't show up as inventory diffs.

## Next

- [ ] Sensor drivers (BMI088, BMM150, INA238). On hold: decide on an
      extensible driver system first, so drivers can be added without
      rebuilding the crate. The existing drivers live only in
      `crates/lemnos/examples/support/board_validator/drivers/`. Devices on
      non-probeable buses can already be declared through
      `lemnos-core`'s `ConfiguredDeviceModel`.
- [ ] Memory breakdown for a Linux refresh on the CM5, measured with and
      without eager binds. HeliOS measured ~6-7 MiB PSS for a refresh including
      Styx/libcamera probing; Lemnos's share is unknown.
- [ ] Decide whether the `tokio` facade feature is worth its cost for
      consumers that only need `AsyncLinuxHotplugWatcher`.

## Release (do last)

- [ ] Merge `dev` into `main`.
- [ ] Bump to 1.1.0. The new API is additive, but `BuiltInDriverBundle::DRIVER_IDS`
      grew, refresh reports include policy binds, and hwmon descriptor
      properties changed.
- [ ] Tag the release; optionally publish to crates.io.
- [ ] HeliOS pins Lemnos to the tag instead of `branch = "main"`.

## HeliOS migration (HeliOS repo, not here)

- [ ] Replace the 250 ms hotplug sleep loop with
      `AsyncLinuxHotplugWatcher::next_events` and run refreshes in
      `spawn_blocking`.
- [ ] Set a bind policy for the fan driver and delete the eager bind loop in
      `LemnosDiscoveryProbe::discover`; drop the manual bind in `apply`
      (`auto_bind_on_request` covers it).
- [ ] Delete `lemnos_fan.rs` and use the built-in fan driver.
- [ ] Use `device_status`, `ErrorKind`, and `Value::flatten_labels` in place of
      the local mapping helpers.
- [ ] Publish deltas from `RuntimeWatchedRefreshReport::refresh.diff`.
- [ ] Add `lemnos-mock` tests for the discover/apply paths.
- [ ] Parse `sensors.toml` once sensor drivers exist.
