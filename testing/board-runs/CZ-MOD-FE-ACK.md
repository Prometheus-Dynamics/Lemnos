# CZ-MOD-FE-ACK: the rp1-csi2 stop is confirmed by the frame end after FORCE

Status: **untested on board.** The design is in the commit "CZ-MOD FE_ACK stop"; this file is the board
check. It is run by the RAZE runner, after the module is loaded the way M34 loads it: through modprobe with
`options lemnos`, never insmod (docs/device-tree.md), with the receiver owned (`owned=` names the receiver's
compatible, as M34's load line does).

## What is assumed about the hardware (why this check exists)
- After FORCE is written to the channel's CH_CTRL with the FE_ACK interrupt enable kept set (`FORCE | IRQ_EN_FE_ACK`),
  the channel raises its FE_ACK interrupt (CSI2_STATUS bit `IRQ_FE_ACK(0)` = BIT(8)) once the DMA has stopped.
- Neither the stock driver (rpi-7.2.y rp1_cfe csi2.c, which writes FORCE alone and never waits) nor our
  documents show this. It is an assumption, so it is checked here.
- Failure mode is safe: if FE_ACK never arrives, STOP reports the receiver wedged and keeps the pool attached.

## Steps (each prints one line; the RAZE runner keeps the console)
1. Load lemnos.ko through modprobe with the receiver owned, bind the receiver, start a stream, run one frame
   (`lemnos-rp1-csi2-check` mode `stop-ack`).
2. Outcome A, expected: STOP returns success, the receiver is not wedged, and the log has
   `stop: frame end after FORCE in <N> us (bound <B> us)`. Record N.
3. Outcome B, the failure this check must catch: STOP returns wedged. Then every stop on this board wedges, and
   the check prints `STOP never confirmed: FE_ACK after FORCE did not arrive within <B> us`. If that line
   appears on the first stop, the assumption is wrong: stop and report before M1.
4. Stop a second time with nothing in flight (no frame started since the START). Two outcomes are both
   expected to be safe, and the driver must be right either way:
   - 4a, the board acks: STOP returns success, the log shows the frame end after FORCE, and the next START
     reports the same frame count and sequence as before the stop (the stop's ack counts no frame).
   - 4b, the board does not ack: STOP returns wedged after the bound and the pool stays attached. That is safe
     and is recorded as a result, not a failure of the driver; the next START re-arms the receiver. Whether the
     board acks an idle FORCE is a board fact to record, not a guess the driver depends on.

## Why the driver has no FE_ACK gate
The receiver counts frame ends only while it is streaming (lrc_isr reads l->streaming under the frame lock).
A stop sets streaming false before it writes FORCE, and START sets it true only after it has cleared the
channel and the status. So a stop's FE_ACK, armed or late, is read while not streaming and reaches no frame
accounting, and no hardware shape reaches the frame path through a stop. The stop-with-nothing-in-flight case
(kernel/test/lemnos_rp1_csi2_check.c) therefore passes with or without a gate; it checks the count and the next
start's sequence, not the gate. A late FE_ACK after START is not a stop's acknowledgement and is not gated:
the board check (outcome 4) is what would show it.

## Recording
- Put the outcome (A or B, with N and B) in testing/board-runs/CZ-MOD-FE-ACK.md under "Results", with the
  kernel and module revision. Outcome B blocks M1 until the assumption is replaced by a measured one.

## Results
(none yet)
