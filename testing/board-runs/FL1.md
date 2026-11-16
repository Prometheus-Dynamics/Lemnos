# FL1: the flash wire and the on-chip stage's device side, board checks

Status: untested on board. Every timing in the wire contract and in the stage was checked only in simulation, on a simulated
clock whose controller passes and commands take the times the model gives them. Nothing has been measured on a board. No board
was contacted by this revision, and nothing was written to a board or rebooted.

The checks below are the ones the simulation cannot make. Each one is a step to run on the target board, in order. A check
that fails is a finding against the wire contract (`drivers/flash-wire/README.md`), not a reason to change a constant without
a rev.

## Bench checks

1. **The send-end to arm gap (the ROM's jump and the stage's entry).** The contract's first window is anchored at send-end: the
   instant the host finished sending the stage to the boot ROM. It assumes the ROM runs the stage only after its last byte, so
   the arm (the first watchdog arm in the stage) is at or after send-end. Measure send-end (the host's last byte acknowledged)
   and the arm (the watchdog's arming read-back). Required: arm is at or after send-end, and the gap is recorded.

2. **Bring-up to visibility.** The stage's init must make the device visible to the host (the controller's run bit) no later
   than `INIT_BUDGET_US` (10 s) after the arm. Measure the arm to the run bit, and the first moment the host's USB stack sees the
   device. Required: both within 10 s of the arm. The init waits all take the init deadline (`arm + INIT_BUDGET_US`); a run that
   reaches the deadline is a failed init and must end with its own report, not a silent reset.

3. **Enumeration fits in what is left.** After visibility, the host's enumeration (the descriptor reads, SET_ADDRESS,
   SET_CONFIGURATION) must finish before the first idle deadline, `arm + IDLE_LIMIT_US` (30 s). Measure the enumeration's end
   on the analyser. Required: it ends before the deadline; if it does not, the stage ends with `Enumerate(NotConfigured)`.

4. **The first fed request within the first window.** The host's first request must be completely sent within
   `FIRST_REQUEST_WINDOW_US` (24 s) of send-end. Measure send-end to the end of the first request's OUT transfer. Required: within
   24 s, with the stage's deadline still open when the request completes.

5. **Controller command times, each against its deadline.** Measure, on the target controller, the worst time of each command a
   session phase issues: the EP0 control data and status stages, the stall, the endpoint configuration (SET_CONFIGURATION), the
   bulk arm, the bulk IN send, and the end-transfer on a bus reset. Record each. Required: each is well inside the phase's deadline
   (`TRANSFER_BUDGET_US`, 3.5 s); the simulation's `Costs` knobs model these as free unless a test sets them.

6. **Address before status.** The controller takes the new address (DCFG.DEVADDR) before the status stage of SET_ADDRESS
   (`address_before_status` returns true in the Rockchip controller). Check on the bus: the status stage is answered with the new
   address. If the controller applies it later, change `address_before_status` and record the change as a rev.

7. **The speed register's encoding.** DSTS.CONNECTSPD 0 means high speed on the target controller (the stage reads
   `Speed::High` from it and refuses any other speed). Check: a high-speed host enumerates; a full-speed link (forced, if the port
   allows) fails enumeration with `NotHighSpeed` and the configuration is undone.

8. **End-transfer on a bus reset.** The end-transfer command for BULK_OUT and BULK_IN uses resource id 0 (the one DEPSTARTCFG
   allocated). Check: after a bus reset with a transfer armed, the controller accepts the end-transfer, and the next session's first
   reply is not the stale one. A failed end-transfer must end the session with a link failure (`Event::Fault`).

9. **The stop procedure.** The host sends nothing more on the session, releases the interface, and resets the port. Check: the stage
   re-enumerates under the unchanged idle deadline; a host that does not configure in time gets `Enumerate(NotConfigured)`, and then
   the watchdog resets the stage into a normal boot (not the maskrom path). The stage's buffers may still hold a surplus transfer.

10. **The storage budget: 64 KiB on a slow card.** A 128-sector write (64 KiB) on the real SD card, and its read-back, must each fit
    `TRANSFER_BUDGET_US` (3.5 s). Measure the write and the read-back on the slowest card the target will see. A call that cannot
    finish ends with status 2 (storage), and the range may be partly written: the check is whether the budget holds, not only whether
    the write is correct.

11. **The watchdog's period.** The reference watchdog's setting TOP 14 (44.739 s) is the period the stage uses. Measure it on the
    target, and check the idle deadline (30 s) falls before it from the same feed.

12. **Surplus bytes of an overlong transfer: open.** A host transfer longer than its exact length leaves its surplus on the endpoint,
    where the next header receive could read it. The stage's behaviour is undefined until the drain's semantics are shown on the
    bench. Check: send one overlong frame and record what the next header receive returns. This check is open; the wire contract
    says so, and no test here closes it.

## Result

Not run. Each check above records its measured value and the date and build id of the image it ran on, in this file, when it runs.
