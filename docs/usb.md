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

Checked on this host: Linux (the crate's tests and a read-only probe), and `cargo check` and `clippy` for `x86_64-pc-windows-gnu` and `x86_64-apple-darwin`. On Linux, the listing, descriptors, control, UVC isochronous streams, interrupt reads, mappings and reattach have run against the two boards' webcams; see *Hardware results*. Bulk has not run (no bulk device yet), and every Windows and macOS path is untested.

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
- **Claims.** `claim_interface(n)` and `detach_and_claim_interface(n)` return an `Interface`. Dropping it releases the claim, and on Linux binds the driver the detaching claim unbound (see *Reattaching a driver* below). `Interface::set_alternate(alt)` selects an alternate setting.
- **Claim check.** A transfer on an interface the handle has not claimed is refused before the kernel sees it (`Busy`, naming the interface). That covers interface-recipient control requests (the UVC class requests, the DFU requests: the interface number is `wIndex`'s low byte) and any bulk, interrupt or isochronous transfer on an endpoint whose descriptor names an interface. An endpoint no descriptor names is left to the kernel. On Linux the kernel answers the same case with `EBUSY` and logs "did not claim interface"; the check says what to do.
- **Control.** `control_in(&ControlSetup, length, timeout)` and `control_out(&ControlSetup, &data, timeout)`. `ControlSetup::vendor(direction, request, value, index)` builds a vendor request to the device, and the struct's fields cover the class and standard kinds and the recipient.
- **Isochronous reads.** `IsoReader::next_block()` waits without a deadline. `next_block_until(deadline)`
  returns `Ok(None)` at the deadline and leaves the transfer in flight. In caller-driven reaping
  mode both reap the device themselves (`reap_until`); in thread mode both wait on the transfer.
  Neither hangs on a device nobody reaps.
- **Reaping (Linux).** By default a reaper thread waits for the kernel's completions while transfers are in flight, and exits when none are. `set_caller_driven_reaping(true)` switches the device to caller-driven reaping: no thread is started, and the caller reaps with `reap_now()` (delivers the completions ready now) or `reap_until(deadline)` (waits until one is ready or the deadline passes, then delivers the ready ones). Both return how many were delivered, and the transfers waiting on them complete. This is for a caller with its own thread or ring. The mode is set before submitting: it is refused while transfers are in flight. Other platforms reap on their own threads and refuse caller-driven reaping.
- **Blocking transfers.** `bulk_read`, `bulk_write`, `interrupt_read`, `interrupt_write`, `clear_halt`, `reset`, `set_configuration`.
- **Async transfers.** `submit_bulk_in(endpoint, buffer)`, `submit_bulk_out`, `submit_bulk_out_zlp` (Linux, below), `submit_interrupt_in`, `submit_interrupt_out`, `submit_isochronous_in(endpoint, buffer, packet_len)` (Linux). `submit_control_in(&setup, length, timeout)` and `submit_control_out(&setup, data, timeout)` are the futures for control transfers: the blocking call runs on its own thread, bounded by the timeout, so no caller needs `spawn_blocking`. A control transfer cannot be cancelled (`cancel()` does nothing for it). Each returns a `Transfer`: a future (any executor works) that also has `wait()`, `wait_timeout()`, `is_complete()` and `cancel()`. Its `TransferOutcome` holds the buffer back, the bytes that moved (`data()`) and, for isochronous transfers, the `IsoPacket`s with their own errors.
- **Streams.** `bulk_reader(endpoint, chunk, depth)` implements `std::io::Read` and keeps `depth` transfers in flight. `bulk_writer(endpoint, chunk, depth)` implements `std::io::Write`, gathers writes into `chunk`-byte transfers, and flushes on `flush()` or drop. `flush_end()` ends the transfer for the device: see *Zero-length packets* below. `isochronous_reader(endpoint, packet_len, packets, depth)` (Linux) keeps `depth` transfers of `packets` packets in flight; `next_block()` returns each completed transfer as an `IsoBlock` (its data and packet results) and recycles the previous block's buffer, so the packet lengths are computed once and the transfer buffers are reused. The packet tables are reused too: the completion reads the packets into a scratch the reaper keeps, and the table comes from a pool that the reader gives back to. A steady stream allocates nothing per transfer.

Buffers are `TransferBuffer`s (a heap vector, or a usbdevfs mapping on Linux). They move into a transfer and come back in its outcome, so a caller that reuses them allocates nothing per transfer. The `submit_*` calls take a `Vec<u8>` and wrap it; `submit_bulk_in_buffer`, `submit_bulk_out_buffer` and `submit_isochronous_in_buffer` take a `TransferBuffer`. On Linux the kernel copies to and from them directly. On Windows and macOS nusb copies them into its own buffers.

## Outcomes and status

A completed asynchronous transfer always returns its outcome, and the outcome always
returns the buffer. A failure of the whole transfer is a status, not an `Err`, so a caller
that reuses buffers never allocates after an error:

- `TransferOutcome { buffer, actual_length, packets, status, reaped }`. `status` is a
  `TransferStatus`: `Ok`, `Cancelled`, `Disconnected` (reconnect; do not retry the same
  handle), `Stalled` (call `clear_halt` on the endpoint, then retry), or `Error(errno)`
  (the errno on Linux, 0 where the platform gives none). It is a copy value; a message is
  built only by `status.to_error(context)`.
- `reaped` is the moment the kernel handed the completion over, taken before the waiter is
  woken: the device's finish time, not the caller's.
- `Transfer::wait()` is `Err` only when no outcome exists (the reaper failed and the buffer
  is still with the kernel).
- An isochronous outcome's `status` is `Ok` unless the URB ended; each packet has its own
  `status` (`IsoPacket { length, actual_length, status }`, a copy value). A packet that fails
  routinely (`EXDEV`, `EPROTO`) costs no allocation.

Isochronous data: `IsoBlock::packets()` yields each packet with the bytes it moved. The
usbdevfs layout places packet `i` at the sum of the *scheduled* lengths before it, not at
the sum of the bytes moved: a short packet leaves a gap after its bytes, and the packets
after it stay where they were scheduled.

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
- **Mapped buffers and the usbfs limit.** `usbfs_memory_mb` (default 16 MiB, `/sys/module/usbcore/parameters`) bounds every usbfs mapping. A buffer larger than it is heap memory from the start, and a mapping the kernel refuses also falls back to the heap.
- **Zero-length packets.** An empty OUT payload is a zero-length packet, and `bulk_write`, `submit_bulk_out` and `flush_end` accept it. A device that reads until a short packet needs the transfer to end on one. When the bytes written are a whole number of the endpoint's packet size (`wMaxPacketSize`: 512 at high speed, 1024 at SuperSpeed), `BulkWriter::flush_end()` sends that terminator after the data, as nusb's `flush_end_async` does. It counts bytes since the last `flush_end`, and writing nothing sends nothing. Drop does not send it. On Linux, `submit_bulk_out_zlp` sets usbdevfs's `USBDEVFS_URB_ZERO_PACKET` on one URB, so the kernel appends the terminator within the transfer; it applies to OUT bulk and interrupt only and is `Unsupported` elsewhere.
- **Zero-copy frames (bulk).** A completed transfer hands its `TransferBuffer` back in the outcome, the
  same memory that was submitted: nothing is copied. A caller that moves whole frames runs its own
  ring: submit `depth` buffers with `submit_bulk_in_buffer`, reap with `reap_until` (caller-driven
  mode), take each `outcome.buffer` as the frame, and resubmit a spare (`exchange`). `TransferBuffer`
  is `Send` and `Sync`, and may outlive the transfer and the device handle as the frame's backing.
  The `BulkReader` copies into the caller's slice; the ring does not.
- **Mapping lifetime (Linux).** A mapped `TransferBuffer` is unmapped when the buffer is dropped, not when
  the device closes. The mapping keeps the device file open, and the kernel frees the DMA memory when
  the last mapping and the last URB using it are gone (`dec_usb_memory_use_count` in
  `drivers/usb/core/devio.c`, upstream commit `0dd68b5d01d022fc9c5e71c82a82b0a94d3d0671`:
  `usbdev_mmap` allocates, `usbdev_vm_close` drops the mapping's count, `usbdev_release` is the
  file's release and does not free mapped memory). A URB in flight holds the memory, so an unmap
  while a transfer is in flight does not free it under the kernel. The kernel tree on this host
  has the headers only, so the citation is the upstream file at that commit.
- **Querying the open device (Linux).** `driver(interface)` reads the kernel driver bound to an
  interface (`USBDEVFS_GETDRIVER`; `None` when none). `speed()` reads the negotiated speed of the
  open file (`USBDEVFS_GET_SPEED`), and `None` for an unknown or wireless speed. Other platforms:
  `Unsupported` (`speed()` is on the listing there).
- **Reattaching a driver (Linux).** A detaching claim (`detach_and_claim_interface`) unbinds the driver and remembers its name. The release does not bind it again on the kernel tested: `USBDEVFS_RELEASEINTERFACE` leaves the interface unbound, and `USBDEVFS_IOCTL` with `USBDEVFS_CONNECT` (the kernel's `usb_rebind_intf`) did nothing for a UVC streaming interface. The bare `_IO('U', 23)` is not a usbdevfs request (`ENOTTY`). The release therefore tries the sysfs bind (`/sys/bus/usb/drivers/<driver>/bind`, `<location>:<config>.<interface>`) when the interface is still unbound. For a UVC streaming interface that bind is refused (`ENODEV`): uvcvideo claims its streaming interfaces from the control interface, when the control interface is probed. The recovery that works is the UVC device's own rebind: unbind and bind the control interface (`<location>:<config>.<control>`) in `uvcvideo`, which re-registers the video nodes and claims the streaming interface.
- **Buffers.** `TransferBuffer` is the memory a transfer moves its bytes through. On Linux, `UsbDevice::allocate_buffer(len)` returns a usbdevfs mapping when the kernel offers `USBDEVFS_CAP_MMAP`: page-aligned memory the host controller uses directly (`UrbBuffer::Mapped`). A failed mapping, or a kernel without the capability, falls back to a heap vector (`UrbBuffer::Heap`), the copy path. Other platforms have vectors only. The streams allocate their slots once and recycle them. `TransferOutcome::into_buffer()` returns the buffer for reuse, and `set_len` sizes it. Mapping runs on hardware (*Hardware results*); the copy fallback and the usbfs limit do not.
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

## DFU firmware download

`UsbDevice::dfu_download(interface, image, block_size)` is the DFU 1.1 sequence a boot ROM takes its firmware through: a DNLOAD per block (the block number in `wValue`), a GETSTATUS after each block until the device is idle (`bwPollTimeout` honoured), a zero-length DNLOAD, one GETSTATUS (dfuIDLE, or the manifest states for a manifestation-tolerant device), then a reset. A disconnect during the manifest is the expected end. The block size must be the device's `wTransferSize` from its DFU functional descriptor (`dfu_transfer_size(descriptors)` reads it from the raw descriptors): a larger DNLOAD is not what the device accepts. The sequence follows the open-source loader's (`UsbDfuCommands::UpdateFirmware`). `UsbDevice::dfu_status(interface)` is the read-only GETSTATUS. `lemnos_usb::wait_for_device(vendor, product, timeout)` polls the listing for the re-enumerated identity; the location is kept, the IDs change. The interface must be claimed. Nothing names a product: the caller brings the image and the IDs. Host-tested against a simulated DFU device; see *Hardware results* for the boards.

## Feature gating

`lemnos-linux`'s `usb` feature (and `lemnos`'s `linux-usb`) enables `lemnos-usb`, which enables nusb. Without it, neither is in the dependency tree. The workspace has no libusb or rusb.

## Testing

- Host tests need no device: descriptor parsing from a recorded device descriptor and a hand-built blob, endpoint lookup, the zero-length boundary (`needs_zero_packet`), errno classification, the transfer future (including a worker-thread transfer, which the control futures use), the blocking waits, and `ControlSetup` encoding (`cargo test -p lemnos-usb`). The URB flag bits are checked in `lemnos-linux-sys` (`urb_flags`).
- `lemnos-linux`'s USB transport tests run through a mock transport (`cargo test -p lemnos-linux --features full`).
- `cargo check` and `cargo clippy --target x86_64-pc-windows-gnu` / `--target x86_64-apple-darwin` cover the other platforms. A target needs its rustup target installed.

## Hardware results (2026-11-03)

Run with `crates/lemnos-usb/examples/usb_probe` (aarch64, musl, release) under the board's device lock. Board #1 (the BCM2712 board), kernel 7.2.9-v8-16k, webcam `1bcf:0050` at `3-1`, Coral at `1-1`. Board #2: webcam `041e:40a3` at `3-1`. Each run was one process; each claim was released and the webcam left bound to `uvcvideo` (the last step of each board's run).

| Path | Board #1 (1bcf:0050, high speed) | Board #2 (041e:40a3, high speed) |
|---|---|---|
| Listing, `driver()`, `speed()` | `driver` `uvcvideo` on both UVC interfaces before the claim; `speed()` High | the same; `speed()` High |
| `GET_DESCRIPTOR(device)` | 18 bytes, the `1bcf:0050` IDs | 18 bytes, the `041e:40a3` IDs |
| `GET_DESCRIPTOR(configuration)` | wTotalLength 1680, read in two steps (9, then 1680) | wTotalLength 896, the same two steps |
| `GET_STATUS(device)` | `00 00` | `00 00` |
| UVC probe GET_MIN / GET_MAX / GET_DEF / GET_CUR (26 bytes) | 333333 ns (30 fps), format 1, frame 1, max payload 3060 | 400000 ns (25 fps), format 1, frame 1, max payload 2400 |
| UVC GET_LEN (probe) | 26 | 48 |
| VC GET_INFO (selector 1, unit 0) | answered (`03`) | stalled (EPIPE, "Broken pipe" in the message) |
| UVC stream: probe, commit, alternate | alt 11, `0x81`, 1020 x 3 bytes per microframe, 32 packets per transfer (97920 bytes), 4 in flight | alt 5, `0x81`, 800 x 3, 32 packets (76800 bytes), 4 in flight |
| stream, reaper thread, 300 frames | 10.21 s = 29.39 fps; 2551 blocks, 81632 packets, 41.7 MB (4.08 MB/s); packet errors 0; UVC ERR 0; bad headers 0; zero-length packets 0; no block timeouts | 12.23 s = 24.53 fps (target 25.00); 3057 blocks, 97824 packets, 41.2 MB (3.37 MB/s); errors 0; bad headers 0; no timeouts |
| stream, caller-driven (`reap_until`), 300 frames | 10.21 s = 29.40 fps; 81632 packets, 0 errors | 12.23 s = 24.53 fps; 97824 packets, 0 errors |
| `allocate_buffer` mapping (`USBDEVFS_CAP_MMAP`) | mapped (`is_mapped()` true) | mapped |
| interrupt, VC status endpoint, 10 s | `0x87` (16 bytes): 20 read timeouts, 0 events (no control changes happened) | `0x83` (64 bytes): 20 timeouts, 0 events |
| bulk | no bulk endpoint on the webcam: SKIP | the same |
| reattach (detach+claim, release, check) | the release alone leaves the streaming interface unbound; the UVC rebind through control interface `1.0` restores it; `video8` and `video9` present throughout | the same |

Board #2 was stopped after these runs: it reset at about 9842 s of its uptime (an rcu stall, with undervoltage logged earlier), and its results above are from before that. They are to be re-run once the lead clears the board.

Other results:

- The Coral (`1a6e:089a`, DFU mode, `fe/01/02`) on board #1: its one interface has no endpoints. Bulk is not available until the firmware load. The read-only `dfu-status` (GETSTATUS) answers status 0, state 2 (dfuIDLE).
- Sustained stream, board #1, 60 s (`stream 3-1 1800`, the probe's 60 s cap): 1800 frames in 59.2 s (30.40 fps), 14802 blocks, 473664 packets (about 8000 per second), 0 packet errors, 0 timeouts. The probe used 0.62 s of CPU (about 1%), and the xHCI interrupt count rose by about 485 per second (`/proc/interrupts`, sampled every 5 s). Nothing ran at elevated priority, pinned to a CPU, or busy-polled.
- Reaping never spins: the reaper thread and `reap_until` block in `poll` (100 ms slices); `REAPURBNDELAY` is called only after a poll reports the device file ready. A hung-up or errored file (`POLLERR`, `POLLHUP`, `POLLNVAL`) now fails the transfers; before, the poll would have returned at once with nothing to reap. Host test: an idle wait sleeps (`lemnos-linux-sys`, `an_idle_wait_sleeps_instead_of_spinning`).
- Packet tables: a steady isochronous stream reuses one table (host test `a_steady_isochronous_stream_reuses_one_packet_table`). The probe runs did not count allocations; the pool is the only per-stream table.
- `next_block_until`: the stream loop uses it with a 2 s deadline and saw no timeouts, so the `Ok(None)` path did not run on hardware. The interrupt read is a blocking `interrupt_read`.
- The claim check (refused unclaimed transfers) is host-tested. The first control run on board #2, before the check existed, had its UVC requests refused by the kernel (EBUSY); nothing reached the device.

## Coral firmware load (board #1, item 20)

The image is `apex_latest_single_ep.bin` from the user's libedgetpu checkout (10783 bytes, sha256 `3b07311b…dc7f`), copied to board #1's `/tmp` and checked there. The Coral was at `1a6e:089a`, DFU state dfuIDLE (2), before the run. The DFU functional descriptor reads `bmAttributes` 0x07 (canDnload, canUpload, manifestation tolerant), `wDetachTimeOut` 1000 ms, `wTransferSize` 256 bytes, `bcdDFUVersion` 1.01.

- First attempt: blocks of 4096 bytes, which is larger than `wTransferSize` (256). The first DNLOAD was accepted, and the GETSTATUS after it timed out after 5 s (`USBDEVFS_CONTROL` failed with `-110`, `ETIMEDOUT`, on the GETSTATUS). The kernel log shows nothing else from the device. The oversized block is the one difference from the libedgetpu sequence that I can identify. Whether it caused the hang is not established.
- Reset (`USBDEVFS_RESET` through `usb_probe reset`): the device disconnected. Its descriptor reads failed (`device descriptor read/64, error -110`, `device not accepting address`), and it was gone from the bus. One reset, then about two minutes of polling: no enumeration (`unable to enumerate USB device` on `usb1-port1`).
- **Result:** no `18d1:9302` re-enumeration. The Coral is not on the bus, and the firmware load did not complete. The loader was not retried (it would only repeat the same device fault). The brief's expectation (the device stays at `18d1:9302` until power-cycled) is not what happened: the device has to be power-cycled to be seen again at all. The next step is a physical power cycle of the Coral, by the user. Then `usb_probe dfu-status 1-1` to see the state, and a retry with the descriptor's `wTransferSize` (the probe does this when no block size is given).
- The webcam on board #1 was not touched by these runs: it is bound to uvcvideo and `video8`, `video9` are present.

## Blocked on hardware

- **Coral bulk (items 20, 21).** The bulk endpoints exist only after the firmware load, and the load did not complete (above). `usb_bulk_engine` (lemnos-linux, feature `usb`) is written; it ran only on the webcam, where it exits with "no bulk endpoint" (the skip path). The zero-length OUT, the bounded IN read and the cancel have not run on a device. No safe echo is known for this protocol; the plan is endpoint discovery, a claim, a zero-length OUT and an IN read with a timeout, nothing more.
- **Bulk (items 20, 21).** Both webcams have no bulk endpoint, and the Coral has none before the firmware load. `UsbBulkEngine` (lemnos-linux) therefore has no hardware run.
- **Isochronous allocation count.** Not measured: the allocator is not hooked, because the workspace forbids the `unsafe` a counting allocator needs.

## Untested on hardware

- Windows and macOS: every runtime path. They are compile-checked only.
- Linux hotplug through `lemnos-uevent` for USB devices (add and remove on a board), overflow resync and the waker thread. Host tests cover the devpath rule only.
- `next_block_until` returning `Ok(None)` on a live device.
- The zero-length packets on the wire (`flush_end` and `URB_ZERO_PACKET`).
- Mappings under the kernel's limit: the fallback when `usbfs_memory_mb` is hit (the cap is read; no run pushed past it).
