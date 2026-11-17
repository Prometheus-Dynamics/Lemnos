# K230-USB board run: the K230 stage on the FL1 flash wire

Status: **untested on board.** This is the plan for review. Nothing in this change has opened, written,
or reset any board. The stage is proven only in simulation (`testing/crates/k230-usbboot-stage/tests/fl1.rs`)
and against a same-rev stand-in rehearsal (section "PC rehearsal").

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
