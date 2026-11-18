# CZ-MOD-PISP: the PiSP front end and back end of lemnos.ko in Rust

Status: **untested on board.** `kernel/rs/pisp_fe.rs` replaces `kernel/lemnos_pisp_fe.c` (stage 1l of the
module's port) and `kernel/rs/pisp_be.rs` replaces `kernel/lemnos_pisp_be.c` (stage 1m). The register offsets,
the order of the writes, the checks, the job accounting and the stops are the C's, line for line; the QEMU
guest checks (`lemnos_pisp_fe_check` against the fake receiver, `lemnos_pisp_be_check` and
`lemnos_pisp_be_version_check` against the fake back end) pass on both guest kernels. No real front end or
back end has run the Rust yet. This file is the board check.

The board needs a kernel with `CONFIG_RUST=y`, built as docs/kernel-config.md says: the module does not build
without one. Run by the RAZE runner on Raze #2 (Compute Module 5), with the module loaded through modprobe with
`options lemnos` (docs/device-tree.md) and the receiver owned, as M34 loads it. The session's rules are
L1-chain.md's (the locks, the pid files, the restore).

## What could differ on the board and not in QEMU
- The frame path runs in the receiver's hard interrupt on a real interrupt line, with the retry work on
  `system_highpri_wq` beside it: the lock is the kernel crate's `SpinLockIrq`, where the C took `spin_lock` in
  the handler and `spin_lock_irqsave` elsewhere.
- The stop on a block that is really busy: ABORT, the channel's stop, the idle reads (8 in a row, 250 at
  most), and the reset of a block still busy after the channel stopped (the C saw STATUS 0x5 there).
- The configuration is written from a packed struct with unaligned word reads; the fake's write log shows the
  same words as the C's, a real block has not taken them.

- The back end's interrupt and its job accounting against the block's own batch counters (the fake's counters
  are the fake's), the watchdog on the block's ordered queue, and the tiles the block reads from coherent
  memory the job slot owns.
- The back end's config slots: `vmalloc_user` memory mapped by the consumer through the stream's file, with
  the area's may-execute cleared through the kernel crate's `VmaNew`.

## Steps (each prints one line; the runner keeps the console)
1. The module's revision, the kernel's `CONFIG_RUST`, and `PiSP FE HW v<major>.<minor>` in the kernel log at
   bind: the version the C module printed on this board.
2. L1-chain.md step 3: 600 frames through sensor, receiver and front end (`lemnos-chain-proof`). Expect what
   the C module gave: every frame written, `dropped` 0, `errors` 0, `stalled` 0 in
   `LEMNOS_PISP_FE_IOC_INFO`, and the receiver's counters readable while the front end streams.
3. With `test_pisp_fe=1`: the write log (`LEMNOS_PISP_FE_IOC_TEST_TRACE`) of one START, three jobs and one
   STOP, taken on the last C revision (next e9483d52) and on this one. The two lists must be the same offsets
   in the same order, with the same values apart from the device addresses.
4. STOP and START fifty times in a row with frames flowing. Expect no `PiSP front end busy after abort` line
   that ends in `still busy`, no `did not stop` line, and `LEMNOS_PISP_FE_INFO_WEDGED` clear after each.
5. A consumer of the front end's `output0` stream that keeps two of four frames for ten seconds (a client
   written for the run: no proof program holds frames today): frames go on in the buffers left, and the front
   end is not restarted (the driver's fix beside this port: the frames handed on no longer count against its
   hold limit).
6. Unbind the receiver while the front end streams, bind it again, and run step 2 again.
7. The back end's version: `LEMNOS_IOC_PISP_BE_VERSION` and the stream's `hw_version` give the version the C
   module gave on this board (0x02252700 with the board's minor revision).
8. L1-chain.md step 4: 1000 queued jobs with none dropped, then 500 paced at 100 jobs a second. Expect
   `finished` 1500 and `refused` 0 in `LEMNOS_PISP_BE_IOC_STATUS`, `wedged` 0, and every output read back.
9. L1-chain.md step 5, the full chain: 600 frames or more through sensor, receiver, front end and back end,
   with the rate, the timestamps and the dropped counts the C module gave.
10. Close the back end's stream with a job on the block, fifty times: each close returns within the stop's
    bound (2 s), the next stream is made (no `EBUSY`), and the log has no `block stopped` line.
11. Unbind the back end with a stream open and jobs posted, bind it again, and run step 8 again.

## Recording
Put each step's line under "Results" with the kernel and module revision. A difference in step 3, a wedge in
step 4 or a `block stopped` line in step 10 is a defect of the port: stop and report it.

## Results
(none yet)
