# L1 board chain run: the steps for the next board session (NOT RUN)

Purpose: the camera chain sensor -> receiver -> front end -> back end, with links, measured on the reference
board (Raze #2, Compute Module 5, kernel 7.2.9-v8-16k). Passing this is the L1 candidate. Nothing here has run yet.

Start only when I2f and I2e are merged. Record the commit at the top of the run's record.

## Rules for the session (from the board prompt and the handover)

- Take `/run/lock/raze-gpu.lock` and `/run/lock/board-gpu.lock` on the board with `flock -w 900`, keep holds short.
- Stop only PIDs you started; write each to a pid file on the board. No pkill/killall/pattern kills.
- Our files under `/tmp/lemnos-run/`; `/tmp/lemnos-run/raw-0aa30510/` must stay intact.
- Build on NVMe/SSD, not the HDD; aarch64 binaries; check the board's libc before copying dynamic binaries.
- Restore the board when done (six services active, stock drivers bound, `lemnos` unloaded, heap permissions
  restored, driver_override empty). Report every change.

## Steps, in order (each recorded as it finishes)

1. Baseline: model, kernel, loaded modules, the camera DT nodes (receiver, PiSP FE/BE, sensor), services and
   what holds `/dev/video*` and `/dev/media*`, free memory, CMA size. No changes.
2. Raw path regression: 9000 frames as root and as the `lemnos` user (lemnos-camera-proof, pattern 1, 910 lines).
   Expect 114.8 fps, 0 gaps, 0 dropped (F2e record).
3. Front end with links: 600 frames through sensor -> receiver -> front end (lemnos-chain-proof). Expect the
   receiver's counters and the sensor latch to READ while the front end streams (failed with EBUSY before F2f1).
4. Back end: queued jobs (1000 jobs, 0 dropped), paced 500 at 100 jobs/s.
5. FULL CHAIN: 600+ frames with frames and timestamps. Record: frame rate; CLOCK_MONOTONIC ns timestamps
   monotone from frame start; sequence continuity; dropped counts; the receiver's counters and the sensor's latch
   check read while the chain runs; sharpen and resample outputs (640x400 readout must be non-zero).
6. Recovery: receiver unbind/rebind during the chain, and back-end unbind/rebind, with counts before and after.
7. Sensor start time after the F2d item 3 fix (F2f-d work), and the sensor-stop check in testing/board-runs/F2f2.md.
8. Heap permissions: through the generated fragment (Q2i) if merged, else by hand and restored.
9. Lit-scene exposure checks only if the scene is lit; say which.
10. LED ring pixel order only if it can be told; do not spend long.

## If frames do not flow

Stop at once and report the evidence (the kernel log, `lemnos-ctl` health and events, the stream's status) before
going further: that is the first hardware evidence that kernel-backed streams work through the shared ring.

## Records

testing/board-runs/L1-chain.md (this run's record), testing/board-runs/F2f1.md and F2f2.md (their checks),
docs/hardware-status.md (rows), the runbook's PiSP sections marked CONFIRMED / NOT RUN.
