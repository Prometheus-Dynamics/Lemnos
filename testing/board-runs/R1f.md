# R1f: what a holder of the lent gadget pair sees, on a real controller

Status: untested on board. The QEMU harness (`testing/crates/gadget-check`, the holder checks) runs the
cases on `dummy_hcd`, the kernel's dummy controller, with the host on the same kernel and the daemon's
gadget on `dummy_udc.0`. The codes and the after-states in `docs/wire.md` (Endpoints) were measured there.
A real controller can differ in each row below; a board run should record the errno seen for each case.

The check on a board (the CM5, Raze #2, under the board lock the board runs take; the gadget is on its device-mode port):

1. Open the gadget with a board file that lends the bulk pair (the `ffs` function with one bulk IN and
   one bulk OUT endpoint, as `docs/boards.md` shows) and a holder that keeps copies of both descriptors.
2. Hold a read on the OUT copy while the host sends nothing. Then, in order, unplug the host side or
   power-cycle its port (case 1), suspend and resume the host (2a), reset the bus (2b), set the
   configuration to 0 and back to 1 (3), unbind and rebind the gadget's UDC attribute with the daemon
   keeping ep0 open (4), then tear the daemon's gadget down and remount (5).
3. For each step, write the errno the held read returned, whether a new read returned at once or waited,
   and whether the same descriptors carried a chunk after the step. Keep the output of `dmesg` for the
   FunctionFS and UDC lines (`functionfs`, `dwc2` or the board's controller driver).

What a real controller may show differently from `dummy_hcd`:

- **The errno of a request in flight at a disconnect (case 1, 3, 4).** `dummy_hcd` completes it with
  `ESHUTDOWN` through `usb_ep_disable`. A device-mode controller driver (the board's `dwc2`, or any
  other) may complete the request with another code from its own disable path (for example
  `ECONNRESET`). If so, the table in `docs/wire.md` needs that code added, and the holder must accept
  it. Not known until the board shows it.
- **Whether a suspend keeps the endpoints enabled (2a).** On the dummy the function's suspend hook
  only queues an ep0 event. A real controller's suspend may disable the endpoints (some controllers
  drop their endpoints on a suspend with the host's remote wakeup), in which case a held read gets
  `ESHUTDOWN` where the dummy gives none.
- **Timing of the reconnect (1).** The dummy reconnects inside the port's power-on request. A real
  port takes the debounce time before the host enumerates; a holder's `EAGAIN` retries must cover it
  (the holder backs off, capped at 8 s, and retries with no limit).
- **Bus reset (2b).** The dummy's reset is a status change; a real reset drives the bus for about
  10 ms and the controller may raise a further reset interrupt on the way back.
- **Unbind with a request in flight (4).** The order of the composite disconnect and the unbind is the
  kernel's, but a real controller's `ep_disable` can complete a request after the gadget has been
  unbound; the held read may then return twice (one `ESHUTDOWN`, one stale completion). Check that a
  holder does not count the second answer.
- **A host that stays configured while the daemon is down (6).** Not run in QEMU at all: the check
  does not kill the daemon. On a board, kill the daemon's own PID (started by the check, never by name)
  with the pair held and the host still configured, and check the holder's reads still complete.

Nothing here is `pkill`, `killall` or a pattern kill: stop only the PIDs the check started, and record
each one in a pid file on the board when starting it.

## END (added with the END descriptor)

The third descriptor, END, is the host's word that a hold is over for good. On the board, check: with a
holder that took `EndpointsOpenEnd` (through `usb_endpoints_end`), END stays open while the host goes
away, suspends or resets (cases 1, 2a, 2b, 3); END reads EOF once the holder releases the pair, once
the holder's connection goes, and once the device is switched off through lemnosd (`set_power` off).
The daemon's gadget setup at start reuses a FunctionFS mount that is already there (`ep0` present) and
never unmounts or remounts under it; on the board, start the daemon with a holder still open and
check that setup does not fail with EBUSY and that the holder's descriptors keep working until the
holder releases them.

Not run in QEMU in this change: END through the harness. The holder checks call the gadget directly,
not through lemnosd's hold, so the END is covered by the lemnosd tests (`service/lemnosd/tests/usb_endpoints.rs`)
and the bridge test, not by the QEMU cases. Case 6 (daemon dies with the pair held) still needs the
daemon as its own process.
