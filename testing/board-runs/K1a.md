# K1a board run: VEPU541 H.264 encoder layouts (`lemnos-driver-rkvenc`)

Status: untested on board. The layouts and the write-side builder are pinned against the header
they were transcribed from, and the builder's refusals and job shape are tested over rk-mpp's
played kernel. No register has been read or written on a device by this PR: the crate has no
device side, and rk-mpp has no block for this encoder yet.

This encoder is not on the reference CM5 board (BCM2712). The run needs a board with a VEPU541
H.264 encoder node behind the vendor kernel's `/dev/mpp_service`, and the node must be bound to the
rk-mpp service. Build on NVMe or SSD, never on the Github HDD. Module loads and unbinds are allowed;
report each.

## Checks

1. **Identity.** Read `reg000` (VERSION, word 0) through the service once the claim is bound.
   Pass: `h264_enc` (bit 8) is 1, and `rkvenc_ver` (bits 24 to 31) is `0x50`, the value the header
   comment gives as the default IP identifier. Any other value: the layout's window or the block is
   wrong; stop.
2. **Read-back window.** Submit one job with the write side zeroed and the read-back window of
   `drivers/rkvenc` (`READ_BACK`: word 7, 202 words). Pass: the completion returns without a
   fault. Record `hw_status` (word 7) and compare its bits with the names in `ret::hw_status`.
3. **Stream length and the address alignments.** From one encode of a small test picture, read
   `ST_BSL` (`bs_lgth`, 27 bits) and compare it with the bytes the caller wrote. Pass: the two
   agree. Also submit `ADR_SRC0` (the source buffer, no alignment in the header) at offset 0 and
   `CMVW_ADDR` (aligned to 1024 bytes in the header) at offset 1024: pass: both jobs complete.
4. **The second window is not reached.** The L2 registers sit at the second window's base, beyond
   the write side. Pass: the write side's jobs complete with the L2 window untouched (read it once
   before and after a job; the values are equal).

Record each result with the kernel version and the board's device tree compatible string.
