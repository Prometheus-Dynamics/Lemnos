# What is still open

This is what is not done on `next`, as of tip 214c0ca7 (2026-11-23). It is read from the tree, the
lander logs and the board-run records. An entry leaves this file when its work is merged and shown.
Items dropped on purpose are not listed here. Items parked for later are in
[`PARKED.md`](PARKED.md). The IDs in backticks are the planning IDs.

States: **in progress** (a session has it now), **next** (not started, no blocker), **blocked** (the
one thing it needs is named), **parked** (in `PARKED.md`).

## Gadget, bridge and lane (2026-11-23 rulings)

- [ ] `SYSTEM-STATS` (next): the scope is not written down; name it when it is picked up.
- [ ] `NO-BLOCKING-CALLS` (a) (next): the scope is not written down; name it when it is picked up.
- [ ] `BRIDGE-LINKS-PUMP` (next): the bridge's links thread (`service/orion/src/links.rs`) does not pump
      its Orion session while it waits at start, so Orion warned the session was quiet 6001 ms past its
      6000 ms idle time on `e199fcb9`. Pump while waiting, or open the session only when ready.
- [ ] `GADGET-WAIT-NOT-POLL` (next): usb-gadget waits on the function's descriptor, not its 20 ms ep0
      poll and 1 ms idle data-stream poll. Then say what a camera stream costs in wakes per second.
- [ ] `LANE-ONE-WAKE` (next): one lane-thread wake per call (`engine/host/src/lane.rs`). Land it apart
      from the other rows and report the per-call cost before and after. Also find the caller of the
      ~297,000 lane calls seen between rounds while the link pair was lent (open, unexplained).
- [ ] `READ-WHEN-WATCHED` (blocked: the Orion pin move to `a3e42986` (I99); `origin/next` pins Orion at
      `172054f4`): a device's values are read only while its Orion topic has a subscriber. Give values
      topics an `expiry_ms`.
- [ ] `USB-LINK-WEDGE` (next): the reproduction is not run. `GADGET-REBIND-VISIBLE` closes the rebind
      part only. Run the 10-run off-board QEMU reproduction (comm core holds the FunctionFS pair, traffic,
      restart `lemnosd`, 90 s watch) and record the capture list. Not closed by the rebind fix alone.
- [ ] `GADGET-STUCK-RECONNECT` (blocked: the `USB-LINK-WEDGE` cause or its reproduction attempt): the
      three-part witness and one soft reconnect on backoff. It must be shown not to fire for an absent
      or sleeping host.
- [ ] `FFS-HELD-ERRNO` (next): case 6 prints the errno; `docs/wire.md` and `R1f.md` name EBADFD where
      `ESRCH` was seen; a same-namespace enable refusal says "still held".
- [ ] `READINGS-AGREE` (next): an Option-of-number publishes when `Some` and is absent when `None`;
      a value the bridge cannot publish is marked so, never listed live. A text value type needs its
      encoding lines sent to the coordinator first.
- [ ] `HEALTH-UP` (next): the scope is not written down; name it when it is picked up.
- [ ] `SAMPLE-RETRY` (next): the scope is not written down; name it when it is picked up.
- [ ] `BRIDGE-ENV-DOC` (next): `lemnos-ctl --bridge-env` writes only LINKS, FILES and STREAMS. The docs
      say it is a fragment to assemble after the env example, or the tool writes the whole file.
- [ ] `PROXY-PASSES-FDS` (next): `service/orion/tests/support/proxy.rs` copies bytes and drops passed
      fds, so topics never arrive through it. Make it pass fds, then add the integration test.
- [ ] `PUSHES-NOT-STALE` (rest) (next, after `PROXY-PASSES-FDS`): `e15c1947` landed the not-ready read.
      Still open: the bridge publishes its starting state for every device as soon as the topic is
      declared, and the test that cuts the files session with a subscriber held open.
- [ ] `PSTORE-READ` (next, low): `slots info` crash_log cannot read `/sys/fs/pstore` (EACCES).
- [ ] `validate --root` (next): the validate step gains a root option.
- [ ] `STAGE-END-LINE` (next): one flake in the gadget check stage stanza.
- [ ] `BOOT-HISTORY` (next): the fell-back reason, the utc-unknown flag, and the JSON form of `boots()`.
      No topic.
- [ ] `LOG-LEVEL` (next).
- [ ] `SIM-SYNTH-SENSOR` (next, after `LOG-LEVEL`).
- [ ] `SIM-CAMERA-CONTRACT` (next, after `SIM-SYNTH-SENSOR`).
- [ ] `SIM-PISP-INSTANCE` (next, after `SIM-CAMERA-CONTRACT`).
- [ ] `SAME-DIGEST-REPUSH` (next): a same-digest manifest re-push answers OK while its job runs. Folded
      into the next flash row; no flash row is open yet.

## Flakes and their confirmation

- [ ] `FLAKE-K230-WATCHDOG` (next): the fix landed (`bf5b6d17`). The before and after loops, eight times
      over, are not recorded. Run them and record the result.
- [ ] `FLAKE-BE-WAIT` (next): the bounded wait landed (`7543ae9d`); the identity fix and diagnostic
      landed (`a3f6caba`), and its timeout cause is still open. Run the 200-run loop and record the cause.
- [ ] `FLAKE-FE-START` (next): the fix landed (`62c90277`). The 200 and 200 fail-before and fail-after
      loops are not recorded. Run them and record the result.
- [ ] `M9` (blocked: a recurrence of the four unexplained flakes; 150 plus 150 loops are clean).
- [ ] `FLAKE-CETE` (blocked: a recurrence, so the recording can capture per-tick wall time and wchan).

## Drivers and checks

- [ ] `GATE-BOUNDS-WARM` (blocked: about 20 warm green trains past `3878b291`; the count is not recorded
      after 2026-11-19). Then recompute every bound from warm greens only.
- [ ] `GAIA-XZ-FIXTURE` (next, low): copy Gaia's `multi-block-unsigned.img.xz` and
      `multi-block-signed.img.xz` into the system-flash tests, verify the hashes, and record the source.
      The Lemnos-made fixtures (`6e13aa1a`) are in.
- [ ] `RKJPEGE` (blocked: a board probe for the ECS buffer bound and the hardware reach, on the reference board or a
      ROCK 5C; the PC bound landed in `baeac12a`).
- [ ] `SILENCE` (blocked: the F2i board run, which needs the F2 camera board session; the sim half is in `69615d96`).
- [ ] `G10` (blocked: a ruling on what G10 names; no commit names it).
- [ ] `FLASH-EXTENT` (blocked: the host-side partial-line flush in the console reader, then the approved
      R1 step; held at `57b45592`).
- [ ] `CMA-READING` (blocked: a read-only board #2 session; the board was handed to another session on
      2026-11-21).
- [ ] `B8-PHASE-2` (blocked: a read-only board #2 session; the variants that need a reboot need the board back).
- [ ] `WDT-CHAIN` (blocked: a board #2 board run for the stamps; code is in `49408740`).
- [ ] `M36-GAP` (UNCLEAR, kept open: no commit, record or note names it; needs its definition).
- [ ] `M41b` (blocked: the next board #2 board run, to measure the 250 ms limit; code is in `3566f592`).
- [ ] `SLOT-MACHINE-ID` (blocked: a board #2 board run for steps 4, 6 and 7; code is in `e0da1dc3`).
- [ ] `DTB-NAME-RUN-A` (blocked: a read-only board #2 session that can read the firmware DTB file name;
      `vclog` is not on the image).
- [ ] `DAEMON-SECCOMP` (blocked: a read-only board #2 session for the two seccomp reads on the M2 image).
- [ ] `HANG-UNDER-LOAD` (next; DEBT; after the speed batches): at load 160 the host's 2 s call deadline
      declared a working device hung, abandoned it and rebound it (lander t8, a fan controller on
      fakes), and the rebind reset the fan's slew: the new instance started at the curve. Under camera
      load on a board the same false hang would reset real fan control. To settle: should the deadline
      count only the time the call was runnable (its thread's run and run-queue time, not wall time),
      or what load margin 2 s has on the board.

## Blocked

- [ ] `MR1` (blocked: the flasher session's RK3566 471 sender subcommand; then the coordinator's go for step E, which reboots the ZERO 3E).
- [ ] `K230-USB` (blocked: the other session retiring `crates/flash/k230`).
- [ ] `BENCHREF` (blocked: the user's yes for the write run of `bench.sh --real` on board #2).
- [ ] `GROW` (blocked: the first-boot board run on `R2-IMG`; `GROW-E2FSCK` also needs a ruling).
- [ ] `R2-IMG` (blocked: the D1 image, built by Gaia in another session).
- [ ] `M37` (blocked: a camera on a reference board for the camera half).
- [ ] `M34` (blocked: a camera on a reference board, then `R2-IMG` and the M34 CRC).
- [ ] `PISP-RUN` (blocked: a camera on a reference board).
- [ ] `R-PASS` (blocked: a camera on a reference board).
- [ ] `F2` (blocked: `R2-IMG`; a camera on a reference board also needed).
- [ ] `LIT` (blocked: a camera on a board and a lit scene, by hand).
- [ ] `F3` (blocked: `F2` first; then the consumer repins).
- [ ] `BR-OPEN` (blocked: `R2-IMG` and the M34 CRC; dependents `C3`, `C4`, `C6`, `D5` to `D17`, `A35`).
- [ ] `CZ-MOD-FE-ACK` (blocked: `R2-IMG` with `lemnos.ko` loaded).
- [ ] `AFTER-IMAGE` (blocked: `R2-IMG`; parked in `PARKED.md`).
- [ ] `BB-GAIA` (blocked: the K230D boot-source key fact move to `8e05f304`, which is not in this clone; verify on origin).
- [ ] `BB-IMG` (blocked: `BB-GAIA` and `BB-OTHER`).
- [ ] `BB-FLASH` (blocked: `BB-IMG`).
- [ ] `UB-RUNS` (blocked: `BB-FLASH`, then a board run).
- [ ] `BB-OTA` (blocked: `BB-FLASH`).
- [ ] `BB-OTHER` (blocked: the other session's board facts for the Aura, the K230D and the A7A and A7Z, and the
      A733 watchdog path).
- [ ] `FEL` (blocked: an Allwinner FEL-class board, which is not on the bench).
- [ ] `RKNPU-E32` (blocked: a ROCK 5C, which is not on the bench).
- [ ] `RKJPEG-DEC` (blocked: a ROCK 5C for the board probe; the probe code is in `4e1aac80`).
- [ ] `A733ENC` (blocked: the C1b proof from the other session, and an A7S, which is not on the bench).
- [ ] `AI2D` (blocked: the K1 proof from the other session).
- [ ] `RKAIISP` (blocked: the AU1 proof from the other session, and an Aura, which is not on the bench).
- [ ] `GAIA-SLOT-FIELD` (UNCLEAR, kept open: Gaia's ask is not in any note read here).
- [ ] `ORION-GIFT-END` (blocked: the Orion API, in another session).
- [ ] `V1G-BOARD` (blocked: the user's hands; a second route to board #2, and a redesign of check 6 so
      `lemnosd` never stops).
- [ ] `BAY-FIRST-BOOT` (blocked: the user at the board for step 0, and a reboot of board #2).
- [ ] `B8` (blocked: the user's hands; settings 2 and 8, tilt rig and compass).
- [ ] `B9` (blocked: the user's hands; move the board away from the fan and steel for the y-axis check).
- [ ] `B10` (blocked: the user's hands; orientation tilt rig and the two-rate compass check).
- [ ] `B11` (blocked: the user's hands; checks 3, 5 to 8 and the -0.369 A current read, with a meter).
- [ ] `B15` (blocked: the user's hands; logic probe for check 2 and the second GPIO holder for check 6).
- [ ] USB VBUS physical check (blocked: the user's hands; optional, since `off` now cuts the port).
- [ ] `G22` (blocked: the user's OK on `notes/G22-23-scope.md`, then the other session).
- [ ] `EEPROM-COMPARE` (blocked: the coordinator's approval; a candidate, not approved).

## Recorded, no fix (not open work)

- `DWC2-NPTX-FIFO`: dwc2 on board #2 logs "Invalid parameter g_np_tx_fifo_size=1024". The driver's own
  default is clamped to the hardware's 32 words, which hold ep0's 64-byte packets. Nothing sets it.
  Recorded only.
