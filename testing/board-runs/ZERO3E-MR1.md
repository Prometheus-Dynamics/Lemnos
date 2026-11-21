# ZERO3E-MR1: maskrom entry and return, run sheet (untested on board)

Board: Radxa ZERO 3E (RK3566), serial b3d1ffecafae5cec, ED25519 host key SHA256:FYYZVU…Q3g.
Current image: Buildroot, kernel 7.3.3, extlinux label `gaia`, root on PARTLABEL=rootfs (SD card, mmcblk0).
Record: testing/board-runs/MR1.md. The hands list (HANDS-LIST.md) and the word-clear question stay open: the return is not hands-free without the stub clear.
Author: ZERO3E board worker, 2026-11-21. No board contact for this sheet. Nothing here has run on the board.

Status: untested on board. The watchdog-return line ran on the ZERO 3E (section 2b) and passed. The 471 stub with the
word clear (section 3) is built and rehearsed on the PC only. Steps E, 1a, 1b, the 472 step and U1 have NOT run: no
maskrom host on the PC, and no go to enter maskrom.

## 0. The register: OS_REG0 at 0xfdc20200 (citation)

Use the word `OS_REG0`, address `0xfdc20200` (PMUGRF base 0xfdc20000 + offset 0x200). Its use:

1. U-Boot, upstream master, `arch/arm/mach-rockchip/rk3568/Kconfig`: `config ROCKCHIP_BOOT_MODE_REG`,
   `default 0xfdc20200`. The boot-mode word that U-Boot reads. (Read 2026-11-21 from the raw file.)
2. Linux, upstream master, `arch/arm64/boot/dts/rockchip/rk3566-lckfb-tspi.dts`, `&pmugrf { reboot-mode { compatible
   = "syscon-reboot-mode"; offset = <0x200>; mode-normal = <BOOT_NORMAL>; mode-loader = <BOOT_BL_DOWNLOAD>; ... } }`.
   The kernel reads and writes the word through a regmap as the reboot-mode store. (Read 2026-11-21.)
3. Linux, `include/dt-bindings/soc/rockchip,boot-mode.h`: `REBOOT_FLAG 0x5242C300`, `BOOT_NORMAL = REBOOT_FLAG + 0`,
   `BOOT_BL_DOWNLOAD = REBOOT_FLAG + 1`. The value read on the board, 0x5242C300, is BOOT_NORMAL.
4. The entry value `BOOT_BROM_DOWNLOAD = 0xEF08A53C` is in U-Boot `arch/arm/include/asm/arch-rockchip/boot_mode.h`
   (from the MR1 record; not re-read this session).

What the citation shows: the word is a plain general-purpose store that the kernel and U-Boot read and write through
regmap. Nothing in those uses reads it to clear it. That is the basis for "no read side effect" for a devmem read.

What it does NOT show: the RK3566 TRM's own entry for PMU_GRF OS_REG0 (not on this PC; the TRM section number is
to be added when it is read). And it does NOT show what the boot ROM does with the word (see U1 below). The record's
"TRM not read" caveat stays.

## 1. Facts this sheet depends on

- Watchdog: dw_wdt at 0xfe600000, clock 24 MHz. TOP 15 = 89.48 s (longest), TOP 12 = 11.18 s.
- Reboot-mode word now: 0x5242C300 (BOOT_NORMAL). Entry word: 0xEF08A53C.
- Maskrom USB id: 2207:350a, bcdUSB bit 0 clear.
- Storage: sector 64 boot area, uboot-env p1 at sector 32768, rootfs p2 at 34816.
- U1 (unproven): the boot ROM acts on 0xEF08A53C and then clears the word. Without a clear, the word stays at
  0xEF08A53C across the reset. Then the next boot (SD) would see the download magic, and U-Boot's handling of it is
  not read here. Either a boot-ROM loop back into maskrom or a U-Boot download wait can follow. Neither has a
  hands-free return.

## 2. Steps

Each step: arms, bounded wait, return, what is written, prediction, STOP.

## 2a. Watchdog-return line (approved by Helios; runs before any step E)

Purpose: prove that the watchdog reset returns the board to the same card, with nothing written.

- Read first, nothing opened: /dev/watchdog0 owner (major:minor from stat, then /sys/dev/char/MAJ:MIN -> driver).
  Must be dw_wdt on the rk3566 node fe600000. /sys/class/watchdog is absent on this image (recorded). /tmp must be
  tmpfs. Serial must be b3d1ffecafae5cec. STOP on any mismatch.
- Timeout range (dw_wdt, mainline master, drivers/watchdog/dw_wdt.c, sha256 4d47b1ae1da06532d463f85eb23a994b48ace18f9cd9f5381b03fcd110400136):
  min_timeout = the first TOP with a non-zero whole-second value, which is 1 s at 24 MHz (TOP 9, about 1.40 s).
  max = TOP 15, 89.48 s. The default on open is 30 s. The OPEN ARMS the watchdog at that default, so the
  probe sets 1 s within the same process, before anything else.
- Action: a static probe (wdt-probe-arm64) is copied to /tmp (RAM, the one write, removed by the reset) and run on
  /dev/watchdog0. It reads the identity, sets the timeout to 1 s, and exits WITHOUT the magic close 'V', so the
  watchdog keeps running. It does not feed.
- Predicted: the reset comes about 1.4 s after the probe exits; the bound is 5 s. The board then boots the same
  card (extlinux gaia). boot_id changes inside 180 s (the bound for the whole return).
- Recorded before and after: boot_id; after the return, read-only sha256 of the first 16 MiB of mmcblk0 and of
  mmcblk0p1 (uboot-env), which must equal 2578fdb1…bea3 and de2f2560…cc31.
- Written: /tmp/wdt-probe in RAM. Nothing on the card, no env, no mount, no reboot command.
- STOP (report, no further contact): wrong driver or serial; /tmp not tmpfs; no reset within 5 s (board still up);
  no new boot_id within 180 s (HANDS CASE: one power cycle); any hash differs from its anchor.
- Rehearsal (PC stand-in, fake ssh, no real node): the driver ran in three modes: pass (reset seen, anchors equal,
  PASS), noreset (STOP after 5 s, board still up), wrongdriver (STOP at part 1). Scripts: /run/user/1000/tp/ZERO3E/
  wdt-return-pc.sh and wdt-board-part1.sh. The probe's refusal paths were run under qemu-aarch64-static on fake files: a non-watchdog node
  exits 4 (GETSUPPORT refused), a missing node exits 3, no argument exits 2. The happy ioctl path is not run on the PC
  (no real watchdog, and the PC will not open one). Probe sha256 recorded before the run.

### E. Enter maskrom from the running system
- Written: OS_REG0 (0xfdc20200) = 0xEF08A53C, 32-bit, by `busybox devmem 0xfdc20200 32 0xEF08A53C`. Then `reboot`.
- Persistent: YES, until something clears it. The word lives in PMUGRF, not on the card, so a card-level restore
  does not reset it. Clearing it is NOT proven (U1). Helios expects nothing persistent: this is the persistent write
  and it is not proven clear. Stated plainly, not waived.
- Watchdog armed before it: none by this sheet. Option: open /dev/watchdog and feed it at 5 s until the reboot starts,
  so a hung reboot still resets. The SoC reset also disarms the watchdog, so once the BROM runs nothing is armed.
- Bounded wait: 90 s on the PC for 2207:350a after `reboot`.
- Return with no hands: NOT PROVEN. If U1 is false (BROM ignores the word) the SD boots normally, but the word is still
  0xEF08A53C and U-Boot may act on it. If U1 is true and the BROM does not clear it, the board re-enters maskrom on each
  reset. Only a host or a power cycle leaves maskrom.
- Prediction (written first): maskrom 2207:350a appears within 90 s with bcdUSB bit 0 clear.
- STOP: no 2207:350a in 90 s (then check the board's SD boot: a boot with no host and no maskrom is a STOP, record it);
  any change to 10.55.0.1 other than the reboot; the word readback not 0xEF08A53C; any write other than the one above.
- Verdict: HANDS LIST (the return is not hands-free until the clear is shown).

### 1a. 471 arm-and-spin stub (PC sends it; no 472)
- Written: the 471 stub into the boot ROM's SRAM over USB (not storage). Nothing on the card. Stub arms TOP 15 at its
  first instruction and spins.
- Watchdog armed before it: TOP 15 by the stub itself.
- Bounded wait: reset within 90 s.
- Return: the watchdog resets the SoC. The return then depends on the word: the SD boots only if the word was cleared.
  If the word is not cleared, the reset returns to maskrom, not to the card.
- Prediction: reset within 90 s; the card boots; 10.55.0.1 answers with a new boot_id.
- STOP: no reset in 90 s (re-send once, then stop and report); a reset to maskrom again (STOP: word not cleared);
  the card boots but the boot_id is unchanged.
- Verdict: HANDS LIST (depends on E).

### 1b. 471 arm-and-return, then recovery
- Written: the 471 stub (arm and return to the ROM, x30 untouched). Nothing on storage.
- Watchdog armed before it: TOP 15 at the stub's first instruction.
- Bounded wait: 90 s observation.
- Return: a reset means the watchdog survived the ROM. No reset: re-send the 1a image once; no reset: nothing of ours
  runs and the board is in maskrom with no code of ours. That corner is a hands action (one power cycle).
- Prediction: the watchdog survives the ROM return and resets inside 90 s.
- STOP: any write to storage; a second 471 that the ROM refuses without a reset.
- Verdict: HANDS LIST (depends on E).

### 2. 472 loader hang, before any write
- NOT APPROVED: needs a new word from the reviewer (MR1.md step 2). The DWC3 endpoint and dw_mmc block writes are not
  yet bound, so the session parks at the first chunk by design.
- Verdict: NOT ON THIS SHEET; stays with the reviewer.

## 3. Proposal for the word clear (for Helios to decide; not in this sheet's steps)

Option A: the 471 stub writes BOOT_NORMAL (0x5242C300) to OS_REG0 before its spin or return. This is a code change to
the stub. It is a write that Helios would approve per code change. With it, every case where the stub runs ends with
the word cleared and the reset returns to the card. The case where the stub does not run (BROM ignores E) still leaves
the word set: that one stays a hands action.

Option B: a PC test first: E with the word cleared by an existing OS write on the running system, then reboot. Shows the
BROM does not re-enter maskrom with a cleared word. Does not test U1.

Neither option is approved. Until one is, every MR1 step after E is a hands action.

## 4. PC-side rehearsal (same rev, stand-in)

Crate: testing/crates/maskrom-rehearsal (lemnos-maskrom-rehearsal), the stand-in for the boot ROM's 471 and 472
entries and the watchdog periods. Run through the slot pool at N=4:
- `cargo test -p lemnos-maskrom-rehearsal`: result in the PC log, filled in below.

Result: (filled in after the run, see the rehearsal line at the bottom).

## 5. Rehearsal result (filled in)

UNPOOLED RUN, rc=0: `cargo test -p lemnos-maskrom-rehearsal` on the same tree (pr/ZERO3E-SWEEP, 5fc004d6), run
before the slot-pool rule, not through slot-run. The output file was overwritten by a later run, so only rc=0 is kept
here, not the test names. The rehearsal reruns queued through the pool were stopped; none ran. Helios accepts these
unpooled results as evidence for this tree (WORKER-RULES correction, 2026-11-21).


## 2b. Result of the watchdog-return line (run 2026-11-21, 14:45)

- Read first: serial b3d1ffecafae5cec; /dev/watchdog0 is 244:0, driver dw_wdt (fe600000.watchdog); /tmp is tmpfs.
- Probe (wdt-probe-arm64, sha256 7ddc99d5…0bbcf, freestanding aarch64, no libc): identity "Synopsys DesignWare Watchdog";
  timeout_before=44 (the default armed at open); set to 1; exited without feed or magic close. Written: /tmp/wdt-probe
  (RAM only).
- Return: boot_id 8a105359-e016-4b0d-8a35-9e595f1ce5c7 -> 04a1f829-f599-4232-91fc-b4a4d1bc9444. New boot seen 28 s after
  the probe started. Uptime 34 s at a later read: a single fresh boot.
- Hashes after: 16 MiB of mmcblk0 = 2578fdb1…bea3 (equal); uboot-env = de2f2560…cc31 (equal). Nothing written.
- PREDICTION MISS, stated: predicted reset about 1.4 s after the probe; the reset instant was NOT logged, and the link
  was down at the poll, so the reset time is not measured. The 5 s "no reset" STOP did not fire because the board was
  unreachable, not up. The pass is on the boot_id change and the anchors, not on the 1.4 s figure.
- Helios ruling (2026-11-21): the reset instant was NOT measured. The link was down when the poll began, so the pass
  rests on the boot_id change plus the anchors, not on timing.
- Next run measures it: poll the link (a timestamped ping or ssh probe once a second) from BEFORE the timeout is set,
  so the last answer before the drop and the first answer after it bracket the reset. Each poll line gets a timestamp.
  This is a rule for the next watchdog run on any board: poll from before the arming, not after.
- Result: PASS on the stated criteria. The watchdog-return line is therefore cleared for the next session.

## 3. Option A stub (built and rehearsed on the PC)

- Change: drivers/rockchip-maskrom-sys/src/stage471.S on branch pr/ZERO3E-STUB (local commit, NOT pushed). Each 471 entry
  that arms the watchdog runs clear_boot_mode after arm_watchdog: movz x9 #0xfdc2<<16; movz w10 #0xc300; movk w10 #0x5242<<16;
  str w10, [x9, #0x200]. OS_REG0 at 0xfdc20200. The 472 prologue is unchanged.
- Disassembly (aarch64-linux-gnu-objdump of the new object): in lemnos_arm_and_spin (0x34) the words at 0x50..0x5c are
  mov x9,#0xfdc20000; mov w10,#0xc300; movk w10,#0x5242,lsl #16; str w10,[x9,#512] (512 = 0x200); in lemnos_arm_and_return
  (0x64) the same at 0x80..0x8c before ret (0x90). lemnos_stage471_entry (0x0) has it at 0x1c..0x28 before its tail jump.
- Entry offsets in new471.o: lemnos_stage471_entry 0x0, lemnos_arm_and_spin 0x34, lemnos_arm_and_return 0x64.
  new471.o sha256 9b00ffe46d4ee9556184dbeb1ed1d9ad4fbb7151a9cac4268e0fd437aa5f59ab.
- Rehearsal, PC, qemu-aarch64-static with stand-in registers (two mapped pages at 0xfe600000 and 0xfdc20000; the stub
  runs unchanged; stand-in symbols for the DDR blob and the 472 body): NEW stub: TORR 0xff, CR 1, kick 0x76,
  OS_REG0 0x5242c300, PASS. OLD stub (origin/next): OS_REG0 0xef08a53c, FAIL (the check discriminates).
  Harness: /run/user/1000/tp/ZERO3E/stub/harness.c. Not run through slot-run for the qemu step itself (single small
  process); the cross builds went through slot-run.

## 4. Blocker: no PC-side host, so E, 1a and 1b cannot run

- The sequence needs a host on this PC that talks to maskrom (sends the 471 image over USB). None is in this repository
  (the host lives in another project, per section 2 of this sheet's MR1 record), and none is on PATH (no rkdeveloptool,
  no upgrade_tool). Atlas confirmed no proven loader exists.
- Without a host, entering maskrom (step E) leaves the board waiting for one power cycle with nothing sent. That is the
  accepted worst case, but it buys nothing: 1a and 1b cannot run. E is therefore NOT run.
- Needed to proceed: a host tool for the 471 send, on this PC, and Helios's go for the session.
