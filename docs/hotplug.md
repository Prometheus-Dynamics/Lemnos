# Hotplug

Device arrival and removal on Linux: one watcher for Lemnos's own inventory
refresh, for the USB layer and for the camera stack. The watcher core is the
crate `lemnos-uevent`, below `lemnos-linux` and `lemnos-usb`: `device_watch` (the
watcher), `uevent` (the netlink socket it reads) and `paths` (`LinuxPaths`, the
sysfs and dev roots). `lemnos-linux` re-exports them, so `lemnos_linux::device_watch`
and `lemnos_linux::uevent` are unchanged. Lemnos's adapter is `lemnos-linux`'s
`watch` (feature `hotplug`); `lemnos-usb`'s `watch()` is the USB adapter on Linux.

## One watcher, three sources

`DeviceWatcher::new(paths, filter)` picks the first source that works:

| Source | Chosen when | Sees | Descriptor |
| --- | --- | --- | --- |
| `Netlink` | `paths` is the real `/sys/class` and the kernel's uevent socket opens | every add, remove, change, bind, unbind, with the device's variables | yes |
| `Inotify` | netlink is not available (test roots, sandboxes that refuse netlink) | entries appearing, going away or changing in the filter's sysfs directories and in `/dev` (`video*`, `media*`) | yes |
| `Rescan` | neither works | add, remove, change, bind, unbind, and a replug on the same port (remove then add), by diffing sysfs walks on each read | no |

`DeviceWatcher::fd_backed` never picks `Rescan` (Lemnos's inventory watcher
uses it), and the single-source constructors (`netlink`, `inotify`, `rescan`)
choose explicitly.

Inotify has two limits. It does not see driver bind or unbind (those change
files inside a device directory, which inotify does not watch), and its
devpaths are the class view (`/class/video4linux/video3`), not the kernel's
device path (`/devices/platform/.../video3`). Both name the same device, and
`DEVNAME` is the same in both views.

## Filters

```rust
use lemnos_linux::Filter;

const CAMERA: Filter = Filter::subsystems(&["video4linux", "media", "usb"]);
const CAMERA_BODIES: Filter = CAMERA.with_devtypes(&["usb_device"]);
```

A filter names subsystems (`SUBSYSTEM`) and, optionally, device types
(`DEVTYPE`). Build it in a `const`: `Filter::subsystems(&[...])` and
`filter.with_devtypes(&[...])` are `const fn`. Inotify events carry no
`DEVTYPE`, so inotify sources filter by subsystem only.

## Events

Every event is a `UeventRef`: `Event(UeventMsg)` or `Overflow`. A
`UeventMsg` has the kernel's shape: `action()`, `devpath()` (below the sysfs
mount, `/devices/...`), `subsystem()`, `devtype()`, `devname()` (the `/dev`
node without the prefix), and `get(key)` for any variable (`MAJOR`, `MINOR`,
`DRIVER`, `BUSNUM`, `DEVNUM`, ...). Netlink events borrow the receive buffer;
inotify and rescan events are written into a buffer the watcher reuses. A
steady stream of events allocates nothing.

`Overflow` means the kernel dropped events: coldplug the filter again.

```rust
use lemnos_linux::{DeviceWatcher, LinuxPaths, uevent::{Action, UeventRef}};
use std::time::Duration;

let mut watcher = DeviceWatcher::new(LinuxPaths::default(), CAMERA);
watcher.coldplug(|item| handle(item));
loop {
    watcher.wait(Some(Duration::from_millis(250)))?;
    watcher.read(|item| match item {
        UeventRef::Event(msg) => match msg.action() {
            Action::Add | Action::Remove | Action::Bind | Action::Unbind => {
                // msg.devpath(), msg.devname(), msg.get("DRIVER"), ...
            }
            _ => {}
        },
        UeventRef::Overflow => {}
    })?;
}
```

`read` never blocks and returns how many records were consumed (`0`: nothing
was pending). `wait` blocks on the descriptor with `poll(2)`; `fd()` returns it
for an `epoll` loop or a reactor, and `AsyncDeviceWatcher` (feature `tokio`)
parks on it through Tokio's reactor (`next_batch`).

## Coldplug

`coldplug` reports every device the filter matches that is present now, as
`add` events in devpath order. Each one has the same shape as a live event:
the variables come from the device's `uevent` attribute in sysfs, and the
`SUBSYSTEM` comes from the directory it was found in when the attribute does
not name one.

Start the watcher before coldplugging. A device that arrives in between is
then reported twice, so consumers should key on the devpath.

## Rescan

The rescan source keeps the last walk. On each `read` it walks sysfs again and
reports the difference: `add` and `remove` for devices that came and went, a
`bind` or `unbind` when a device's `DRIVER` appears or goes, a `change` for any
other variable change, and a remove then an add when the bus and device
numbers change under the same devpath (a replug). Because it has no descriptor,
`wait` sleeps for the timeout and reports that a read is due.

## Lemnos's inventory watcher

`LinuxHotplugWatcher` (feature `hotplug`) is the same watcher with Lemnos's
subsystems (`LEMNOS_FILTER` in `watch.rs`: GPIO, LEDs, PWM, hwmon, I2C, SPI,
TTY, USB). It maps each event's subsystem to an interface and reports one
`InventoryWatchEvent` per read. Its sources are netlink and inotify, as before.

## Migration notes for the camera stack

These are the changes the camera stack needs. The camera stack is its own
repository; Lemnos does not edit it.

The camera stack's two watchers (`LinuxVideoFsWatcher` in the camera crate's
Linux watch module, and the UVC camera hotplug in its UVC crate) are replaced by
`DeviceWatcher`. What stays in the camera stack: the mapping from events to the
stack's own device keys and backend tags, the sysfs enumeration that builds the
camera's descriptors and UVC function (`sysfs::enumerate`), and the
`/dev`-path rule used for events (`DEVNAME` becomes `/dev/<name>`, else the
sysfs devpath below `LinuxPaths::sys_root()`).

1. **Construction.** `LinuxVideoFsWatcher::new()` becomes
   `DeviceWatcher::new(LinuxPaths::default(), Filter::subsystems(&["video4linux", "media", "usb"]))`.
   It cannot fail: the rescan source is the last resort.
2. **Polling.** `poll()` becomes `read(|item| ...)`. `uses_uevents()` becomes
   `source() == DeviceWatchSource::Netlink`. Overflow (`UeventRef::Overflow`) is
   still "something changed": keep reporting all backends.
3. **Blocking and settling.** The UVC `Hotplug` waits for bursts with its own
   socket and a 50 ms settle. Use `wait(Some(settle))` then `read`; bursts
   coalesce in the kernel queue the same way.
4. **UVC bind and unbind.** `DriversChanged` came from `usb` events whose
   interfaces changed. Filter `usb` and check `devtype() == Some("usb_interface")`
   with `Bind` or `Unbind` (netlink and rescan report them; inotify does not, so
   a camera stack that needs them on a host without netlink should run the
   rescan source: `DeviceWatcher::rescan`).
5. **Initial state.** Replace the first `sysfs::enumerate` used only to seed
   the watcher with `coldplug` (its `add` events give the devpaths), then
   rebuild descriptors for the devpaths you care about.
6. **Async.** An `AsyncFd` over the raw socket becomes
   `device_watch::AsyncDeviceWatcher` (feature `tokio`).
7. **Dependency.** `lemnos-linux` with `default-features = false` and no
   features gives the watcher, `uevent`, the hal and the device model. Its
   external crates are `libc`, `embedded-hal`, `embedded-hal-async`,
   `num-traits`, `ordered-float` and `thiserror` (with their proc-macro
   dependencies). It does not pull `rusb`, `serialport` or `tokio`.
8. **Not carried over.** The camera stack's rescan-on-every-poll when netlink
   is refused: inotify is now the second choice, and it is faster. Rescan
   remains as the source of last resort. Events are `UeventRef`s; the camera
   stack keeps its own backend tags on top of them.

Public API that stays: `UeventSocket`, `Uevent`, `UeventRecv` (`recv`, `wait`).
New: `UeventMsg`, `UeventRef`, `Action`, `UeventSocket::drain`.

## Tests

- `uevent` (unit): recorded payloads for USB add, bind, unbind and change,
  V4L2 and media node add and remove, the borrowed view against the owned one,
  malformed input, and the encode/parse round trip.
- `tests/device_watch.rs`: a fake sysfs and `/dev` tree (USB device, an
  interface bound to `uvcvideo`, a V4L2 node, a media node, class and bus entries
  as symlinks). It covers coldplug shape and the device-type filter, rescan's
  unbind, bind, remove and replug, inotify's class and `/dev` entries (add,
  change, remove, not-in-filter), the rescan and inotify coldplug shape agreeing,
  `new` choosing inotify on test roots, and the async watcher refusing a rescan
  source and waking on a `/dev` change.
- `tests/hotplug.rs`: Lemnos's inventory watcher on the same fake roots (GPIO,
  PWM channel children, async wake-up), and the subsystem to interface map.
