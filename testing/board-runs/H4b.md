# H4b: isochronous streams, host and gadget

Status: untested on board. The host side (`UsbDevice::iso_in_start`, `iso_in_next`,
`iso_in_stop` on usbfs, several URBs queued at once) is covered against the fake usbfs, `uvc`
against a mock camera and the allocation counter, and the package boundary against its mocks.
The gadget side (alternate settings, isochronous endpoints, a ring of AIO transfers per
isochronous endpoint) is covered against a stand-in configfs and FunctionFS tree of plain files
and pipes, which complete every transfer in place. No real controller has run a queued
isochronous transfer, and no transfer has been cancelled while the kernel held it.

Run with the board lock named in `AGENTS.md`. Builds on NVMe or SSD.

## Host: `uvc` with several transfers queued

A UVC camera that streams at high bandwidth (an alternate setting of 3 x 1024 bytes a
microframe, YUYV at its largest size), plugged into the board.

1. Bind `uvc` to it (`[[each]]` with `usb:class:0e`, or a device entry), `select_format` the
   largest uncompressed mode, `start`, and attach a consumer to `frames`. `usbmon`
   (`/sys/kernel/debug/usb/usbmon/<bus>u`) shows 4 isochronous URBs of 32 packets submitted on
   the streaming endpoint before the first completes, and each completion followed by a
   resubmission of the same size.
2. Over 60 s, `counters` shows `frames` at the camera's rate and `damaged` at 0; every frame
   holds width x height x 2 bytes. Load the board's CPUs (`stress-ng --cpu 0` for 30 s): frames
   stay at rate and `damaged` stays at 0, or record how far they fall.
3. `stop`: usbmon shows every URB of the stream unlinked before `SET_INTERFACE` to setting 0.
   `start` again: 4 new URBs.
4. Unplug while streaming: health becomes `Gone`, no panic, no hang, no URB left (usbmon shows
   none submitted after the disconnect). Plug back in: the camera streams again.
5. Allocation: under a heap profiler (`heaptrack lemnosd ...`), streaming for 60 s adds no
   allocation on the driver's lane after the first frame.

## Gadget: isochronous endpoints in an alternate setting

A board with a USB device controller, set up as `drivers/usb-gadget` says, connected to a Linux
host PC. The gadget described as:

```text
interface 0xff 0 0 stream
alternate
endpoint in isochronous 1023 1
endpoint out isochronous 1023 1
```

1. Enable it. On the PC, `lsusb -v` shows interface 0 with settings 0 and 1, the endpoints in
   setting 1 only, `bmAttributes` 0x05, `wMaxPacketSize` 1023 and `bInterval` 4 at high speed.
2. On the PC, a program that sets alternate 1 (`libusb_set_interface_alt_setting`) and reads the
   IN endpoint with several isochronous transfers queued. The gadget's caller posts a send job of
   1023 bytes (a counter in the first bytes) per interval as fast as completions come. The PC
   sees every counter value in order with no gap over 60 s, while the board's CPUs are loaded.
3. A send of 1024 bytes completes `Refused`, and nothing goes on the wire.
4. The PC writes 1023-byte packets to the OUT endpoint, one per interval, numbered; receive jobs
   of 1023 bytes get them in order with no gap while receives are kept posted. A receive of 1022
   bytes completes `Refused`.
5. The PC sets alternate 0 and then 1 again: the gadget reports `Configured` each time, the
   stream picks up again, and nothing from before the switch is delivered after it.
6. Unplug the cable mid-stream: sends and receives complete `NotConfigured`; `dmesg` on the
   board shows no warning from `f_fs` or the controller, and the gadget comes back on replug.
7. `set_halt` on either isochronous endpoint answers `NOT_SUPPORTED`.
