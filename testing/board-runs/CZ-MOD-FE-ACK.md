# CZ-MOD-FE-ACK: the rp1-csi2 stop is confirmed by the frame end after FORCE, or by idle

Status: **partly run on board** (passes 2 and 3, against the module before CSI2-STOP-CONFIRMED). The
rule as it now stands (CSI2-STOP-CONFIRMED) is **confirmed in QEMU, untested on board**. It is run by the
RAZE runner, after the module is loaded the way M34 loads it: through modprobe with `options lemnos`,
never insmod (docs/device-tree.md), with the receiver owned (`owned=` names the receiver's compatible, as
M34's load line does).

## The rule (CSI2-STOP-CONFIRMED)
A stop writes FORCE and waits a bound (four frame times, at least 250 ms). It is confirmed when:
- **ACKED**: the channel's FE_ACK came within the bound; or
- **IDLE**: the channel was between frames at FORCE (no frame start without its frame end) and no frame
  started within the bound. The board raises no FE_ACK for an idle FORCE (pass 3), so idle is proven by the
  absence of a frame start, never by a quiet line counter.

Otherwise it is **UNCONFIRMED**: the buffers stay pinned, START is refused with EIO, and the module looks
for a late FE_ACK on a backoff from 10 ms doubling to 8 s. A late FE_ACK releases the buffers and the stop
reads **LATE**. The witness for every step is `/sys/class/misc/lemnos-rp1-csi2-<block>/dma` (and `fe_dma`
for the front end), read with `cat`; see docs/kernel-module.md "Stopping a receiver".

## Steps for the current rule
Read `dma` after every step and record all six lines.
1. Start the chain and stream. `dma` reads `state streaming`, `pinned` equals the stream's buffer count and
   `pinned_bytes` their bytes.
2. Stop the receiver first, the sensor still sending. Expected: `state idle`, `stop acked`, `pinned 0`,
   `stop_us` below `bound_us`.
3. Start again (expected to work), then stop the receiver again after stopping nothing else. Expected as 2.
4. Start, stop the sensor first, wait one second, then stop the receiver (the order that wedged in pass 2).
   Two outcomes, both recorded:
   - 4a: the receiver was between frames: `stop idle`, `pinned 0`, `state idle`.
   - 4b: a frame was in flight: `state wedged`, `stop unconfirmed`, `pinned` and `pinned_bytes` the pool's,
     the kernel log line `DMA did not stop: ...`, and `lemnos-ctl show` for the receiver reads health
     `failed` with the words. A START is refused with EIO.
5. After 4b only: start the sensor again and read `dma` every second for 60 s. Record whether it reaches
   `stop late` with `pinned 0` (the log line `DMA stopped late: ...`). If it does, a START works. If it does
   not, record that and leave the block as it is: do not unbind (a wedged unbind keeps the module until
   reboot). Report to Helios.
6. With nothing in flight: start and stop with no sensor stream at all. Expected `stop idle`.

## Results so far (the module before CSI2-STOP-CONFIRMED)
- Pass 2 (2026-11-22, files in pass2-20261122): sensor stopped first, then the receiver: wedged 2/2. The
  wedge held 9 dma-buf objects, 11,538,432 bytes. The stream before it ran 54,277 frames at 119.966 fps with
  0 errors.
- Pass 3 (2026-11-22, files in pass3-20261122): receiver stopped first: clean, 3/3 including a second start
  and stop. M33 passes with the receiver stopped first. No FE_ACK was seen for an idle FORCE.
- So under the old rule an idle FORCE could not be confirmed and a sensor-first stop always wedged until
  unbind. The current rule confirms the idle case and recovers on a late FE_ACK; steps 1-6 check it.

The sections below are the first version of this check (one rule: FE_ACK or wedged until unbind), kept for
what it assumed and why.
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

## Late FE_ACK after START (a stop wedged, its DMA stopped later)
No reset of the CSI-2 channel that discards in-flight DMA is documented for this block. START writes CH_CTRL = 0
and dphy_start resets the host, but neither is specified to abort a running DMA, so this is not relied on.
What the driver does instead: a wedged receiver (and a wedged PiSP front end on it) refuses every START with
EIO until it is unbound (lrc START, l->wedged; lpf START, l->wedged). So no START follows a wedge on that
channel, and a FE_ACK from its DMA can arrive only while the receiver is not streaming, where no frame is
counted. kernel/test/lemnos_rp1_csi2_check.c "late: the late FE_ACK counts no frame" delivers one such late
FE_ACK (fake TCSI_IOC_LATE_ACK, FAKE-ONLY) and checks the counts and the refusal. The fake cannot show a count
before a fix, because the driver never counts it: the test is a regression check, not a fail-before test.
Whether the board acks a FORCE whose DMA never stopped is outcome 4 above.

## Recording
- Put the outcome (A or B, with N and B) in testing/board-runs/CZ-MOD-FE-ACK.md under "Results", with the
  kernel and module revision. Outcome B blocks M1 until the assumption is replaced by a measured one.

## Results
See "Results so far" above.
