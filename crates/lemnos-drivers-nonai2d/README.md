# lemnos-drivers-nonai2d

The Kendryte K230 non-AI 2D engine through the `nonai-2d` V4L2 mem2mem driver of the K230 Linux
SDK: pixel-format conversion between dma-bufs, at one size, up to 1920x1080. No vendor library.

**Status: written from the SDK driver's source, untested on board.**

The driver is multi-planar and takes one dma-buf per plane: two for NV12 and NV21, three for I420
and YV12. Its buffers are physically contiguous (`vb2_dma_contig`), so they come from a CMA heap.

Not covered: the OSD and border modes (their controls pass user pointers and physical addresses),
the conversion coefficient control, and the driver's three-plane RGB.
