# K1d board run: the A733 encoder's QP map and CTU info records (`lemnos-driver-a733-video`, `encoder`)

Status: untested on board. Nothing here was read or written on a device, and no job runs on the
engine: the records are pinned to the vendor header by a compiled probe (gcc and clang on x86_64),
and the tests check the packing and the refusals (`drivers/a733-video/src/encoder/tests.rs`).

The engine has no register layout in the header, so the check for these records cannot yet be
written: a run needs the facts listed under "Not checked here" first.

## Checks, once the facts are known

1. **Where the QP map is read from.** The register the engine takes the QP map's bus address from
   (and whether it takes the CTU info buffer's), written through the module route with a buffer of
   `qp_map::pack_map` output. Pass: the job completes with no fault and the IOMMU reports no fault
   for the map's range.
2. **QP map effect.** One encode at rate control `QPMAP` (value 3) with every entry set to a QP
   from 0 to 63 in turn. Pass: the stream length changes with the QP, and no entry with `en` clear
   changes the output against the same picture with the map zeroed.
3. **CTU info.** Read the CTU info buffer after an encode. Pass: each word's `r0` field (bits 3 to 7)
   is zero, and the words decode with `ctu_info::CtuInfo::from_word` to flags and values in range.

## Not checked here

- The register each record's address goes to, and the buffer's size: the header gives neither.
- Whether the engine reads the QP map, writes the CTU info, or both: the header does not say.
- Which macroblock each byte of a CTU word covers, and the map's stride: the header gives `mb[4]`
  and no order.
- The H.264, H.265 and JPEG parameters' register encodings, the bitstream's header and trailer, and
  how the engine reports a picture's length. The header defines their parameters (`VencH264Param`,
  `VencH265*`, `JpegEncInfo`) and not their registers.
