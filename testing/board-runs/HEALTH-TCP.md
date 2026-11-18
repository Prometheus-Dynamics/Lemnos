# HEALTH-TCP board run: a trial that waits on a TCP port

Status: untested on board. The `tcp:<address>:<port>` health source has run only on a PC: the
`system-slots` tests connect to listeners on the loopback addresses, to an accept queue that is
full, to an address that is not on the machine and to a zone whose interface does not exist. The
drop-in `packaging/systemd/lemnosd.service.d/health-tcp.conf` has only been scored by
`scripts/check-packaging.sh`. No board has run a trial with the source, and the daemon has not
run under the unit with the drop-in.

Run in a local session on the bench board, after the PR merges. Take the board lock for the whole
run. Build on NVMe or SSD. Restarts and slot changes are allowed; report each one. The board needs
its USB network gadget up (the interface is called `usb0` below; use the board's name for it) and
an SSH server.

## Setup

1. Install two slots, `A` (the default) and `B`, as `testing/board-runs/I5f.md` does. `B`'s board
   file has `confirm_uptime_s = 30`, `confirm_deadline_s = 600` and one `health` entry, changed
   per check.
2. On a systemd image, install `health-tcp.conf` as
   `/etc/systemd/system/lemnosd.service.d/health-tcp.conf`, except in check 5.

## Checks

1. **IPv4, the port answers.** `health = ["tcp:<the gadget interface's IPv4 address>:22"]`.
   `trial B`. Expect the trial to confirm by itself about 30 s after the daemon's start
   (`resolved` `confirmed`, `by: auto`). The SSH server's log shows connections that are closed
   with nothing sent, one per second while the trial waited, and none after the confirm.
2. **IPv6 link-local with a zone.** `health = ["tcp:[<the interface's fe80:: address>%usb0]:22"]`.
   Expect the same. If the interface comes up after `lemnosd` starts, `list` shows, until it
   does, `health source tcp:[...%usb0]:22 doesn't accept a connection: No such device (os error
   19)`, and the daemon answers its calls throughout.
3. **The port does not answer.** Stop the SSH server before the trial boot. Expect `list` to
   show `doesn't accept a connection: Connection refused (os error 111)` for the source, no
   confirm, and the restart at 600 s with that reason in `resolved` (`fell-back`). Repeat, and
   start the SSH server at about 120 s: the trial must confirm at the next look.
4. **The address is not up yet.** Take the gadget interface's address away (or keep the gadget
   unbound) through the trial boot. Expect a reason at every look and no confirm: `Network is
   unreachable`, `Cannot assign requested address`, `No route to host`, or `is busy` and then
   `did not accept a connection within 2 s` when the connect went out on another route. Give the
   address back: the trial must confirm within two looks. `lemnosd` answers `list` throughout.
5. **Without the drop-in, under systemd.** Remove `health-tcp.conf`, `systemctl daemon-reload`,
   and run check 1. Expect `doesn't accept a connection: Address family not supported by
   protocol (os error 97)` at every look and the fall-back at 600 s. Put the drop-in back.
6. **What it does not prove.** With check 1 confirmed, unplug the USB cable before the trial
   boot of a second trial. Record whether the trial still confirms (the interface keeps its
   address without a host) or waits (the address goes with the link): the source proves the port
   on the board, never the route from the PC, and this check records which the board does.

After the run the board is on its default slot with its board file as it was.
