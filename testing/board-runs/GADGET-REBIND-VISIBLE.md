# GADGET-REBIND-VISIBLE: a gadget is bound again only after the host has seen it go

Status: **untested on board.** Host tests only (the helper's trees on stand-in directories).

## What was seen (Raze #2, Lemnos e199fcb9, 2026-11-23, by Atlas)
One `systemctl restart lemnosd` with the comm core holding the FunctionFS pair and a session
connected. The PC saw the device go at 3 s and come back at 9 s (one clean re-enumeration), and
answered at 13 s. 29 s after the re-enumeration the PC's NCM transmit queue timed out, and from
then on: no IPv4 on the interface, carrier still up, 161 transmit errors, the ACM console taking
writes and returning nothing, the device still enumerated in configuration 1. The PC logged no
second disconnect. The board needed a power cycle. Files: `pm14-board/08-held-gadget.txt`,
`08-pc-capture.txt`, `08-pc-dmesg.txt` in Atlas's session directory. The board's own lines were
in a RAM-only journal and were lost.

## What this row changes, and what it does not show
A bind that follows a let-go waits until `UDC` has been clear for 500 ms (docs/daemon.md, "A
rebind the host can see"). Before it, an enable on a gadget that was still bound cleared and set
`UDC` a few milliseconds apart. Whether that happened on the board is **not known**: in the
sequence above the first rebind had `UDC` clear for about six seconds, which the host saw: the
new daemon's first setup finds the tree held and takes the gadget off its controller (the QEMU
gadget check's case 6 shows that unbind), and binds it again once the holder has let go and the
tree is adopted, or after the takeover. A blip
could only have come from a second enable later, and nothing recorded says there was one. The
row removes the possibility; it is not shown to be the cause.

## Steps
1. Image with this row. Link up, a session connected, the comm core holding the pair.
2. `systemctl restart lemnosd`. Keep traffic going on NCM and the pair for 120 s.
3. Record, to `/data` (the journal is RAM-only): the helper's lines (`lemnosd-gadget: gadget ...:
   bound again after it was ... (UDC clear for N ms)`) and the daemon's (`gadget ...: bound, then
   off the bus for 500 ms (soft_connect), then bound again` or `the controller's pullup cannot be
   taken off`), every
   one of them with its time; the capture list below at 0, 30, 60 and 120 s.
4. Pass: the link carries traffic at 120 s, every `bound again after it was adopted` (or `taken
   over`, `remade`) line is followed by one `bound again after it was unbound (UDC clear for N
   ms)` line with N a few milliseconds, then an `off the bus for 500 ms ..., then bound again`
   line, and the host's kernel log shows one disconnect and one enumeration for it. Expected after
   a restart the store did not cover: one such group. A restart the store covers shows none (GADGET-KEEP). Any other `bound again after it was unbound` line, more than a few seconds
   after the restart, is a second enable: record what the daemon said just before it.
5. Ten restarts.

## Capture list (board, then PC)
Board: dmesg; the journals of lemnosd, lemnos-orion and the Orion node from the restart on;
`lemnos-ctl list`, `show gadget`; `/sys/class/udc/*/state` and `current_speed`; the gadget tree's
`UDC`; the functionfs lines of `/proc/mounts`; which processes hold the endpoint files;
`ip -s link show usb0`, its `carrier` and `queues/tx-0/tx_timeout`; the dwc2 debugfs files when
debugfs is mounted. PC: dmesg from the restart on; `lsusb -v` of the device; `ip -s link show`
of its interface; a usbmon capture across the restart when it can be set up first.

## Results
(none yet)
