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
  adapter's bench checks are listed in `testing/crates/k230-usbboot-stage/src/fl1/usb.rs`.
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

**R2 is dropped** (no image exists for report-then-SoC-reset; the FL1 stage image gets its own review first).

**The capture (read only).** A separate unit reads both CH342 ports from before R0 to the end and writes no byte to
either. The console is never written in this run, so no step writes to the console.

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
