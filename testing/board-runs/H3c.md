# H3c board run: `nonai2d` and `vglite` as contract drivers

Status: untested on board. Both drivers are tested on the host's own streams with sealed memfds
standing in for dma-bufs and an engine or GPU that only counts; no job has reached either block
through a driver. The blocks' own checks are `E5.md` and `E6.md` (`nonai2d`) and `E3.md`
(`vglite`): run those first, since a driver can only carry what they show works.

Run in a local session after this PR merges, on a K230 board running the K230 Linux SDK's kernel,
with the `nonai-2d` V4L2 node and `/dev/vg_lite` present, and a contiguous DMA heap
(`/dev/dma_heap/linux,cma` or the board's). Build on NVMe or SSD, never on the Github HDD. Module
loads and unbinds are allowed; report each.

## Checks

1. **The nodes.** `v4l2-ctl --list-devices` (record which `/dev/video*` is `nonai-2d`),
   `ls -l /dev/vg_lite /dev/dma_heap/`. Record the kernel version.
2. **Both bind from a board file.** A board file with a `nonai2d` device (no `device` setting)
   and a `vglite` device with both a `scale` and a `convert` stream, every stream
   `memory = "dma-buf"` on the contiguous heap. `lemnosd --board <file>`; `lemnos-ctl list`.
   Pass: both `running`, health `ok`. Then the same file with one stream's `memory` line removed:
   that device is `retrying` with `NOT_SUPPORTED`.
3. **`nonai2d` jobs.** With a consumer of the `jobs` stream (the `image.convert/1` example
   client, or a few lines over `lemnos-client`), post 1000 conversions `AR24` to `XR24` at
   1920x1080 and 100 of each listed pair at 64x32 with known pixels. Pass: every completion's
   length is the output frame's, the bytes are the input's in the output's order, and
   `lemnos-ctl call <id> counters` shows the jobs and no errors. Record the time per 1080p job.
4. **`vglite` jobs.** On the `scale` stream: 100 bilinear scales 640x480 to 320x240 and back, 100
   area halvings, in each of `AB24`, `AR24`, `XB24`, `R8  `; on the `convert` stream: each of the
   10 listed pairs at 64x32. Compare against `E3.md`'s reference for the same operation through
   `Vglite` directly: byte-equal. A pair the GPU writes wrongly (for example the `X` byte of
   `XB24`) is recorded, not hidden. Record the time and the staged bytes per 1080p job.
5. **What a job names wrongly.** On each device post a job whose output stride takes the frame one
   row past its dma-buf. Pass: it completes empty, `errors` counts it, and `dmesg` shows no IOMMU
   or bus fault and nothing from either kernel driver.
6. **Recovery.** While jobs run, unload the `vg_lite` module (or unbind the GPU) and the
   `nonai-2d` module; wait 20 s; load them again. Pass: jobs complete empty meanwhile, health is
   degraded with the reason, the reopen is tried 250 ms after the failure doubling to 8 s, and
   jobs pass again once the modules are back. Neither device is ever `off`.
7. **A consumer that goes.** Kill the consumer mid-run, attach a new one, run check 3 or 4 again.
   Pass: the new consumer's jobs complete on its own buffers.
