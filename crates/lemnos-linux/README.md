# lemnos-linux

`lemnos-linux` implements Linux-specific discovery, transport, and hotplug support for Lemnos, in pure Rust (no C libraries, no `i2cdev`/`spidev`/`gpio-cdev`/`inotify` crates). Its only `unsafe` code lives in `lemnos-linux-sys`.

## Scope

This crate provides:

- `LinuxBackend`
- Linux transport configuration
- discovery probes for GPIO, LED, PWM, hwmon, I2C, SPI, UART, and USB surfaces
- Linux path helpers
- optional hotplug watching: netlink uevents on the real `/sys`, inotify on other roots or as a fallback (`LinuxHotplugWatcher::source`)
- `hal`: embedded-hal 1.0 implementations: `I2cBus` (i2c-dev; addresses claimed with `I2C_SLAVE`, never forced), `Spidev`, `GpioChip`/`GpioLines`/`GpioLine` (GPIO uAPI v2: bias, drive, debounce, edge events), `StdDelay`; the backend's sessions run on the same code
- `uevent`: the kernel's uevent socket and parser
- `v4l2::m2m` (feature `v4l2-m2m`): V4L2 memory-to-memory sessions, MMAP or dma-buf queues, drains and events ([docs/m2m.md](../../docs/m2m.md))
- `dma` (feature): DMA buffers, dma-heap allocation, dma-buf sync, mappings and sync-file fences ([docs/dma.md](../../docs/dma.md))
- `transfer` (feature `spi` or `i2c`): the SPI and I2C transfer engines of `lemnos_bus::transfer`

## Features

- `full`: enables the common Linux capability bundle
- `gpio`, `pwm`, `i2c`, `spi`, `uart`, `usb`: enable interface-specific support
- `hotplug`: enables hotplug watching (uevent, inotify fallback)
- `gpio-sysfs`: enables sysfs-based GPIO discovery
- `gpio-cdev`: enables character-device GPIO support (uAPI v2, Linux 5.10+)
- `dma`: enables DMA buffers (dma-heap, dma-buf sync, fences)
- `v4l2-m2m`: enables V4L2 memory-to-memory sessions
- `tracing`: enables tracing integration

Use this crate directly for Linux-specific integrations or indirectly through the `lemnos` facade.
