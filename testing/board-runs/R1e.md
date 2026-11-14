# R1e: the NCM interface's name

Status: untested on board. The name is written to the NCM function's `ifname` before the bind, read
back after it, and reported as the `ifname` item and by the `usb.function` method 11. The stand-in
tree tests check the writes and the read-back; the QEMU gadget check (`testing/crates/gadget-check`)
checks that the guest's kernel takes the name and that `/sys/class/net/lemnos0` exists. Nothing here
has run on a board, a real configfs or a real host.

What only a board can show:

- that a board file's `function ncm ... ifname=lemnos%d` gives the board an interface `lemnos0` (the
  first free number on the board), and that `ip link` on the board lists it once the gadget is set
  up (the PC on the other end names its own side itself);
- that the kernel of the board takes the name: 5.12 or later. On an older kernel the setup refuses
  the board by name (`this kernel's NCM function can't be given an interface name`), and the
  gadget does not come up. Check the board's kernel before the first run;
- that the readback after the bind (`functions/ncm.usb0/ifname`) gives the same name on the board's
  controller as in QEMU. The read-back is the only source the daemon has for the name, so a board
  where it differs from `/sys/class/net` is a bug to report.

Before running: take `/run/lock/board-gpu.lock` as `AGENTS.md` says, use the CM5, and build on NVMe
or SSD. Add `ifname=lemnos%d` to the NCM line of the gadget's `interfaces`, start the daemon as root for
the root phase, then check with `cat /sys/kernel/config/usb_gadget/lemnos-*/functions/ncm.usb0/ifname`
and `ls /sys/class/net` on the board (the host side is `ip link` on the host computer).
