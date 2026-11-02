# lemnos-usb

One USB layer for Lemnos: listing, hotplug, descriptors, control, bulk, interrupt and (on Linux) isochronous transfers, in Lemnos's own types. Linux runs on usbdevfs (through `lemnos-linux-sys`); Windows (WinUSB) and macOS (IOKit) run on nusb. No libusb.

Start with `list_devices()` and `UsbDevice::open(&info)`. See [docs/usb.md](../../docs/usb.md) for the API, the platform table, the error kinds, and the migration notes.
