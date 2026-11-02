# USB

Lemnos has one USB layer, `crates/lemnos-usb`. A flashing tool's USB boot code, a camera stack's UVC access and Lemnos's own USB drivers all open devices through it. There is no libusb in the dependency tree, and no consumer imports the platform's USB crate directly.

## Layers

- `crates/lemnos-linux-sys` (`usbfs`): the usbdevfs ioctls, the URB table that owns each in-flight buffer until the kernel returns it, and the descriptor read. This is the workspace's only `unsafe` code, as for every other Linux interface.
- `crates/lemnos-uevent`: Linux device watching (netlink uevents, inotify, a sysfs rescan) behind one `DeviceWatcher`. `lemnos-usb` uses it for hotplug on Linux, and `lemnos-linux` re-exports it under the same paths.
- `crates/lemnos-usb`: the public API, in Lemnos's own types and error kinds. On Linux it runs on usbdevfs. On Windows and macOS it runs on nusb 0.2 (WinUSB and IOKit).
- `crates/lemnos-linux`, feature `usb`: the bus session for USB devices that discovery found in sysfs. It maps the crate's errors to bus errors. Discovery stays sysfs-based.
- `crates/lemnos-drivers-usb`: the device model. It is unchanged.

nusb is used for listing on every platform, for hotplug and every transfer off Linux. On Linux, hotplug is `lemnos-uevent`'s `DeviceWatcher` (subsystem `usb`, devtype `usb_device`), the same watcher the camera stack uses; nusb is not used on Linux for hotplug or transfers. nusb's Linux backend reaps every URB on its device file from its own thread, so isochronous URBs cannot share a device with it, and nusb has no isochronous API. Linux transfers therefore go through usbdevfs directly.

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

- **Listing.** `list_devices()`, `find_device(&location)`. A `UsbDeviceInfo` has the vendor and product IDs, the class, the manufacturer, product and serial strings, the speed, the interface summaries and the `location()`. It also has `usb_version()` (`bcdUSB`) and `device_version()` (`bcdDevice`), known before the device is opened: a loader and its boot ROM tell their modes apart by these. On Linux nusb reads them from sysfs (`version`, `bcdDevice`); on Windows and macOS from the platform's descriptor. `winusb_bound()` (Windows) says whether WinUSB is the driver bound to the device.
- **Location.** `UsbLocation` is the bus identifier and the port chain, in the sysfs form (`3-1.2`). It is stable while the device stays in its port. The bus identifier is zero-padded on Linux in nusb; `UsbLocation::new` drops the padding, so `003-1` and `3-1` are the same location.
- **Hotplug.** `watch()` returns a `UsbWatch`: a `futures_core::Stream` of `UsbHotplugEvent::{Connected, Disconnected(location)}`, with `next_event()` for threads. It starts before its initial listing, so an arrival in between is not missed.
- **Descriptors.** `UsbDevice::device_descriptor()` is the 18-byte device descriptor (`DeviceDescriptor`): `usb_version`, the class, `max_packet_size_0`, IDs, `device_version`, the `manufacturer_index`, `product_index` and `serial_number_index` string indices, and `num_configurations`. It is read when the device is opened, with no request: on Linux it is the first 18 bytes of the descriptor blob, on Windows and macOS nusb's cached copy. `UsbDevice::configurations()` (parsed when the device was opened) and `active_configuration()`. `ConfigurationInfo::find_endpoint(interface, alternate, direction, transfer_type)` returns an `EndpointInfo` (address, direction, transfer type, max packet size, packets per microframe, interval).
- **Claims.** `claim_interface(n)` and `detach_and_claim_interface(n)` return an `Interface`. Dropping it releases the claim, and on Linux reattaches a driver that the detaching claim unbound. `Interface::set_alternate(alt)` selects an alternate setting.
- **Control.** `control_in(&ControlSetup, length, timeout)` and `control_out(&ControlSetup, &data, timeout)`. `ControlSetup::vendor(direction, request, value, index)` builds a vendor request to the device, and the struct's fields cover the class and standard kinds and the recipient.
- **Blocking transfers.** `bulk_read`, `bulk_write`, `interrupt_read`, `interrupt_write`, `clear_halt`, `reset`, `set_configuration`.
- **Async transfers.** `submit_bulk_in(endpoint, buffer)`, `submit_bulk_out`, `submit_bulk_out_zlp` (Linux, below), `submit_interrupt_in`, `submit_interrupt_out`, `submit_isochronous_in(endpoint, buffer, packet_len)` (Linux). `submit_control_in(&setup, length, timeout)` and `submit_control_out(&setup, data, timeout)` are the futures for control transfers: the blocking call runs on its own thread, bounded by the timeout, so no caller needs `spawn_blocking`. A control transfer cannot be cancelled (`cancel()` does nothing for it). Each returns a `Transfer`: a future (any executor works) that also has `wait()`, `wait_timeout()`, `is_complete()` and `cancel()`. Its `TransferOutcome` holds the buffer back, the bytes that moved (`data()`) and, for isochronous transfers, the `IsoPacket`s with their own errors.
- **Streams.** `bulk_reader(endpoint, chunk, depth)` implements `std::io::Read` and keeps `depth` transfers in flight. `bulk_writer(endpoint, chunk, depth)` implements `std::io::Write`, gathers writes into `chunk`-byte transfers, and flushes on `flush()` or drop. `flush_end()` ends the transfer for the device: see *Zero-length packets* below. `isochronous_reader(endpoint, packet_len, packets, depth)` (Linux) keeps `depth` transfers of `packets` packets in flight; `next_block()` returns each completed transfer as an `IsoBlock` (its data and packet results) and recycles the previous block's buffer, so the packet lengths are computed once and the transfer buffers are reused. The completion path still builds one packet table per transfer.

Buffers are `TransferBuffer`s (a heap vector, or a usbdevfs mapping on Linux). They move into a transfer and come back in its outcome, so a caller that reuses them allocates nothing per transfer. The `submit_*` calls take a `Vec<u8>` and wrap it; `submit_bulk_in_buffer`, `submit_bulk_out_buffer` and `submit_isochronous_in_buffer` take a `TransferBuffer`. On Linux the kernel copies to and from them directly. On Windows and macOS nusb copies them into its own buffers.

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
| `InvalidInput` | a malformed request: endpoint zero, the wrong direction, a zero-length terminator on an IN or isochronous transfer |
| `Other` | anything else |

## Behaviour notes

- **Timeouts.** On Linux, a zero `Duration` waits forever (usbfs semantics). On Windows and macOS it waits up to a day.
- **Windows.** Control transfers go through a claimed interface, because WinUSB binds per interface. `winusb_bound()` reports the device-level driver only. A composite device binds WinUSB per interface, which the listing does not show.
- **macOS.** Claims are plain (there is no driver to detach). Device-level control transfers are used.
- **IN transfers on Windows and macOS.** nusb reads whole packets, so a read rounds its request up to the endpoint's packet size. Bytes past the caller's buffer are dropped.
- **Isochronous.** Linux only. A packet's length is the chunk size, and the last packet may be shorter.
- **Zero-length packets.** An empty OUT payload is a zero-length packet, and `bulk_write`, `submit_bulk_out` and `flush_end` accept it. A device that reads until a short packet needs the transfer to end on one. When the bytes written are a whole number of the endpoint's packet size (`wMaxPacketSize`: 512 at high speed, 1024 at SuperSpeed), `BulkWriter::flush_end()` sends that terminator after the data, as nusb's `flush_end_async` does. It counts bytes since the last `flush_end`, and writing nothing sends nothing. Drop does not send it. On Linux, `submit_bulk_out_zlp` sets usbdevfs's `USBDEVFS_URB_ZERO_PACKET` on one URB, so the kernel appends the terminator within the transfer; it applies to OUT bulk and interrupt only and is `Unsupported` elsewhere.
- **Buffers.** `TransferBuffer` is the memory a transfer moves its bytes through. On Linux, `UsbDevice::allocate_buffer(len)` returns a usbdevfs mapping when the kernel offers `USBDEVFS_CAP_MMAP`: page-aligned memory the host controller uses directly (`UrbBuffer::Mapped`). A failed mapping, or a kernel without the capability, falls back to a heap vector (`UrbBuffer::Heap`), the copy path. Other platforms have vectors only. The streams allocate their slots once and recycle them. `TransferOutcome::into_buffer()` returns the buffer for reuse, and `set_len` sizes it. Untested on a device (see *Untested on hardware*).
- **Hotplug.** On Linux, `watch()` is `lemnos-uevent`'s `DeviceWatcher` (netlink, falling back to inotify and then a sysfs rescan), filtered to USB devices. Arrivals are listed through nusb before they are reported, so a device gone by then is not reported. A removal is reported for a device this watch saw. On overflow the watch lists again and reports the difference. Windows and macOS use nusb's watch.

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
| the device descriptor's fields (nusb `DeviceDescriptor`) | `device_descriptor()`; `usb_version()` / `device_version()` on the listing |
| `control_in` / `control_out` awaited on `spawn_blocking` | `submit_control_in` / `submit_control_out` (futures) |
| nusb `EndpointWrite::flush_end_async` | `BulkWriter::flush_end()` (blocking), with the terminator as a zero-length packet |
| `speed()` | `UsbDeviceInfo::speed()` (from the listing) |
| waiting on the device file for completions | `bulk_reader` and `submit_*` handle the completions; no device file descriptor is exposed |

A stream's depth is its in-flight count, and its buffers are recycled, so no transfer allocates in steady state.

## Feature gating

`lemnos-linux`'s `usb` feature (and `lemnos`'s `linux-usb`) enables `lemnos-usb`, which enables nusb. Without it, neither is in the dependency tree. The workspace has no libusb or rusb.

## Testing

- Host tests need no device: descriptor parsing from a recorded device descriptor and a hand-built blob, endpoint lookup, the zero-length boundary (`needs_zero_packet`), errno classification, the transfer future (including a worker-thread transfer, which the control futures use), the blocking waits, and `ControlSetup` encoding (`cargo test -p lemnos-usb`). The URB flag bits are checked in `lemnos-linux-sys` (`urb_flags`).
- `lemnos-linux`'s USB transport tests run through a mock transport (`cargo test -p lemnos-linux --features full`).
- `cargo check` and `cargo clippy --target x86_64-pc-windows-gnu` / `--target x86_64-apple-darwin` cover the other platforms. A target needs its rustup target installed.

## Untested on hardware

- Linux descriptor reads, control transfers (blocking and the futures), claims, bulk and interrupt transfers, URB completions, zero-length packets on the wire (`flush_end` and `URB_ZERO_PACKET`), isochronous streams, the reaper's shutdown, and reattaching a driver after a detaching claim.
- Linux usbdevfs mappings (`USBDEVFS_CAP_MMAP`): the mapping, the URBs that use mapped memory, the page alignment, and the copy fallback when a mapping fails. Written from the kernel's usbdevfs interface, with no device to run it on. Host tests cover the page rounding, the heap buffer and the packet lengths only.
- Linux hotplug through `lemnos-uevent` for USB devices: the add and remove mapping, the listing check, overflow resync, and the waker thread. Host tests cover only the devpath-to-location rule. This host's devices are root-owned with no write permission for the build user, so the probe exercised the permission path alone (`EACCES` mapped to `PermissionDenied`). It did not open the Broadcom boot device, which is a board in USB boot mode. Run the flashing tool's and the camera stack's own tests on a board before relying on these.
- Windows and macOS: every runtime path. They are compile-checked only.
