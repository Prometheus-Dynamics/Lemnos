# GADGET-KEEP: the USB gadget kept across a restart of lemnosd

Status: untested on board. QEMU (dummy_hcd) passes the orion-link check (10 restarts, a control
request while no daemon runs, one Orion session held through all of them and never lost, a SIGKILL at each of the 7 start
steps) and `testing/qemu/run-gadget-keep.sh` under a real systemd with the packaged unit. The stand-in
tree tests (`backends/linux/src/gadget/tests.rs`) check the
store's protocol: a kept gadget is taken up as it was, one taken down on purpose leaves the store and
the next enable waits for its barrier, a set that is not whole or not the board's is let go and set up
again, and a remake keeps the board's NCM function. The QEMU checks (`testing/qemu`, dummy_hcd) run the
daemon with the gadget's network link carrying traffic through restarts and kills. Nothing here has run
on a board's controller.

What only a board can show:

- that `systemctl restart lemnosd` leaves the gadget on the bus: the host's kernel log shows no USB
  disconnect, and the host's NCM interface keeps its name and its ifindex;
- that the board's NCM interface keeps its ifindex (`ip -o link show <name>` before and after) and that
  a TCP connection opened over the link before the restart is still open and carries data after it;
- that a `kill -9` of the daemon's main PID (by recorded PID, as the bench rules say) does the same:
  the unit restarts it after 1 s, the store keeps the files, the link stays up;
- that the daemon's log has one `network NAME ifindex N → NAME ifindex N (taken up as it was)` line for
  the gadget after each restart, and no `network ... (remade)` line;
- that a stop (`systemctl stop lemnosd`) takes the gadget off the bus, and the next start brings it
  back (V1g check 6);
- that the store holds the gadget's files while the daemon is down: `systemd-analyze fdstore lemnosd`
  between the kill and the restart lists `lemnos-ffs.<udc>.record`, `.ep0` and one `.epN` per endpoint,
  and, with `lemnos-orion` holding the link's pair, `lemnos-end.<device>`;
- that an Orion session over the link goes on through the restart: `lemnos-orion` logs no second offer,
  the daemon logs `the hold the daemon before lent is taken up: the holder's link goes on`, and the
  host's session is never `Lost`;
- that a control request the host sends to the Orion link's interface while no daemon runs is stalled
  by the next daemon (one `... the host made while no daemon ran was refused (stalled)` line), well
  inside the host's timeout;
- that a restart the store covers logs no `bound, then off the bus` line and no `bound again` line from
  the helper (the bind is not touched), and that a start that finds the tree there without the store's
  files (after a stop, or a store that was let go) logs one `bound, then off the bus for 500 ms
  (soft_connect), then bound again` line, and the host shows one disconnect and one enumeration;
- after that pulse, the dwc2's own state, read-only, from its debugfs
  (`/sys/kernel/debug/usb/<udc>/`): `dr_mode` reads `peripheral` (Raze #2 read it so on 2026-11-23,
  before the fix: no `Configuration mismatch` line at probe, so the controller is in its peripheral
  state from the gadget's init and the pullup call is not skipped), and `state`'s `DCTL` has bit 1
  (`SftDiscon`) clear with the link up. The probe's `Invalid parameter g_np_tx_fifo_size=1024` line is
  the driver's own default (1024 words, from the old s3c driver, `params.c`) clamped to the
  hardware's non-periodic FIFO depth of 32 words: nothing on the board sets it, and 32 words (128
  bytes) holds ep0's 64-byte packets, so it is not a fault to fix here.

Before running: take `/run/lock/board-gpu.lock` as `AGENTS.md` says, use the CM5, and build on NVMe
or SSD. The unit must carry `FileDescriptorStoreMax=64` (packaging/systemd/lemnosd.service). The board's
link is the route the run uses: run the checks from a script on the board that restores the daemon on
its own, and read the result after.
