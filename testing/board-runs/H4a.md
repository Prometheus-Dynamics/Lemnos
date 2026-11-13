# H4a: a device for each USB camera, by class

Status: untested on board. The board format's `[[each]]` entry, the host's looks (at start, on a
hint, every retry cap), the USB class and vendor:product patterns, pinning by a device entry and
`except` are covered by tests on the simulated board, on a fake sysfs tree for the Linux backend,
and with the `uvc` driver on mock cameras. No test has listed a real `/sys/bus/usb/devices`, and
no real camera has been bound this way.

Run with the board lock named in `AGENTS.md`. Two UVC cameras of the same make, and one USB
device that is not a camera (a disk or a serial adapter), plugged into a hub on the board.

```toml
[[each]]
id = "camera"
driver = "uvc"
match = "usb:class:0e"
need = "usb"
config = { frame_bytes = 4147200 }

[each.streams.frames]
buffers = 4
buffer_bytes = 4147200
```

Checks, in order:

1. Start lemnosd with both cameras plugged in. `lemnos-ctl list` shows `camera@usb:B-P` for each,
   named by the port `ls /sys/bus/usb/devices` shows it on, both running and `state: idle`. The
   other device has no device and is untouched (its kernel driver still bound).
2. Stream from each camera at once (`select_format`, `start`, attach a consumer to each
   `frames` stream): frames from both, at the rate of their formats.
3. Pull one camera. Within the hint gap its device goes retrying with the reason that it is
   gone; the other keeps streaming. Plug it back into the same port: the same id and the same
   index come back, running, with no restart of lemnosd.
4. Plug it into another port: a new device `camera@usb:B-P2` is made for it, and the old id stays
   listed, retrying.
5. Stop lemnosd and set `LEMNOSD_BOARD` to a file without the hotplug watcher's events (or block
   uevents): a camera plugged in is bound within 8 s by the look the retry cap brings.
6. Add a `[[devices]]` entry naming one camera by `usb:VVVV:PPPP` of a third camera of another
   make, and one with `disabled = true` naming a port: the first is bound only under its own id,
   the second is not bound at all and its kernel driver keeps it. `except = ["usb:B-P"]` on the
   entry does the same for that port.
7. `usb:class:0e:01` and `usb:class:ef:02:01` bind the same cameras as `usb:class:0e`.

Record: the `lemnos-ctl list` output of each step, and how long from plug to running in steps 3
and 5.
