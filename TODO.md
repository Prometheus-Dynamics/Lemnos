# What is still open

This is what is not done on `next`, as of tip 0d37cc55 (2026-11-21). It is read from the tree, the
lander logs and the board-run records. An entry leaves this file when its work is merged and shown.
Items dropped on purpose are not listed here. Items parked for later are in
[`PARKED.md`](PARKED.md). The IDs in backticks are the planning IDs.

States: **in progress** (a worker has it), **starting** (not yet started, no blocker), **blocked** (the
one thing it needs is named).

## Run control and units

- [ ] `RESTART-UNIT` (in progress): the helper crate is green; still to do are the driver integration,
      packaging, and the systemd QEMU live test.
- [ ] `WDT-ORDER` and `PING-ON-OPEN` (in progress): the kernel and tests are running.
- [ ] `PWM-SYSFS-UNIT` (in progress): proven in QEMU under systemd 258 and 259 semantics.
- [ ] `UNIT-GRANTS` (in progress): the PIO `/dev/pio0` and `/dev/mtdN` grants are approved; the three
      designs go to the coordinator together.
- [ ] `SLOTS-FOREIGN-DISK` (in progress).
- [ ] `INSTALL-PROGRESS` (in progress): the wire shape is sent when it is in its gate.
- [ ] `GATE-BOUNDS-WARM` (in progress).
- [ ] `TEST-SOCKET-PATH` (in progress): the fail-before is running.
- [ ] `GAIA-XZ-FIXTURE` (in progress): the gates are running.
- [ ] `SHM-BELL-LOAD` (in progress): ten runs at load of 60 or more, alongside a live lander train.

## Flakes and their confirmation

- [ ] `FLAKE-K230-WATCHDOG` (in progress): before and after loops, 8 times over, are running.
- [ ] `FLAKE-BE-WAIT` (in progress): a kernel build, then a 200-run loop. The bounded wait has landed.
- [ ] `FLAKE-FE-START` (done in `62c90277`, confirmation in progress): the 200 and 200 fail-before and
      fail-after loops close it.
- [ ] `M9` (blocked): a recurrence has not been seen in 150 plus 150 loops; the `BE-WAIT` 200-run loop
      is watching for it.
- [ ] `FLAKE-CETE` (in progress): recurrence recording.

## Drivers and checks in progress

- [ ] `VALIDATE-RES` (in progress): a `uses` naming a bus, controller or line the backend does not
      have is caught by `lemnos-ctl validate`, not only at bind.
- [ ] `RAWBUS-RO` (in progress): a read-only raw-bus device that still reads its registers.
- [ ] `WDT-MUT` (in progress): a mutation test for the watchdog driver.
- [ ] `ASKBUF-MOCK` (in progress): mock tests for `ask_buffer` in `k230isp` and `rkisp`.
- [ ] `REFTOML` (in progress): corrections to `testing/boards/reference.toml`.
- [ ] `DOCS` (partly done in `0a84030b` and `9a9e9bf7`; in progress): the rest of the stale documentation.
- [ ] `CZ-GUEST` and `CZ-FIX` (partly done; in progress): the last C file,
      `testing/qemu/test_block/lemnos_test_helpers.c`, ported to Rust.
- [ ] `FL1` (partly done in `d4942290` and `6a6992ad`; code-only rest in progress): the flasher split.
- [ ] `RKJPEGE`, `SILENCE`, `STANDIN`, `G10` (starting): each is settled from the code and
      the records; a fix lands where the change is missing.
- [ ] `FLASH-EXTENT` (in progress on a board): the K230 run R0 exited 0 and returned to ROM. R1 needs a
      ruling. Held at `57b45592`.
- [ ] `CMA-READING` (in progress on board #2): a read-only sweep.
- [ ] `B8-PHASE-2` (in progress on board #2): read-only variants only. Variants that need a reboot are
      blocked on a reboot of board #2, which this pass does not allow.
- [ ] `WDT-CHAIN`, `M36-GAP`, `M41`, `M41b`, `SLOT-MACHINE-ID` (in progress on board #2): the read-only
      parts. Parts that need a reboot or a slot hand-over are blocked on a reboot.
- [ ] `DTB-NAME-RUN-A` (in progress on board #2): a read-only `vclog -m` read.
- [ ] `DAEMON-SECCOMP` (in progress on board #2): two reads.

## Blocked

- [ ] `MR1` (partly done in `888a5742`): step E enters maskrom, which reboots the ZERO 3E. Not allowed
      in this pass.
- [ ] `K230-USB` (partly done): the other session retires `crates/flash/k230`, in another session.
- [ ] `BENCHREF` (partly done): `bench.sh --real` writes to the target disk. Needs a yes for a write run
      on board #2.
- [ ] `GROW` (partly done in `382ea5c8`, `935ce31b` and `29441a58`): the board check grows a partition
      and cuts power. Persistent and needs hands.
- [ ] `R2-IMG`: the D1 image, built by Gaia in another session.
- [ ] `M37` (partly done) and `M34` (partly done in `9ca0c233`): a camera on a board, and `R2-IMG`.
- [ ] `PISP-RUN`, `R-PASS`, `F2`: a camera on a board, and `R2-IMG`.
- [ ] `LIT`: a camera and a lit scene, by hand.
- [ ] `F3`: `F2` first. The Lemnos side is done (T134, T139, T141); the consumer repins come after.
- [ ] `BR-OPEN` (partly done in `T30`): `R2-IMG` and the M34 CRC. `C3`, `C4`, `C6`, `D5` to `D17` and
      `A35` all run on the new image.
- [ ] `CZ-MOD-FE-ACK` (board): `R2-IMG`, an image with `lemnos.ko`.
- [ ] `AFTER-IMAGE`: `R2-IMG`.
- [ ] `BB-OTHER`, `BB-GAIA`, `BB-IMG`, `BB-FLASH`, `UB-RUNS`, `BB-OTA`: the facts from the other session and the K230D
      boot-source key from Gaia. The rest of the chain follows.
- [ ] `FEL` (partly done in `T1`): an Allwinner FEL board, which is not on the bench.
- [ ] `RKNPU-E32`, `RKJPEG-DEC` (board): a ROCK 5C, which is not on the bench.
- [ ] `A733ENC`: C1b proof from the other session, and an A7S, which is not on the bench.
- [ ] `AI2D`: K1 proof from the other session, in another session.
- [ ] `RKAIISP`: AU1 proof from the other session, and an Aura, which is not on the bench.
- [ ] `I6d`: Orion DO item 6, in another session.
- [ ] `GAIA-SLOT-FIELD`: Gaia asks, in another session.
- [ ] `ORION-GIFT-END`: the Orion API, in another session.
- [ ] `V1G-BOARD`: the user's hands. A second route to board #2 is needed, because check 6 stops lemnosd.
- [ ] `BAY-FIRST-BOOT`: the user at the board (step 0), and a reboot of board #2.
- [ ] `B8`, `B9`, `B10`, `B11`, `B15` hands checks, and USB VBUS: the user's hands (tilt rig and
      compass; logic probe; load, unplug and chip swap; meter; moving the board away from the fan).
- [ ] `G22`: the user's OK on `notes/G22-23-scope.md`, then the other session.
- [ ] `ROUTE-A-BOUND`: not approved; the other session's F3 dropped route A, and DROP is proposed.
- [ ] `EEPROM-COMPARE`: not approved.
