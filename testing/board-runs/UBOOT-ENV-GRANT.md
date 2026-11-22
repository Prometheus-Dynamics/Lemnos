# UBOOT-ENV-GRANT: the daemon's U-Boot environment writes through the root phase's hand-over

Status: **untested on board.** The guest proof below ran on the PC against a file-backed disk. No board has run the daemon's
`uboot` record, and the board's own partition naming and mode are not checked here.

## What is built

- `drivers/system-slots/src/uboot.rs` `root_phase`: the root phase of the `uboot` record, run as root before the drop. The boot
  partition is handed over read only (`boot_node::hand_over`). The environment's partition is found on the disk by its type GUID
  (`gpt::find_env_partition_numbered`), numbered from the GPT, and its node is `boot_node::partition_node` (the kernel's partition
  entry under the disk in sysfs, `<disk>p<N>` or `<disk><N>`). That node is handed over read-write, or read only when
  `uboot_env::refuse_if_fat_env` is not clean on the boot partition. The device form's named device is handed over read-write.
- `drivers/slot-store/src/uboot_env.rs`: the hand-over registry (`hand_over`, `handover_of`), `FileEnv::open` taking the handed
  file, a write on a read-only hand-over refused by name (`READ_ONLY`, with the root phase's reason), the partition-relative
  geometry (`Place::geometry_in_partition`), and `fat_env` reading the boot partition through `boot_node::open`.
- `drivers/slot-store/src/boot_raw.rs` `BayVolume::open_read`: the bay reads of `set_default` go through the read-only boot hand-over.
- `packaging/systemd/grants.toml`: the `config:uboot_env_device` row is `how = "root-phase"`, `wired` in
  `service/lemnosd/src/privilege.rs` (the system-slots root phase). Its `open` tag is cleared: the path is granted by the root phase.

## Proof

**Guest (QEMU, x86_64, `testing/uboot-grant/run.sh`).** The guest is the Fedora kernel `/boot/vmlinuz-7.3.4-202.nobara.fc44.x86_64`
(virtio-blk and devtmpfs built in), a disk image file with a complete GPT (protective MBR, backup header), the boot partition
(partition 1, FAT with slot `os-1` in bay 0) and the environment (partition 2, 64 KiB, one valid copy). The guest's disk nodes are
made root:disk 0660, as the packaged rule makes them. The checks, in order:

1. Probes as uid 1000 with no supplementary group: `/dev/vda`, `/dev/vda2` (environment) and `/dev/vda1` (boot) each refuse
   `open(O_RDWR)` with EACCES (errno 13). The real nodes, not a stand-in.
2. A root child runs the root phase (`uboot::root_phase`), drops to uid 1000 (`drop_to`), runs `set_default("os-1")` through the
   handed partition, and refuses an open of `/dev/vda` by name after the drop (EACCES).
3. A root child reads the environment back: `default=os-1`, `default_bay=bay0`.

The harness then checks the disk: every byte the run changed lies in partition 2's blocks.

Run: `lt/slot-run.sh 1 env CARGO=<cargo wrapper> bash testing/uboot-grant/run.sh` (the guest takes one slot; the builds take four).
Result on the new tree: `uboot-grant: passed (5 checks)`; `disk: 8192 byte(s) changed, all in the environment partition`.

Fail-before, the same guest on origin/next with only a stub `uboot::root_phase` (`not handed over`) planted: the three probes are
refused as above, and the daemon's `set_default` fails `set_default os-1: /dev/vda: Permission denied (os error 13)`; the guest
prints `uboot-grant: failed (2 of 5 checks)`.

**Unit tests** (`drivers/system-slots/src/uboot/grant_tests.rs`): the root phase on a fake sysfs with a boot partition and an
environment partition (partition 2) hands over `…/mmcblk0p2` read-write, and the daemon's write goes through it; the disk image is
unchanged. With a FAT environment on the boot partition the hand-over is read only, and the write is refused by name with the
reason, and nothing is written. Fail-before: both fail on origin/next at the hand-over line (`the uboot record's node is not
handed over`). `drivers/slot-store/src/uboot_env_tests.rs` (`a_node_handed_over_read_only_refuses_the_write_by_name_with_the_reason`)
and `drivers/slot-store/src/gpt.rs` (`the_environment_partition_is_numbered_by_its_entry_position`) test the registry's read-only
refusal and the partition number.

## Not tested, and why

- **On board.** The partition's node name on the board (`mmcblk0p2` on the CM5's SD or eMMC; the kernel's name, read from sysfs
  here, not udev's), the partition's mode and group on that kernel, the daemon under the unit's real drop, and the board's
  `set_default` read back by slot-init's `lemnos-ctl` view. None of these ran on a board. Board-side check: one `set_default` by the
  daemon, then read the environment with `lemnos-ctl` and as root; the partition's owner and mode before the start
  (`stat -c '%U:%G %a' /dev/mmcblk0p2`); the root phase's log line (`lemnosd: root phase: <device>: /dev/mmcblk0p1: handed over read only;
  /dev/mmcblk0p2: handed over read-write`).
- **The device form** (`uboot_env_device`): built (the named device is handed over read-write), not in the guest and not in a unit test.
- **The boot disk's resolution from the device tree** (`boot_sources`, the controller's `of_node`): covered by the existing root-phase
  tests, not by the guest, which names the disk directly (x86 has no device tree).
- **Install and free on the `uboot` record** in the daemon: the boot partition is read only there, so they are refused by name
  (`READ_ONLY`). They were refused by name before this change too (root:disk).
