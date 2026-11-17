# MR1: maskrom flasher, board facts (ZERO 3E)

## Status (2026-11-17): rehearsal on the PC, board not contacted

Nothing has run on the board in this session. No USB, network or ssh contact with the board was made for this record.
The board steps are not run: step E waits for the PC rehearsal to pass against the stand-in, and steps 2 and 3 wait for a
reviewer's word. The PC rehearsal is `testing/crates/maskrom-rehearsal` (crate `lemnos-maskrom-rehearsal`): a same-rev stand-in
for the boot ROM's 471 and 472 entries, with the stage's own regions and build id, and the script in plan order
(E, 1a, 1b, 2, 3, 4, 5). It is not a product host and is not linked into anything. The stand-in's timing is the plan's
numbers, not a measurement.

### Rehearsal results (PC, same rev as this commit)

| Check | Command | Result |
|---|---|---|
| Faithful script, all steps | `cargo test -p lemnos-maskrom-rehearsal` | `rehearsal_faithful_script_passes_every_step ... ok` |
| Failing ROM: 471 spin never resets | same | `rehearsal_no_reset_after_spin_fails_at_step_1a ... ok` (script reports step 1a) |
| Failing ROM: 472 refused | same | `rehearsal_refused_472_fails_at_step_2 ... ok` (script reports step 2) |
| Refusals outside maskrom, before a returned 471 | same | `rehearsal_471_is_refused_outside_maskrom ... ok`, `rehearsal_472_is_refused_without_a_returned_471 ... ok` |
| Watchdog periods (TOP 15, TOP 12 at 24 MHz) | same | `rehearsal_watchdog_periods_match_the_plan ... ok` |
| 471 return keeps the watchdog and resets inside 90 s | same | `rehearsal_watchdog_survives_a_471_return_and_resets_inside_the_window ... ok` |

Fail-before: with the spin's reset removed from the stand's failing mode (the stand resets anyway), the test
`rehearsal_no_reset_after_spin_fails_at_step_1a` fails with `panicked at tests/rehearsal.rs:17` and the stand is restored
after. The stand must fail where the ROM is said to fail: that case is the one named.

Steps as rehearsed (sim only):
- E: running system to maskrom. 1a: 471 spin resets within 90 s, system boots. 1b: 471 return, the watchdog survives the
  return and resets. 2: 471 return, 472 accepted, no frame, reset with zero feeds. 3: baseline read of 4 sectors past the
  boot area, same bytes written back, read again and compared byte for byte, feeds 12 (4 baseline reads, 4 writes, 4
  verify reads), then END and reset. 4: a cut after the write request, reset, re-entry reads the baseline unchanged.
  5: a wrong-CRC chunk is refused with status 1, does not feed, and does not reach storage.

Deviation from the plan, stated: the same-bytes check compares bytes; it does not compute sha256 (no hash dependency
added). Board runs of the same check use the sha256 of the whole region, as the plan's step 3.3 says.

### Board steps (unrun)

| Step | State |
|---|---|
| E (enter maskrom) | not run. Waits for the PC rehearsal to pass against the sender, as the coordinator ruled |
| 1a, 1b | not run. After E |
| 2 (472 hang, no write) | not run. Waits for a reviewer's word (plan step 2) |
| 3 (same-bytes round trip) | not run. Waits for a reviewer's word (plan step 3) |
| 4, 5 | not run. After 3 |

Status: untested on board. This record holds what was read or attempted on the bench. Nothing was
written to the board, and it was not rebooted.

## Read-only attempts

| When (PC) | What | Result |
|---|---|---|
| 2026-11-14, first session | `ssh -o BatchMode=yes root@10.55.0.1` (uname, cmdline, device tree, reboot-mode and watchdog nodes, mmc) | Connection refused on port 22 (ssh exit 255). The host answered with a reset: the link is alive, sshd is not listening on 22. |
| 2026-11-14, later | same ssh command, once | Connection refused again. |
| 2026-11-14, later | `ip neigh show dev enp12s0f3u4u2u2` | 10.55.0.1 lladdr changed from ba:94:74:1f:ce:b1 to 5a:4d:2b:ce:d8:37 (REACHABLE). About 120 other 10.55.0.x entries INCOMPLETE. Not started by this session; not probed. |
| 2026-11-14 | sysfs of the gadget (3-4.2.2) | RNDIS/Ethernet Gadget, 0525:a4a2, configured. Same device as before. |

Not read, because ssh was refused: the live device tree (watchdog node and clock, the reboot-mode
node and its magic), U-Boot version and environment, the SD partition table, and the sha256 of the
boot areas.

## Hub facts (read-only, for the record)

Hub 3-4.2 is 05e3:0610 (bus 3, device 17). Port 2 carries the ZERO 3E (its `device` link points at
3-4.2.2); port state `configured`, `disable` 0, power control `auto`. Its power-switching mode
(wHubCharacteristics) is not readable without opening the hub, which was not done.

## Entry facts from the sources (to prove on the bench)

- The reboot-mode magic: BOOT_BROM_DOWNLOAD = 0xEF08A53C ("enter bootrom download mode") in U-Boot's
  `arch/arm/include/asm/arch-rockchip/boot_mode.h`. The kernel header
  `include/dt-bindings/soc/rockchip,boot-mode.h` defines BOOT_BL_DOWNLOAD = 0x5242C301 (rockusb loader
  mode), a different value.
- The register: PMUGRF OS_REG0 at PMUGRF base 0xfdc20000 + 0x200 = 0xfdc20200. The mainline
  rk3566-lckfb-tspi.dts uses offset 0x200 under `&pmugrf` for its `syscon-reboot-mode`. The RK3566
  TRM was not read, and this is inferred from the DT and the barebox RK3568 patch: unverified.
- The watchdog: `wdt` at 0xfe600000 (rk356x-base.dtsi:1377), clocks TCLK_WDT_NS (xin24m, 24 MHz) and
  PCLK_WDT_NS, compatible rockchip,rk3568-wdt snps,dw-wdt. Largest period 2^31 / 24e6 = 89.48 s.

## Next, on the bench (in order, each reported before and after)

1. Write BOOT_BROM_DOWNLOAD to 0xfdc20200 from the running OS, reboot, and see 2207:350a in maskrom
   (bcdUSB bit 0 clear). If the magic is ignored, the board boots the SD: record it and stop.
2. Load the 471 stub alone, which arms the watchdog and hangs. Expect the reset and a good SD boot.
3. Load the 472 loader with a deliberate hang before any write. Expect the reset and a good SD boot.
4. Write the current image back (same bytes), read back an equal sha256, boot to 10.55.0.1.
5. Kill mid-write, then the corrupt-chunk run.

Helios reviews the plan before step 4.

## S0 facts (read-only, window 200053)

Source: `/mnt/nvme-wd/lemnos-gates/tmp/MR1/window-200053.txt`, run by the coordinator's watcher on the
bench. Nothing was written to the board. The window was short because an earlier kexec hook of the
coordinator's was still on the board; the coordinator is removing it. Not part of this record.

- Kernel: `Linux zero3e 7.3.3 #1 SMP PREEMPT` (aarch64). Cmdline: `root=PARTLABEL=rootfs rootwait
  console=ttyS2,1500000 earlycon`. Model: Radxa ZERO 3E.
- Device tree: no `reboot-mode` node (the `find` for it returned nothing).
- PMU_GRF OS_REG0 (0xfdc20200) reads `0x5242C300`. That is BOOT_NORMAL in
  `include/dt-bindings/soc/rockchip,boot-mode.h` (REBOOT_FLAG + 0). So this is the boot-mode register;
  it is confirming evidence for the register, not for the BROM's reaction to the BROM magic.
- Watchdog: `/dev/watchdog0` (char 244,0), a dw_wdt; `/dev/watchdog` (10,130) also present.
- USB device controller: `fcc00000.usb`. The kernel log shows `irq/29-dwc3`.
- Storage: `/dev/mmcblk0`, 124735488 sectors (SR64G). GPT partitions: `uboot-env` p1 at sector 32768
  (64 KiB), `rootfs` p2 at 34816 (516 MiB), `data` p3 at 1091584 (227 MiB). The GPT alternate header is
  not at the disk end (kernel warning).
- Root: ext4 on p2; `/` is 99% full (488.8M of 505.9M).
- Boot-area sha256 (read-only): first 16 MiB of mmcblk0 `2578fdb1e5097dd43dd55cc2d1ca4124ad1cf4b006e49a435391c4012e26bea3`;
  `uboot-env` (p1) `de2f256064a0af797747c2b97505dc0b9f3df0de4f489eac731c23ae9ca9cc31`. The p2 hash is
  not in the window file and is not recorded here.
- Boot area at LBA 64: the first bytes read are `RKNS`, so the SD carries a valid boot sector there.
- Userspace: busybox init, dropbear (S50dropbear), no systemctl, no fw_printenv.

Entry method for step 1a, from the coordinator: `busybox devmem 0xfdc20200 32 0xEF08A53C`, then
`reboot`. Whether the boot ROM acts on that value is unproven; step 1a finds out. Nothing on the board
until the coordinator's go.

Corrections to this record, from the sources I checked:
- The `BOOT_BROM_DOWNLOAD` value and the register address are confirmed by the upstream Kconfig:
  `arch/arm/mach-rockchip/rk3568/Kconfig` sets `ROCKCHIP_BOOT_MODE_REG` default `0xfdc20200`. The
  rk3568 IRAM base `CFG_IRAM_BASE` is `0xfdcc0000` (`include/configs/rk3568_common.h:8`).
- The rk3568 TEXT_BASE values (TPL 0xfdcc1000 and SPL 0x0 from the plan) were not found in the upstream
  files read so far. They are not cited here until the source line is read.

## S0 hashes, one session (read-only, after the K2 hook was removed)

Session: one ssh connection, `root@10.55.0.1`, kernel `7.3.3`, boot_id `4244a6d0-8c9c-43db-b4fc-01c2feee2e81`.
Commands only read: `dd if=<dev> ... | sha256sum`. Nothing was written, nothing rebooted.

| Area | sha256 |
|---|---|
| mmcblk0, first 16 MiB | `2578fdb1e5097dd43dd55cc2d1ca4124ad1cf4b006e49a435391c4012e26bea3` (matches window-200053) |
| mmcblk0p1 (uboot-env, 64 KiB) | `de2f256064a0af797747c2b97505dc0b9f3df0de4f489eac731c23ae9ca9cc31` (matches window-200053) |
| mmcblk0p2 (rootfs, full partition) | `c8db5e28cee401849849850eea42f97345883c1ab69205db6291cb0ad90e176a` |

The 16 MiB and p1 hashes are the same as in the earlier window, so the boot areas did not change between
the two sessions.

## Repository split (FL1)

The PC-side host (`testing/crates/rockchip-maskrom`, the host protocol and the flash flow) does NOT land here: it is
another project's, and its handover is kept with the PC side. What lands: the wire (`drivers/flash-wire`), the shared
stage rules (`drivers/flash-stage`), the Rockchip stage and its `-sys` crate under `drivers/`. The paths below are the
ones this record was written against; they are now under `drivers/`.

## Board plan (for review; mirrors /mnt/nvme-wd/lemnos-gates/tmp/MR1/board-plan.md)

# MR1 board plan: ZERO 3E (RK3566) maskrom reflash, for review

Status: DRAFT for review. Nothing here has run on the board. No board or USB contact has been made
for this plan. Code: pr/MR1 (commit: see the PR; this plan describes the code in the same commit).

## 0. Facts

Board (read-only; testing/board-runs/MR1.md):
- The SD card (mmcblk0) is the only storage. GPT: uboot-env p1 at sector 32768 (64 KiB), rootfs p2
  at 34816 (516 MiB, ext4, mounted rw while the system runs), data p3 at 1091584 (227 MiB). The used
  region ends at sector 1556479.
- Boot sector: sector 64 starts `RKNS`. Loader area = sectors 64..32767 (up to the uboot-env partition).
  The split inside it is read from the card before any write (see 3.2).
- Anchors, from one boot (boot_id 4244a6d0-…): first 16 MiB of mmcblk0
  2578fdb1e5097dd43dd55cc2d1ca4124ad1cf4b006e49a435391c4012e26bea3; p1 de2f256064a0af797747c2b97505dc0b9f3df0de4f489eac731c23ae9ca9cc31;
  p2 c8db5e28cee401849849850eea42f97345883c1ab69205db6291cb0ad90e176a (taken once, while the system ran).
- p2 is NOT stable while the system runs: three reads in a later session gave three values
  (the coordinator's read, 2026-11-16). So p2 is compared only in maskrom, against the baseline read there.
- The live copy `/mnt/nvme-wd/lemnos-gates/zero3e-sd/sd-used.live-2026-11-16.img` (sha256 2ef199f8…,
  760 MiB) is a read of the running system. It is NOT byte-equal to the card at maskrom time. Its use:
  a last-resort restore image only, for a person to inspect or restore by hand. The flasher never uses it
  as a baseline.
- Watchdog: dw_wdt at 0xfe600000, 24 MHz (rk356x-base.dtsi:1377; clk-rk3568.c:1208). Longest period
  89.48 s (TOP 15). Reset mode.
- Reboot-mode register: PMU_GRF OS_REG0 at 0xfdc20200 (rk3568 Kconfig ROCKCHIP_BOOT_MODE_REG). On the
  board it reads 0x5242C300 = BOOT_NORMAL (include/dt-bindings/soc/rockchip,boot-mode.h).
- Entry value: BOOT_BROM_DOWNLOAD 0xEF08A53C (U-Boot arch/arm/include/asm/arch-rockchip/boot_mode.h).
- SD controller: sdmmc0 at 0xfe2b0000 (rk356x-base.dtsi:1063). The board's live DT was not read: TO CHECK.
- DWC3 (UDC): 0xfcc00000 (rk356x-base.dtsi:265; the board's UDC is fcc00000.usb).

## 1. What is built (code, host-tested)

- 471 stub, position-independent (movz immediates, adr, no literal pools), in
  drivers/rockchip-maskrom-sys/src/stage471.S. Three entries: arm-and-spin (1a), arm-and-return
  (1b, G3), and the 472 prologue (arm first, then adr to the Rust body). Byte-identical images at two
  link bases were checked with aarch64 binutils for the stub; the 472 prologue is checked the same way
  (see MR1.md, proof output).
- Watchdog bindings (stage crate bind.rs): arm, set session period, feed; the same register writes as the
  stub, in its order, with the addresses above.
- DWC3 device bring-up (core soft reset, device soft reset, RUN_STOP) and dw_mmc reset (controller, FIFO),
  each with a bounded wait; register offsets and bits from drivers/usb/dwc3/core.h and
  drivers/mmc/host/dw_mmc.h of the local kernel tree (linux-rpi-7.2 at 53679a578).
- MMIO: the -sys crate's `Mmio` implements the register trait over volatile reads and writes; one
  operation per unsafe block, each with SAFETY notes.
- The 472 session: arm first, set the period, bring up DWC3 and the SD host, wait for the first chunk
  (bounded), park. Every failure parks without a feed. The first chunk never comes: the endpoint
  transfers are NOT bound (see 6).
- Chunk round (G2): the payload's CRC-16 is checked before the write; a bad chunk never reaches storage.
  Read round (new): a sector read is sent with its CRC; the watchdog is fed after the send. Sim-tested.
- Host flash: the baseline is read twice in maskrom and must agree before any write (BaselineDiffers
  refuses). Compare-first on the loader area. A changed loader area is refused by name unless the plan
  says U3 is proven. A same-bytes plan must equal the baseline. Failures after loader mode reset to
  maskrom. Retries forever with backoff 1 s doubling to 8 s, logged per attempt (limit only for tests).
- Probe 1b (G3 order): arm-and-return 471 first; if no reset in 90 s, re-send the 471 arm-and-spin (1a's
  image); if still none, the 472 arm-and-spin. Sim-tested for each outcome.
- Sim: the boot ROM's watchdog behaviour per stage (brom_stops_wdt, rom_ignores_second_471), kills at
  writes and reads, a flipped read, a boot sector check. 

Host tests and proofs: see MR1.md (counts and rc of the proof chain).

## 2. Unproven (U1–U4), with the evidence read

U1 (the boot ROM acts on 0xEF08A53C in OS_REG0): unproven. Step E tests it. No source read shows it.

U2 (the boot ROM keeps the watchdog running after the 471 returns): unproven. Step 1b tests it.

U3 (a blank or missing boot sector at 64 makes the boot ROM enter maskrom instead of hanging): UNPROVEN.
What was read, and what each citation covers:
- U-Boot doc/board/rockchip/rockchip.rst (master): writes the idbloader at `seek=64`. It states no
  backup copy and no SD fallback. Its only "enters BootROM mode" statement is for RK3066 NAND ("If all
  other boot options fail then it enters into a BootROM mode on the USB OTG port", section 3, RK3066).
  It does not cover RK3566, nor a blank SD card. It does not say a present card with a blank sector 64
  is the same case to the ROM.
- rkbin RKBOOT/RK3566MINIALL.ini (master): `NEWIDB=true` only. Nothing about a backup IDB, nor about
  where the ROM reads on SD.
- Radxa ZERO 3 eMMC guide (docs.radxa.com/en/zero/zero3/low-level-dev/install-os-on-emmc): enters maskrom
  with the microSD card REMOVED and the maskrom button held (ZERO 3W). It covers a card-removed boot on
  the W variant, not a blank card on the 3E. Not a proof for either case.
- Backup IDB copies on SD at fixed offsets after sector 64: NO SOURCE READ SHOWS THEM. So the design does
  not rely on backup copies.
Consequence (by design): the loader area is never changed without the flag (see 3). Only a same-bytes
flash runs on the card, and it never touches the loader area, so U3 is not relied on there.

U4 (the 472 loader's load address): the design does not depend on it. The 472 prologue branches
PC-relatively to the Rust body in the same image; there is no absolute address in the prologue, and the
placeholder origin is gone (loader472.ld). What is proven for the image: the Rust body has no absolute
data. That is shown for the whole 472 image by the two-base check (section 7). A wrong load address then
falls back to the same watchdog reset as before.

## 3. The write path (step 3 and any later image)

3.1 Watchdog and feeds: 471 arms TOP 15 (89.48 s). 472 re-arms first, lowers to TOP 12 (11.18 s) for
the session. A chunk round feeds once, after its read-back equals the payload (write) or after the
send (read). Each transfer is bounded to a third of 11 s. Any fault does not feed; the reset comes
within 11.2 s of the last feed.

3.2 Compare first (the rule the flasher follows):
  a. In maskrom, the loader reads the whole used region to the PC twice, each with a CRC per chunk.
     The two sha256 values must be equal, else refused (BaselineDiffers), nothing written.
  b. The loader area (sectors 64..32767 as read, not as assumed) is hashed on the card and compared
     with the same bytes of the image.
  c. EQUAL: the loader area is not written at all. Sector 64 is never touched. Only the rest of the
     image is written, each chunk read back. The card boots at every moment: a cut at any write leaves
     the boot sector as it was (sim-tested at every write).
  d. DIFFERENT: refused by name (LoaderAreaDiffers, nothing written) unless `loader_change_proven` is set,
     which needs U3 proven for this board. Then, and only then: zero sector 64 and read it back; write
     the rest; write the loader area without sector 64; write sector 64 last, each chunk read back. The
     card is not bootable between the zeroing and the last write. This path is NOT reachable without the
     flag.

3.3 Step 3, the same-bytes round trip, as the coordinator set it:
  (1) read the whole used region to the PC twice in maskrom; equal sha256 required; save it to the
      NVMe with its sha256. Path: /mnt/nvme-wd/lemnos-gates/zero3e-sd/ (not the repo).
  (2) the compare-first rule on the loader area (3.2 c: equal, so not written).
  (3) write the same bytes back to everything outside the loader area, each chunk read back.
  (4) read the whole region again; its sha256 must equal the baseline's.
  (5) normal reset. The board boots. Boot area (first 16 MiB) and p1 must equal the anchors; p2 is
      expected to differ once the system runs (it is written by the running system), so p2 is compared
      only in maskrom, against the baseline.

3.4 What a cut or a failed write leaves on the card, and the unattended return:
- Cut during the baseline reads: nothing written. The card is the card. The board boots (sim-tested).
- Cut during step (3): the loader area was never written, so sector 64 is always the original. The
  card boots. The PC re-enters (E from the running system, or the watchdog's reset if the card boots
  into the OS): the host re-enters with backoff and re-reads the baseline. Unattended.
- A chunk whose read-back differs: the round does not feed; the watchdog resets within 11.2 s; the
  card holds a wrong chunk in the region outside the loader area (the bytes already written). The PC
  re-enters and rewrites from the saved baseline, the same bytes. The boot sector is untouched, so the
  card still boots. Unattended.
- Step (4) differs from the baseline: refused by the host; the card holds the written bytes. Same
  return as above. Unattended, and the board still boots (the boot sector is untouched).
- Only the flagged, changed-loader path can leave sector 64 missing. That path is refused by default;
  if it is ever enabled, the U3 caveat applies to every cut in it (see 6).

## 4. Steps, in order

Each step: who arms the watchdog, the feed rule, the proof, the unattended return.

E. Enter maskrom from the running system: `busybox devmem 0xfdc20200 32 0xEF08A53C`, then `reboot`.
- Arms: nothing (the OS runs).
- Proof: 2207:350a appears with bcdUSB bit 0 clear (maskrom).
- Unattended return if U1 fails: the board boots the card normally. The card is unchanged.
- Unattended return if maskrom is entered and the PC does not send a 471: the host sends one on its
  next attempt (the host never gives up). A host outage with no host available is a power cut (hands).
- ACCEPTED HANDS-ONLY CORNER (step E only, per the reviewer): a maskrom left with no host running and no
  power cycle. It costs one power cycle and changes nothing on the card.

1a. The 471 arm-and-spin stub: PC sends it, no 472.
- Arms: the stub's first instruction, TOP 15. Feed: none (the image spins).
- Proof: a reset within 90 s; the card boots; 10.55.0.1 answers with a new boot_id.
- Unattended return if no reset: the PC re-sends it once; then it stops and reports. A stopped host is a
  host outage (above). ACCEPTED HANDS-ONLY CORNER for 1a only: one power cycle, nothing changes on the card.

1b. The 471 arm-and-return stub, then recovery (G3 order, probe-wdt):
- Arms: TOP 15 at the stub's first instruction; the stub returns to the boot ROM (x30 untouched).
- No 472 in the first send. Observe 90 s.
- Reset: the watchdog survived the ROM (WdtSurvived). Done.
- No reset: re-send the 471 arm-and-spin (1a's image). The boot ROM accepted a 471 in 1a's fresh maskrom
  session; whether it accepts a second 471 in the same session is UNPROVEN (the ROM may not run it:
  the probe tests that case in the sim, and the board result decides it). Reset: SpinStubReset.
- No reset: the 472 arm-and-spin. The ROM's acceptance of a 472 image of ours is UNPROVEN (rkdeveloptool's
  DownloadBoot sends 471 then 472 from a real loader; the BROM's check of our image is not read).
  Reset: FallbackReset.
- No reset at all: the board is in maskrom with no code of ours running. ACCEPTED HANDS-ONLY CORNER for
  1b only (one power cycle, nothing changes on the card). Step 2 does not follow unless the reviewer
  gives a new word.

2. The 472 loader hang, before any write. NEEDS A NEW WORD FROM THE REVIEWER.
- Blocked on code: the DWC3 endpoint transfers and the dw_mmc block writes are not bound (only the
  controller bring-up). So the session hangs at the first chunk by design: it arms, brings the device
  up, waits, and parks without feeding. The reset is the exit.
- Arms: the prologue (TOP 15), then the session (TOP 12). Feed: none before the first verified chunk.
- Proof: a reset within 11.2 s after the park; the card boots; nothing was written (the storage is
  untouched).
- Unattended return if the 472 does not run: the 1b fallback's case: a maskrom with nothing running. It is
  stated here as a risk for this step, not accepted.

3. The first SD write (same-bytes round trip). NEEDS A NEW WORD FROM THE REVIEWER.
- The saved baseline (3.3, step 1) is read by the loader in maskrom, not from the running system. The GO
  for the running-system copy is recorded; it is a last-resort restore image only.
- Unattended return: as 3.4. The card's boot sector is never written on this path. The U3 corner does not
  apply to it (no blank boot sector occurs).
- Proof: the whole region's sha256 equals the baseline after the write; the boot board answers 10.55.0.1
  with a new boot_id; the boot area and p1 equal the anchors.

3.0 (the read to the NVMe): GO given; the coordinator runs it. Not run by this plan.

4. The cut mid-write run. After step 3 passes.
- The PC stops sending chunks mid-write. The watchdog resets within 11.2 s. The card still boots (3.4).
  The PC re-enters, re-reads the baseline, and completes. Final sha256 = the baseline.

5. The corrupt-chunk run. After step 4 passes.
- Now defined: G2 is built. The PC sends one chunk with a wrong CRC. The round refuses it before any write
  (Corrupt), does not feed, and the watchdog resets. The PC re-enters and resends. The bad chunk never
  reaches storage: the sim tests this.
- The board run itself needs the link protocol's host side (see 6).

## 5. Status of the host side (stated, not hidden)

The host flash in this code still uses the rockusb command set (READ_LBA/WRITE_LBA over the usbplug
protocol of the earlier design). The 472 loader speaks its own protocol (ruled). The host side of that
protocol is NOT written. So the board run of steps 3–5 cannot start until the host speaks the 472
protocol. This is a gap, not a choice.

## 6. Open items, and what would close them

- U3: the design refuses the changed-loader path without the flag. To close U3 on the board: a read-only
  check first (the boot ROM's own behaviour cannot be read read-only). Closing it needs a board test with
  a blank sector 64 that has a recovery path the PC controls. None is available now. So U3 stays a
  refusal.
- U2 and U1: step 1b and step E.
- U4: the two-base check passes for the image (section 7); the board's load address stays unproven.
- G1 completion: the DWC3 endpoint transfers and the dw_mmc block writes.
- The host side of the 472 protocol (section 5).
- The 472 session serves frames over the DWC3 link in the model (section 7). The first board frame is untested,
  so step 2 is the first step a board hang would show.


## 7. This round: what changed, what is proven, what is not

Direction (decided by the owner): the PC-side flasher moves to another project. This repository keeps the
on-board side (the 471 stub and the -sys glue, the 472 loader, the stage crate) and the frame wire
definitions (`frame.rs`, no dependency on the host crate, for a later shared wire crate). Nothing was
restructured in this round.

Built and host-tested (no board):
- The frame and CRC (`drivers/flash-wire/src/frame.rs`, moved from the stage in FL1): request, reply and CRC-16/CCITT
  in one module, with no dependency on the loader or the host crate.
- The position-independence proof (`drivers/rockchip-maskrom-stage/scripts/check-pic.sh`): the 472 image
  is linked as a PIE (`--no-dynamic-linker`, script `loader472.ld`, image base the symbol `BASE`) at BASE=0,
  0x00a00000 and 0x60010000. It passes: (a) every byte before the relocation table is identical across bases
  except the 166 relocated 8-byte slots; (b) the tables have the same image offsets and each addend differs by
  exactly the base; (c) the BASE=0 image relocated to X equals the BASE=X image relocated to X. Only
  R_AARCH64_RELATIVE is present; the entry is at offset 0; `.got` (8 bytes) and `.got.plt` (24 bytes) are
  zero-filled, unreferenced, and no object emits a GOT relocation.
- The DWC3 bulk link (`dwc3link.rs`): OUT carries the host's frames, IN carries the replies. A receive posts
  one TRB for the length it expects; a short count is a cut frame. A reply posts one TRB and must be taken in
  full. `DmaMem` is the DMA memory (TRBs, receive and transmit buffers, cache maintenance). The -sys crate
  implements it over static cache-line-aligned buffers, one cache operation or barrier per unsafe block, each
  with a SAFETY comment. `Bus` shares the registers and the clock between the link, the SD card and the
  watchdog feed; calls never nest.
- The session (`entry472.rs`): arm the watchdog; set the period; bring up DWC3; configure the bulk OUT and IN
  endpoints; identify and bring up the SD card; serve requests over the link until an END. END returns the
  reset kind it asked for. The reset itself is the watchdog's.
- The model endpoint controller (`models.rs`, `SimDma`) drives the same link code in the tests. Covered: a read
  round (sector with CRC, then END status); a write round (stored, verified, fed once, acknowledged); a bad CRC
  (refused with status 1, never stored, never fed); a cut payload (link failure, nothing stored, no feed); the
  END with the maskrom kind; a bus reset mid-transfer (the payload never completes: timeout, no feed); a reply
  the host never takes (timeout); a reply longer than the transmit buffer (refused before any transfer). Session
  tests: arming order; a write and END run to completion with one feed; a bus reset mid-transfer stops the
  session with no feed after it; a stuck DWC3 reset, a stuck endpoint, an absent card and no host each stop
  the session at their own step, with no feed after the stop.
- SD identification and block I/O (`sd.rs`, `io.rs`): as before, against the card model.

Not done, and not proven:
- Nothing has run on a board. The endpoint sequences, the TRB lifecycle, the cache maintenance (which assumes a
  64-byte line and a coherent view of the buffers) and the SD sequence are checked against kernel code and the
  models only.
- A bus reset is seen only as a TRB that is never handed back. The DWC3 event that reports the reset is not read.
- END with the maskrom kind returns the kind; the boot-mode handoff that makes the boot ROM take the maskrom path
  is not written.
- A frame longer than its receive TRB is not split. The protocol makes frames exact, so a longer one would
  desynchronise the link; it is not detected.
- U1 (the boot ROM acts on the reboot value), U2 (the watchdog survives the boot ROM), U3 (refused by default:
  no source shows a blank boot sector makes the ROM enter maskrom).

Proof of this round (host only; nothing on the board): see "Proof of the code in this record".

## Proof of the code in this record (host only; nothing on the board)

Run on the tree of this round before the squash (`proof22`, gate wrapper, NVMe, `taskset -c 6-31`):
`cargo fmt --all -- --check` passes; `cargo test` passes for `lemnos-rockchip-maskrom-stage` (79 tests),
`lemnos-rockchip-maskrom` (43 unit and 20 integration tests), and `lemnos-driver-rockchip-maskrom-sys` (no tests);
`cargo clippy -p ... --all-targets -- -D warnings` passes for the three; `cargo build --target aarch64-unknown-none`
passes for the -sys crate; `drivers/rockchip-maskrom-stage/scripts/check-pic.sh` prints `check-pic: ok`
(166 relocation slots, the two-base checks (a), (b), (c) pass); `scripts/check-docs.py` holds;
`scripts/check-boundaries.sh` holds. Untested on board.

## Live SD mapping (verified, read-only)

Run by the coordinator on the board (boot id 033d48d0-cced-4528-88f6-ea2fe8bd695e). Commands:
`readlink -f /sys/block/mmcblk0/device` returned `/sys/devices/platform/fe2b0000.mmc/mmc_host/mmc0/mmc0:aaaa`;
`/sys/class/mmc_host` lists only `mmc0`, which is `fe2b0000.mmc`. So the SD card is on sdmmc0 at
0xfe2b0000, as `bind.rs` assumes. This item is verified; nothing else on the board changed.
