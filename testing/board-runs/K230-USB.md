# K230-USB board run: the K230 stage on the FL1 flash wire

Status: **run on the board, 2026-11-16 to 2026-11-18: the read-only steps (R0 to R3), the bounded write test, and a
whole image written and verified on the card over the boot ROM's USB path, one load per piece.** The stage's own SD
host reads and writes the slot's card; the image region's SHA-256, computed on the board, equals the source's with
its two boot-header sectors held as zero (those two sectors are not written in this session, so the card does not
boot). The FL1 stage's USB device path (blocker B2) is still untested on board: the bytes went through the ROM's
load-and-run instead. The runs are in "Board steps", in order; the entries before R3c read their "card did not
answer" as an empty slot, which R3c showed to be the host's own fault.

## Hypothesis

The K230 (K230D AI-CAM, board B, SD boot) can run the FL1 stage from the ROM's USB download mode. The stage
arms both watchdogs first, reads the arming back, serves the FL1 session over the dwc2 device path, and every
bounded wait ends in a SoC reset that returns the board to the ROM's download mode. No step writes the card
until its return path is proven.

## Facts this plan relies on

- Observed (run-L2, board B, 2026-11-16): the ROM accepted a 51,992-byte load at `0x80360000` in 1000-byte pages
  and started it. The stage then raised `CPU Exception: NO.2` (illegal instruction) at `mepc 0x80360006`
  with `x30 = 0`, and never answered identify. **Blocker B1 (below).**
- Observed (run 1, same board): a soft reset from the stage returns the board to USB download mode in about
  one second.
- Observed: `GET_CPU_INFO` answers exactly `K230` (4 bytes). Port path `3-4.2.1.4.1` (bus 3) is the binding.
- Vendor source (SDK clone `/mnt/nvme-wd/styx-work/k230-research/src/k230_sdk`, commit `7e302f733`):
  - `src/little/uboot/arch/riscv/cpu/k230/platform.h:47-53`: `SRAM_START_ADDRESS 0x80000000`,
    `FIRMWARE_CODE_ADDR 0x80300000`, `FIRMWARE_CODE_MAX_LEN 512 KiB`, `UART0_BASE_ADDR 0x91400000`.
    The load address `0x80360000` used by run-L2 is inside that 512 KiB window.
  - `src/little/uboot/configs/k230d_canmv_burntool_defconfig:2`: the burner U-Boot links at `0x80300000`
    (`CONFIG_SYS_TEXT_BASE`). Our image links at `0x80360000` because run 1 ran there. Both are inside the
    SDK's window; the SDK does not name the ROM's own limit.
  - `src/little/uboot/configs/k230d_canmv_burntool_defconfig`: `CONFIG_BOOTDELAY=1`, `CONFIG_AUTOBOOT_STOP_STR="stop"`.
  - `board/common/gen_image_cfg/genimage-sdcard.cfg`: SPL copies at sectors 2048 and 3072, environment at
    `0x1e0000`, U-Boot at sector 4096 (from the plan's earlier notes; the file is in the SDK clone).
- `soc_glb_rst` (`0x91102060`, value `0x10001`): the SoC reset the stage uses as its return. Sourced in
  `testing/crates/k230-usbboot-stage/src/sysctl.rs` (kendryte `sysctl_boot.h:47`, `sysctl_boot.c:43-49`).
- Not verified: whether the ROM returns to download mode after a reset with a bootable card (the vendor
  documents the order; nothing observed). Step R5 is the test.
- Not verified: the watchdog clock (the stage assumes the lowest clock, 1 MHz, for its longest period).

## Blockers before any board contact

- **B1. The `NO.2` at `0x80360006` (run-L2): cause not established; checks added.** Findings, PC side:
  - The image run-L2 loaded is not in the tree: the file at `lemnos-gates/tmp/k230-usbboot-image/image.bin`
    has a different SHA-256 and size. The current build of the tree gives the same 48,544 bytes as that file, and
    `check.sh` passes on it. So run-L2's bytes cannot be read; the run record must carry the image's SHA-256.
  - The current image is `riscv64imac` (no V, no F or D). Its code at `0x80360006` is the second half of the
    `addi` at `+4`; no branch or jump in the image targets `+6`, so the current entry cannot reach it.
  - The trap state in the capture: machine mode, `mstatus.VS` Off, `FS` dirty. A vector instruction would trap
    with VS Off, but the image has none, so the vector hypothesis does not fit this build.
  - Added: `drivers/k230-usbboot-image/check-entry.py`, run by `check.sh` on every build. It refuses vector and
    floating-point instructions, CSRs outside the allowlist, and branch targets in the middle of an instruction;
    its self-test checks each rule.
  - Before any R step: the PC dumps the exact bytes loaded and the bytes at the trap address; the SHA-256 is in
    the run record. If the same image still traps, B1 stays open.
- **B2. The stage's device path has not run on hardware.** The dwc2 `UsbHw` (in `drivers/k230-usbboot-sys`)
  is written from the SDK's dwc2 registers; its enumeration, EP0 and bulk sequences are unverified. The
  adapter's bench checks are listed in `testing/crates/k230-usbboot-stage/src/fl1/usb.rs`. The first run's
  plan and its PC rehearsal: "B2: the first run of the stage's own USB device path" below. Run 1: the stage stopped
  itself before USB because the `time` CSR does not run in the ROM's state; **still open**.
- **B3. Gaia's kernel hang before earlycon** is Gaia's; it is not a precondition here (the stage does not
  boot the kernel in this plan).

## PC rehearsal (no board)

Command: `cargo test -p lemnos-k230-usbboot-stage --test fl1`. The stand-in is the same crate at the same rev,
with the controller and the card replaced by models: `Host` (a scripted USB host), `Ram` (a card), and `wdt::Model`
(the watchdog register model, with a fault per way the block can disagree with its arming). The stand-in
must fail like the real thing, and it does in these cases:

- `CrIgnored`: CR writes dropped. The entry stops before any USB or SD bring-up.
- `CounterStuck`: CR enabled, the counter frozen. The entry stops before bring-up.
- A host that never configures: the session ends at the enumeration limit; the cause is reported and the SoC
  reset is the last bus write.
- An identified host that goes quiet: the session ends at its idle limit with `Serve(Idle)`; the report prints
  it and the SoC reset is the last write, with no watchdog kick after it.

## Board steps (in order; each has a gate)

Every step is run by the PC-side flasher (Atlas) or by the coordinator in a local session with the
board-run lock. Nothing in this list runs before the review word.

**Run record, 2026-11-16, board B (rev 651454f5).** The board re-enumerated after a power cycle at Bus 003,
Dev 059 (`29f1:0230`, port `3-4.2.1.4.1`); the CH342 is at Dev 058 with the same by-id console paths. The
run holds `/run/user/1000/lemnos-k230d.lock` (the Raze lock was lost in the reboot; Helios is raising it).
No card is in the board, and the run does no card I/O: no `--sd-controller`, so report mode never reaches the
SD path (`run_report` in `drivers/k230-usbboot-sys/src/image.rs`).

**Images (sha256 before the host's mode patch; the patched image is what is sent).** Built by
`drivers/k230-usbboot-image/check.sh` at this rev: `tmp/k230-usbboot-image/image.bin`, 48,544 bytes. Recorded in
the run record at run time, with the bytes at the old trap address 0x80360006 (the entry file's `+6`, which is
`0f 81 03 a4`, the middle of the `addi` at `+4`).

**R0. The return path by SoC reset (the `soc` mode image).** Only the return is tested; no report, no records.
- *Hypothesis:* a SoC reset (`soc_glb_rst`, `SOC_AT_ONCE`) taken right after the start returns the ROM to
  download mode. The device disappears, then comes back as `29f1:0230` answering `K230`, within 10 s of the start.
- *Steps:* identify (must be `K230`); load the image with mode `soc`, sd none, at `0x80360000`, pages of 1000; start;
  time the disappearance and the return by polling the port; no records are read.
- *Pass:* disappears within 2 s of the start and returns `K230` at the same port path within 10 s.
- *Stop (no further step):* the device does not return within 10 s, returns with another identity, or the port path
  changes. Report the evidence; do not run R1.

**R1. The watchdog return (the `report` mode image).** Only after R0 passes.
- *Hypothesis:* the report image prints its records over USB and takes no SoC reset; the watchdog (WDT0, TOP 11)
  returns the ROM to download mode. The return comes within three periods: the period is `2^27` cycles at the
  lowest clock (1 MHz), 134 s, so the bound is 420 s.
- *Steps:* identify (`K230`); load the image with mode `report`, sd none, at `0x80360000`; start; read the records
  the stage sends while the device is up; time the disappearance and the return.
- *Pass:* the device returns `K230` at the same port path within 420 s of the start, and the disappearance is
  between 100 s and 420 s after the start (the watchdog, not an early reset).
- *Stop:* no return within 420 s, a return before 100 s (an unexplained reset), or a changed port path. Report; no
  hands fallback; the run is blocked.

**Run record, 2026-11-16 22:45 (board B, rev 651454f5, no card, lock `/run/user/1000/lemnos-k230d.lock`).**
- Image: `2eba415f…` (48,544 bytes, before the mode patch). The patched image sent: `soc` `9c007d91…`, `report`
  `8cb09874…`. Bytes at the old trap address `+6`: `0f 81 03 a4`.
- Capture (`K230-USB-cap-R01`, read only, `tmp/K230-USB/bench/R01-console.log`): 7 lines. Two `boot failed with exit
  code 13` lines, at each return (the ROM's SD boot fails with no card, then the ROM goes back to USB download).
- **R0 (`soc`): PASS.** Identify `K230`; start at t0 = 0.101 s; gone at 0.432 s after the start; back as `K230` at
  0.672 s on `3-4.2.1.4.1`. Record `R0-soc.txt`. Mechanism: the reset returns to download mode through the ROM's
  card-failure path (no card), not a direct ROM reset. A bootable card would boot instead.
- **R1 (`report`): STOP by its own rule.** Start at t0 = 0.195 s; no records (`records end: no answer after 500 ms`);
  gone at 0.931 s after the start; back as `K230` at 1.384 s. The rule's lower bound (100 s) assumed the watchdog
  clock is 1 MHz, the lowest clock. The observed return (about 0.9 s to 1.4 s) fits a fast watchdog clock, so
  the watchdog did return the board: the stop is the bound, not a failed return. **Not yet decided:** whether the
  bound is corrected to the bench clock (this needs the clock measured) or R1 is re-run with a corrected bound.
  No further step ran.
- Board at the end: `29f1:0230` at Bus 003 Dev 061, download mode. Capture stopped by unit name. Lock released.

**What reset the board: the evidence, 2026-11-16 (analysis only, no board contact).**
- *Capture* (`R01-console.log`, 7 lines, `if02` only): `1794894323.826145 !Sboot failed with exit code 13` and
  `1794894325.378365 boot failed with exit code 13`. No banner, no `CPU Exception`, no mepc or mcause line, no ROM
  banner before the exit-13 lines, and nothing on `if00`. The capture has no timestamps relative to the probe, so
  the alignment to each run is within about one second.
- *R0 reset source: our entry.* The entry writes `!` then the cause letter to the UART before its SoC reset
  (`image.rs`, label 90). The first byte pair `!S` is the soc-mode branch (`t5 = 0x53`). So R0's reset came from our
  entry's `soc` path. The ROM's `boot failed 13` follows it with no card.
- *R1 reset source: not established.* No `!` (the entry's own cause path did not run), and no report banner, which
  `run_report` prints as its first action after `configure_if_unset`. So the image did not reach report code. A
  reset with neither line is not the readback failure path, which also prints `!m` or `!M`. The reset came between
  0.73 s and 0.93 s after `t0`: the probe polls every 200 ms, so the disappearance is known only to within one poll.
- *Images.* Sent `soc` `9c007d91…` and `report` `8cb09874…` both match the probe's record. Each differs from the
  built file only in the mode block (rodata, `0xb810`); their code bytes are identical to the ELF. `check-entry.py`
  passed on the ELF listing, which covers that identical code (`check-entry.py` was not run on the patched `.bin`
  itself). The entry's `+6` word is `0f 81 03 a4`, the second half of the `addi` at `+4`: nothing branches there.
- *Watchdog arithmetic.* The entry arms WDT0 with TORR `0xbb`, so TOP 11 and a period of `2^27` cycles. The SDK's
  `sysctl_clk.c` parents `WDT_0_CLK` to `SYSCTL_CLK_ROOT_OSC_IN` (the 24 MHz oscillator; a divider is not located).
  At 24 MHz the period is 5.59 s; at the 100 MHz upper bound of the stage's own model it is 1.34 s. Neither fits a
  reset at 0.73 to 0.93 s. The stage's 1 MHz lower bound (134 s) is further still. **So "our watchdog fired as
  computed" is refuted at both clocks, and the R1 bound of 100 s to 420 s was wrong.** The earlier observation that
  the board returned at 1.384 s is a return, not a watchdog period.
- *Silent failures before the first record.* In R1 the banner did not print, so the USB report path never ran; the
  "records end: no answer" is a consequence, not a separate failure. The steps before the banner are the readback
  loops (both bounded), the `call` with its stack, `PadGuard::new`, and `configure_if_unset`. Any hang there would be
  reset by the watchdog at its period (5.6 s at 24 MHz), not at 0.9 s.
- *Conclusion.* **R0: the soc path of our entry reset the board (`!S`), and the return is the ROM's card-failure
  path.** **R1: the reset is unexplained; our watchdog did not fire as computed, and the stage did not trap to a
  printed CPU exception.** Deciding the cause needs a board read that prints first (the banner or a `!`-style
  marker before any arming), with absolute timestamps in the probe. No rerun until that is decided.

**R1b: the diagnostic run (2026-11-16, one run; no card; lock `/run/user/1000/lemnos-k230d.lock`).**
- *Image.* The report image with UART markers and a copy of the watchdog registers; no new behaviour. Built
  at `tmp/K230-USB/r1b-build/k230-usbboot-image/image.bin` (49,424 bytes, sha256 `f30fddb7…`). Sent
  (patched `report`, sd none): **`0fdb2d63c3008cb1f296f05c9f194ff00d44efcd212d7e1a396da5972958a51d`**, 49,424 bytes.
  `check-entry.py` on the build's ELF listing: `entry check holds`. The sent file's `.text` (45,028 bytes) is
  byte-identical to the built `.text`, and the mode block is at `0xbb80`, in `.rodata`.
- *Markers, in order, on the UART:* `!B` at entry, before any arming. `W` after WDT0 is armed and TORR is read back.
  `K` after WDT0's readback. `C` just before the call to the report. `P` after the report's pad guard, `U` after the
  UART is configured; then the diagnostic lines and the report banner. The steps "after the clock setup" and "before
  and after the USB re-init" do not exist in the report path (it does no clock setup and starts USB after the
  banner), so no marker is placed there.
- *Watchdog state.* Read before any arming and saved (`lemnos_diag`), then printed after `U`: WDT0 and WDT1 CR, TORR,
  CCVR as the ROM left them. Also WDT0's TORR and CR after arming, and the TOP as the low nibble of TORR.
- *Hypotheses:* H1, the entry never runs (`!B` absent). H2, the ROM left WDT0 enabled with a count (the pre-arm
  values show it). H3, a hang between `C` and the banner (no `U` or banner). H4, the watchdog fires at its own
  period: TOP 11 is `2^27` cycles, 5.59 s at 24 MHz, 1.34 s at 100 MHz; the observed reset (0.73 to 0.93 s after the
  start) fits neither, so H4 is refuted unless the clock is about 145 MHz.
- *Pass of the run:* a complete marker sequence to the banner, with the watchdog values and TOP printed; any
  reset must then be reported with the last marker and the time.
- *Stop:* nothing else runs after this one run; no fix on the board side until the cause is known.
- **Outcome of the one run: not run past identify.** At 22:55:33 the probe's identify got `EPROTO`
  (`the boot ROM stopped answering and stays enumerated`). The ROM was deaf after idling since R1. Nothing was loaded:
  the probe refuses to send an image to a deaf ROM. The board stays enumerated (`29f1:0230`, Dev 061) and deaf.
  No markers were produced, so H1 to H4 are untested. Recovering a deaf ROM needs a reset from the user (RST or the
  KEY), which is outside this plan's paths; not done.

**Identify after the RST, and the exit codes (2026-11-17, read only, one identify).**
- *Identify, 00:01:09 (`identify-deafcheck.txt`).* The board re-enumerated at Dev 062 after the RST at 23:20:42. A
  single identify at 00:01:09 answered `K230`. So the ROM was not deaf about 40 minutes after its re-enumeration. The
  earlier `EPROTO` at 22:55 came about 10 minutes after the R1 run that had sent it an image and a start. The idle
  model is not supported by this: the deaf state followed a load, not idleness alone (not settled).
- *Exit codes 13 and 19 (`boot failed with exit code N`).* The message is not in the SDK clone
  (`/mnt/nvme-wd/styx-work/k230-research/src/k230_sdk`, `7e302f733`). The SPL prints `uboot boot failed` and returns
  its code from `k230_img_load_boot_sys` (`k230_spl.c:150-156`, `k230_img.c:785-844`), but no table maps 13 or 19 to a
  cause. The Gaia repositories (`gaia`, `Gaia-Image-Builder`) have no such string either. **The meaning of 13 and 19 is
  not established.** So the difference cannot be attributed to a cold RST versus a soft reset, and this record does not
  say a card is present. The code 19 was printed in the earlier card-era capture (`boot-capture-2.log`, 18:56 on
  2026-11-16); that is a reason to check the slot, not evidence of a card. The user states the card is out.
- *Re-arm.* The R1b waiter is armed with a 60-minute window (`armed-R1b.sh`): it watches sysfs for the board to leave
  and return on `3-4.2.1.4.1`, then identifies and loads at once, under `/run/user/1000/lemnos-k230d.lock`. The capture
  runs read only on both ports. The window opened at 00:01:19 on 2026-11-17.

**R1b, the run (2026-11-17 00:02, card in the PC's reader, not the slot; one run; lock held).**
- Identify `K230` at +0.133 s. Loaded 49,424 bytes, sha256 of the sent image `0fdb2d63…`, at `0x80360000`; start at t0 = +0.139 s.
- *Markers on `if02` (`R1b-console.log`, one read, so only the order is known):*
  `1794898923.631202 !BWKC`, then `boot failed with exit code 19`.
  `!B` at entry, `W` after WDT0 is armed and read back, `K` after WDT0's readback, `C` before the call into the report.
  **No `P`, no `U`, no banner, and no diagnostic lines.** The first Rust marker is `P`, written after `PadGuard::new`
  through `uart::write_byte`.
- Probe: the board left the bus at +1.517 s (detected by poll) and returned as `K230` at +1.764 s. 0 record packets.
- *The gap between `C` and `P`.* Two readings. (a) `lemnos_image_run` or `PadGuard::new` hangs or faults silently. (b)
  the Rust UART path drops every byte: `uart::write_byte` waits on LSR bit 0x20 for at most 100,000 reads and returns
  `Stuck`, which every caller ignores. The raw assembly markers never check LSR, so they print; the Rust writes would
  print nothing if LSR never shows empty. That matches every Rust output being absent (`P`, `U`, the banner), while the
  stage carries on silently: no USB service, no feed, so WDT0 resets the board. **Not settled:** reading `LSR` and the
  raw `P` position in the next image would separate (a) from (b).
- *Watchdog arithmetic against the markers.* Arm at about +0.2 s after the start; reset in the window (`!BWKC` seen,
  the return and detection bounds): with TOP 11 the period is 2^27 cycles, which is 1.342 s at 100 MHz. The reset
  falls at about +1.34 s after arming, inside the window. So the watchdog reset fits at 100 MHz. The earlier R1 detection at
  0.93 s after the start does not fit this and is still unexplained.
- *Conclusion as far as the record goes.* The report image reached its `C` marker and then produced no Rust output. The
  reset came without any stage output, as the watchdog's would. The cause is not settled between a silent hang or fault
  before `P`, and a UART write that never sees its transmitter empty. No change to the board side until the next
  diagnostic (`LSR` value and a raw marker at `P`) decides it.

**R1c: the diagnostic that separates a dropped write from a silent hang (written before the image change).**
- *Changes:* raw markers `P` (after `PadGuard::new`) and `U` (after the UART is configured), written with volatile
  stores and no LSR poll; the LSR read once before the first Rust write and printed as hex by the raw path; a raw `T` on
  the first give-up of `uart::write_byte` (the Rust path's bounded LSR wait), so the dropped error is visible.
- *Hypothesis A, dropped writes:* the LSR poll in `uart::write_byte` never sees the transmitter empty. Then `P`, `U` and
  `T` all appear (the raw markers print; the Rust write gives up and prints `T`), and the LSR value is shown as not
  empty.
- *Hypothesis B, silent hang or fault:* `PadGuard::new` or the call before it hangs or faults. Then `P` does not appear,
  and nothing after `C` does, including `T`.
- *Pass of the run:* the marker string after `C` is decisive: `P` and `T` present is A; `P` absent is B.
- *Open question, unexplained by any run so far:* R1's first detection of the board leaving the bus at 0.93 s after the
  start (the earlier image, no markers). The marker run gives the board leaving at 1.52 s. The two do not agree with a
  fixed watchdog period from one arming, so the 0.93 s figure is not settled.
- *Rehearsal:* the image is rebuilt, its entry checked (`check-entry.py`), the sent bytes hashed, and the probe's own
  crate tests run before the run. A PC rehearsal cannot run the board's UART or watchdog.

**R1c, the run (2026-11-17 00:15, card in the PC's reader; one run; lock held; no RST).**
- Image: build `2f5295c4…` (50,880 bytes); sent (patched report) **`3f1c13058bb3c8be3409787e567ea5c0a4dd22beb81e6383e6987778c92dbeb8`**. Identify `K230` at +0.159 s; start at t0 = +0.166 s.
- Console (`R1c-console.log`, one batch on `if02`): `!BWKCPL00000041` then `UT`, then `boot failed with exit code 19`.
  - `!B W K C` as before: entry, WDT0 armed and read back, the readback done, the call made.
  - **`P` appears.** The report reached `PadGuard::new`: hypothesis B (a silent hang or fault before `P`) is ruled out.
  - **`L00000041`:** LSR read once, before the first Rust write: `0x41`. Bit 0x40 (TEMT, transmitter empty) is set; bit 0x20 (THRE) is clear, and that is the bit `uart::write_byte` polls.
  - **`U`, then `T`:** `U` is written raw after the UART is configured. `T` is the first Rust write giving up, which is the bounded LSR wait expiring.
  - Nothing after `T`: no banner, no diagnostic lines, no records. The run continued silently, as the Rust writes were dropped.
- Probe: the board left at +1.222 s after the start and returned as `K230` at +1.699 s. 0 record packets.
- **Conclusion: hypothesis A (dropped writes) is confirmed.** The Rust UART path never sees THRE set in this ROM state,
  so every Rust output is dropped; the report continues without output, and the watchdog (never fed) resets the board.
  The raw markers print because they do not poll.
- **Open question (not settled), the reset time.** The reset falls between the last detection and the one before:
  R1 (no markers) detection at 0.931 s; R1b (four raw markers) at 1.517 s; R1c (five raw markers and the LSR read) at
  1.222 s. With 0.2 s polls the reset is in (0.73, 0.93], (1.32, 1.52] and (1.02, 1.22] respectively. The images differ in
  how long their markers spin before the call, so the spread may be the image's own early code and not a fixed period.
  The period assumption (TOP 11 at 100 MHz, 1.342 s) is not contradicted by R1b or R1c and is contradicted by R1.
  It stays open until a run records the arm time on the same clock as the capture.

**R1d: the report reaches its banner and serves its records (written before the run).**
- *Changes since R1c:* the stage's UART clears PTIME (IER bit 7) while DLAB is clear, and waits on USR TFNF (`0x7c`,
  bit 1) instead of LSR.THRE. The image prints IER and USR raw (`I`, `S`) and the clear result (`C`) before the first
  write. After the first give-up, no Rust write is tried again. The probe polls records for the whole window and notes
  the first read error only.
- *Provenance:* the SDK's K230 RT-Smart UART driver defines PTIME as IER bit 7 and writes IER `0x81` at init
  (`src/big/rt-smart/kernel/bsp/maix3/board/interdrv/uart_canaan/drv_uart.c`, lines 101 and 277), and it waits on
  LSR.TEMT, never THRE. The DesignWare register map (USR at `0x7c`, TFNF at bit 1) is the vendor IP's and is not in the
  SDK clone: a bench check.
- *Hypothesis A (PTIME fix works):* `I` shows bit 7 set, `C` shows `1`, `S` shows TFNF set, and the banner
  (`lemnos stage: report`) and the step lines print after the `U` marker. Records are read by the probe after the
  banner, within the watchdog's window, if the report serves them.
- *Hypothesis B (the TFNF wait or the clear is wrong):* `T` appears and the banner does not, with `S` showing TFNF
  clear: the FIFO never has room, so the write gives up (a dead transmitter).
- *Hypothesis C (the report hangs after the banner):* the banner prints and the step lines stop; no records.
- *Pass of the run:* the banner and at least one record packet.
- *Open question, unchanged:* the 0.93 s detection of R1, and the arm-time stamp. The capture stamps each read on
  CLOCK_REALTIME; the CH342 batches bytes, so a marker's time is good to about 16 ms, not to a byte. That is the cheap
  arm stamp: the `W` marker's batch time. The probe prints the start request's CLOCK_REALTIME with each line.

**R1d, the run (2026-11-17 00:34, card in the PC's reader; one run; lock held; no RST).**
- Image: build `f9c53d8e…`; sent (patched report) **`6893edb591993ea94009215545984c15d9e5c9375dd0d179f1a47b99e68efeaa`**, 51,120 bytes. Identify `K230` at +0.239 s; start at t0 = +0.248 s (CLOCK_REALTIME 1794900849.277).
- Console (`R1d-console.log`, `if02`): `!BWKC`, then `PL00000041`, `I00000080`, `S00000001`, `C00000001`, then `U`, then `boot failed with exit code 19`.
  - `I00000080`: IER read before the clear: **PTIME (bit 7) is set** on the ROM's state.
  - `C00000001`: the clear ran (DLAB clear), so IER is now `0x01`.
  - `S00000001`: USR read after the entry's raw writes: **BUSY set, TFNF clear** (transmit FIFO full).
  - No banner, no `T`, no diagnostic lines, no records. The reset came at about +0.81 s after t0, inside the Rust TFNF wait.
- **Hypothesis A (the PTIME fix works): not confirmed.** The clear ran, but the transmit FIFO had no room at the Rust write,
  so the write had not finished when the watchdog reset the board.
- **Hypothesis B (the TFNF wait is wrong): not settled.** The wait did block, on a FIFO that was full. The raw markers
  (the `I`, `S`, `C` lines with a spin between bytes) may have filled the 16-byte FIFO before it drained. That would make
  the Rust wait correct and the window too short. Not settled: the FIFO depth and drain rate are the DesignWare
  block's, not in the SDK clone.
- **Probe fault found in this run.** The probe counts a disappearance only in its poll loop. The reset happened during the
  five-second record window, so the poll loop saw the board back and kept waiting until its deadline. The run unit was
  stopped by name. The probe must count absence in the record window too. Fix before the next run.
- **Open question (arm stamp):** the `W` marker's capture batch time is the cheap arm stamp, with about 16 ms of CH342
  batching error. This run's markers arrived in one batch, so the arm stamp is not better than that.

**Provenance for the UART write (this supersedes the R1d USR wait).** The K230 RT-Smart UART driver
(`src/big/rt-smart/kernel/bsp/maix3/board/interdrv/uart_canaan/drv_uart.c`, SDK clone `7e302f733`) defines
`UART_IER_PTIME` as IER bit 7 (line 101) and writes IER `0x81` at init (line 277), and it waits on LSR.TEMT
(bit 6, line 373) before a transmit completes. It never polls THRE, and it never reads the USR status bits for FIFO
room. The DesignWare USR TFNF/TFE bits exist only when the FIFO-status option is built in; R1d read USR as `0x01`
(BUSY only), so on this instance they read 0 and cannot say the FIFO has room. The stage therefore waits on LSR.TEMT
per byte, as the vendor's driver does, and clears PTIME (a known state, harmless to clear).

**Fail-before (the stand-in).** The PTIME stand-in (`ier` 0x81, LSR 0x41, USR 0) fails on the R1d wait (USR.TFNF,
`uart.rs:204`: `Err(Stuck)`), and the banner test fails too (USR 0, so no TFNF). Both pass with the TEMT wait.

**R1e: the banner and records appear (hypothesis, written before the run).**
- *Changes since R1d:* the write waits on LSR.TEMT, not USR.TFNF; PTIME is still cleared; the probe counts absence in
  the record window (sysfs `idVendor` gone, with a failed read) and ends the record loop at that instant, so a
  disappearance at any point is timestamped.
- *Hypothesis A (the fix works):* the markers `!BWKC`, `PL...`, `I00000080`, `S...`, `C00000001`, then `U`, then the
  banner `lemnos stage: report` and its step lines print; records arrive while the probe reads (the probe logs the first
  three), before the watchdog's window ends.
- *Hypothesis B (a different stop):* the banner prints and the step lines stop (the reset comes in the pad dump or the
  gate), or the banner prints and no records arrive.
- *Hypothesis C (silent again):* `U` appears and no banner, with `T` printed once: the TEMT wait gave up, so the UART's
  transmitter never empties on this state.
- *Pass of the run:* the banner, and at least one record packet. `T` absent.
- *Open questions, unchanged:* the 0.93 s R1 detection; the arm-time stamp (the `W` marker's capture batch time, with
  about 16 ms of CH342 batching error).

**R1e result and the cause found in it.** Identify `K230`, load 51,232 bytes, start at t0 = +0.244 s. Console:
`!BWKCPL00000041`, `I00000080`, `S00000001`, `C00000001`, `U`, then the pad dump rows, then `boot failed with exit
code 19`. The probe logged the board gone at +0.775 s (seen in the record window: the absence is counted now) and back
as `K230` at +1.295 s; 0 record packets, 674 read errors. No banner, no step lines, no diagnostic lines, and no `T`.
- *Cause (established by the code and the capture):* `say` and `say_hex` check `GAVE_UP` before they write; the pad rows
  call `write_line` directly, and they printed. `GAVE_UP` is set only on the Err branch, which prints a raw `T` first.
  No `T` was printed, so the flag was already set when the run began. `.bss` is `NOLOAD`: the ROM copies only the file,
  so the flag held the value SRAM had before the load (a previous run's). The entry never zeroed `.bss`.
- *Fix (R1f):* the entry zeroes `.bss` from `lemnos_bss_start` to `lemnos_bss_end` (both defined in `image.ld`) before the
  first static is read. `testing/crates/k230-usbboot-stage/tests/bss_zeroed.rs` fails on the R1e source and passes with the fix.

**R1f: the report's lines appear (hypothesis, written before the run).**
- *Hypothesis A (the cause was the stale flag):* after `U`, the step lines, the banner `lemnos stage: report`, and the diagnostic
  lines (`diag wdt0 before arm CR` and the rest) print, then the pad dump and the gate. Records appear while the probe reads.
- *Hypothesis B (a different stop):* the lines print and the reset comes later (in the gate or the records).
- *Hypothesis C (the flag is not the cause):* no lines after `U`, and a `T` appears (a real give-up in the TEMT wait).
- *Pass of the run:* the banner and the diagnostic lines printed, and at least one record packet; `T` absent.
- *Standing authority (per the coordinator):* further diagnostic runs on this path (R1g and later) keep these conditions,
  and each is reported. New things (a card insert, a hands action, anything beyond USB-boot loads) need a new ok.

**R1f, the run (2026-11-17 00:53; card in the PC's reader; one run; lock held; no RST).**
- Image: build `9aa6f18a…`; sent (patched report) **`fa15e659f6e0cf2112dd471b7ad4ffb95cd88a0a7df9ca1e16ee4b367bdc7dca`**,
  51,264 bytes. Identify `K230` at +0.244 s; start at t0 = +0.252 s (CLOCK_REALTIME 1794902016.979).
- Console (`R1f-console.log`, one batch per line on `if02`): `!BWKC`, `PL00000041`, `I00000080`, `S00000001`, `C00000001`,
  `U`, the diagnostic lines (`diag wdt0 before arm CR 0000000c`, TORR `00000000`, CCVR `0000ffff`; WDT1 the same;
  `diag wdt0 after arm TORR 0000000b`, `TOP 0000000b`, `CR 00000001`), the banner
  `lemnos stage: report (no writes; watchdog armed before this line)`, `step 1: boot mode soc_boot_ctl 0000000f`,
  `step 2: pad dump read`, the pad dump rows, `step 3: no SD controller given; the card is not read`,
  `step 3: no card report`, `step 4: report printed on the console only; USB untouched; SoC reset now`,
  `lemnos stage: end`, then `boot failed with exit code 19`.
- **Hypothesis A confirmed:** the stale `.bss` flag was the cause. Every Rust report line printed, with no `T`.
- **Records:** none, by design. The report writes to the console only (bench ruling of 2026-11-16, commit `eb25445c`).
  The probe's record expectation does not apply to this image.
- **The 0.93 s question:** the board left at +0.884 s and returned as `K230` at +1.183 s. The reset is the report's own
  SoC reset at `lemnos stage: end`, not the watchdog. The probe's `STOP` verdict comes from its old watchdog bound (100 s
  lower bound), not from a failed return. The R1 0.93 s detections were the same kind of reset, taken on paths that
  reached their end.
- Board at the end: `29f1:0230` at Dev 067, download mode. Capture stopped by name. Lock released.

**R2: the report-and-reset return (hypothesis, written before the run; 2026-11-17; no card; one run).**
- *Image:* the R1f report image (build `9aa6f18a…`), sent patched as R1f (`fa15e659…`, 51,264 bytes). It prints its
  cause and its end on the console (`lemnos stage: end`), then takes its own SoC reset. This is the approved R2
  definition: report, then SoC reset, with no card.
- *Hypothesis:* the board prints `lemnos stage: end` on the console, leaves the bus within 5 s of that line (the SoC
  reset), and returns as `29f1:0230` answering `K230` within 10 s of the line.
- *Pass:* the probe's verdict (which reads both consoles read only) reports PASS: the end line present, the departure
  in `[end-0.5, end+5]` s, the return K230 by `end+10` s.
- *Stop:* no end line, a departure outside the window, a return that is not K230, or no return. Report; no further step.
- *Conditions:* the k230d lock (`/run/user/1000/lemnos-k230d.lock`), the read-only capture on both ports, the probe's one
  identify and one load, and the board left in download mode.

**R2, the run (2026-11-17 00:56; no card; one run; lock held; no RST).** Identify `K230` at +0.300 s; load of 51,264
bytes (`fa15e659…`); start at t0 = +0.307 s. The report printed its end on the console at +0.383 s, the board left the bus at
+0.763 s, and came back as `K230` at +1.277 s. The probe's verdict, with both consoles read only, is **PASS**. Board in
download mode (`29f1:0230`, Dev 068 at the end) at the end; capture stopped by name; lock released. The report's own
reset (the R2 path) returned the board; no watchdog was involved.
- **Capture gap, recorded:** `R2-console.log` holds only 3 lines. The probe opened the same two ports with its own
  read-only reader, so the bytes were split between the two readers and the capture unit did not get the report's
  output. The PASS rests on the probe's reader (its end line at 0.383 s). Capture and probe must not both read a port in
  a later run: the probe's `--console` should be the only reader, or the capture should be the only one.

**R3 readiness: the controller and the no-card path (hypothesis, written before the run).**
- *Controller:* the slot's SD card is on SDIO1 (MMC1), per Gaia's K230D AI-CAM device tree (`tmp/K230-USB/aicam/k230d_canmv_junroc_ai_cam.dts`,
  lines 43 and 46: "SD: SDIO1 (MMC1, GPIO26-GPIO31)", "Boot: SD card boot via SDIO1"). The stage's parameter 1 is `sdhci1`.
  The run tries controller 1 first, then 0, read only.
- *Reader ownership:* one reader per port: the probe is the only reader, and it writes every console line to the record,
  stamped on CLOCK_REALTIME. No separate capture runs during R3.
- *Hypothesis (no card, the path that must end cleanly):* on controller 1 the report finds no card, prints that, and ends
  (`lemnos stage: end`); its own SoC reset returns the board to `29f1:0230` answering `K230`. The second run (controller 0)
  then does the same. The verdict for each: PASS by the end rule.
- *Stop:* a run without its end line (the watchdog cut it, so the no-card SD wait is longer than the watchdog window),
  or a return that is not K230. Report, and no further step.
- *Stand-in (the rehearsal):* `testing/crates/k230-usbboot-stage` `sd` tests: `a_card_that_stops_answering_mid_check_is_an_error_to_report_as_a_card_fault_candidate`
  and `a_write_the_card_never_finishes_is_an_error_not_a_hang` (a card that does not answer fails, it does not hang).

**R3 with no card (2026-11-17 01:03; the card is not in the board; one script run; lock held; no RST).**
- The script (`bench/run-R3.sh`) ran the report with controller 1 then 0, each one probe invocation, sole reader of the
  consoles. Identify `K230` before each load; the image is the R1f/R2 report image (`fa15e659…`).
- Controller 1 (SDIO1, the slot): card read attempted, `step 3: the card did not answer or a read failed`, `no card report`,
  `lemnos stage: end` at +0.697 s, the board left at +1.236 s and came back as `K230` at +1.687 s. **PASS.**
- Controller 0: the same lines; end at +0.697 s, left at +1.133 s, back as `K230` at +1.493 s. **PASS.**
- Board at the end: `29f1:0230` at Dev 073, download mode. Lock released.
- The records (`R3-sd1.txt`, `R3-sd0.txt`) carry every console line with CLOCK_REALTIME stamps.
- Earlier, a failed script run (exit 127) made no USB contact: the probe binary was missing, not the board.

**R3 again, the card's place not known (K230-USB-4, rev `abd1d51b` plus the probe's log line; hypothesis, written
before the run; 2026-11-18).**
- *State found, by sysfs and the kernel log only (nothing opened):* `29f1:0230` at `3-4.2.1.4.1`, the CH342
  (`1a86:55d2`, serial `5B08068863`) at `3-4.2.1.3` with its two ttys. At 14:52:11 the board was still Dev 073, the
  number the 2026-11-17 01:03 run left it at, so it had not reset since. At 14:53:13 the board and the CH342 left
  the bus together and came back (board Dev 076, CH342 Dev 075). This runner had opened no device and no tty (it
  was building the image), and software on the PC cannot drop both at once: it is a power cycle or a reset by a
  person. The board came back in the ROM's download mode, so whatever is in the slot now does not boot.
- *Image:* built at this rev by `check.sh`: 51,264 bytes, sha256 `9aa6f18a…`, byte-identical to the R1f/R2/R3
  image. The report path issues CMD0, 8, 55/41, 2, 3, 9, 10, 7, 16 and 17 only (`sd.rs`, `report.rs`): no write
  command. It does not feed the watchdog.
- *Script:* `bench/run-R3.sh` of 2026-11-17 copied to the HDD scratch (`K230R/bench/run-R3.sh`; the NVMe is read only)
  with three changes: the paths; the script takes `/run/user/1000/lemnos-k230d.lock` itself (flock, whole script);
  a run that does not pass ends the script (this record's Stop rule, which the old loop did not enforce). The
  consoles are found by the adapter's USB id through sysfs. Controller 1 then 0, one probe invocation each, the
  probe the only reader of the consoles.
- *Hypothesis A (the no-boot card is in the slot):* on controller 1 the report prints `card cid0` to `cid3`, the
  block count, `verdict code`, the two block CRCs and the two 48-byte headers (sectors 2048 and 3072), then
  `lemnos stage: end`; the board returns as `K230`. On controller 0: `the card did not answer or a read failed`.
- *Hypothesis B (no card in the slot):* both controllers print `the card did not answer or a read failed` and
  `no card report`, as on 2026-11-17. Then the card is not in the slot and a person has to move it.
- *Hypothesis C (a card is there and the watchdog cuts the read):* the card's start (the ACMD41 loop) and the long
  report outlast the watchdog (about 1.3 s after arming, R1b); the lines stop inside step 3 with no end line. The
  board still returns as `K230` through the ROM's failed card boot; the probe says STOP by its watchdog bound, and
  the script ends there.
- *Risk named:* with a card that boots, the report's reset would boot it and the board would not return as
  `29f1:0230`. Against it: the 14:53:13 return to download mode with the slot as it is now.
- *Rehearsal (PC, same rev):* `cargo test -p lemnos-k230-usbboot -p lemnos-k230-usbboot-stage` (all pass; the
  stand-ins that fail like the board: `a_card_that_stops_answering_mid_check_…`, `a_write_the_card_never_finishes_…`,
  `a_card_that_stays_busy_is_called_not_ready_after_bounded_tries`), and the script in its dry mode (`DRY=1`: every
  check runs, the two probe commands are printed, no device or tty is opened).

**R3, run of 2026-11-18 14:56 (one script run; lock held; no reset by hand): STOP, the console was lost; the
card's place is not established by this run.**
- Controller 1: identify `K230` at +0.112 s; 51,264 bytes loaded (sent sha256 `1f793e11…`, the same bytes as the
  2026-11-17 controller-1 run); start at t0 = +0.119 s; the board left at +1.124 s and came back as `K230` at
  +1.556 s (Dev 077). **No console line at all**, not even the entry's raw `!BWKC`. With no end line the probe held
  the run to the old watchdog bound: `STOP: the board left at 1.124 s, outside the step's bound 100..420 s`. The
  script ended there; controller 0 did not run. Records `K230R/bench/run-R3.out`, `R3-sd1.txt`.
- *Cause (established):* the tty was at 9600 baud, cooked, echo on (`stty -a` after the run). The probe's reader
  never set the line; it had been relying on the 2026-11-16 capture scripts' `tcsetattr` to 115200, and the
  adapter's re-enumeration at 14:53:13 put the tty back to its defaults. The rehearsal did not cover it: the dry
  mode opened no tty and no test had a tty at another speed.
- *Also found:* those capture scripts changed the speed only, so echo was on in every earlier run too: the kernel
  sent each console byte the board printed back to the board's receive line. The record's "the console is never
  written" held for the programs, not for the tty. No effect is known (the stage never reads its UART), but it
  is a write, and it is closed now.
- *What the run does say:* the return. Loaded with controller 1, the stage reset the board and it came back to
  download mode at +1.556 s. The departure at +1.124 s is in the range of the two no-card runs (1.236 s, 1.133 s),
  which fits hypothesis B but does not prove it: a watchdog cut (hypothesis C) lands near the same time.
- *Fix:* `Console::open_readonly` sets 115200, raw, echo off before it reads (rustix termios, no unsafe), and
  drops the carriage return raw mode leaves. Test `a_console_left_at_another_speed_with_echo_on_is_set_to_115200_raw_before_reading`
  (a pty at 9600, cooked, echo on, held open as the adapter's tty is): fails on the R3 source (`left: 9600, right:
  115200`), passes with the fix, and checks nothing is echoed back.

**R3b: the same run with the console readable (hypothesis, written before the run; 2026-11-18).** The same script,
image and bytes; the only change is the reader's line setting. Hypotheses A, B and C as above, unchanged. Added
pass condition for the reader itself: the entry's raw `!BWKC` appears, whatever follows.

**R3b, the run (2026-11-18 14:58; one script run; lock held; no reset by hand): both controllers PASS; the card
did not answer on either.**
- Controller 1 (SDIO1, the slot): `step 3: card read (CID, CSD, boot area)` at 1795039121.956, `step 3: the card
  did not answer or a read failed` at 1795039122.273 (0.317 s later, the same wait as both no-card runs of
  2026-11-17), `no card report`, `lemnos stage: end` at +0.697 s; the board left at +1.146 s and came back as `K230`
  at +1.587 s. Sent sha256 `1f793e11…`.
- Controller 0: the same lines (0.317 s), end at +0.697 s, left at +1.073 s, back as `K230` at +1.511 s.
- **Hypothesis B holds as far as this image can tell: the result is the no-card result.** The limit: this host has
  never read a card on this board (`sdhci.rs` marks its clock UNVERIFIED and sets no divider), and the report
  prints one line for every failure, so an empty slot and a card the host cannot reach look the same here.
- *The reader:* `!BWKC` and every line arrived; the fix holds on the real tty (`stty` after: 115200).
- *LSR:* `L000000d9` where every earlier run read `L00000041`: data ready, framing error, break and FIFO error
  are set, so bytes had arrived on the board's receive line. They are the 9600-baud echo of the ROM's `boot failed`
  line from the 14:56 run. The echo did reach the board.
- *The ROM's exit code:* `boot failed with exit code 13` on both returns. It was 13 on 2026-11-16 22:45 and today
  (each after a power cycle) and 19 in every run after the RST of 2026-11-16 23:20. That pairing is an
  observation; what 13 and 19 mean is still not established.
- Board at the end: `29f1:0230`, Dev 079, download mode. Lock released by the script's exit. Records
  `K230R/bench/run-R3b.out`, `R3b-sd1.txt`, `R3b-sd0.txt`, each console line stamped on CLOCK_REALTIME.

**R3c: which failure it is (hypothesis, written before the run; 2026-11-18).** A diagnostic in the R1b to R1f
pattern, under the same conditions; the image reads more and writes nothing more.
- *Changes:* before its card read the report prints five of the controller's registers as the ROM left them
  (`sd rom PRESENT_STATE`, `HOST_CONTROL_POWER`, `CLOCK_TIMEOUT_RESET`, `INT_STATUS`, `CAPABILITIES`: SDHCI
  standard offsets 0x24, 0x28, 0x2c, 0x30, 0x40; status and setting registers whose read has no effect; the data
  buffer is not read). After a failed read it prints the step (`sd error in: controller start`, `card start` or
  `boot area read`), the cause in words, the command (`sd error command`), and the same five registers again
  (`sd end …`). The card commands are unchanged: CMD0, 8, 55/41, 2, 3, 9, 10, 7, 16, 17.
- *Image:* 53,216 bytes, sha256 `43d3f475…` before the patch; `check-entry.py`: `entry check holds`.
- *Hypothesis A (the slot is empty):* PRESENT_STATE bit 16 (card inserted) is clear on controller 1 both times;
  the error is in `card start`, at CMD0 or CMD8, with no reply.
- *Hypothesis B (a card is there and the host does not reach it):* PRESENT_STATE bit 16 is set on controller 1,
  and the error names the command that failed. Then no hands are needed and the host's setup is the work.
- *Hypothesis C (the card-detect bit says nothing):* bit 16 reads the same on both controllers, or the same as it
  will with the card's place known. The board's device tree enables `mmc1` with no card-detect property
  (`tmp/K230-USB/aicam/k230d_canmv_junroc_ai_cam.dts:82`), so the line may not reach the controller. Then the bit is
  recorded as the empty-or-unknown value, and it decides only against a run with the card's place known.
- *Pass of the run:* the ten register lines and the three error lines on each controller, `lemnos stage: end`,
  and the return as `K230`. About 16 more lines (60 ms at 115200), far inside the watchdog's window.
- *Rehearsal (PC):* the image rebuilt and its entry checked; `an_error_names_its_command_and_its_cause_in_fixed_words`
  and `the_snapshot_reads_only_aligned_registers_of_the_window_and_never_the_data_buffer`; all three crates' tests,
  fmt, clippy (host, and the riscv target for the image, where a missing SAFETY comment on `lemnos_diag` was found
  and added); the script in its dry mode with this image. The board's registers cannot be rehearsed on the PC.

**R3c, the run (2026-11-18 15:05; one script run; lock held; no reset by hand): both controllers PASS, and the
result overturns the reading of every R3 so far.** Sent 53,216 bytes (`43d3f475…` before the patch). Controller 1:
end at +0.745 s, left at +1.145 s, back as `K230` at +1.582 s. Controller 0: end at +0.745 s, left at +1.129 s, back
at +1.562 s. Records `K230R/bench/R3c-sd1.txt`, `R3c-sd0.txt`.

| line | controller 1 (the slot) | controller 0 |
|---|---|---|
| `sd rom PRESENT_STATE` | `01f70000` | `03f700f0` |
| `sd rom HOST_CONTROL_POWER` | `00000f02` | `00000000` |
| `sd rom CLOCK_TIMEOUT_RESET` | `000e022f` | `00000000` |
| `sd rom INT_STATUS` | `00000000` | `00000000` |
| `sd rom CAPABILITIES` | `276a6481` | `256ec881` |
| the error | `sd error in: card start`, `no reply: the wait ran out`, `sd error command 00000000` | the same |
| `sd end PRESENT_STATE` | `01f70001` | `03f700f1` |
| `sd end HOST_CONTROL_POWER` | `00000f00` | `00000f00` |
| `sd end CLOCK_TIMEOUT_RESET` | `00000007` | `00000007` |
| `sd end INT_STATUS` | `00000000` | `00000000` |

- **Our host never got a command out (established).** The failure is CMD0 on both controllers. CMD0 expects no
  reply, so a controller whose card clock runs completes it with or without a card. Here the wait of a million
  status reads ran out with the command inhibit still set (`PRESENT_STATE` bit 0) and no status bit. The host had
  written `CLOCK_CONTROL` 0x0005: no divider, no PLL enable, no programmable-clock bit. **So "the card did not
  answer" in R3 (2026-11-17) and R3b was the host, not the slot: those runs say nothing about a card.**
- **The ROM reached a card on the slot's controller (likely, not proven).** As the ROM left it, controller 1 has
  bus power on at 3.3 V and the 4-bit data width (`0f02`), and its clock at divider field 2 with the PLL and
  programmable bits (`022f`): 25 MHz from the 100 MHz base the capabilities give. A host goes to four data lines
  and 25 MHz only after a card has answered its start. Controller 0 was left untouched (all zero). The ROM then
  printed `boot failed with exit code 13`: it read the card and refused to boot it, which is what the no-boot card
  is for. Not proven: there is no such snapshot from a slot known to be empty.
- **Hypothesis C held for the card-detect bit:** bit 16 reads 1 on both controllers, so it decides nothing.
- *LSR* is `L00000040` in both runs: the receive line is clean now that nothing echoes.

**R3d: the host's clock, the long reply, the error statuses (hypothesis, written before the run; 2026-11-18).**
- *Changes (`drivers/k230-usbboot-sys/src/sdhci.rs`), each from the SDK's RT-Smart driver
  (`src/big/rt-smart/kernel/bsp/maix3/board/interdrv/sdio/drv_sdhci.c`, SDK `7e302f733`) or from the ROM's state
  above:*
  1. The clock. Divider field for 400 kHz from the capabilities' base clock (100 MHz gives 125, 200 MHz gives
     250; `drv_sdhci.c:726-737`, `:844`, `:894`), with the programmable-clock bit as the SDK sets it and the PLL
     enable as the ROM left it, in the standard's order (divider and internal clock, stable, PLL, stable, card
     clock). The timeout register takes the ROM's 0x0e. A short settle before the first command.
  2. The 136-bit reply is moved up one byte (`drv_sdhci.c:320-328`): the controller does not store the CRC byte.
     Unmoved, an SDHC card's CSD structure reads 0 and the stage would call it "not SDHC". No card had answered,
     so no run had shown it.
  3. The error statuses are enabled (`drv_sdhci.c:568`), and an error's bits are left for the report to print. With
     none enabled a command with no reply could only run out its wait.
- *Not changed:* the card commands (CMD0, 8, 55/41, 2, 3, 9, 10, 7, 16, 17), one data line, no write command, the
  entry, the watchdog (not fed), the report's own reset.
- *Image:* 52,051 bytes, sha256 `875455ae…` before the patch; `entry check holds`.
- *Hypothesis A (the card is in the slot and the host reaches it):* controller 1 prints `card cid0` to `cid3`, the
  block count, `verdict code`, the two CRCs and the two 48-byte headers, then the end. Controller 0 (the WiFi
  module's bus, no card) fails in `card start` at CMD8 with the controller's error, `sd end INT_STATUS 00018000`
  (command timeout).
- *Hypothesis B (the slot is empty):* controller 1 fails in `card start` at CMD8 with the controller's error and
  `INT_STATUS 00018000`. This time it means an empty slot: CMD0 completed and CMD8 got no reply.
- *Hypothesis C (still no command out):* CMD0 runs out its wait again; `sd end CLOCK_TIMEOUT_RESET` shows the word
  that did not do it.
- *Hypothesis D (the card answers and a later step fails):* the error names it: 41 (never ready), 2, 9 or 10 (the
  long reply), 17 (the data path), `not SDHC`; or the lines stop with no end line because the card's start outlasts
  the watchdog, and the board still returns as `K230`.
- *Pass of the run:* the end line and the return on each controller, and controller 1 decided between A and B.
- *Fail-before (the stand-in):* `sdhci.rs` now has a register model that fails like the board did: a command
  completes only when the clock word has what the ROM's word had, a status appears only when enabled. On the
  first host's start and reply three tests fail (`left: 5, right: 32045` on the clock word, `Timeout(0)` at CMD0,
  `left: 0, right: 1` on the CSD structure); with the changes all six pass. `K230R/sdhci-fail-before.log`.
- *Rehearsal (PC):* fmt, clippy (host and riscv), the three crates' tests, the boundary check, the image's entry
  check, the script's dry mode with this image. The board's controller cannot be rehearsed past its model.

**R3d, the run (2026-11-18 15:12; one script run; lock held; no reset by hand): both controllers PASS; the clock
runs now, and CMD0 ends as a command-line conflict.** Sent 52,051 bytes (`875455ae…` before the patch). Controller 1:
end at +0.437 s, left at +0.894 s, back as `K230` at +1.333 s. Controller 0: end at +0.437 s, left at +0.852 s, back at
+1.319 s. Records `K230R/bench/R3d-sd1.txt`, `R3d-sd0.txt`.
- Both controllers: `sd error in: card start`, `the controller reported an error`, `sd error command 00000000`, 14 ms
  after the read began (0.317 s before: the wait no longer runs out).
- Controller 1: `sd end CLOCK_TIMEOUT_RESET 000e7d2f` (the 400 kHz word, stable), `sd end INT_STATUS 00038100`.
  Controller 0: `000efa2f`, `00038000`. Bits 16 and 17 together (command timeout and command CRC) are the
  standard's report of a conflict on the command line; bit 8 on controller 1 is the card-interrupt status.
- **Hypothesis C's premise is gone (a command now ends in the controller, at once), and none of A, B, D is
  reached:** CMD0 itself fails, before any card could answer. Still no evidence either way about the slot.
- *Cause (from the SDK, to be decided by R3e):* the first `sdhci.rs` and R3d reset the controller and never write
  its vendor control byte. The SDK's driver writes `MSHC_CTRL_R` (0x508) to 0 after every full reset of a controller
  without a PHY (`drv_sdhci.c:271-284`), and the slot's controller is that kind (`:889`, `have_phy = 0`).

**R3e: the vendor control byte after the reset (hypothesis, written before the run; 2026-11-18).**
- *Change:* `start` writes 0 to the byte at 0x508 after the reset, as the SDK does. The report prints that word
  too (`sd rom MSHC_CTRL`, `sd end MSHC_CTRL`). Nothing else changes; the commands are the same, with no write
  command.
- *Image:* 52,115 bytes, sha256 `e4346650…` before the patch; `entry check holds`.
- *Hypothesis A (a card is in the slot):* controller 1 prints the card report (CID, blocks, verdict, CRCs, the two
  headers) and the end. `sd rom MSHC_CTRL` has bit 0 clear (the ROM cleared it too).
- *Hypothesis B (the slot is empty):* controller 1 fails in `card start` at CMD8 with `INT_STATUS 00018000` (command
  timeout alone): CMD0 completed, CMD8 got no reply.
- *Hypothesis C (the byte is not the cause):* CMD0 ends as a conflict again (`00038000`).
- *Hypothesis D (the card answers and a later step fails):* as R3d's D.
- *Controller 0* has a PHY the SDK sets up after a reset (`drv_sdhci.c:209-240`) and this host does not: it is
  expected to fail, and its result decides nothing about the slot.
- *Fail-before:* the model now puts the conflict check on at a full reset and ends every command as a conflict
  while it is on. Without the write two tests fail (`Err(Reply(0, "the controller reported an error"))` where
  `Ok(None)` is expected); with it all seven pass. `K230R/sdhci-conflict-fail-before.log`.
- *Rehearsal (PC):* as R3d (fmt, clippy on both targets, the three crates' tests, boundaries, entry check, dry mode).

**R3e, the run (2026-11-18 15:14; one script run; lock held; no reset by hand): the card is in the slot and was
read. Hypothesis A holds.** Sent 52,115 bytes (`e4346650…` before the patch). Records `K230R/bench/run-R3e.out`,
`R3e-sd1.txt`, `R3e-sd0.txt`, every console line stamped on CLOCK_REALTIME.
- *Controller 1 (SDIO1, the slot):* identify `K230`; start at t0 = +0.156 s. Console:
  - `1795040096.377503 sd rom MSHC_CTRL 000f0000`: the ROM had cleared bit 0 on this controller.
  - `1795040096.379077 step 3: card read (CID, CSD, boot area)`
  - `1795040096.582201 card cid0 03534453`, `cid1 52363447`, `cid2 8014a7c9`, `cid3 ab015100`: manufacturer 0x03,
    OEM `SD`, product `SR64G`, revision 8.0, serial `14a7c9ab`, date 2021-01.
  - `card blocks hi 00000000`, `card blocks lo 076f5000`: 124,735,488 sectors, 59.5 GiB.
  - `1795040096.584626 verdict code 00000003`: no firmware header, and the blocks hold other bytes.
  - `1795040096.609471 copy0 block crc32 89148991`, `copy1 block crc32 89148991`; the first 48 bytes of each
    header (sectors 2048 and 3072) are zero.
  - `1795040096.647766 lemnos stage: end` (+0.667 s). The board left at +1.087 s and came back as `K230` at
    +1.519 s. **PASS.** The card's start, the two long replies, and sixteen sectors took 0.2 s.
- *Which card:* the CRC32 of the 4 KiB blocks at sectors 2048 and 3072 of the no-boot image
  (`tmp/K230-USB/card-b/k230d-ai-cam-varA-noboot.img`, sha256 `49a61489…`) is `89148991` for both, computed on the
  PC. The card in the slot holds that image's boot blocks: the firmware header zeroed, the payload left.
- *Controller 0:* `sd rom MSHC_CTRL 000f0001` (untouched by the ROM). CMD0 completed; `sd error in: card start`,
  `the controller reported an error`, `sd error command 00000008`, `sd end INT_STATUS 00018001` (command timeout,
  after a completed command). Nothing on that bus answers as an SD card. End at +0.443 s, left at +0.916 s, back
  as `K230` at +1.354 s. **PASS.**
- *The ROM:* `boot failed with exit code 13` after each return, with this card in the slot.
- *Closed by this run:* the vendor control byte is the conflict check (the bench decided it); the clock setup;
  the long reply's byte offset (the capacity and the CID read sensibly); the one-line data path (two block CRCs
  match the image). The return through a card that does not boot: five scripts, ten returns today, each back to
  `29f1:0230`.
- *What every R3 before this one said about the slot:* nothing. The 2026-11-17 entry "R3 with no card" and R3b
  reported the host's own failure.
- Writes: none, to the card, the boot settings or the stored environment. Board at the end: `29f1:0230`, Dev 086.

**R3f and R0 with the card in: what else runs on this path without a write (hypothesis, written before the runs;
2026-11-18).**
- *R3f:* R3e again, the same script and the same bytes. Hypothesis: the same CID, block count, verdict code 3 and
  both CRCs `89148991` on controller 1; the same CMD8 error on controller 0; both PASS. A difference in any card
  line is a read fault to chase before any write is ever reviewed.
- *R0 with the card in* (`bench/run-R0.sh`: the probe once, mode `soc`, no SD controller, the R3e image; the
  same lock and checks, and a dry mode): the entry's SoC reset right after the start, with a card that does not
  boot in the slot. Hypothesis: `!S` on the console, the board gone within 2 s and back as `K230` within 10 s, the
  ROM's `boot failed with exit code 13` in between. Stop: no return, or another identity.
- *Rehearsal:* both scripts in dry mode; the image and probe are the ones R3e rehearsed.

**R3f and R0 with the card in, the runs (2026-11-18 15:16; one script run each; lock held; no reset by hand).**
- *R3f:* every console line of both controllers is the same text as R3e's (`diff` of the two records' console
  lines without their stamps: identical): the CID, 124,735,488 sectors, verdict code 3, both CRCs `89148991`.
  Controller 1: end at +0.668 s, left at +1.097 s, back as `K230` at +1.532 s. Controller 0: end at +0.443 s, left
  at +0.794 s, back at +1.230 s. Both **PASS**. Records `K230R/bench/R3f-sd1.txt`, `R3f-sd0.txt`.
- *R0 with the card in:* identify `K230`; 52,115 bytes loaded; start at t0 = +0.138 s; console
  `1795040204.875268 !B!Sboot failed with exit code 13`; gone at +0.429 s, back as `K230` at +0.661 s. **PASS.**
  Record `K230R/bench/R0-soc.txt`.
- Board at the end: `29f1:0230`, Dev 089, download mode, as found. The lock is released at each script's exit.

**The write half (reviewed 2026-11-18: approved for this card only; sectors 2048 and 3072 are not written in
this session).** The card is the bench no-boot card read in R3e (CID `0353445352363447 8014a7c9ab015100`,
124,735,488 sectors). Nothing but this card is written.
- *How the bytes reach the stage.* Not the stage's own USB device path: that is blocker B2 and has not run on
  hardware. The ROM's load-and-run is used as it is: each load carries the stage image in `piece` mode at
  `0x80360000` and one job at `0x80380000` (a 64-byte header with its CRC, then up to 1016 sectors of payload at
  `0x80381000`; the load ends inside the ROM's range, `0x80400000`). The stage arms `wdt0` first, starts the card on
  controller 1, refuses unless the CID and the size are the job's, checks the payload's CRC, does the one job,
  prints the outcome, and takes its SoC reset. `testing/crates/k230-usbboot-stage/src/piece.rs`,
  `run_piece` in `drivers/k230-usbboot-sys/src/image.rs`, host `k230-card` (`testing/crates/k230-usbboot/src/bin`).
- *Jobs.* `write`: each sector written (CMD24), read back (CMD17) and compared. `zero`: the same with zero
  sectors and no payload. `hash`: sectors read and hashed (SHA-256), nothing written. `test`: the bounded write
  test: the range's content kept in the stage, the pattern written and read back, the content written back and
  read back.
- *The watchdog* is fed after a sector that was written and read back equal, or read and hashed, and nowhere
  else. A sector that reads back different ends the piece unfed.
- *The proof of a flash.* Each written sector is compared at write time, the stage prints the SHA-256 of what it
  read back and the host compares it with the SHA-256 of what it sent, and at the end one `hash` job over the
  whole image region must print the image's SHA-256.
- *The host* reads the outcome on the consoles (read only), waits for the ROM to answer `K230` again before the
  next piece, and stops at the first piece that does not end `piece ok` with the expected digest. A progress
  file holds the first sector not yet verified, so a cut run resumes.
- *Rehearsal (the register model, `sdhci.rs` tests):* the model has a card now (commands, the data buffer both
  ways, busy after a write). Through the real host code: a written sector reads back and is fed once; a card that
  takes the data and keeps its old content is a mismatch and is not fed; a card that stays busy is an error;
  the write test leaves the range as found; another card, a range past the card and a damaged payload are refused
  with no write. *Mutation proof for the write path* (no defect in it is known yet, so there is no true
  fail-before): with CMD24's transfer direction set to read, four of those tests fail
  (`K230R/write-mutation.log`). A real fail-before is added if the board shows a defect.
- *Image:* 62,520 bytes, sha256 `af32b725…`; footprint `0x1f470`, below the job address (checked by `check.sh`).

**W0 and W1: the card confirmed again, then the bounded write test (hypothesis, written before the runs).**
All at the 400 kHz clock R3e proved, one data line. Script `K230R/bench/run-card.sh` (the lock, the same checks, a
dry mode); one tool run per line below.
- *W0:* `hash` of sectors 2048..2056 and of 3072..3080. Hypothesis: the stage prints the CID and size of R3e and
  each SHA-256 equals that of the same 4 KiB of the no-boot image (`f30463e7…` for both, computed by the tool from the file).
  Also the first `piece` load: the job header at `0x80380000`. Stop: any other card, or a digest that differs.
- *W1:* the range is sectors 1,048,576..1,048,584 (the 512 MiB mark; the image ends at sector 415,744, so the
  image never covers it). `hash` it (the content before), `test` it with a 4 KiB pattern
  (`K230R/bench/pattern.bin`), `hash` it again. Hypothesis: the test prints the pattern's SHA-256 for what it
  read back, `piece before` equal to `piece after`, and both `hash` runs print the same digest as `piece before`.
  Stop: a mismatch, a card error, a missing end line (the watchdog cut it), or a restore that differs.
- *If W1 stops with the pattern left on the card:* eight sectors past the image hold the pattern; nothing the
  image or the ROM reads. It is reported, not hidden.

**W0 and W1, the runs (2026-11-18 15:56; lock held per run; no reset by hand): all PASS.** Records
`K230R/bench/W0a.txt`, `W0b.txt`, `W1a.txt`, `W1b.txt`, `W1c.txt` (every console line stamped).
- *W0:* the stage printed `card cid0 03534453`, `cid1 52363447`, `cid2 8014a7c9`, `cid3 ab015100`, `card blocks lo
  076f5000`. Sectors 2048..2056: `piece sha256 f30463e7…9da4`, equal to the no-boot image's; sectors 3072..3080: the
  same. The same card. The load of 131,136 bytes (the job header at `0x80380000`) ran; one piece is 1.3 to 1.5 s
  from the load to the ROM answering again.
- *W1a:* sectors 1,048,576..1,048,584 before: `ad7facb2…2ca7` (the SHA-256 of 4096 zero bytes).
- *W1b, the write test:* load 139,264 bytes; `piece sha256 f918d54c…6865`, the pattern's, for what the stage read
  back after writing it; `piece before ad7facb2…2ca7`, `piece after ad7facb2…2ca7`; `piece ok`; end; the ROM back.
  **The first bytes this path has written to a card: 4096 bytes of pattern to sectors 1,048,576..1,048,584, then the
  4096 bytes that were there before, each sector read back and equal.**
- *W1c:* the range again: `ad7facb2…2ca7`. The range is as it was found.
- *Rate at 400 kHz:* the test's 40 sector operations took about 0.55 s, 14 ms each. The whole image at that rate
  is hours of card time, so the data clock is next.

**W2: the data clock (hypothesis, written before the runs).** With `--fast` the stage raises the card clock after
the card has started to the word the ROM leaves (`CLOCK_CONTROL` divider field 2: 25 MHz from the 100 MHz base), one
data line. In order: `hash` of sectors 2048..2056 (read only; must print `f30463e7…`); `hash` of sectors 0..1016
against the no-boot image (a rate for reads); the write test again on the W1 range; `hash` of that range.
Hypothesis: the same digests as at 400 kHz, and a sector operation well under a millisecond of card time. Stop: a
digest that differs (a data path that does not hold at this clock: stay at 400 kHz), a card error, a missing end.

**W2, the runs (2026-11-18 15:57): the digests hold, the rate did not change.** Records `K230R/bench/W2a.txt` to
`W2d.txt`.
- `hash` 2048..2056 with `--fast`: `piece clock: data rate`, `f30463e7…` as expected. `hash` 0..1016: `645fbfa0…c9ed`,
  equal to the no-boot image's first 1016 sectors. The write test: pattern `f918d54c…`, before and after
  `ad7facb2…`. The range after: `ad7facb2…`. All PASS.
- **The rate:** 1016 sectors hashed in 12.0 s, 11.9 ms a sector: the 400 kHz figure. The card clock had not
  changed. *Cause (to be decided by W3):* `data_clock` wrote the new divider with the PLL enable left on; the
  standard's order for a change is card clock and PLL off, divider, stable, PLL on, stable, card clock on.
- *Fail-before:* the model takes a divider only from a clock write with the PLL enable off. On the W2 code
  `the_data_clock_is_the_word_the_rom_leaves_and_the_card_still_answers` fails (`left: 125, right: 2`: the card
  still runs at the old rate); with the order fixed it passes. `K230R/dataclock-fail-before.log`.

**W3: the data clock with the order fixed (hypothesis, written before the runs).** Image 62,552 bytes, sha256
`c5a6c028…`. The same four runs as W2. Hypothesis: the same digests, and the 1016-sector hash in well under the
12 s of W2. If the digests differ at the real 25 MHz (one data line, no delay tuning), the flash stays at 400 kHz.

**W3, the runs (2026-11-18 15:59): the rate is still the old one, and the write test failed at this setting; the
range was put back.** Records `K230R/bench/W3a.txt` to `W3f.txt`.
- `hash` 2048..2056: `f30463e7…`, PASS, 102 ms for the eight sectors (102 ms at 400 kHz in W0). `hash` 0..1016:
  `645fbfa0…`, PASS, 12.0 s again. So the card clock was still at the old rate.
- **The write test (`W3c`): STOP.** `piece FAILED: card error at sector 00100000`, `the controller reported an
  error`, `sd error command 00000018` (CMD24), 121 ms after the clock change (the eight reads of the content
  first). The stage then wrote the kept content back as its code does; the error printed is the first.
- `hash` of the range after (`W3d`): `ec7736d7…`, not the `ad7facb2…` of before. **Eight sectors past the image
  (1,048,576..1,048,584) held other bytes than before the test.**
- *Put back (`W3e`, `W3f`, at the 400 kHz setting, no `--fast`):* a `zero` job over the eight sectors (their
  content before was zeros: `ad7facb2…` is the SHA-256 of 4096 zero bytes), each read back: `piece sha256
  ad7facb2…`, `piece ok`; then `hash`: `ad7facb2…`. The range is as it was found. Eight write-and-read-back
  sectors took 202 ms at 400 kHz: 25 ms a sector.
- *What W3 says:* cycling the PLL enable did not change the rate, and with it CMD24 failed. The cause of the
  failure is not established (the error bits were not printed; W4 prints them).

**W4: the SDK driver's own order for the frequency change (hypothesis, written before the runs).** Image 62,808
bytes, sha256 `693d2c8b…`. `data_clock` now does what `drv_sdhci.c:714-743` does: card clock and
programmable-clock bit off, then the new divider with both on, then stable; the PLL enable is left as it is.
After the change the stage prints the clock word, and after a card error the controller's registers (`sd end …`).
The model's rule is now "a divider is taken when the programmable-clock bit rises", which the W2 and W3 code both
fail. Runs: `hash` 2048..2056 and 0..1016 with `--fast` (read only), and only if the rate is up and the digests
hold, the write test and the range's hash. Hypothesis: `piece clock word` with divider field 2, the 1016 sectors
in a small part of 12 s, the same digests. If the digests differ or the write test fails, the flash runs at
400 kHz and the data clock is left as an open item.

**W4, the runs (2026-11-18 16:02): reads run at the data clock; the write test failed again, and its cause is
found; the range was put back.** Records `K230R/bench/W4a.txt` to `W4f.txt`.
- `piece clock word 000e022f`: the ROM's own word. `hash` 2048..2056: `f30463e7…`. `hash` 0..1016: `645fbfa0…` in
  1.51 s, 1.5 ms a sector (11.9 ms before). **Reads hold at 25 MHz on one data line, eight times faster.**
- **The write test (`W4c`): STOP.** `piece FAILED: card error at sector 00100000`, `the controller reported an
  error`, `sd error command 00000018`, `sd end INT_STATUS 00018111` (command timeout on a CMD24), `sd end
  PRESENT_STATE 01f70400`. The range after: `ec7736d7…` again.
- *What was on the card:* `ec7736d7…` is the SHA-256 of the pattern's first sector followed by seven zero
  sectors (computed on the PC). So the first pattern sector was written, and the failure came after it.
- *Cause (established by the code and these lines):* after a write the stage polls the card's status with CMD13
  up to `BUSY_POLLS` = 1000 times. A poll lasts as long as the card clock makes it: 1000 polls are about 0.3 s
  at 400 kHz and about 15 ms at 25 MHz, and the card was still programming. The stage gave up (CMD13, "the card
  stayed busy"), then tried to write the kept content back; a card that is programming does not answer CMD24,
  which is the command timeout printed. The restore's error replaced the first one in the report: that is
  fixed too (the first failure is the one reported).
- *Put back (`W4e`, `W4f`, 400 kHz):* `zero` over the eight sectors, read back, `ad7facb2…`; `hash`:
  `ad7facb2…`. The range is as it was found.
- *Fix and fail-before:* `BUSY_POLLS` is 200,000 (half a second and more at the fastest clock). The model's card
  is now busy for a time, not a count of polls (a poll at divider 2 uses up 1/62 of what one at 125 does). On
  the W4 code `a_write_at_the_data_clock_waits_out_the_cards_busy_time` fails with `Card(40, Reply(13, "the card
  stayed busy after the write"))`; with the fix it passes. `K230R/busy-fail-before.log`. This is the write
  path's true fail-before.

**W5: the write test at the data clock (hypothesis, written before the runs).** Image 63,048 bytes, sha256
`b4fe9a9f…`. `test` on the W1 range with `--fast`, then `hash` of it at 400 kHz. Hypothesis: the pattern's digest
read back, before and after `ad7facb2…`, the range `ad7facb2…`. Stop as W1. If it fails again, the flash runs at
400 kHz, where W1 passed.

**W5, the runs (2026-11-18 16:05): the write test passes at the data clock.** `W5a`: `piece clock word 000e022f`;
`piece sha256 f918d54c…6865` (the pattern, read back); `piece before ad7facb2…`, `piece after ad7facb2…`; `piece
ok`; the ROM back. The test's 40 sector operations took about 80 ms, 2 ms each. `W5b` (400 kHz): the range is
`ad7facb2…`. **The range 1,048,576..1,048,584 is as it was found (zeros), and nothing else has been written.**

**What the write test wrote, in all (W1 to W5), all inside sectors 1,048,576..1,048,584 (bytes 536,870,912 to
536,875,008 of the card):** the 4096-byte pattern (sha256 `f918d54c…`) in W1b, W2c and W5a, each followed by the
4096 zero bytes that were there; the pattern's first 512 bytes in W3c and in W4c (the failed tests); 4096 zero
bytes in W3e and W4e (the put-backs). Final content: 4096 zero bytes (`ad7facb2…`), as before the first test.

**FLASH: the whole image except its two header sectors, then the hash on the board (hypothesis, written
before the run).**
- *Source:* `/mnt/ssd-samsung/gaia/boards/k230d-ai-cam/test-A/k230d-ai-cam-varA.img`, read in place, 212,860,928
  bytes (415,744 sectors), sha256 `033903a1…c6cd` (checked). Sectors 2048 and 3072 (the first sector of each SPL
  copy's header) are written as zeros, never as the source has them: the card stays one the ROM does not boot.
  With those two sectors zero the source's SHA-256 is `49a61489…b616`, which is the no-boot image's (computed by
  the tool, equal to `card-b/SHA256`).
- *Plan (the tool's dry run):* 108 pieces: 83 `write` pieces carrying 43,073,536 bytes, and 25 `zero` runs
  covering 169,787,392 bytes (a run is at most 16,384 sectors). Every sector 0..415,744 is written. `--fast`
  (25 MHz, one data line). A progress file (`K230R/bench/FLASH.progress`) holds the first sector not yet
  verified; the run is a systemd user unit under the bench lock, and stopping between pieces is safe.
- *Hypothesis:* every piece ends `piece ok` with the SHA-256 of what was sent, and the ROM answers `K230` after
  each. Then one `hash` job over sectors 0..415,744 prints `49a61489…b616`.
- *Stop:* the first piece that fails, by the tool. The card then holds the image up to that piece and the no-boot
  image's content after it (the two are the same bytes there); it still does not boot.

**FLASH, first run (2026-11-18 16:06 to 16:07): 14 pieces verified, then STOP at piece 15.** Record
`K230R/bench/FLASH.txt`.
- Pieces 1 to 14 (sectors 0..20,320): each `piece ok` with the digest of what was sent, the ROM back after each;
  a 1016-sector `write` piece takes 5.6 to 6.1 s from the load to the ROM answering again.
- Piece 15 (`write`, sectors 20,320..21,336): `piece FAILED: card error at sector 00004f60`, `the controller
  reported an error`, `sd error command 00000018`, `sd end INT_STATUS 00608100`: the data CRC and data end-bit
  errors on CMD24's data, at the piece's first sector. The card drops a block whose CRC fails, so nothing of
  piece 15 was written. The tool stopped; the progress file says 20,320; the ROM was back as `K230`.
- *What it is:* one failed data transfer in about 14,000 sector writes at 25 MHz on one data line with no delay
  tuning. The stage treated any transfer error as final.
- *Also seen:* the payload's CRC was checked twice (0.78 s each for 508 KiB, unfed), once more than needed; the
  second check is removed, so the longest unfed stretch is back under one second.
- *Fix and fail-before:* a sector's write or read is tried up to three times; after an error the controller's
  command and data lines are reset (`SOFTWARE_RESET` bits 1 and 2, the standard's recovery); every extra try is
  counted and printed (`piece retries`). A sector that reads back different is still final. The model's card can
  fail a write's data CRC a set number of times, holding the data line until it is reset. With one try
  `a_write_whose_data_fails_its_crc_is_sent_again_and_counted` fails (`Card(41, Reply(24, "the controller reported
  an error"))`); with three it passes, and a sector that fails every try still ends the piece.
  `K230R/retry-fail-before.log`.

**FLASH, resumed (hypothesis, written before the run).** Image 64,632 bytes, sha256 `02da42a6…`. The same
command; the tool resumes at sector 20,320 from the progress file. Hypothesis: the remaining pieces end `piece
ok`; a few report a transfer tried again. Stop: a sector that fails three tries, or reads back different.

**FLASH, resumed (2026-11-18 16:10): 3 more pieces verified (to sector 23,368), then STOP again.** Piece 4 of the
run (`write`, sectors 23,368..24,384): the first sector failed all three tries; the last try's status is `sd end
INT_STATUS 00018100`, a command timeout: the card did not answer CMD24. Nothing of the piece was written; the
progress file says 23,368; the ROM was back.
- *What it is:* both failures are a piece's first write, after the 0.78 s payload check with the fast clock
  already running; no sector in the middle of a piece has failed in about 17,000. After the first failed data
  phase the card was left waiting in its transfer and answered no further data command, so the two extra tries
  could not work. Why the first write after that pause fails about one piece in eight is not established.
- *Fix and fail-before:* the recovery now also stops the card's transfer (CMD12; no answer to it is not an
  error) and polls its status until it is ready, then tries again (`Card::recover`). The model's card waits for
  the stop after a failed data phase and answers no data command until it comes. With the R-only recovery the
  retry test fails (`Card(41, Reply(24, …))`); with the stop it passes. `K230R/stop-fail-before.log`.

**FLASH, resumed again (hypothesis, written before the run).** Image 64,712 bytes, sha256 `2da8d976…`. Resumes at
sector 23,368. Hypothesis: the pieces end `piece ok`, and about one `write` piece in eight reports one transfer
tried again. Stop: as before.

**FLASH, resumed again (2026-11-18 16:11 to 16:33): done. Every sector 0..415,744 written and read back.**
Record `K230R/bench/FLASH.txt` (all three runs; every console line stamped), `run-FLASH.out`.
- This run: sectors 23,368..415,744 in 91 pieces, 35,790,848 bytes from the source and 165,105,664 bytes of
  zeros, 1344.1 s; `verdict: PASS`.
- **In all three runs: 108 pieces `piece ok`, each with the SHA-256 of what was sent: 83 `write` pieces carrying
  43,073,536 bytes from the source and 25 `zero` runs covering 169,787,392 bytes: 212,860,928 bytes, sectors
  0..415,744, 1458.7 s of tool time.** Sectors 2048 and 3072 were written as zeros (inside their pieces), not as
  the source has them. The ROM answered `K230` after every piece.
- *Transfers tried again:* 11 in all, each one a `write` piece's first sector, each followed by a sector that
  verified (`piece retries 00000001`). No sector failed three tries; none read back different.
- *Rates seen:* a 1016-sector `write` piece is about 5 s from load to the ROM answering again (0.3 s of load and
  entry, 0.8 s for the payload's CRC, about 3 ms a sector written and read back, 0.9 s for the return); a
  16,384-sector `zero` run is about 50 s.

**HASH: the whole image region read back on the board (2026-11-18 16:34 to 16:44): VERIFIED.** One `hash` job
over sectors 0..415,744 (212,860,928 bytes), 25 MHz, one data line, the watchdog fed after each sector read and
hashed. The stage printed `piece retries 00000000`, **`piece sha256
49a61489aa1adbefa6878458b8640c15a9564983557459ad136838530c63b616`**, `piece ok`, `lemnos stage: end`; 628.2 s; the ROM
back as `K230` (Dev 111). That is the SHA-256 the tool computed from the source with sectors 2048 and 3072 zero,
and it is the no-boot image's (`card-b/SHA256`). Record `K230R/bench/HASH.txt`.
- **So the card holds the whole of `k230d-ai-cam-varA.img` (sha256 `033903a1…`) except the 1024 bytes of sectors
  2048 and 3072, which are zero. It does not boot; the ROM still falls back to USB.**
- *Everything written to the card in this session:* sectors 0..415,744 (bytes 0 to 212,860,928) once, as above;
  and the write tests inside sectors 1,048,576..1,048,584, left as zeros. Nothing else, and nothing but this card.

**A: the image's own SPL over ROM USB, the card still no-boot (the coordinator's word given 2026-11-18;
hypothesis, written before the run).**
- *What is loaded:* `/mnt/ssd-samsung/gaia/boards/k230d-ai-cam/test-A/k230d-ai-cam-varA.img` (sha256 `033903a1…`),
  file offset 1,049,108 (sector 2048, after the 528-byte firmware header and the payload's 4-byte version word),
  216,920 bytes, sha256 `686d659c…5f28`, loaded at `0x80300000` and started there. The first word is `732540f1`
  (`csrr a0, mhartid`, U-Boot's first instruction). Provenance: the SDK's defconfig links the SPL at
  `CONFIG_SPL_TEXT_BASE=0x80300000` (`configs/k230d_canmv_defconfig:9`) with its BSS at `0x80380000` (`:42`), and its
  image code skips a 4-byte version word before the image (`board/canaan/common/k230_img.c:296`). The load ends at
  `0x80334f58`, inside the ROM's range.
- *Nothing of ours runs in this step:* no stage, so **no watchdog is armed**. The SPL brings up the DDR and reads
  U-Boot from the card (sector 4096; its header and payload are intact on the no-boot card).
- *The console:* `K230R/bench/uboot-console.py` is the only reader and writer of both ttys (raw, no echo). It sends
  one space when `Hit any key to stop autoboot` appears, then at the `K230# ` prompt the one command `reset`, and
  nothing else. Every byte received and sent is logged with a stamp.
- *Hypothesis:* SPL lines appear within a few seconds of the start, then U-Boot's banner and its countdown
  (`bootdelay=5` is in the image); the key stops it; `reset` takes the SoC reset; the ROM finds a card it does
  not boot and comes back as `29f1:0230`, by itself.
- *What a hang looks like, and when it is called one:* no countdown line within 60 s of the start, or no return
  as `29f1:0230` within 60 s of `reset`; the script's limit is 90 s in all. The board is then off the bus with
  nothing armed. The coordinator is told at once, nothing else is done to the board, and one press of RST by a
  person brings the ROM back (the card does not boot).
- *If the countdown never comes but U-Boot boots on:* it loads the kernel from the card and the board stays
  wherever that ends; the same hands ask.
- *Rehearsal (PC):* the load through the tool's dry mode (file, offset, size, digest, address checked against the
  ROM's range, nothing opened); the console script against a pty stand-in (`fake-uboot.py`) that prints a banner
  and the countdown, takes the key, gives the prompt and ends on `reset` (the script sent exactly `b' '`,
  `b'reset\r'`); and the stand-in's failing case, a banner and then silence, which the script ends as `LIMIT`
  with exit 1. The SPL itself cannot be rehearsed on the PC.

**A, the run (2026-11-18 16:48; one run; lock held; no hands): PASS. The board came back to ROM USB by itself.**
Records `K230R/bench/A-load.txt`, `A-console.log` (every byte received and sent, stamped), `run-A.out`.
- Loaded and started as written above (216,920 bytes, sha256 `686d659c…`, at `0x80300000`), at 1795045688.735.
- Console (`ttyACM1`, the adapter's second port): `U-Boot SPL 2022.10 (Nov 16 2026 - 19:39:27 -0700)`; the DDR
  training lines (`PMU Major Msg: … Firmware run has completed 2667`); `NONE_SECURITY`; `imge: uboot load to 0
  compress =1 …`; `U-Boot 2022.10 (Nov 16 2026 - 19:39:27 -0700)`; `CPU: rv64imafdcvsu`; `Model: kendryte k230d canmv
  junroc ai cam`; `DRAM: 128 MiB`; `MMC: mmc0@91580000: 0, mmc1@91581000: 1`; `Loading Environment from MMC... OK`;
  `Hit any key to stop autoboot:  3`.
- Sent: `b' '` at the countdown; the prompt `K230# ` came; sent `b'reset\r'`. Nothing else was sent.
- After `reset`: `reboot system`, then the ROM's `boot failed with exit code 13`; `29f1:0230` back at Dev 112
  0.4 s after the command.
- **Proven:** this image's SPL and U-Boot run from a ROM USB load with the card as it is; U-Boot's countdown can
  be stopped from the PC; `reset` at its prompt returns the board to ROM USB with the no-boot card.

**B: what this U-Boot offers, read only (hypothesis, written before the run).** The same load; at the prompt,
one command per prompt, then `reset`: `version`, `help`, `bdinfo`, `printenv`, `mmc list`, `mmc dev 1`, `mmc info`,
`wdt list`, `help wdt`, `help bootcount`, `help reset`, `dm tree`, `reset`. None writes storage or saves the
environment (`mmc dev 1` selects the device for `mmc info` in RAM only). Hypothesis: each prints or says
`Unknown command`; `reset` returns the board as in A. A hang is called as in A (limit 150 s here). The answer
sought: a watchdog U-Boot can arm before `boot`, or anything else that returns a hung kernel without hands.

**B, the run (2026-11-18 16:48; one run; lock held; no hands): PASS; the board back at Dev 113 0.5 s after
`reset`.** Record `K230R/bench/B-console.log`. Bytes sent: `b' '`, then each of the thirteen commands above
followed by `\r`, in that order; nothing else.
- `version`: `U-Boot 2022.10 (Nov 16 2026 - 19:39:27 -0700)`, `riscv64-linux-gcc.br_real (Buildroot
  2021.11-18033-g83947c7bb6) 14.3.0`, `GNU ld (GNU Binutils) 2.43.1`.
- `help`: no `wdt` and no `bootcount` (`wdt list`, `help wdt`, `help bootcount`: `Unknown command`). `dm tree`:
  no watchdog device (clocks, five serial ports, the two `snps_sdhci` controllers, `dwc2_usb`, the pinctrl, the
  timer). `reset` is there (`reset -w` for a warm reset). The memory commands are there (`md`, `mw`, `mm`,
  `nm`), also `k230_dfu` and `reboot_to_upgrade` ("k230 reboot and enter burn mode"), `usb`, `loadb/x/y`, `gzwrite`,
  `mmc`.
- `printenv`: `bootargs=root=/dev/mmcblk0p1 rootwait rw rootfstype=ext4 console=ttyS0,115200 earlycon=sbi`;
  `bootcmd=mmc dev 1 && mmc read 0x3000000 0x1c00 0x400 && ext4load mmc 1:1 0x200000 /boot/Image && ext4load mmc
  1:1 0x2200000 /boot/k230d-ai-cam.dtb && bootm 0x3000000 - 0x2200000`; `bootdelay=3`; the three consoles on
  `serial@91400000`; `Environment size: 371/8187 bytes`. The boot command only reads the card.
- `mmc list`: `mmc0@91580000: 0`, `mmc1@91581000: 1 (SD)`. `mmc info` on device 1: manufacturer 3, OEM `5344`, name
  `SR64G`, SD High Speed (50 MHz), four data lines, SD version 3.0, 59.5 GiB: U-Boot sees the card this session
  flashed.
- `bdinfo`: one DRAM bank, `0x0` to `0x8000000` (128 MiB); `relocaddr 0x7f47000`.
- **Answer:** this U-Boot has no watchdog command, no watchdog driver and no boot counter. What it does have is
  `mw`: the watchdog block's registers can be written from the prompt, the three writes the stage's entry makes
  (`TORR`, the restart key to `CRR`, enable in `CR`: `wdt.rs`, `drv_wdt.c`).

**C1: the watchdog armed from the prompt by register writes, with nothing booted (hypothesis, written before
the run).** A test of the mechanism while U-Boot is still there to type `reset` at.
- *Sent at the prompt:* `md.l 0x91106000 4` (read), `mw.l 0x91106004 0xbb` (TORR: TOP 11, the stage's own value),
  `mw.l 0x9110600c 0x76` (the restart key), `mw.l 0x91106000 0x1` (enable, reset on the first timeout),
  `md.l 0x91106000 4` (read). Memory-mapped registers only; nothing stored anywhere.
- *Hypothesis:* the second read shows CR `1` and a counter that runs; with nothing feeding it, the board resets
  after one period of TOP 11 and comes back as `29f1:0230` by itself. The time from the enable to the reset is the
  period under U-Boot's clocks, which sets the TOP for a 60 s capture (each TOP step doubles it; TOP 15 is 16
  times TOP 11).
- *If it does not fire:* the script types `reset` 60 s after the last command (U-Boot is alive at its prompt),
  and the answer is that this watchdog cannot be armed this way: stop and report.
- *Rehearsal:* the command list and the fallback on the pty stand-in (the stand-in received exactly these bytes,
  then `reset\r` from the fallback).

**C1, the run (2026-11-18 16:50; one run; lock held; no hands): the watchdog was armed and counting, and did not
fire within the 60 s before the fallback.** Record `K230R/bench/C1-console.log`.
- Before: `91106000: 0000000c 00000000 0000ffff 00000000` (CR, TORR, CCVR as the ROM and SPL leave them).
- After the three writes: `91106000: 00000001 0000000b 07ffeba2 00000000`: enabled, TOP 11, and the counter
  already down by `0x145d` (5213) from `0x7ffffff`, at most 16 ms after the enable.
- No reset in 60 s. The script then typed `reset` (the fallback), and the board came back at Dev 114.
- *Reading:* the counter runs at some 0.3 to 0.6 MHz here (5213 counts in 9 to 16 ms), so TOP 11 (`2^27` counts)
  is minutes under U-Boot's clocks, not the seconds assumed from the stage's runs. The hypothesis's period was
  wrong; whether the timeout resets the board from U-Boot is still not shown.

**C1b: the same test with the rate measured and a TOP that fits it (hypothesis, written before the run).**
- *Sent at the prompt:* `mw.l 0x91106004 0x66` (TOP 6: `2^22` counts), `mw.l 0x9110600c 0x76`, `mw.l 0x91106000
  0x1`, `md.l 0x91106008 1`, `sleep 2`, `md.l 0x91106008 1`. Nothing booted.
- *Hypothesis:* the two counter reads two seconds apart give the rate (about 0.3 to 0.6 MHz); the board resets
  7 to 13 s after the enable and comes back as `29f1:0230` by itself. Fallback: `reset` typed 40 s after the last
  command; then the watchdog's timeout does not reset the board from here, and C2 is off.

**C1b, the run (2026-11-18 16:52; one run; lock held; no hands): the watchdog armed from the prompt reset the
board, and the ROM came back by itself.** Record `K230R/bench/C1b-console.log`. Bytes sent: `b' '`, then the six
commands above, each with `\r`; the fallback was not used.
- Enable written at 1795045929.013. Counter `003ff892` at .016, `0028fb00` at 1795045931.025 after `sleep 2`:
  1,506,706 counts in 2.009 s, **750 kHz**. TOP 6 (`2^22` counts) is then 5.59 s.
- The ROM's `boot failed with exit code 13` at 1795045934.638 (the ROM prints it about 0.35 s after a reset), so
  the reset came about 5.3 s after the enable; `29f1:0230` back at Dev 115 4.0 s after the last command.
- **Under this U-Boot's clocks the watchdog counts at 750 kHz, its timeout resets the SoC, and the reset returns to
  ROM USB with the no-boot card.** Periods: TOP 9 is 44.7 s, TOP 10 is 89.5 s, TOP 11 is 179 s (which is why C1
  saw nothing in 60 s).

**C2: the kernel booted under the armed watchdog, a 60 s capture (the word given 2026-11-18; written before the
run).**
- *What is known about this kernel and the watchdog (read on the PC from the image file, read only):*
  - The device tree U-Boot's `bootcmd` loads, `/boot/k230d-ai-cam.dtb` in the image's first partition (4863
    bytes, sha256 `1e75347b…`): **it has no watchdog node at all.** Its nodes are the PLIC and CLINT, the reset
    controller (`canaan,k230-rst`), the pinctrl, the clock controller (`canaan,k230-clk`), five `snps,dw-apb-uart`
    (only `serial@91400000` is `okay`), the USB PHY, `snps,dwc2` (disabled), and `mmc@91581000`
    (`canaan,k230-sdio`). `chosen` has the same `bootargs` as U-Boot's environment.
  - The kernel, `/boot/Image` (31,796,736 bytes, sha256 `85536fa3…`, `Linux version 7.3.3`), carries its
    config: `CONFIG_WATCHDOG=y`, `CONFIG_WATCHDOG_CORE=y`, `CONFIG_WATCHDOG_HANDLE_BOOT_ENABLED=y`, and no
    DesignWare watchdog driver (no such option set; the strings `dw_wdt` and `snps,dw-wdt` are not in the image).
    **So no driver binds the watchdog: nothing in this kernel feeds it, stops it or takes it over.**
  - What could still stop it: the clock driver (`CONFIG_COMMON_CLK_K230=y`) turning off clocks nobody claimed,
    which the kernel does late in its start; a kernel that hangs before then never gets there. This record's B3:
    "Gaia's kernel hang before earlycon is Gaia's; it is not a precondition here". A change of the watchdog's
    clock rate by that driver would change the period; not known.
- *Sent at the prompt:* `mw.l 0x91106004 0xaa` (TOP 10), `mw.l 0x9110600c 0x76`, `mw.l 0x91106000 0x1`,
  `md.l 0x91106000 4`, `boot`. After `boot` nothing is sent, whatever appears.
- *The period chosen:* TOP 10, `2^26` counts, **89.5 s** at the 750 kHz measured in C1b: the first value past the
  60 s capture (TOP 9 is 44.7 s), with the reset about half a minute after the capture ends.
- *Hypothesis:* `boot` reads the card and starts the kernel; whatever it prints is captured with a stamp on each
  line; about 89.5 s after the enable the watchdog resets the board and `29f1:0230` comes back by itself.
- *Stop:* not back within the period plus 30 s (the script's limit is 125 s from its start): nothing more is
  done to the board, no load, no reset attempt, no byte to the console; the coordinator is told at once with the
  last console lines. One RST press by a person is the accepted worst case.
- *Not used:* `reboot_to_upgrade`, `k230_dfu`, `saveenv`, any `mmc write`.
- *Rehearsal:* the command list on the pty stand-in with a hold; the script sends nothing after its last
  command (it has no fallback in this run).

**C2, the run (2026-11-18 16:54; one run; lock held): the kernel did not hang, it booted to a login prompt; the
watchdog did not bring the board back. STOPPED; nothing done to the board since.** Records
`K230R/bench/C2-console.log` (345 lines, each stamped), `C2-load.txt`, `run-C2.out`.
- Bytes sent: `b' '`, `mw.l 0x91106004 0xaa\r`, `mw.l 0x9110600c 0x76\r`, `mw.l 0x91106000 0x1\r`, `md.l 0x91106000
  4\r` (answer `91106000: 00000001 0000000a 03fff878 00000000`: enabled, TOP 10, counting), `boot\r` at
  1795046041.772. Nothing after.
- U-Boot: `MMC read: dev # 1, block # 7168, count 1024 … OK`, `31796736 bytes read in 955 ms`, `4863 bytes read`,
  `## Booting kernel from Legacy Image at 03000000`. Then `OpenSBI v1.7`, `Linux version 7.3.3`, `Machine model: K230D
  CanMV Junroc AI Cam`, `earlycon: sbi0`, `printk: console [ttyS0] enabled`, `mmc0: new high speed SDXC card at address
  aaaa`, `mmcblk0: mmc0:aaaa SR64G 59.5 GiB`, `mmcblk0: p1 p2`, **`[    1.974241] clk: Disabling unused clocks`**,
  `EXT4-fs (mmcblk0p1): mounted filesystem … r/w`, `Run /sbin/init as init process`, `Starting syslogd: OK`,
  `Starting klogd: OK`, `Running sysctl: OK`, `usb0: no USB device controller`, `Starting crond: OK`, `Starting dropbear
  sshd: OK`, `Welcome to the CanMV K230D AI-CAM`.
- *The last line:* `k230d-ai-cam login: ` at 1795046047.169, 5.4 s after `boot`. Nothing after it in the 119 s to the
  script's limit.
- *The watchdog:* no reset within its 89.5 s plus 30 s. The kernel passed `clk: Disabling unused clocks` two
  seconds into its start; with no driver holding the watchdog's clock, that is the step that fits a counter that
  stopped. Not proven: the counter was not read after it.
- *The board now:* running Linux at a serial login prompt. On the PC, `3-4.2.1.4.1` still lists `29f1:0230` at
  Dev 115, the number from before the load, with no disconnect in the PC's kernel log: the ROM's enumeration left
  standing, not a ROM that answers. It has not been opened.
- **The card has been written by the booted system.** It mounted `/dev/mmcblk0p1` read-write and started its
  services, so the first partition (sectors 8,192..249,856) no longer holds exactly what was flashed, and the
  16:44 hash is no longer true of it. Sectors 2048 and 3072 are outside both partitions and still zero as far as
  anything run here goes; the ROM still cannot boot the card.
- *B3, as this run saw it:* this image's kernel did not hang before earlycon when started this way (SPL over ROM
  USB, U-Boot's own `boot`). Why a boot from the card's own headers would differ is not looked at here.
- *Ways back (none taken; they wait for a word):* a login on the console and `reboot` (the SoC reset, then the
  ROM with a card it does not boot, as `reset` did from U-Boot three times today); or one press of RST.
  (Later: (b) was taken and refuted; see the way back after C2 below.)

**The way back after C2 (2026-11-18 17:2x; main session; rules: a word from the coordinator for each way).**
- *(a) serial SysRq:* not possible. The image's kernel (embedded config, read on the PC) has
  `# CONFIG_MAGIC_SYSRQ is not set`. Nothing sent.
- *(b) login and `reboot`:* allowed because the image's own files give root no password (`/etc/shadow` root field
  empty in the image's p1; the image recipe's os.fragment lines 39-43: root login on, getty on ttyS0 at 115200, no root
  password set). Tool `K230R/bench/login-reboot.py`, rehearsed on three PC stand-ins (no password: back; a password
  asked: stops after `root`; no prompt: stops). Typed: a CR, `root`, `reboot`; no password was asked. The shutdown
  was clean (p1 remounted read-only), then `reboot: Restarting system` and nothing: no console output and no fresh
  enumeration in 118 s. **Refuted:** Linux's restart does not reset this SoC with this image (most likely no SBI system
  reset). Record `K230R/bench/WAYBACK-console.log`, `WAYBACK.md`.
- *(c) one RST press* (the user, 17:42): the board came back as ROM USB under a fresh device number.

**C3: C2 with `clk_ignore_unused` (hypothesis, written before the run, `K230R/bench/C3-plan.md`).** C2's watchdog
stopped when Linux disabled its clock as unused; with `clk_ignore_unused` on the kernel command line the clock stays
on, nothing in Linux feeds or stops the watchdog, so it resets the SoC about 89.5 s after the enable and the ROM
comes back as 29f1:0230 under a new device number. Refuted if the command line carries the word and no reset comes.
Commands at the prompt (no saveenv): `printenv bootcmd`, `printenv bootargs`,
`setenv bootargs "${bootargs} clk_ignore_unused"`, `printenv bootargs`, the C2 watchdog writes (TOP 10, restart key,
enable), `md.l`, `boot`. Rehearsed on the U-Boot stand-in (`C3-rehearsal.log`, 9 of 9 sent).

**C3, the run (2026-11-18 17:42; one run; lock held; no hands): confirmed.** Record `K230R/bench/C3-console.log`,
`run-C3.out`. U-Boot's `bootcmd` loads `/boot/Image` and the tree from the card and runs `bootm`; it does not set
`bootargs`, so the appended word reached the kernel: `Kernel command line: root=/dev/mmcblk0p1 rootwait rw
rootfstype=ext4 console=ttyS0,115200 earlycon=sbi clk_ignore_unused`. The login prompt came at about 6 s; 84 s
later the console printed the ROM's `boot failed with exit code 13` (the card has no boot copy), and the board
enumerated as 29f1:0230 dev 117 (was 116), 89.9 s after the last command: the watchdog's 89.5 s period.
- *What it settles:* C2's missing reset was Linux gating the watchdog's clock. With `clk_ignore_unused` (or a kernel
  that keeps the watchdog's clock, or a watchdog driver that owns it), a booted kernel under the armed watchdog comes
  back by itself: the way back after a kernel boot no longer needs hands.
- *Image requirement this gives (for the K230D image, via the coordinator):* either `clk_ignore_unused` on the command
  line, or the watchdog's clock marked critical / the dw_wdt driver bound to it, so an armed watchdog survives the
  kernel's unused-clock pass. And Linux's own `reboot` does not reset this board today; an image that needs `reboot`
  must give the kernel a working system reset.

**R2 is dropped** (no image exists for report-then-SoC-reset; the FL1 stage image gets its own review first).

**The capture (read only).** A separate unit reads both CH342 ports from before R0 to the end and writes no byte to
either. The console is never written in this run, so no step writes to the console.

## B2: the first run of the stage's own USB device path (plan, PC rehearsal, run 1)

Branch `pr/K230-B2`. The plan and rehearsal below were made with no board contact; run 1 follows them.

**Hypothesis.** The check build, loaded by the ROM over `29f1:0230` at `0x80360000` and started there, arms both
watchdogs, measures the watchdog clock, brings the dwc2 core up as a device, and is enumerated by this PC as
`1209:0001` "Lemnos flash stage" at port `3-4.2.1.4.1`, at high speed; EP0 answers the descriptors, GET_STATUS,
GET_CONFIGURATION and stalls an unknown request; the bulk pair carries FL1 frames up to the largest chunk, CRC
checked each way, refuses the largest chunk and one byte with status 5; END makes the stage print its record and
take the SoC reset; the ROM is back as `29f1:0230` under a new device number. Any one of these can fail: each is a
separate line of the record.

**The build.** `bash drivers/k230-usbboot-image/check.sh --b2` (the `b2` feature): the FL1 session
(`fl1::run::stage_with`) over the rewritten dwc2 driver (`drivers/k230-usbboot-sys/src/dwc2.rs`), storage a RAM
window of 128 sectors at LBA 1048576 and a read-only record sector at 1048704 (`fl1::check::Loopback`); any other
sector is status 2. Build id `0x4b32333042320001` (not the card build's). `check.sh --b2` refuses the image if any
SD host symbol or SD base address load is in it ("SD symbols 0, SD base loads 0"); the Loopback tests refuse writes
outside the window. At rev `efa03253`: 23,968 bytes, ends `0x803b6030` (inside the vendor tool's range to
`0x80400000`; the ROM's own limit is unverified), sha256 `ba26d539…a074f` before the mode patch, `b203849c…ee9fd` as
loaded (mode `check`, `0x4b`).

**Watchdog.** The entry arms both at setting 11 before any Rust (at 750 kHz, C1b: 179 s). The check then measures
`wdt0`'s clock over 20 ms of the timebase (27 MHz, `k230.dtsi:20`, unverified in the ROM's state; the record keeps
the ticks so the console's stamps check it), adds 5 %, and the shared entry re-arms at setting 15 and lowers to the
smallest period covering the run budget, 60 s (init, the 30 s idle limit, the host's whole check). At 750 kHz that
is setting 10, 89.5 s. It is never fed: one period from the arming bounds the whole run whatever the host does. A
clock outside 100 kHz..200 MHz ends the run before USB (report, SoC reset). Every stage wait ends in a reset: END,
the idle limit (30 s), the enumeration limit (the idle deadline), a failure: each prints and takes the SoC reset;
the watchdog is the backstop.

**Host side** (`k230-b2 run`, `testing/crates/k230-usbboot/src/b2`): holds `/run/user/1000/lemnos-k230d.lock`;
loads through the ROM (identity `K230` first); waits at most 15 s for `1209:0001` at the port (sysfs only); refuses
before claiming if the product is not "Lemnos flash stage" or the interface string is not the wire's; a `1209:0001`
at any other port is never opened and, if it is the only one, the run is refused as the wrong port; checks access
with `access(2)` on the node (opens nothing); a denial stops the run and says so. Then EP0 (descriptors against the
shared stage's bytes, GET_STATUS device and `0x81`, GET_CONFIGURATION, two unknown vendor requests that must stall,
GET_STATUS after them), identify (must be the check build), writes and reads of 1, 8, 9, 64, 127 and 128 sectors
(status 0, the reply's CRC checked, compared), a chunk with a bit flipped after its CRC (status 1, window
unchanged), the largest chunk and one byte (status 5, window unchanged), 129 sectors (status 5), an 11-byte request
(status 5), identify again, the record sector (bench checks below), END. No request addresses a sector outside the
window and the record. Then the ROM back at the port under a new number, at most 200 s (the entry's 179 s worst
case with margin). Bounds: control 1 s, bulk 5 s.

**Access on this PC (read only, 2026-11-18): will not work without a system change.** `/dev/bus/usb/003/117`
(`29f1:0230`) is `crw-rw----+ root root`, ACL `user:sozo:rw-`, from `TAG+="uaccess"` in
`/etc/udev/rules.d/60-atlas-usbboot.rules:19`, which matches `29f1:0230` only. No rule matches `1209:0001` (the
only `1209` rules are hidraw and other products); the hwdb gives it a name and nothing else. Nodes without a rule
on bus 3 are `crw-rw-r-- root root` (13 of them): `open(O_RDWR)` fails EACCES (libusb `LIBUSB_ERROR_ACCESS`), and a
read-only open fails every usbfs ioctl (`devio.c`, FMODE_WRITE). So the run will stop at the access check with the
stage enumerated: the record still gets the enumeration from sysfs (product, interface string, speed, configuration,
device number) and the way back (the stage's idle limit, 30 s after configuration, then the SoC reset). EP0 and
bulk need access: granting it is the user's call.

**Bench checks the record carries** (`fl1::check::record`, printed as `b2 <name> <hex>` and read over USB):
speed (DSTS ENUMSPD 0, and sysfs 480); SET_ADDRESS order (`set-address` equals the kernel's device number, DCFG read
right after the write holds it, `address-status-in` 1: the status IN went out after the address was written);
bulk OUT re-arm after each frame (`out-rearms` equals `out-frames`, `out-longest` 65539, `out-oversize` 0); also
GSNPSID, GHWCFG3, GAHBCFG as the ROM left it, how the core reset ended, bus resets, setups, stalls, endpoint 0 status
stages, IN transfers and bytes, the longest FIFO wait in reads, the measured watchdog clock and the setting chosen.
The console's words must equal the USB record's on the stable ones.

**PC rehearsal** (`k230-b2 rehearse CASE`; `cargo test -p lemnos-k230-usbboot --test b2`): the board is the check
build's own code (`k230-usbboot-sys` `b2::check` and `b2::end`, the dwc2 driver, the FL1 device and session) in a
thread over a register model of the dwc2 core, the watchdogs at 750 kHz, the console and the SoC reset, with a kernel
that enumerates as Linux does; the host is the same `b2::run` the board run uses, through the same traits. Not a
timing model. Results at `efa03253`, every case as it must end:

| Case | Host's verdict | Board's reset | ROM back |
|---|---|---|---|
| good | passed (every EP0, bulk and record check; console agrees) | SoC, 1.06 s | 0.30 s after END |
| core from 4.20a | passed (`reset-done` 2) | SoC | yes |
| no access | stopped at the access check, never opened | SoC at its idle limit, 30.0 s | yes |
| wrong product | refused, never opened | SoC, 30.0 s | yes |
| wrong port | refused at the 15 s bound, never opened | SoC, 30.0 s | yes |
| never enumerates (bus hang in core reset) | gave up at 15 s | watchdog, 89.5 s | yes |
| bit flipped host to device | write status 1 (CRC) | SoC after END | yes |
| bit flipped device to host | read reply CRC mismatch | SoC after END | yes |
| endpoint 0 OUT never re-armed (the old driver's fault) | gave up: the kernel cannot enumerate | SoC, 30.0 s | yes |

Found by the rehearsal, each failing before its fix: a bus reset right after a request's data stage ended the
whole enumeration (the shared `flash-stage` `enumerate`); a zero-length status OUT reported twice; a stall on
endpoint 0's OUT half carried past the next setup by the re-arm; the report's SoC reset dropped by the image's bus.

**Faults of the driver before this branch** (`drivers/k230-usbboot-sys/src/dwc2.rs` at `8c46f7c8`, now
`dwc2_legacy.rs` for the card image). The register offsets and bits all match the SDK's `hw.h` (k230_sdk
`7e302f7`, `src/little/linux/drivers/usb/dwc2/hw.h`); the faults are in the sequences:
- `:149-180` endpoint 0's OUT completion is dropped (`:178`): no status stage is ever reported, so the FL1 adapter's
  `ep0_out_status` times out on every request with an IN data stage. Enumeration cannot complete.
- `:228-229` endpoint 0 OUT is enabled once, at core reset; the core clears it at each setup, so the second
  request's status stage is NAKed for ever.
- `:325-333` a stall of endpoint 0 sets only DOEPCTL0: an IN data or status stage is never stalled; the host times
  out.
- `:291-303` bulk IN waits for room for the whole transfer: any reply over the 512-word FIFO (a read of four
  sectors or more) times out.
- `:184-188` bulk OUT armed for 4,156 bytes, not a whole number of packets, and below the FL1 chunk (65,538).
- `:200-238` core reset: only the pre-4.20a CSFTRST self-clear (no CSFTRST_DONE, `hw.h:129`); no AHB idle wait
  (`hw.h:127`); no soft disconnect first; GAHBCFG left as the ROM had it (`hw.h:74-80`); device mode not waited
  for (`hw.h:175`); no FIFO flush; FIFO sizes not checked against GHWCFG3 (`hw.h:289-303`); the bulk pair enabled
  once, before any bus reset, and never again.
- `:243-256` a bus reset neither clears DEVADDR (`hw.h:458`) nor re-arms endpoint 0.
- Citations: `:16` gives DXEPCTL's bits as `hw.h:14-45` and `:222` the MPS as `hw.h:42-45`; they are at
  `hw.h:547-582`.
- In the stage crate at the same rev: `fl1/usb.rs:141-143` reports high speed without reading DSTS;
  `fl1/usb.rs:92-96` configures nothing; `wdt.rs:44` takes 1 MHz as the lowest watchdog clock, C1b measured 750 kHz.

**Way back, and why no hands.** Every outcome ends in a reset the board makes by itself: the stage's report and SoC
reset (END, idle 30 s, enumeration limit, failure), else the watchdog within one period (89.5 s at 750 kHz; 179 s
before the period is lowered). Nothing is written to the card, the ROM's settings or an environment. The host waits
200 s for the ROM; if it is not back, the run is stopped and reported, not recovered by hand.

**Open.** The `1209:0001` pair is pid.codes' test pair: it is replaced before any release. The ROM-load step opens
`29f1:0230` as the earlier K230-USB runs did; the B2 allowance names only `1209:0001`, so the load step's cover is
for the reviewer to confirm.

### B2 run 1 (2026-11-18, rev `df366ac6`): stopped by the stage before USB; the timebase does not run

GO relayed by the coordinator; access for `1209:0001` from `60-atlas-usbboot.rules:20` (added by the user). Lock
`/run/user/1000/lemnos-k230d.lock` held by `k230-b2 run` for the whole run; both CH342 ports read only. Image sha256
`b203849c…ee9fd` as loaded (mode `check`). Hypothesis written first (`run1-hypothesis.md`).

- 0.000 s: the ROM (devnum 117) answered `K230`; load and PROG_START sent.
- +0.20 s (console): the banner "lemnos stage: check (B2: …)", after the entry's own `!BWKC`.
- +11.18 s: "b2 the watchdog clock could not be measured; no USB bring-up", then the record words, all zero but
  the magic (nothing measured, USB never touched), "lemnos stage: end", "flash stage: bring-up failed" (setup step 4),
  and the SoC reset.
- 11.63 s: port empty; 11.73 s: `29f1:0230` back at `3-4.2.1.4.1` as devnum 118. The stage never enumerated, so the
  host made no open of `1209:0001`; no EP0 or bulk request was sent.

**Finding.** `ticks-arm-lo` and `ticks-now-lo` are both 0: the `time` CSR reads 0 in the boot ROM's state and does not
move. The measurement waited its whole bound (20,000,000 timebase reads, 11 s) for 20 ms that never passed
(`TimebaseStuck`), so the 27 MHz timebase assumed from `k230.dtsi:20` is not running under the ROM. The stage did what
the plan says: no USB bring-up, the report, the SoC reset; the board returned by itself, as found. Consequence beyond
the measurement: every deadline of the FL1 session (`fl1::check`'s clock, the shared idle and transfer budgets) is
read from the same CSR, so with it stopped no session deadline can pass; only read-count bounds and the watchdog
would end waits. B2 stays open: the stage needs a running time source (the timebase enabled by the stage, or the
watchdog counter or another running counter as its clock) before the next run. Not changed in this run.

Records: `run1-hypothesis.md`, `run1-dry.log`, `run1-run.log`, `run1-stdout.log` (in the session's K230B2 folder).

### B2 fix after run 1 (rev `959ff98b`): the stage starts the timebase and checks it

**Stand-in first.** The rehearsal's model now has a `time` CSR that reads 0 and never moves until the stage writes the
system timer's enables (`TimeCsr::StoppedUntilEnabled`, every case's default), plus `time-dead` (never moves) and
`time-running` (already running). With run 1's stage unchanged, case `time-stopped` failed as the board did: the
banner, "b2 the watchdog clock could not be measured; no USB bring-up", the zero record, the SoC reset, the ROM back,
the host GaveUp (`rehearsal-time-stopped-before-fix.log`). It stays as a permanent case.

**Why `time` is stopped, and the vendor's enable** (k230_sdk `7e302f7`):

- `src/little/uboot/arch/riscv/cpu/k230/start.S:142-143`: the SPL calls `harts_early_init` early in start-up; the
  same file reads `time` at `start.S:44`, before the enable, so nothing in the vendor start-up relies on `time`
  running in the ROM's state.
- `src/little/uboot/arch/riscv/cpu/k230/cpu.c:73-79` (`harts_early_init`): writes `1` to `0x91108020` (`:76`, the
  machine timer clock for CPU0), `1` to `0x91108030` (`:77`, CPU1), and `0x69` to `0x91108000` (`:78-79`, commented
  "enable stc0").
- `src/little/uboot/arch/riscv/cpu/k230/platform.h:98`: `SYSCTL_STC_BASE_ADDR` = `0x91108000`.
- rt-smart agrees: `bsp/maix3/board/board.h:183` (`STC_BASE_ADDR`), `c908/clint.c:52-53` (the same two clock writes).
- The field layout of `0x69` is in no source read; the stage writes the vendor's value as is.

**Chosen: the vendor enable, not the watchdog-counter fallback**, because the enable is found with file:line, is three
writes, and keeps the FL1 deadlines on the clock they were written for. It is trusted only after a cross-check:

- `fl1::check::enable_timebase` writes the three pairs in the vendor's order (`sysctl::STC_ENABLES`); the image's bus
  (`Mmio::write32`) allows exactly those three (address, value) pairs and nothing else in that block.
- `fl1::check::timebase_runs` then waits for `wdt0`'s counter (armed and shown counting by the entry) to count 7500
  (10 ms at 750 kHz; bounded by 20,000,000 reads) and requires `time` to have moved over them. If not, the stage
  prints "b2 the timebase does not move against the watchdog counter after its enable; no USB bring-up" and takes the
  SoC reset (setup step 5), before USB.
- The record gains `tb-cross-counts` and `tb-cross-us`; the host checks them against the measured clock (within a
  quarter) and that the console prints the same.

**Rehearsal fidelity fix found on the way.** The model's watchdog stepped its counter on every CCVR read; in the
cross-check's tight loop that ran it about 7 times fast, so the host's new check failed the good case. The B2 model's
watchdogs now count with time only, by whole cycles with the remainder carried (`wdt::Model::timed`,
`advance_cycles`), as the board's do.

**Rehearsal at `959ff98b`** (the board's bounds, every case as it must): good Passed, rev-420a Passed, no-access
Stopped, wrong-product Refused, wrong-port Refused, never-enumerates GaveUp, corrupt-out Failed (status 1), corrupt-in
Failed (host CRC), no-ep0-rearm GaveUp, time-stopped **Passed** (tb-cross 7500 counts in 9997 us), time-dead GaveUp
(the timebase line at +0.010 s, SoC reset, ROM back), time-running Passed (`rehearsal-*.log`). Tests: the stage
crate, the image crate, the host crate (15 B2 tests, 3 new), shared flash stage, maskrom rehearsal and the Rockchip
stage pass; clippy `-D warnings` clean. `check.sh --b2`: "b2: SD symbols 0, SD base loads 0", "image link checks
hold", raw sha256 `260494e8…6da4` (23,376 bytes; its listing has the three stores in order at `0x80362ae4`, `0x80362ac6` and `0x80362ade`);
the card build's `check.sh` holds too (`1240091…c654`).

### B2 run 2 (2026-11-18, rev `959ff98b`): the timebase runs; enumeration fails after SET_ADDRESS

Conditions as run 1: lock held by `k230-b2 run` for the whole run (free before and after), port `3-4.2.1.4.1` showed
`29f1:0230` "K230 USB Boot Device" before the load, product and interface checks before any open, no card code in the
image, consoles read only. Hypothesis written first (`run2-hypothesis.md`); dry run (`run2-dry.log`): ROM devnum 118,
access to its node granted, nothing opened. Loaded image sha256 `7c08b17d…6beb9` (mode `check`).

Times from the load (1795054241.847; kernel times from `journalctl -k`):

- 0.000 s: the ROM answered `K230`; load and PROG_START sent ("loaded and started").
- +0.20 s (console): `!BWKC`, then the banner. No timebase line and no "could not be measured" line followed.
- +0.229 s: kernel "USB disconnect, device number 118" (the ROM gone).
- +0.479 s: "new high-speed USB device number 119"; +5.777 s: "device descriptor read/all, error -110".
- +5.865 s: number 120; +16.016 s: read/all -110; "3-4.2.1.4-port1: attempt power cycle".
- +16.643 s: number 121; +27.280 s: read/all -110.
- +27.377 s: number 122; +30.293 s: "device descriptor read/all, error -71"; "unable to enumerate USB device".
- console, about 30 s after the banner: the record words, "lemnos stage: end", then "flash stage: the host did not
  configure the device in time" (the shared idle limit) run together with "boot failed with exit code 13" (not
  attributed: printed after the stage's last line), and the SoC reset.
- +30.537 s: number 123, `29f1:0230`, "K230 USB Boot Device": the ROM back at the same port. The host logged it at
  +30.658 s. No `1209:0001` ever showed, so nothing was opened and no EP0 or bulk request was sent.

Record (console only; USB never reached):

| word | value | reading |
|---|---|---|
| gsnpsid | `4f54430a` | core v4.30a (the model has 4.20a and its rev_420a case) |
| ghwcfg3 | `0bc884e8` | DFIFO depth `0x0bc8` words |
| gahbcfg-found | `00000000` | as the ROM left it |
| reset-done | `00000002` | core reset done |
| dsts | `00057800` | enumerated speed 0 (high speed); frame `0x578` |
| bus-resets / setups | 7 / 11 | four attempts, about three SETUPs each |
| set-address / dcfg-after-address | `0x11` / `0x110` | address 17 written into DCFG; under xHCI the host controller picks the address, so it is not the kernel's devnum |
| address-status-in | 1 | the SET_ADDRESS status stage was sent |
| ep0-status-out | 1 | one OUT status stage completed in four attempts |
| stalls / configured | 0 / 0 | |
| out-* / in-* | 0 | no bulk traffic |
| wdt-first / wdt-last / wdt-elapsed-us | `07fe9691` / `07fe5bf9` / 20000 | 15,000 counts in 20 ms |
| wdt-hz / wdt-top | 750000 / 10 | as C1b; 89.5 s chosen |
| ticks-arm-lo / ticks-now-lo | 0 / `3053fc3f` | 810,810,431 ticks at 27 MHz = 30.03 s, matching the console's banner-to-end span |
| tb-cross-counts / tb-cross-us | 7500 / 10000 | `time` moved 10 ms while the watchdog counted 10 ms at 750 kHz |

**Findings.**

1. The vendor enable starts `time` from the ROM's state, at 27 MHz: the cross-check, the watchdog measurement
   (750,000 Hz, as C1b) and the 30 s idle limit (ticks against the console's stamps) agree. Run 1's blocker is closed.
2. The stage's USB device path does not enumerate on this core. Each attempt gets past the bus reset, the first
   request and SET_ADDRESS (the address is in DCFG, its status IN was sent), then the full device descriptor read at
   the new address times out (-110); the fourth attempt ends in a protocol error (-71). Only one EP0 OUT status stage
   completed across four attempts. The PC rehearsal enumerates the same code, so the model differs from the v4.30a
   core here: per the stand-in rule, the next step is a model case that fails like this before any change to the
   driver, then the fix, then run 3.
3. The host's 15 s bound to see the stage ended at 30.658 s, not 15 s: the sysfs scan printed nothing between +0.024
   s and +30.658 s while the kernel was retrying. Likely cause (not checked): the scan reads attributes of every
   device on the bus, including the parent hub's, and attributes such as `bConfigurationValue` wait for the device
   lock the hub's enumeration holds. The board was not at risk (the stage's idle limit reset it, and the watchdog
   was armed at 89.5 s), but the bound must hold: the scan should read only lock-free attributes until the port's
   device is the stage.
4. The record check "set-address equals the kernel's devnum" is wrong under xHCI, where the host controller chooses
   the address; it must compare with the address the stage is spoken to at, or be dropped.

Records: `run2-hypothesis.md`, `run2-dry.log`, `run2-run.log`, `run2-stdout.log`; rehearsals `rehearsal-*.log`
(in the session's K230B2 folder). The board is back at the ROM as found; the lock is free.

### B2 fix after run 2 (rev `4cd9318b`): EP0 trace, the status OUT arm, the scan bound, the address check

**What the references say about EP0 on this core.** These are read-only facts. No code was copied.

Linux `drivers/usb/dwc2/gadget.c`:
- 1186-1191: the setup arm is EPENA without CNAK. "For Setup request do not clear NAK".
- 2047-2079 (`dwc2_hsotg_program_zlp`): the status ZLP is armed with `MC(1)|PKTCNT(1)|XFERSIZE(0)`, then
  CNAK|EPENA|USBACTEP. It is written unconditionally, with no EPENA test. Line 2076: "clear NAK set by core".
- 2509-2521: some cores send no OUTDONE for the setup packet.
- 2684-2770 (`dwc2_hsotg_complete_in`): after the IN data stage completes, the status OUT is armed
  (`dwc2_hsotg_ep0_zlp(hsotg, false)` at 2767).

Linux `core.h`: the newest revision named is `DWC2_CORE_REV_4_20a`. No rule specific to 4.30a was found in
gadget.c, core.c, hw.h or params.c.

SDK 7e302f7 U-Boot `drivers/usb/gadget/dwc2_udc_otg_xfer_dma.c:69-95` (`dwc2_ep0_complete_out`): it sets
DOEPTSIZ0 = PKTCNT 1 | 8, then EPENA|CNAK. Its `dwc2_udc_otg.c` has two more relevant parts:
- 461-487: GUSBCFG with TRDT 5 and PHYIF16;
- 1112-1119: the kendryte part clears the DM/DP pulldowns at `USB0_TEST_CTL3` (0x9158507c).

SDK rt-smart CherryUSB `port/dwc2/usb_dc_dwc2.c`:
- 322-334 (`dwc2_ep0_start_read_setup`): in slave mode the endpoint is not enabled for the setup.
- 845-975 (`usbd_ep_start_read`/`start_write`): EPENA|CNAK is written unconditionally.
- 1128: TRDT is set on ENUMDONE.

The other candidates were checked on this core:
- DMA, descriptor DMA and thresholds: GAHBCFG was 0 as the ROM left it (run 2), so DMA is off. The stage runs in
  slave mode and writes no threshold register.
- The EP0 TxFIFO is number 0. Its size is set by the stage in GNPTXFSIZ, as before.

**The stage's fault.** `arm_ep0_out` skipped the arm while DOEPCTL0.EPENA was set. After a SETUP the core
leaves EP0 OUT enabled with NAK set, so the status OUT stayed NAKed. The fix:
- the arm is written every time, with CNAK except for the setup arm;
- the status OUT is armed after IN XferCompl, as in gadget.c 2684-2770 above.

**Model.** `b2sim` gets `CoreRev::V430a` (the default; GSNPSID `4f54430a`). On each SETUP it sets NAK on EP0 IN
and OUT and keeps EP0 OUT's EPENA. That is what gadget.c 1186-1189 and 2076 handle. It also models
DIEPINT0/DOEPINT0 with write-1-to-clear.
- Case `core-430a` against the old stage fails: GD(64) times out on all three tries. The trace shows the skipped
  OUT arm (`…0106` with EPENA set), the 18-byte IN done, and no status (`rehearsal-core-430a-before-fix.log`).
- With the fix it passes.
- `rev-pre-420a` stays as a separate case.

The model fails at the first GD(64), while the board in run 2 got to SET_ADDRESS. Not checked: Linux's
`hub_port_init` may accept a valid 64-byte descriptor even when its status stage fails. The model's host does not.

**Host scan.** Linux `usb/core/sysfs.c` (5.10 in the SDK) takes the device lock for two attributes:
- `product` (`usb_string_attr`, 123-140);
- `bConfigurationValue` (`usb_actconfig_show`, 24-45 and 90).

These read without the lock: idVendor and idProduct (709-723), busnum and devnum (199-215), speed (144), and the
interface string (1121-1132).

The scan now reads only the lock-free attributes on every device. On the stage's or the ROM's ids it reads the two
locking attributes on a thread with a 100 ms bound. A stalled read is kept pending, keyed by devnum, and is never
waited on twice. Two tests use the `SlowTree` stand-in, whose attribute reads block. Both fail on the old scan
("the scan blocked") and pass now (`scan-before-fix.log`):
- the 15 s bound ends on time while the attributes block;
- a stage whose attributes block for a while is taken once they read.

**Address check.** Under xHCI the host controller picks the USB address, and the kernel's devnum is a separate
count (run 2: address 0x11, devnum 119-122). The host cannot see the address, so the comparison with devnum is
dropped. The record check keeps `set-address != 0` and DCFG.DAD (`(dcfg >> 4) & 0x7f`) == set-address.

**Card image.** `check.sh` builds with one codegen unit. With 16 units the inlining moved and the card image grew
to 66864 bytes, which ran past 0x80380000. With one unit it is 64824 bytes, with or without the trace code. The
trace is a feature (`ep0-trace`), on only for `b2`.

Rehearsal: all 14 cases AS IT MUST (`rehearsal-*.log`). 380 tests pass, and clippy is clean with and without the
feature. Images:
- B2: raw sha256 `deece853…3d76`, 27064 bytes; patched to `check`, `9b79d8ec…cf79`.
- Card: `cf089a11…3c0e`, 64824 bytes.

### B2 run 3 (2026-11-18, rev `4cd9318b`): EP0 works and the stage enumerates; the host may not open its node

Conditions are as in runs 1 and 2:
- the lock was held for the whole run, and was free before and after;
- the port showed `29f1:0230` devnum 123 before the load;
- the checks came before any open;
- the image has no card code.

Hypothesis first (`run3-hypothesis.md`). Dry run: access to the ROM node granted, nothing opened.

Times from the load (1795056230.634):
- 0.000 s: loaded and started.
- +0.21 s: the banner. The timebase runs, as in run 2.
- +0.648 s: kernel "new high-speed USB device number 124". It shows 1209:0001 with product "Lemnos flash stage" on
  the first attempt, with no retries and no errors.
- +0.649 s: the host saw the stage with all its checks correct:
  - port 3-4.2.1.4.1;
  - speed 480 and configuration 1;
  - the product;
  - interface "lemnos flash wire D0D1".
- +0.649 s: the host's verdict was **Stopped**: `/dev/bus/usb/003/124` was mode 600, owner 0:0, with no ACL. The
  host opened nothing and changed nothing on the host.
- about +30.3 s (console): the record, the EP0 trace, "lemnos stage: end", then "the host sent no request before the
  idle deadline", and the SoC reset.
- +30.720 s: the ROM was back at the same port as `29f1:0230` devnum 125, on its own. The lock was free afterwards.

Record, as it differs from run 2:
- bus-resets 2, setups 9;
- set-address 0x11, and dcfg-after-address 0x110 (DAD = 0x11, so the check holds);
- address-status-in 1;
- ep0-status-out 7;
- stalls 0, configured 1;
- no bulk traffic;
- tb-cross 7500 counts / 10000 us, and wdt-hz 750,000.

**The EP0 trace (127 entries).** It is decoded from the head word: kind, flag, and frame number.

1. The first GET_DESCRIPTOR(device, 64) at address 0 (t 007-014):
   - At the SETUP, DOEPCTL0 = `80028000`: EPENA, NAK and USBACTEP. This is the state the 4.30a model assumes,
     now seen on the board.
   - IN armed for 18 bytes (DIEPTSIZ0 `00080012`).
   - SETUP_DONE: DOEPCTL0 `00028000`, DOEPTSIZ0 `40080038` (STUPCNT 2, 8 bytes taken), DOEPINT0 `08`.
   - IN done.
   - **status OUT armed** (`…0206`): DOEPCTL0 goes from `00028000` to `80008000`, so NAK is cleared.
   - DOEPINT0 `2010`.
   - OUT done: DOEPTSIZ0 `20000000`, XferCompl.
   - setup re-armed.

   This is the step the old stage never took.
2. The bus reset (GINTSTS `04009c28`), ENUMDONE (DSTS `00070800`), SET_ADDRESS 0x11 (DCFG `110`), and its status IN
   done.
3. At the new address, each of these takes the same full sequence, and every status OUT completes:
   - GD(device, 18);
   - GD(config, 9);
   - GD(config, 32);
   - GD(string 0);
   - GD(string 2);
   - SET_CONFIGURATION 1, with its status IN;
   - GD(string 3).
4. After that there was no further EP0 traffic until the idle reset.

**Findings.**
1. Run 2's enumeration failure was the status OUT left NAKed by the old arm. The trace confirms the model's 4.30a
   behaviour on the board. Enumeration and EP0 now work on the first attempt.
2. The host was denied the stage's node 0.649 s after it appeared. The ROM's node, opened by the same host, is
   granted.
3. The coordinator reports two things: the udev rule for the stage is installed and verifies, and the likely cause is
   a race. The uaccess ACL is applied late in udev's processing, after the host has already tried the open. That is
   the next model case and host fix (wait for the udev database entry and W_OK, bounded at 5 s), then run 4.
4. No bulk echo, negative case or USB record was reached in this run.

Records: `run3-hypothesis.md`, `run3-dry.log`, `run3-run.log`, `run3-stdout.log`. The board is back at the ROM as
found, and the lock is free.

### B2 fix after run 3 (rev `ee6fd671`): the host waits for udev before the open

The coordinator found that the rule for the stage is installed and `udevadm verify` passes. The likely cause of
run 3's denial is a race: udev applies the uaccess ACL late in its rules, after the kernel has published the
device, and the host tried the open 0.649 s after the load.

- **Model first.** The rehearsal gets the fault `late_acl` and the permanent case `late-acl`. udev's database entry
  and the node's ACL come 400 ms after the kernel publishes the stage; until then `access` is denied (mode 600, no
  ACL). Against the old host the case failed as the board did: Stopped, "may not open"
  (`rehearsal-late-acl-before-fix.log`).
- **Host.** `Board` gets `udev_done`. On the board it reads the device's `dev` attribute (`189:<minor>`, the driver
  core's attribute, no device lock) and checks that `/run/udev/data/c189:<minor>` exists.
  - After the port, product and interface checks, and before any open, the host polls (100 ms) until udev is done
    and `access(R|W)` is granted. The wait is bounded by `Bounds::udev`, 5 s.
  - Past the bound it stops exactly as before and opens nothing. If access is granted but there is still no entry,
    it stops with "udev has not finished the device".
  - The other plan bounds are unchanged. The new 5 s bound ends at most about 6 s after the load, inside the
    stage's 30 s idle limit.
- **Tests:**
  - the late ACL is waited for and then opened;
  - an ACL after the bound stops the run unopened;
  - access granted without udev's entry is not enough;
  - `udev_done` reads the entry from `dev`, and is false with no `dev`, with no entry, or with a non-USB major.
- **Checks.** All host and driver tests pass, and clippy is clean. All 15 rehearsal cases are AS IT MUST, with
  `late-acl` granted 0.400 s after the checks.
- **Read-only check on this host before run 4.** The ROM's node `/dev/bus/usb/003/125` (`189:380`) carried
  `user:sozo:rw-`, and `/run/udev/data/c189:380` existed.

### B2 run 4 (2026-11-18, rev `ee6fd671`): PASSED; the stage's USB device path works end to end

Conditions are as in runs 1 to 3:
- the lock was held by `k230-b2 run` for the whole run, and was free before and after;
- the port showed `29f1:0230` devnum 125 before the load;
- the checks came before the open;
- there were no card writes, no request outside LBA 1048576..1048704, and no request outside the window;
- the consoles were read only.

Hypothesis first (`run4-hypothesis.md`). Dry run: access to the ROM node granted, nothing opened. The stage image
is the same as run 3's (patched sha256 `9b79d8ec…cf79`).

Times from the load (1795056961.124):
- 0.000 s: loaded and started.
- +0.53 s: the port was empty.
- +0.860 s (1.009 s from the run's start): 1209:0001 devnum 126 at 3-4.2.1.4.1 with all its checks correct:
  - speed 480 and configuration 1;
  - product "Lemnos flash stage";
  - interface "lemnos flash wire D0D1".

  The kernel enumerated it on the first attempt, with no errors.
- At the same moment: "access granted, udev done, 0.000 s after the checks".
  - udev had finished by the time the host took the stage, so the race seen in run 3 did not recur here.
  - The new wait was not exercised on the board. It stays as the guard, and the `late-acl` case covers it.
  - Because this run went through, the read-only capture for a stop (`run4-access.txt`) was not triggered.
- +1.056 s: interface 0 claimed.
- EP0, all as expected:
  - device descriptor 18 bytes;
  - configuration descriptor 9 bytes, and 32 bytes in full;
  - strings 0, 2 and 3 of 4, 38 and 46 bytes;
  - GET_STATUS for the device and for endpoint 0x81;
  - GET_CONFIGURATION;
  - both unknown vendor requests stalled;
  - GET_STATUS after the stalls.
- Bulk:
  - identify: the check build `0x4b32333042320001`, 104 bytes.
  - Write and read at 1, 8, 9, 64, 127 and 128 sectors: status 0, CRC held both ways, read back equal.
  - A chunk with a flipped bit: status 1, window unchanged.
  - 65539 bytes: status 5, window unchanged.
  - 129 sectors: status 5. 11 bytes: status 5.
  - identify after the refusals: the same record.
- +1.738 s: END status 0, and the verdict was **Passed**. The record over USB agreed with the console's stable words.
- +3.061 s: the ROM was back at 3-4.2.1.4.1 as `29f1:0230` devnum 127, by the stage's own SoC reset after END.

Record over USB:

| word | value | reading |
|---|---|---|
| gsnpsid | `4f54430a` | v4.30a |
| dsts | `0036e800` | high speed |
| bus-resets / setups | 2 / 21 | one enumeration, then the host's EP0 checks |
| set-address / dcfg-after-address | `0x11` / `0x110` | DCFG.DAD equals set-address |
| address-status-in / ep0-status-out | 1 / 17 | every control read's status OUT completed |
| stalls / configured | 2 / 1 | the two vendor requests |
| out-frames / out-rearms | 31 / 31 | bulk OUT re-armed after every frame |
| out-longest / out-oversize | 65539 / 0 | the oversize chunk arrived whole and was refused by status |
| in-transfers / in-bytes / in-fifo-waits | 22 / 312056 / 0 | |
| wdt-hz / wdt-top | 750000 / 10 | as runs 2 and 3 |
| ticks-now-lo | 42,108,979 | 1.56 s at 27 MHz since the arm, as the console shows |
| tb-cross | 7500 / 10001 us | |

The console's later copy has out-frames 32 and in-transfers 24, because the USB read came before END. The trace
had 278 entries, of which 160 were kept.

**Findings.**
1. B2's goal is met on the board. Through its own dwc2 device path on the v4.30a core, the stage:
   - enumerates at high speed;
   - answers every EP0 check, stalls included;
   - serves the bulk wire in both directions at every planned size with CRCs;
   - refuses the bad chunks as specified;
   - reports a record that matches the console;
   - resets back to the boot ROM on END.
2. Run 3's denial is guarded by the bounded udev wait. In this run udev was already done when the stage was
   taken, so the board did not exercise the wait.

Records: `run4-hypothesis.md`, `run4-dry.log`, `run4-run.log`, `run4-stdout.log`. The board is back at the ROM as
found, and the lock is free.

### B2 card writes: the card's layout read from the card, and the scratch range (2026-11-18, read only)

The coordinator approved card writes under a scope that starts read-only: find a scratch range from the card itself,
model the stages, and write the plan. No write run comes before the coordinator's go.

**How the card was read.** Through the proven piece path:
- `k230-card piece --op hash`, stage image `2da8d976…52d8`, `--fast`;
- the bench lock held for the whole run;
- port and product checked before each load;
- one ROM load per region, `piece retries 0` on each;
- the ROM back after each.

Nothing was written. Records are `survey.out` and `survey-S1.txt` to `survey-S5.txt` in the session's K230B2 folder.

The source image is `k230d-ai-cam-varA.img`, read on the PC: GPT with a protective MBR; first usable sector 34, last
usable 415,710; p1 `rootfs` 8,192..249,855; p2 `data` 249,856..413,695; backup header at 415,743 with its entries
from 415,711.

The non-zero runs in its boot area:
- 2,048..2,472: SPL copy 0;
- 3,072..3,496: SPL copy 1;
- 4,096..4,797: U-Boot;
- 6,144..6,159 and 6,400..6,415: not attributed;
- 7,168..7,488 and 7,680..7,701: inside U-Boot's `bootcmd` read of 7,168..8,191.

| sectors | card sha256 | against the source |
|---|---|---|
| 0..33 (protective MBR, primary GPT) | `fda1a6cc…92e3` | equal |
| 415,711..415,743 (backup GPT) | `e841d204…fa09` | equal |
| 34..8,191 (boot area: both SPL copies, U-Boot) | `fa32a323…3c86` | equal, with 2048 and 3072 zero |
| 124,735,455..124,735,487 (the card's end) | `72d7e12e…30b2` | not zeros: data from before this session that the GPT does not name. Kept as found. |
| 1,048,576..1,050,623 | `30e14955…cb58` | all zeros |

**Findings.**
- The card's partition table and boot copies are the image's. Neither of the two Linux boots since the flash (C2,
  C3) resized or moved them.
- **Scratch range: sectors 1,048,576 to 1,050,623 (1 MiB at the 512 MiB mark).** The margins are:
  - 632,832 sectors past the backup GPT;
  - 634,880 past the end of p2;
  - 123,684,864 before the card's end.
  The range is the same mark as the write tests and the B2 window.

**Model** (`tests/card_scratch.rs`; the cases are permanent).

What today's stages do outside the range:
- **The FL1 card profile** writes every sector from 61,440 up, marked or not: first-1, last+1, both straddles, the
  backup GPT, and the card's end. It refuses 0, 1, 2048 and 3072 only without the compared mark.
- **The card image's stage mode** (the pre-FL1 stage) writes every write on its 8-sector grid, 2048 and 3072
  included. It refuses a plain write to sector 0, but it takes an erase of sectors 0..7, which wipes the primary
  GPT.

**The scratch stage** (`fl1::scratch::Scratch`) refuses every read and write outside the range, marked or not, and
touches nothing. It writes the whole range, each write read back equal. With the range check removed, its two
refusal tests fail.

**Before a write run:**
- the B2 path's card build with this storage, and the host's write mode: done, see the next section;
- their rehearsal in `b2sim`: done, see the next section;
- the coordinator's go on the plan (`PLAN-card.md`).

The plan's one boot check was ruled on: the C3 procedure, approved as described, after the final hashes.

### B2 card writes: the scratch build, its checks and its rehearsal (PC only, no board contact)

**The images** (`drivers/k230-usbboot-image/check.sh`, one codegen unit):

| build | check | file bytes | footprint | sha256 |
|---|---|---|---|---|
| scratch (`--b2-card`, the write run's image) | links only the scratch run: no piece, report, pre-FL1 stage, probe or RAM-window symbol; its own build id once, the check build's never; the kick has exactly one caller, the gate | 36,360 | `0x49a40` | `c8de108f72a0e031b58c8a2669e47a465a86f4a28f249b868fb9ab263293d23c` (was `6ff070bb…` before the feed gate) |
| check (`--b2`) | no SD host | 27,064 | `0x575f0` | `dafa94a67a176371056d542a4f29485c32232b5bb5fc0caa0503c80b7b884796` |
| card (default: piece, report, stage) | ends below the piece job's address `0x80380000` | 65,376 | `0x1ff90` | `ce8c161e46bd062112300f3c0fa11df473e5006f8aef29de642f30e6357f2956` |

**The check build's bytes changed since run 4, with the same code.** Run 4 passed with `deece853…`. The bounds
change made the card image's `crc32` `#[inline(never)]` to fit, and the check build's bytes became `dafa94a6…`, same
size. A normalised disassembly diff shows only the `compiler_builtins` block (memset and its neighbours) moved and
the offsets that follow it. The write run starts with a link-only run of these bytes (`k230-b2 run`, no card code in
that image), under the same conditions as runs 3 and 4.

**Size limit for the next card-image change: 112 bytes.** The card image (the piece path's) ends at `0x8037ff90`,
112 bytes below the piece job's address `0x80380000`. `check.sh` now prints the margin on every build. A change to
code the card image links (the SD host, the card protocol, the bounds, the piece or the report) that adds more than
that fails the check. Making room means a size change on purpose, not a rearrangement: an impl block placed earlier
in `sdhci.rs` renamed the image's symbols and moved its code with no code change (now noted in the file).

**What the scratch build does** (`drivers/k230-usbboot-sys/src/scratch.rs`; build id `0x4b32333042320002`):
1. The entry arms both watchdogs. The timebase is started and checked and the watchdog clock is measured, as in the
   check build. The same setting is chosen (covering 60 s at the measured clock: 10, 89.5 s at 750 kHz). The build
   feeds only after a verified write round (below).
2. The card comes up before USB:
   - the SD pads must be on the SD function;
   - the host must have named a controller (the write run names `sdhci1`, as the piece path does);
   - the card must start and be the card of record (CID `03534453523634478014a7c9ab015100`, 124,735,488 blocks);
   - the data clock (25 MHz) must start.

   Any failure here prints its line and takes the SoC reset, with no write to the card and USB never pulled up.
3. The storage is `Scratch` over `Sectors` over `Retried`:
   - any request outside 1,048,576..1,050,623, read or write, is refused whole before the card;
   - under it, the bounds refuse anything outside the declared range and every protected sector;
   - a failed block try is recovered with CMD12 and tried again, up to 3 times, and counted. This is the piece
     path's rule for the bench's one-in-14,000 data CRC failures at 25 MHz.
4. It runs only on the new `scratch` mode, which also carries the check flag:
   - the card image resets on the check flag;
   - the check build runs its RAM window and identifies as itself, and the host then sends no write.
5. The end prints `b2 card-cid0..3`, `b2 card-blocks-lo` and `b2 card-retries`, then the check build's record, the
   cause, and the SoC reset.

**Feeding: from "never fed" to "fed only on verified progress" (coordinator's ruling A).** The check build is never
fed: one period bounds its whole run. The write run can't be bounded that way. Its 4,098 writes each have a
read-back on the card, plus a read-back over USB, and at the piece path's measured card speed that can take longer
than one 89.5 s period. So the scratch build feeds, but only on verified progress:
- **The rule.** The dogs are kicked once after each write round whose read-back compared equal, at the same setting.
- **Where it is decided.** The shared loop calls the session watchdog after a write round's compare and after a read
  round's send. In the scratch build that watchdog is a gate (`fl1::scratch::VerifiedFeed`). The gate kicks only when
  the storage wrapper (`fl1::scratch::Watched`) saw, since the last kick, a write the storage took whole followed by a
  read of the same sectors equal to it by CRC. So the gate holds the same evidence as the round's own compare.
- **What never feeds:** a read round, a refusal, a failed or cut-off write, the idle wait, the USB poll. The build has
  no interrupt and no timer.
- **The kick sites in the image source**, from a grep of every watchdog restart-register write:
  - `fl1::scratch::kick`: **the feed**, `#[inline(never)]`. Its only caller is the gate (`VerifiedFeed`'s
    `Watchdog::feed`, also `#[inline(never)]`).
  - The arming, before the session: the entry's assembly (two loads of the kick value, one per dog), and
    `fl1::guard::Dogs::arm` and `set_top` through `guard::start`, inlined into `scratch::scratch` (four).
  - Present in the source but not linked in the scratch image: `Dogs`'s own `Watchdog::feed` (the check build's
    `Rounds`, which never feeds), `wdt::Dog::feed` and `fast_reset` (pre-FL1 stage), and `image.rs` `feed_wdt0`
    (piece path).
  - In the disassembly the kick value 0x76 is loaded in exactly these places: 2 in the entry, 4 in `scratch`, and 2 in
    `kick`.
- **The check.** `check.sh --b2-card` requires the kick to be one function, referenced by exactly one instruction,
  and that instruction to be inside the gate. Today it is: `kick defined 1 time(s), referenced from 1 instruction(s)`.
  It fails on a build with a second, direct call planted, as `kick has 2 callers` (`check-b2-card-planted-feed.log`).
- **One consequence, as intended.** A read round still moves the session's own idle deadline: the shared loop counts
  it as activity. It does not feed the dogs. A host that only reads keeps the session up, and the watchdog ends it
  one period after the last verified write.

**The host's write mode** (`k230-b2 write`, `testing/crates/k230-usbboot/src/b2/scratch.rs`):
- It loads only the image whose sha256 is given (a wrong sha: STOP, nothing loaded).
- It patches the `scratch` mode with `sdhci1`, then waits, refuses, waits for udev and claims as runs 3 and 4 do,
  and makes the same EP0 checks.
- The steps, with the first failure sending END and nothing more:
  1. identify, byte for byte;
  2. the range read before, which must be zeros;
  3. the first sector, then the last, each written and read back over USB;
  4. the range in 16 writes of 128 sectors, every other one marked, each read back;
  5. 11 deliberate writes outside, each unmarked and marked (22 in all), each expected refused:
     - first−1;
     - last+1;
     - both straddles;
     - 128 sectors ending past the last;
     - sectors 0, 1, 2048 and 3072;
     - the backup GPT header;
     - the card's last sector.

     Status 4 is expected without the mark in the boot area, 2 otherwise;
  6. the range read back unchanged;
  7. 16 zero writes, each read back, and the range equal to the before read;
  8. identify, then END.

**Rehearsal** (`b2sim` with `Build::Scratch`):
- The board side is the scratch build's own `scratch` and `end` over the dwc2, watchdog and console models, and an
  SDHCI model with a sparse card of the card of record's size and CID in its slot (`sdcard.rs`).
- The model card has the card of record's GPT header in sector 1, zeros in the range, and a non-zero pattern
  elsewhere.
- The plan's regions O1 to O7 and R0 are hashed from the model card before and after, standing in for the piece path's
  hash jobs. Every block write is logged.

Results:
- **Fail-before.** The range limit was removed for this run (storage over the whole card, protected sectors only),
  and `scratch-good` was run: **NOT AS IT MUST**. All three of these caught it:
  - the host stopped at the first deliberate write (1,048,575 answered status 0, not 2);
  - the write log shows 1 write outside the range;
  - O6's hash changed (`rehearsal-scratch-good-before-fix.log`).
- **With the limit, all 7 cases end AS IT MUST:**

| case | verdict | card writes | notes |
|---|---|---|---|
| `scratch-good` | Passed | 4,098, all inside | O1 to O7 and R0 hash as before; 22 refusals; retries 0 |
| `scratch-crc-retry` | Passed | 4,098, all inside | one sector fails its data CRC twice: retries 2 |
| `scratch-drops-writes` | Failed, status 3 at the first write | 1 (dropped) | the stage's compare catches the card; END, back |
| `scratch-wrong-card` | GaveUp | 0 | "not the card of record"; USB never up; SoC reset |
| `scratch-no-sd` | GaveUp | 0 | no controller named |
| `scratch-pads-off` | GaveUp | 0 | an SD pad on another function |
| `scratch-on-check-image` | Failed at identify | 0 | the check build answers; no write sent |
| `scratch-stall-before-compare` | Failed, status 2 at the stalled write; then the watchdog | 2 + 5×128 + 1, all inside | see below |

In the good case the watchdog setting is 10 at 750 kHz, as in runs 3 and 4. The model logs every kick: **68 kicks
after the card's ready line, 34 per dog, one per verified write round** (2 edges, 16 range writes, 16 zero writes),
and the record's `rounds` is 34. Reads and refusals made none. (Before the gate the count was 116: the shared loop's
read rounds fed too.)

**`scratch-stall-before-compare`** (ruling A's case):
- The card takes the sixth range write's first sector and then stays busy for good, so that round never reaches its
  read-back and compare.
- The host then does not send END. It keeps the session busy for up to 150 s with a read of a range sector and an
  identify every 500 ms. This is rehearsal only (`Plan::keep_asking`); the board run never sets it.
- **Fail-first:** a feed was planted in the shared poll loop (`serve::step`, every step) with the gate bypassed. The
  case was **NOT AS IT MUST** (`rehearsal-scratch-stall-before-compare-planted-feed.log`):
  - 1,802 kicks after the stall;
  - no watchdog reset;
  - the board stayed up until the host stopped and the session's idle end, a SoC reset at 180.8 s.
- **With the plants removed: AS IT MUST** (`rehearsal-scratch-stall-before-compare.log`):
  - 7 feeds (14 kicks), for the 2 edges and 5 range writes;
  - **no kick after the stall** (stall at 0.941 s from the load, last kick at 0.906 s);
  - **the watchdog reset at 90.370 s, 89.464 s after the last kick**, within one period (89.48 s at 750 kHz);
  - the board came back;
  - every region outside the range hashed as before.

`tests/b2_card.rs` holds the same cases.

### B2 card write run 1 (2026-11-18 21:23:56 to 21:47:46, rev `08948e8e`): the writes PASSED; C3 stopped at its gate

One script (`card1.sh`, outside the repository) held the bench lock on descriptor 9 for the whole run. The
hypothesis was written first (`card1-hypothesis.md`). Before every load the port showed `29f1:0230` with product
"K230 USB Boot Device". All four images matched their sha256 first: check build `dafa94a6…4796`, scratch build
`c8de108f…d23c`, piece stage `2da8d976…52d8`, and the SPL source `033903a1…c6cd`. Wall time: 1,429.8 s.

| step | at (s) | took (s) | result | ROM back as |
|---|---|---|---|---|
| 2a: check build `dafa94a6`, link only | 4.8 | 4.8 | PASSED, as run 4; no card contact | dev 18 |
| before O2 0..33 | 5.9 | 1.1 | `fda1a6cc…92e3` = survey S1 | dev 19 |
| before O3 415,711..415,743 | 6.9 | 1.0 | `e841d204…fa09` = survey S2 | dev 20 |
| before O4 34..8,191 | 20.3 | 13.4 | `fa32a323…3c86` = survey S3 | dev 21 |
| before O1 0..415,743 | 648.9 | 628.6 | `73064ef562c392409be896d8412f9095bce13c1d62b35ff962a49676b9c09698` (as found) | dev 22 |
| before O5 card's last 33 | 650.0 | 1.1 | `72d7e12e…30b2` = survey S4 | dev 23 |
| before O6 1,048,568..1,048,575 | 651.0 | 1.0 | `ad7facb2…2ca7` (zeros) | dev 24 |
| before O7 1,050,624..1,050,631 | 652.0 | 1.0 | `ad7facb2…2ca7` (zeros) | dev 25 |
| before R0 the range | 656.1 | 4.1 | `30e14955…cb58` (zeros) | dev 26 |
| 2b: the write run, scratch build `c8de108f` | 686.8 | 30.7 | PASSED, every step as rehearsed (below) | dev 28 |
| after O2, O3, O4 | 687.8 / 688.9 / 702.2 | 1.0 / 1.1 / 13.3 | each = before | dev 29 to 31 |
| after O1 | 1,330.7 | 628.5 | `73064ef5…9698` = before | dev 32 |
| after O5, O6, O7 | 1,331.7 / 1,332.7 / 1,333.7 | 1.0 each | each = before | dev 33 to 35 |
| after R0 | 1,337.9 | 4.2 | `30e14955…cb58` = before (zeros) | dev 36 |
| C3: SPL load, U-Boot, the commands up to `md.l` | 1,339.1 | 1.2 | load exit 0; the gate refused `boot` (below) | |
| C3: the watchdog's return | 1,429.7 | 90.6 | not booted; wdt0 reset the SoC 90.0 s after `md.l` (at about 0.1 s polling) | dev 37 |

**2b, the write run.** Every step was as rehearsed in `scratch-good`:
- the console printed "b2 card: the card of record, storage limited to 1048576..1050623", with CID
  `03534453 52363447 8014a7c9 ab015100` and blocks low word `076f5000`;
- identify returned the scratch build `0x4b32333042320002`'s record, byte for byte;
- the range read zeros;
- the first sector (unmarked) and the last (marked) each returned status 0 and read back equal over USB;
- 16 writes of 128 sectors returned status 0 and read back equal;
- the 22 refusals came back as planned: status 4 for the unmarked writes at 0, 1, 2048 and 3072, and status 2 for
  every other one, the marked writes at those four sectors included;
- after the refusals the range read back as written;
- the 16 zero writes left the range equal to the before read; identify returned the same record; END returned status 0;
- `b2 rounds 00000022` (34 verified write rounds, so 34 feeds), `b2 card-retries 00000000`.

Verdict Passed. The stage reset 0.74 s after END, and the ROM was back 1.22 s later. Every region outside the range,
and the range itself, hashes the same after as before.

**C3: stopped at the gate, correctly, on a defect in the gate.** The console script for this step is
`k230-card1-console.py`: the bench console script plus one gate before `boot`. The gate compares everything the
console said from "U-Boot SPL" on with C3's record. It refused `boot`, differing at byte 2,208 of 2,223:
- this run's line was `91106000: 00000001 0000000a 03fff8a3 00000000  ................`;
- C3's line was `91106000: 00000001 0000000a 03fff87d 00000000  ........}.......`.

The gate masked the counter word (wdt0's CCVR, the third word) in hex but not in the ASCII column, where the same
counter shows again: 0x7d prints as `}`, 0xa3 as `.`. With the ASCII characters masked as well, the whole 2,223 bytes
of this run equal C3's. So the only difference before `boot` was the live counter (0x03fff8a3 here, 0x03fff87d in C3;
both are 2.6 ms after the enable). Nothing else was sent. U-Boot sat at its prompt, and the watchdog armed by the
commands (`00000001 0000000a`) reset the SoC. The ROM was back as dev 37, 90.0 s after the last command. Per the
stop rule, no C3 boot, no re-hash after C3, no rerun and no recovery.

**Why the rehearsal missed it.** The replay stand-in changed the counter's hex word but left C3's ASCII column, which
mask v1 never looked at. A stand-in that passed where the board fails.

**Fix (`daafe07d`; PC only, not run on the board).**
- The mask now covers the counter in both places. The four ASCII characters must be the rendering of the hex word, and
  the word must lie in 0x03f00000..0x04000000.
- `--self-test` uses C3's and this run's real md.l lines. Mask v1 fails it (`--mask v1`, exit 1); v2 passes. A change
  outside the counter (CR 0), and ASCII characters that are not the word's, are still refused.
- The replay rehearsal (`k230-card1-replay.py`) now renders the ASCII column from the counter it sets. With this run's
  counter, v1 refuses `boot`, as on the board, and v2 sends it. C3's exact replay sends it. A changed `bootargs` byte, a
  late counter, or ASCII characters left stale are refused.

C3 is not to be rerun until the coordinator says so.

**Board state at the end:** the ROM, `29f1:0230` "K230 USB Boot Device", dev 37 at 3-4.2.1.4.1. Bench lock released.
The card holds what it held before the run (every hash equal; the range zeros).

### B2 card C3 rerun (2026-11-18 21:52:22 to 21:54:10, gate `559c7b9f`): booted as C3 did; the GPTs and boot area unchanged

Approved by Helios through the coordinator. It was C3 alone, then O2 to O4, with nothing else and no recovery. One script,
`card2.sh`, held the bench lock on descriptor 9 for the whole run, and the hypothesis was written first
(`card2-hypothesis.md`).
- Before the load the port showed `29f1:0230` "K230 USB Boot Device".
- The gate script was `k230-card1-console.py` at `559c7b9f` (sha256 `5e518912…e721`), and its `--self-test` passed.
- The load was C3's own: the same SPL source (sha256 `033903a1…c6cd`) and the same commands. There was no saveenv, and
  nothing was typed after `boot`.
- Wall time: 107.5 s.

| step | at (s) | took (s) | result | ROM back as |
|---|---|---|---|---|
| SPL load | 1.6 | 1.4 | exit 0 | |
| U-Boot, commands, gate, `boot`; the watchdog's return | 92.1 | 90.5 | gate passed; booted to the login prompt; wdt0 reset the SoC; console BACK | dev 38 |
| O2 0..33 | 93.1 | 1.0 | `fda1a6cc…92e3` = before | dev 39 |
| O3 415,711..415,743 | 94.2 | 1.1 | `e841d204…fa09` = before | dev 40 |
| O4 34..8,191 | 107.5 | 13.3 | `fa32a323…3c86` = before | dev 41 |

**The gate.** "2223 bytes since the SPL banner equal C3's record (counter 03fff89b masked, reference 03fff87d)".
`md.l` read `00000001 0000000a`: wdt0 was enabled at TOP 10 before `boot`.

**After `boot`.** All times are from the SPL load.
- U-Boot read 1,024 blocks at 7,168, then `Image` (31,796,736 bytes in 955 ms) and the device tree.
- "Booting kernel from Legacy Image", checksum OK.
- "Kernel command line: root=/dev/mmcblk0p1 rootwait rw rootfstype=ext4 console=ttyS0,115200 earlycon=sbi
  clk_ignore_unused".
- p1 was mounted read-write at 2.04 s kernel time. "Welcome to the CanMV K230D AI-CAM" followed, then the login
  prompt, 6.0 s after the load (C3: about 6 s).
- Nothing was typed. "boot failed with exit code 13" (the ROM) came 89.5 s after `md.l`, and the ROM was back as dev 38
  89.9 s after the last command (C3: 89.9 s).

The console from `boot` on equals C3's, apart from timestamps and:
- the xor/raid6 benchmark figures;
- two probe lines in another order (`mmc0: SDHCI controller on 91581000.mmc`, `Btrfs loaded`);
- one added line, `EXT4-fs (mmcblk0p1): recovery complete`. p1's journal was replayed: C3 ended with p1 mounted
  read-write when the watchdog reset the SoC, and so does this run. p1 is not hashed in this step. Its content has
  changed since the flash (O1 is recorded as found).

Like C3's, this console prints the kernel's GPT notices (the card is larger than the image's GPT). They are notices
only: O2 and O3 show nothing wrote either GPT.

**Board state at the end:** the ROM, `29f1:0230` "K230 USB Boot Device", dev 41 at 3-4.2.1.4.1. Bench lock released.

## No state only hands can undo

- Steps R1 to R3 write nothing to the card, the boot ROM's settings, or the stored environment.
- Every load is followed by a reset path that is proven before the load (R0.2, then R1 and R2). A stage that does not
  return is not recovered by hand in this plan: the run stops and is reported as blocked.
- The SoC reset is the only reset the stage takes after a failure; the watchdog is the reset if the stage
  dies before the report. Neither depends on the host.

## Run record

One record per run: the rev, the image's sha256, the commands sent with timestamps, the USB results, the
UART captures on both ports, and the reset each return took. Written to NVMe, not the Github HDD.

## Open items for the reviewer

- B1 (the `NO.2` at `0x80360006`) must be closed before R1.
- The load address is `0x80360000`, one constant: `image::LOAD_ADDRESS`. Evidence: run 1 and run-L2 loaded
  there and the ROM started the image; the vendor's kburn default is `0x80360000` (cited by Atlas,
  `kburn_k230.h:31`, not in our SDK clone); the SDK's U-Boot burner links at `0x80300000` but is a different
  binary. `check.sh` and `tests/image_link.rs` fail if the linker origin differs.
- The card's boot copy: the plan's earlier notes said the ROM reads sectors 2048 and 3072; not verified.
