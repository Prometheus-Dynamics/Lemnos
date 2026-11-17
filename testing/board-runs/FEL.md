# FEL: the Allwinner FEL thunk, board checks

Status: **untested on board.** This revision contacted no board, wrote nothing to one, and rebooted none. What has run is
the host side: the build reproduces the pinned bytes, the layout offsets match the symbols, and the pinned bytes run under
user-mode emulation (`qemu-arm-static` and `qemu-aarch64-static`) with six tests per target, checked by
`drivers/allwinner-fel-sys/scripts/check-blob.sh`. None of that shows the boot ROM's FEL loop, the SoC's watchdog or the
SRAM. The checks below do.

Provenance: the FEL flow and the thunk's layout come from the PC-side project's clean-room record (Atlas,
`crates/atlas-socflash/src/fel/mod.rs`, which cites public linux-sunxi wiki pages and states that no sunxi-tools or
U-Boot source was used). The thunk here adds the watchdog and the bounds, so its bytes are not that project's bytes.
The AArch64 thunk has no reference bytes; the A733's FEL state is UNCONFIRMED in that record.

## What the host side showed (this revision)

| Check | Command | Result |
|---|---|---|
| The build reproduces the pinned words, both targets | `drivers/allwinner-fel-sys/scripts/check-blob.sh` | `364 bytes equal the pinned words; offsets match the layout` (armv7), `456 bytes equal the pinned words; offsets match the layout` (AArch64) |
| The thunk section has no relocations | the same script | refused otherwise (found once, before labels were made local) |
| The harness, armv7 under qemu-user | the same script | `test result: ok. 6 passed; 0 failed` |
| The harness, AArch64 under qemu-user | the same script | `test result: ok. 6 passed; 0 failed` |

## Board checks, in order

Each check records its measured value and the date and the build id (`lemnos-allwinner-fel-blob` layout hash and blob hash)
in this file when it runs. A check that fails is a finding against the blob, not a reason to change a pinned word without
a new version.

1. **Which instruction set the FEL loop runs.** The A733's FEL state is UNCONFIRMED. Write the armv7 blob (364 bytes)
   and the AArch64 blob (456 bytes) to the same SRAM address in turn, each with the return-only entry
   (`0xe12fff1e` for armv7, `0xd65f03c0` for AArch64, one word at a free address). Run each with FEL_RUN. Required: exactly
   one of the two returns and the FEL loop answers its version request afterwards. Record which one returned.

2. **Return is proven before any write.** With the blob that returned in check 1, repeat the return-only run, then read the
   FEL version request. Required: the request answers. Only after this may the real SPL entry and its data be written.

3. **The watchdog's values for the SoC.** The address, arm and disarm values are the SoC's (the caller's data). Find them
   from the SoC's watchdog register map, write them into the fields, and run the return-only entry. Required: the FEL loop
   answers afterwards, and the watchdog reads back as disarmed (the disarm value).

4. **A bad table ends in a reset, not a hang.** Write the armv7 or AArch64 blob with a valid swap, then patch the first
   table entry's size word (at `table_offset + 8`, which is 216 for armv7 and 240 for AArch64) to 6. Run FEL_RUN. Required:
   the board resets within the watchdog's period, and the FEL loop does not answer before the reset. Record the time from the
   run to the reset.

5. **An SPL run through the swap.** With the real SRAM addresses, write an SPL whose load address lies in a swap's stack
   buffer, through the thunk, so the thunk swaps its bytes into place. Required: the SPL runs from its entry, and the FEL loop
   answers after it returns. Record the buffers' contents before and after.

6. **The watchdog covers the SPL.** The thunk does not feed the watchdog while the SPL runs. Required: the SPL's longest run
   is shorter than the watchdog's period; record both.

## Result

Not run. No board was contacted for this revision.
