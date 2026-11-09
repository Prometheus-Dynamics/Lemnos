# lemnos-drivers-g2d

The Allwinner G2D 2D engine (A733 and the BSP's other `g2d_rcq` chips) through the BSP kernel's
`/dev/g2d`: fill, copy with scaling and format conversion, quarter turns and flips, and
Porter-Duff blending of two RGB layers, between dma-bufs. No vendor library.

**Status: written from the BSP driver's source, untested on board.** The structures' sizes and
offsets are pinned against the C compiler; no request has reached an engine.

The BSP driver does not compare an image with its dma-buf, and a request can name a physical
address. So a `Job` can be built only from a `Surface`, whose frame was checked against the
dma-buf's real size, and rectangles checked against that surface.

Not covered: mask ROPs, batched mixer tasks, LBC rotation, stride alignment (a padded row is a
wider surface), YUV blend layers (the driver's own check refuses them), the mainline kernel (it
has no G2D driver yet).

Unverified until a board runs it: the fill colour's byte layout, the NV12/NV21 chroma order, and
the scaling limits.
