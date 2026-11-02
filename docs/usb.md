# USB

Lemnos has one USB layer, `crates/lemnos-usb`. A flashing tool's USB boot code, a camera stack's UVC access and Lemnos's own USB drivers all open devices through it. There is no libusb in the dependency tree, and no consumer imports the platform's USB crate directly.

## Layers

- `crates/lemnos-linux-sys` (`usbfs`): the usbdevfs ioctls, the URB table that owns each in-flight buffer until the kernel returns it, and the descriptor read. This is the workspace's only `unsafe` code, as for every other Linux interface.
- `crates/lemnos-usb`: the public API, in Lemnos's own types and error kinds. On Linux it runs on usbdevfs. On Windows and macOS it runs on nusb 0.2 (WinUSB and IOKit).
- `crates/lemnos-linux`, feature `usb`: the bus session for USB devices that discovery found in sysfs. It maps the crate's errors to bus errors. Discovery stays sysfs-based.
- `crates/lemnos-drivers-usb`: the device model. It is unchanged.

nusb is used for listing and hotplug on every platform, and for every transfer off Linux. It is not used on Linux for transfers. nusb's Linux backend reaps every URB on its device file from its own thread, so isochronous URBs cannot share a device with it, and nusb has no isochronous API. Linux transfers therefore go through usbdevfs directly.

## Platforms

| | Listing, hotplug, descriptors | Control | Bulk and interrupt | Async (`Transfer`) | Isochronous |
|---|---|---|---|---|---|
| Linux | yes | usbdevfs ioctl | usbdevfs ioctl and URBs | URBs, reaper thread | URBs |
| Windows | nusb (WinUSB) | through a claimed interface | nusb | nusb, worker per endpoint | no (`Unsupported`) |
| macOS | nusb (IOKit) | device, after a claim | nusb | nusb, worker per endpoint | no (`Unsupported`) |

Checked on this host: Linux (the crate's tests and a read-only probe), and `cargo check` and `clippy` for `x86_64-pc-windows-gnu` and `x86_64-apple-darwin`. Only the Linux listing and the permission path have run against a device; see *Untested on hardware* below.

## API

```rust
use lemnos_usb::{ControlSetup, Direction, UsbDevice, UsbLocation};

for info in lemnos_usb::list_devices()? {
    println!("{} {:04x}:{:04x} {:?}", info.location(), info.vendor_id(), info.product_id(), info.product());
}
let device = UsbDevice::open_location(&"3-1.2".parse::<UsbLocation>()?)?;
let config = device.active_configuration()?;
let endpoint = config.find_endpoint(0, None, Direction::In, lemnos_usb::TransferType::Bulk);
let interface = device.detach_and_claim_interface(0)?; // Linux unbinds the kernel driver
let mut reader = device.bulk_reader(0x81, 16 * 1024, 4); // 4 transfers in flight
```

- **Listing.** `list_devices()`, `find_device(&location)`. A `UsbDeviceInfo` has the vendor and product IDs, the class, the manufacturer, product and serial strings, the speed, the interface summaries and the `location()`. `winusb_bound()` (Windows) says whether WinUSB is the driver bound to the device.
- **Location.** `UsbLocation` is the bus identifier and the port chain, in the sysfs form (`3-1.2`). It is stable while the device stays in its port. The bus identifier is zero-padded on Linux in nusb; `UsbLocation::new` drops the padding, so `003-1` and `3-1` are the same location.
- **Hotplug.** `watch()` returns a `UsbWatch`: a `futures_core::Stream` of `UsbHotplugEvent::{Connected, Disconnected(location)}`, with `next_event()` for threads. It starts before its initial listing, so an arrival in between is not missed.
- **Descriptors.** `UsbDevice::configurations()` (parsed when the device was opened) and `active_configuration()`. `ConfigurationInfo::find_endpoint(interface, alternate, direction, transfer_type)` returns an `EndpointInfo` (address, direction, transfer type, max packet size, packets per microframe, interval).
- **Claims.** `claim_interface(n)` and `detach_and_claim_interface(n)` return an `Interface`. Dropping it releases the claim, and on Linux reattaches a driver that the detaching claim unbound. `Interface::set_alternate(alt)` selects an alternate setting.
- **Control.** `control_in(&ControlSetup, length, timeout)` and `control_out(&ControlSetup, &data, timeout)`. `ControlSetup::vendor(direction, request, value, index)` builds a vendor request to the device, and the struct's fields cover the class and standard kinds and the recipient.
- **Blocking transfers.** `bulk_read`, `bulk_write`, `interrupt_read`, `interrupt_write`, `clear_halt`, `reset`, `set_configuration`.
- **Async transfers.** `submit_bulk_in(endpoint, buffer)`, `submit_bulk_out`, `submit_interrupt_in`, `submit_interrupt_out`, `submit_isochronous_in(endpoint, buffer, packet_len)` (Linux). Each returns a `Transfer`: a future (any executor works) that also has `wait()`, `wait_timeout()`, `is_complete()` and `cancel()`. Its `TransferOutcome` holds the buffer back, the bytes that moved (`data()`) and, for isochronous transfers, the `IsoPacket`s with their own errors.
- **Streams.** `bulk_reader(endpoint, chunk, depth)` implements `std::io::Read` and keeps `depth` transfers in flight. `bulk_writer(endpoint, chunk, depth)` implements `std::io::Write`, gathers writes into `chunk`-byte transfers, and flushes on `flush()` or drop.

Buffers are owned `Vec<u8>`s. They move into a transfer and come back in its outcome, so a caller that reuses them allocates nothing per transfer. On Linux the kernel copies to and from them directly. On Windows and macOS nusb copies them into its own buffers.

## Errors

`lemnos_usb::Error` has an `ErrorKind`, a message that names the operation, and the OS code when there is one:

| Kind | Meaning |
|---|---|
| `PermissionDenied` | the device node or the driver rule refuses the open or the claim |
| `Busy` | another claim, handle or kernel driver has the device or interface |
| `Disconnected` | the device is gone, or a transfer was cut off by a disconnect |
| `Unsupported` | the platform or the driver does not do this (isochronous off Linux) |
| `NotFound` | no device at the location, or no such endpoint in a claimed interface |
| `Timeout` | the transfer's timeout passed |
| `Stall` | the device stalled the endpoint (`clear_halt` clears it) |
| `Cancelled` | the transfer was cancelled |
| `InvalidInput` | a malformed request: endpoint zero, the wrong direction, an empty payload |
| `Other` | anything else |

## Behaviour notes

- **Timeouts.** On Linux, a zero `Duration` waits forever (usbfs semantics). On Windows and macOS it waits up to a day.
- **Windows.** Control transfers go through a claimed interface, because WinUSB binds per interface. `winusb_bound()` reports the device-level driver only. A composite device binds WinUSB per interface, which the listing does not show.
- **macOS.** Claims are plain (there is no driver to detach). Device-level control transfers are used.
- **IN transfers on Windows and macOS.** nusb reads whole packets, so a read rounds its request up to the endpoint's packet size. Bytes past the caller's buffer are dropped.
- **Isochronous.** Linux only. A packet's length is the chunk size, and the last packet may be shorter.
- **Alignment.** Buffers are ordinary `Vec`s, not page-aligned. DMA-buffer allocation is a separate Lemnos item (see TODO.md); when it lands, USB buffers come from it.
- **Hotplug.** `watch()` uses nusb's watcher on every platform. The single hotplug watcher (TODO.md) is meant to serve this too; until it does, a USB watch and a Linux uevent watch both exist.

## Migrating a flashing tool from nusb 0.2

| nusb 0.2.7 | lemnos-usb |
|---|---|
| `nusb::list_devices().await` | `lemnos_usb::list_devices()` (blocking; `find_device` for one location) |
| `DeviceInfo::bus_id()`, `port_chain()` | `UsbDeviceInfo::location()` (`UsbLocation`, displayed `bus-port.port`) |
| `DeviceInfo::serial_number()`, `product_string()` | `serial_number()`, `product()` (also `manufacturer()`) |
| `DeviceInfo::driver()` (Windows) | `winusb_bound()`, the answer the tool needs for its install prompt |
| `info.open().await` | `UsbDevice::open(&info)` |
| `device.active_configuration()` and the endpoint scan | `active_configuration()` and `ConfigurationInfo::find_endpoint(...)` |
| `detach_and_claim_interface(n).await` (Linux) | `detach_and_claim_interface(n)`; `claim_interface(n)` elsewhere |
| `interface.endpoint::<Bulk, Out>(addr)?.writer(size)` | `device.bulk_writer(addr, size, depth)` (blocking `Write`), or `submit_bulk_out(addr, data)` (a future) |
| `endpoint.reader(size)` | `device.bulk_reader(addr, size, depth)` (blocking `Read`), or `submit_bulk_in(addr, buffer)` |
| `nusb::watch_devices()` | `lemnos_usb::watch()` (a `Stream`, and `next_event()`) |
| `nusb::Error`, `ErrorKind` | `lemnos_usb::Error`, `ErrorKind`: the same kinds, plus `Stall`, `Timeout` and `Cancelled` |

A tokio-based tool can `.await` a `Transfer` directly; no runtime feature is needed. Blocking writers and readers run fine inside `spawn_blocking`.

## Migrating a camera stack from raw usbdevfs

| Raw usbdevfs (a camera stack's own module) | lemnos-usb |
|---|---|
| `UsbDevice::open(path)` | `UsbDevice::open(&info)` or `open_location(&location)` (the bus number and port chain, not the device node) |
| `claim_interface` / `detach_and_claim` / `release` | `claim_interface`, `detach_and_claim_interface`, dropping the `Interface` |
| `set_interface(n, alt)` | `Interface::set_alternate(alt)` |
| `control(setup, buf, timeout)` | `control_in` / `control_out` with a `ControlSetup` |
| `bulk(ep, buf, timeout)` | `bulk_read` / `bulk_write` (blocking) |
| `UrbRing` isochronous submit and reap | `submit_isochronous_in(ep, buffer, packet_len)`, then `Transfer::wait()` or `.await`; each packet's result is in `TransferOutcome::packets` |
| `clear_halt`, `reset` | `clear_halt`, `reset` |
| `speed()` | `UsbDeviceInfo::speed()` (from the listing) |
| waiting on the device file for completions | `bulk_reader` and `submit_*` handle the completions; no device file descriptor is exposed |

A stream's depth is its in-flight count, and its buffers are recycled, so no transfer allocates in steady state.

## Feature gating

`lemnos-linux`'s `usb` feature (and `lemnos`'s `linux-usb`) enables `lemnos-usb`, which enables nusb. Without it, neither is in the dependency tree. The workspace has no libusb or rusb.

## Testing

- Host tests need no device: descriptor parsing from a hand-built blob, endpoint lookup, errno classification, the transfer future, the blocking waits, and `ControlSetup` encoding (`cargo test -p lemnos-usb`).
- `lemnos-linux`'s USB transport tests run through a mock transport (`cargo test -p lemnos-linux --features full`).
- `cargo check` and `cargo clippy --target x86_64-pc-windows-gnu` / `--target x86_64-apple-darwin` cover the other platforms. A target needs its rustup target installed.

## Untested on hardware

- Linux descriptor reads, control transfers, claims, bulk and interrupt transfers, URB completions, isochronous streams, the reaper's shutdown, and reattaching a driver after a detaching claim. This host's devices are root-owned with no write permission for the build user, so the probe exercised the permission path alone (`EACCES` mapped to `PermissionDenied`). It did not open the Broadcom boot device, which is a board in USB boot mode. Run the flashing tool's and the camera stack's own tests on a board before relying on these.
- Windows and macOS: every runtime path. They are compile-checked only.
