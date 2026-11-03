# Documentation

This directory holds repository-level documentation for the Lemnos workspace.

## Guides

- [development.md](development.md): repository layout, validation commands, and CI expectations
- [architecture-crate-map.md](architecture-crate-map.md): explains how the workspace crates fit together
- [testing.md](testing.md): describes test entry points, helper scripts, and validation flows
- [foundation.md](foundation.md): Lemnos 2.0 layering (embedded-hal, `lemnos-hal`, one Linux implementation) and the migration record (see provenance.md)
- [system-service.md](system-service.md): `lemnosd`, the board's hardware service: its client API, LED intents and animations, the updater integration, kernel versus userspace backends, process model and packaging
- [composite-devices.md](composite-devices.md): design only: composite (virtual) devices, the fan controller and its failsafe
- [board-definition.md](board-definition.md): board definitions (TOML/JSON), the built-in drivers, generating them, and adopting a board in an app
- [looks.md](looks.md): LED looks: the blocks and their parameters, look files, built-in names, brightness, reloads and the `lemnos-ctl` and Rust APIs
- [io-blocks.md](io-blocks.md): per-board IO hardware blocks (RP1 PIO with its program map, PWM, GPIO edges, DMA, buses): what each does, what Lemnos uses it for, and the plan
- [dma.md](dma.md): DMA buffers (dma-heap allocation, dma-buf sync, mappings, sync-file fences) and the transfer interface for buses and DMA engines
- [m2m.md](m2m.md): V4L2 memory-to-memory sessions (scalers, converters, codecs): queues, dma-buf import, drains and events
- [hotplug.md](hotplug.md): device arrival and removal: the generic watcher (netlink, inotify, sysfs rescan), its filters and events, Lemnos's inventory watcher, and the camera-stack migration notes
- [compact-model.md](compact-model.md): the `no_std` compact device model (`lemnos-device`), the lite table (`lemnos-lite`) and how the runtime consumes them
- [usb.md](usb.md): the USB layer (`lemnos-usb`): platforms, API, errors, and the migration notes for a flashing tool (from nusb) and a camera stack (from raw usbdevfs)

## Where To Start

- Building an application: start with the root [README.md](../README.md) and [`crates/lemnos/README.md`](../crates/lemnos/README.md)
- Writing a driver: read [`crates/lemnos-driver-sdk/README.md`](../crates/lemnos-driver-sdk/README.md), [`crates/lemnos-driver-manifest/README.md`](../crates/lemnos-driver-manifest/README.md), and [`crates/lemnos-macros/README.md`](../crates/lemnos-macros/README.md)
- Integrating Linux hardware: read [`crates/lemnos-linux/README.md`](../crates/lemnos-linux/README.md)
- Testing without real hardware: read [`crates/lemnos-mock/README.md`](../crates/lemnos-mock/README.md)
