# BOARD-USB0: the bench board behind the PC's gadget link (ZERO 3E), identity and what can run on it

Status: untested on board. Nothing was run on a block. This session only read the board. It
wrote no file on it, copied nothing to it, loaded or unbound no module, and did not touch the
watchdog, the boot settings or storage. Rev under test: `next` at `abd1d51b` (2026-11-18).

## Identity (asked, not assumed)

Read over the gadget network (`root@10.55.0.1`, dropbear, the PC's existing `usb0` address), on
2026-11-18:

| Fact | Value |
|---|---|
| Model, compatible | `Radxa ZERO 3E`; `radxa,zero-3e`, `rockchip,rk3566` |
| Serial (`/proc/device-tree/serial-number`) | `b3d1ffecafae5cec` |
| Kernel | mainline `7.3.3 #1 SMP PREEMPT Fri Nov 13 08:13:06 MST 2026`, aarch64, modules on |
| Image | Buildroot 2026.02.3 on the SD card, busybox init, dropbear; `extlinux.conf` label `gaia`, tree `rk3566-radxa-zero-3e-gadget.dtb` |
| U-Boot | `2026.10` (`/chosen/u-boot,version`) |
| Boot | boot_id `033d48d0-cced-4528-88f6-ea2fe8bd695e`, up 2 d 21 h at the read |
| lemnosd | not on the board (no binary, no process, no `/run/lemnos`) |
| Reboot-mode register | `0xfdc20200` reads `0x5242C300` (BOOT_NORMAL) |

The board is the one the earlier records mean. Its serial is in the live tree dumped on 2026-11-13
(the K2 bench files), the boot_id is the one in the SD read log of 2026-11-16, the root file system's
UUID (`d2f491f4-…`) is the one in the MR1 window, and the two MR1 anchors were read again and are
equal: first 16 MiB of `mmcblk0` `2578fdb1…bea3`, `uboot-env` `de2f2560…cc31`.

### The host key changed, and why

The ED25519 host key the board presents (`SHA256:FYYZVU…Q3g`) is not the one the PC had stored for
`10.55.0.1` (`SHA256:k3pByS…/Xk`). Facts read on the board:

- `/etc` is ext4 on the SD card, not a tmpfs. The image's files are dated 1970-01-01.
- The clock is the PMIC's RTC with no backup: every cold boot starts at 2017-08-05 09:00. At the
  read the board's clock said 2017-08-08 06:4x.
- `/etc/dropbear` is a real directory changed at 08-05 10:18, holding one key file of the same
  minute. `/etc`, `/` and `/crond.reboot` (made by crond when it starts) are dated 10:17. This
  boot's syslog has no start and no login between 09:00:11 and 17:12, so those times belong to an
  earlier boot, one that started at board clock 10:17 (a warm restart after a cold power-up).
- The image's `S50dropbear` ships `/etc/dropbear` as a link into `/var/run` (a new key every boot)
  and, on a writable root, replaces it with a real directory; `dropbear -R` then writes the key at
  the next login.

So the stored key was a per-boot key and the present one is the first persistent key, made on the
board. Why the directory was not made on earlier boots is not on the board (inference: the root
file system was 99% full then, MR1.md; it is 87% now). The earlier bench logins ran with host-key
checking off, so nobody saw the change. This session pinned the new key in a private file and left
`~/.ssh/known_hosts` as it was.

## Blocks the running kernel holds

| Block | Node and driver | Device node |
|---|---|---|
| JPEG / video encoder | `video-codec@fdee0000`, `rockchip,rk3568-vepu`, `hantro-vpu` (module) | `/dev/video2` |
| Its IOMMU | `iommu@fdee0800`, `rockchip,rk3568-iommu`, `rk_iommu` | |
| Video decoder | `video-codec@fdea0400` (reg `0xfdea0000`), `rockchip,rk3568-vpu`, `hantro-vpu` | `/dev/video1` |
| Its IOMMU | `iommu@fdea0800`, `rk_iommu` | |
| RGA | `rga@fdeb0000`, `rockchip,rk3568-rga`, `rockchip-rga` | `/dev/video0` |
| Watchdog | `watchdog@fe600000`, `rockchip,rk3568-wdt`, `dw_wdt` (built in) | `/dev/watchdog0`, `/dev/watchdog` |

Not present at all: an NPU node or driver (`rknpu`, `rocket`), a node at `0xfded0000` (the JPEG
decoder), `rkvdec`, `rkvenc`, `/dev/mpp_service`, `/dev/dma_heap`. `CONFIG_VIDEO_ROCKCHIP_VDEC` is
off. No version word of any block was read: nothing here reads one without either a driver of
ours or a blind `/dev/mem` read of a block whose power and clock state is unknown, and that would
be a change on a guess.

## Plan: what each open record can do on this board as it is

| Record, check | Verdict | Why |
|---|---|---|
| `K1b.md` 1 to 3 | WRONG BOARD | K1b means the VPU720-class encoder (`rockchip,rkv-jpeg-encoder-v1`, through `/dev/mpp_service`). The RK356x has no such block. The vendor `rk356x.dtsi` (the board vendor's 6.1 kernel at `0eb18670e`) has only `vepu@fdee0000`, `rockchip,vpu-encoder-v2`, a different block with a different register file |
| `RKJPEGE.md` | WRONG BOARD | The read bound is decided by the K1b probe on the VPU720 block |
| `RKJPEG-DEC.md` | NEEDS IMAGE | The chip has the block by its compatible: vendor `rk356x.dtsi` has `jpegd@fded0000`, `rockchip,rkv-jpeg-decoder-v1`, with `iommu@fded0480`. The running image has no node, driver or service for it. "Same block" rests on the compatible only; the version word is unread |
| `E32.md` 1 to 6 | NEEDS IMAGE | The chip has the one-core block the record lists second: vendor `npu@fde40000`, `rockchip,rk3568-rknpu`, window `0x10000`, and `iommu@fde4b000`. Source fact for check 5, not a board result: the IOMMU's registers lie inside the NPU's own window on this chip too |
| `I5i5.md`, `I5i7.md` | NEEDS IMAGE and HANDS | The board boots a plain `extlinux.conf`, not the slot layout or `boot.scr`; no daemon is on it; the console points need the serial port. Points that write the environment or storage do not run without a yes. `I5i5.md` point 4 is another board family |
| `MR1.md` | listed, not run | Next board step is E (the reboot-mode write, then a restart into maskrom). It waits for the coordinator and for the PC-side host |

No row is RUN. No probe was bent to fit: the four probe records all go through the vendor
kernel's service node or DRM node, and this image has neither.

## Read-only facts for the U-Boot records

- GPT (primary, read with `dd`): three partitions. `uboot-env`, type
  `3de21764-95bd-54bd-a5c3-4abe786f38a8`, sectors 32768 to 32895 (64 KiB); `rootfs`, sectors 34816
  to 1091583; `data`, sectors 1091584 to 1556479. Exactly one partition has the environment's type
  GUID. The header's alternate LBA is 1558527, not the disk's end (124735488 sectors).
- The `uboot-env` partition is all zero: U-Boot has never saved an environment there. That is the
  starting state of `I5i7.md` point 4.
- `/chosen` has `bootargs`, `kaslr-seed`, `smbios3-entrypoint`, `stdout-path`, `u-boot,version`.
  No `lemnos,boot-source`.

## What would unlock runs

- Hands: a serial adapter on `ttyS2` (the U-Boot prompt for the console points; the console during
  MR1 step E), and a power cycle for MR1's accepted corner.
- A vendor-kernel image (the 6.1 tree has `rk3566-radxa-zero-3e.dtb`, the service node and the NPU
  driver): `RKJPEG-DEC.md` on this chip's decoder and `E32.md` as the one-core case.
- A slot image (the boot script, the daemon, U-Boot built with the fragment): the `I5i5.md` and
  `I5i7.md` points.

## Baseline and restore

Baseline at the start and at the end (loaded modules, bound drivers, processes, mounts, device
nodes, kernel log tail): equal, apart from this session's own login processes and the kernel's
worker threads. 55 modules both times; no module loaded or removed. boot_id unchanged; the
reboot-mode register unchanged; the first 16 MiB hash unchanged. The only trace is dropbear's
login lines in `/tmp/messages` (tmpfs). The board's `eth0` and its DHCP client were not touched.
