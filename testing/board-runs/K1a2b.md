# K1a2b board run: the VEPU580 H.264 encoder through `rk-mpp:rkvenc2` (`lemnos-driver-rkvenc`)

Status: untested on board. Nothing here was read or written on a device. The layouts are pinned to
the header with gcc and clang; the job is checked on rk-mpp's played kernel and by the session's own
chain check.

This encoder is not on the reference CM5 board. The run needs a board with a VEPU580 node behind
the vendor kernel's `/dev/mpp_service`, bound to the `mpp_rkvenc2` platform driver (and not to
`mpp_rkvenc`, which would make the claim ambiguous and refused). Build on NVMe or SSD, never on the
Github HDD. Module loads and unbinds are allowed; report each.

## Checks

1. **Identity.** Read `reg0000` (VERSION, word 0) through the service once the claim is bound.
   Pass: `CAP` (bit 8) is 1 and `IP_ID` (bits 24 to 31) is `0x50`. Any other value: stop.
2. **Status read-back.** Submit one job with only `TQ8_CKG` set (word 22, BASE class) and no
   address. Pass: the completion returns without a fault and the status class (words 4096 to 4275)
   reads back.
3. **Picture-class translation and the offset.** Submit a job with `ADR_SRC0` (word 160, PIC class,
   translated) at offset 4096 of a claim buffer. Pass: the job completes, and the job's table has
   one entry, index 0, relative to the PIC class (the kernel reads it as `reg[0]` of the class).
4. **OSD and debug are not reached.** Nothing in this run writes the OSD class. Pass: the OSD words
   read back as they were before the job.

Record each result with the kernel version and the board's device tree compatible string.
