# Flashing a chip from a PC

Lemnos does not ship a flasher. A chip that can only be reflashed over USB is reflashed by a PC-side
flasher, which is another project. This repository holds the board side of that path: the code the chip
runs, built as blobs the PC side loads, and the one wire both sides speak.

## The split

| Side | Where | What |
|---|---|---|
| PC side | another project | the USB transport to the chip's boot ROM or loader, the host protocol, the flash flow (compare first, then write), the retries, the outcome reporting. It pins the wire below by a locked revision |
| Wire | `drivers/flash-wire` (`lemnos-flash-wire`) | the request and reply frames, the CRC, the reset kinds, the identity record, and the layout hash. No dependencies, no `unsafe`. Published for the PC side to pin |
| Stage rules | `drivers/flash-stage` (`lemnos-flash-stage`) | the entry contract (arm the watchdog first, read the arming back), bounded waits, the feed rule, the serve loop over a link, and the order rules the stage enforces itself: a write needs a verified payload, and a boot area or header needs the compared mark. Plus a simulated device for a PC side's conformance run. No `unsafe` |
| Chip stages | `drivers/rockchip-maskrom-stage` and its `-sys` crate | a chip's on-chip code built on the shared rules. Only the `-sys` crate holds `unsafe` (the register and cache primitives, and the assembly). Built as an image the boot ROM runs; the image's position independence is proved by `scripts/check-pic.sh` |

The chip stages are the only code here that runs on a board. Nothing on a board is reached by the PC
side from this repository, so a board run is a separate record (`testing/board-runs/`).

## The device side

A stage presents itself to the host as a USB device before any request can be made: the host enumerates
it, sets an address and a configuration, and then uses the bulk pair. The device's identity, the control
requests it answers, and the bulk endpoints a host claims are in `drivers/flash-wire/README.md`. The
enumeration itself is the shared device side in `drivers/flash-stage`; each chip supplies only its
controller. The enumeration is not on a board yet.

## Compatibility

A PC side and a stage agree on the wire by hash, never by version: see the README of
`drivers/flash-wire`, "How the PC side pins it". A stage states its hashes and its build id in its
identity record, and a skew is refused by family. The VID:PID `1209:0001` is the pid.codes test pair and
is to be replaced before a release.

## Timing

Every session is bounded by the contract in `drivers/flash-wire/README.md` (the transfer, round, idle and
enumeration limits, and the watchdog's period that covers them). A host must meet those numbers.

## Rules every stage keeps

Stated in `drivers/flash-stage/README.md`. In short: arm the watchdog first and read it back; never feed
without a verified round; every wait is bounded; a cause is reported before any reset; a chunk is written
only after its CRC matches; a boot area or header is written only with the compared mark.

## Reset kinds

An END carries a reset kind in its reserved byte: 0 is a normal reset and 3 asks the boot ROM for its
maskrom path. Any other value reads as a normal reset. Whether the boot ROM acts on the maskrom value is
not proven; it is the open item in `testing/board-runs/MR1.md`.

## Where the chip records are

Each board run is a record in `testing/board-runs/`. The Rockchip maskrom path is `MR1.md`. Its status
is untested on board.
