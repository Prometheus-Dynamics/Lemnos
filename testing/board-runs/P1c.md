# P1c: the bridge publishes the identity facts on the reference board

untested on board. Nothing below has been run on a board; the PC run is in the PR description.

Read-only checks. Nothing is written to the board, no module is loaded, no slot is changed. Run
them in a local session on the reference board, with `/run/lock/board-gpu.lock` taken as
`docs/testing.md` says, and after `P1.md` checks 1 to 3 have passed.

## Checks

1. The bridge serves the facts. With lemnosd and `lemnos-orion` running, read the hello document
   through a signed `Identify` (`orion_hello::verify`, or any Orion tool that checks it). Expected:
   `hello.family` equals `[board] id` in the board file; `hello.serial` equals the serial that
   `P1.md` check 1 recorded; `hello.version.slot` names the running slot; `hello.version.lemnos`
   is the daemon's version; `hello.offers` lists the contract ids of the devices that are running,
   sorted, one space apart, and includes `system.identity/1`.

2. A slot marker that moves while lemnosd runs is followed without a rebind. On a board with a
   second slot already installed, and only with the owner's go-ahead (it changes the boot marker
   on the data partition), write the marker of the other slot by hand, wait 5 seconds, and read
   `hello.version.slot` again. Expected: it names the slot the marker names. Put the marker back.

3. `hello.offers` follows a device. Switch one device off with `lemnos-ctl power <device> off`,
   read `hello.offers` (it must no longer name that device's contract), then switch it on again
   and read it back (it must name the contract again).

Record the output of each check here, with the date and the board's `lemnos-ctl --version`.

## Serial source on an RK3566 board (mainline-based kernel, Linux 7.3.3)

Read on a board on 2026-11-13 by another team, read-only: `/proc/device-tree/serial-number` holds
16 lowercase hex digits, already normalised; `/proc/cpuinfo` has no `Serial` line on that kernel.
The source is `device-tree` for that kernel. Not checked: whether the boot loader fills the
property from the OTP CPU id or from elsewhere, and whether the vendor kernel behaves the same.
The serial value is not recorded here.
