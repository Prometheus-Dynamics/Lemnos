# lemnos-linux

`lemnos-linux` implements Linux-specific discovery, transport, and hotplug support for Lemnos.

## Scope

This crate provides:

- `LinuxBackend`
- Linux transport configuration
- discovery probes for GPIO, LED, PWM, hwmon, I2C, SPI, UART, and USB surfaces
- Linux path helpers
- optional hotplug watching

## Features

- `full`: enables the common Linux capability bundle
- `gpio`, `pwm`, `i2c`, `spi`, `uart`, `usb`: enable interface-specific support
- `hotplug`: enables inotify-backed watch support
- `gpio-sysfs`: enables sysfs-based GPIO discovery
- `gpio-cdev`: enables character-device GPIO support
- `tracing`: enables tracing integration

Use this crate directly for Linux-specific integrations or indirectly through the `lemnos` facade.
