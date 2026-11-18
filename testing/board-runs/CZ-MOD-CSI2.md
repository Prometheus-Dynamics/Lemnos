# CZ-MOD-CSI2: the rp1-csi2 receiver of lemnos.ko in Rust

Status: **untested on board.** `kernel/rs/rp1_csi2.rs` replaces `kernel/lemnos_rp1_csi2.c` (stage 1m of the
module's port). The register offsets, the order of the writes, the frame and sequence accounting and the stop
are the C's, line for line; the QEMU guest checks (`lemnos_rp1_csi2_check` and its wedge parts,
`lemnos_pisp_fe_check`, the compose check, against the fake receiver) pass on both guest kernels. No real
receiver has run the Rust yet. This file is the board check.

The board needs a kernel with `CONFIG_RUST=y`, built as docs/kernel-config.md says: the module does not build
without one. Run by the RAZE runner on Raze #2 (Compute Module 5), with the module loaded through modprobe with
`options lemnos` (docs/device-tree.md) and the receiver owned, as M34 loads it. The session's rules are
L1-chain.md's (the locks, the pid files, the restore).

## What could differ on the board and not in QEMU
- The lanes and polarities are read from the device tree's endpoint through the firmware-node calls
  (`fwnode_graph_get_endpoint_by_id` on port 0, `fwnode_property_read_u32_array`), where the C used
  `of_graph_get_endpoint_by_regs` and `of_property_read_u32_array`. The guest has no such node, so the guest
  only shows the default (2 lanes, no swap). With one endpoint under port 0 the two give the same node.
- The frame path runs in the block's hard interrupt on a real interrupt line, with the retry work on
  `system_highpri_wq` beside it. Outside the handler the lock is taken with `spin_lock_irq_disable`, where the
  C took `spin_lock_irqsave`.
- The stop's bound is given to `__usecs_to_jiffies` capped at 32 bits; the C let a longer one wrap. No mode
  the block takes reaches it (it needs a frame period over 17 minutes).
- `gone` is set holding `going` and `ctl` both; the C held `going` alone.

## Steps (each prints one line; the runner keeps the console)
1. The module's revision, the kernel's `CONFIG_RUST`, and the bind line in the kernel log:
   `CSI-2 host 0x<version>, <n> lanes, /dev/lemnos-rp1-csi2-<device>`. The version and the lane count must be
   the ones the C module prints on this board (load the last C revision first and keep its line).
2. D5.md's raw run (colour bars, 3600 frames) from the sensor through the receiver alone. Expect what the C
   module gave there: `frames 3600, dropped 0, errors 0` in `LEMNOS_RP1_CSI2_IOC_INFO`, no sequence gap, and
   `LEMNOS_RP1_CSI2_IOC_FRAME_STARTS` valid with the count moving at the sensor's rate (J4b2.md).
3. CZ-MOD-FE-ACK.md, all of it, on this revision: the stop confirmed by the frame end after FORCE, the stop
   with nothing in flight, and the next START's count and sequence.
4. STOP and START fifty times in a row with frames flowing. Expect no `DMA did not stop` line and
   `LEMNOS_RP1_CSI2_INFO_WEDGED` clear after each; each START's first frame has sequence 0.
5. A consumer that keeps every buffer for a second and then gives them back: `valid` reads 0 while nothing is
   armed, the channel's address is zero then (no frame lands in a held buffer: the held buffers' contents do
   not change), and frames come again within two frame periods of the first buffer given back.
6. L1-chain.md step 3 (the front end fed by the receiver's channel), then unbind the receiver while it
   streams, bind it again, and run step 2 again.

## Recording
Put each step's line under "Results" with the kernel and module revision. A wedge in step 4, a changed held
buffer in step 5 or a lane count in step 1 that differs from the C module's is a defect of the port: stop and
report it.

## Results
(none yet)
