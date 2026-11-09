# lemnos-drivers-rkvpss

Rockchip's `rkvpss` offline scaler, through the vendor kernel's `rkvpss-offline` video node. One
job reads a dma-buf and writes up to six others, each with its own crop, scale, pixel format and
vertical flip. Written from the vendor driver's source for the RV1126B. **Untested on board.**

```rust,no_run
use lemnos_drivers_rkvpss::{find, Channel, Crop, Job, Output, PixelFormat, Rkvpss, Surface};
# fn demo(src: std::os::fd::BorrowedFd<'_>, dst: std::os::fd::BorrowedFd<'_>) -> std::io::Result<()> {
let vpss = Rkvpss::open(find().expect("no rkvpss-offline node"))?;
vpss.select_offline(Channel::Ch0.bit())?;
let source = Surface::new(src, PixelFormat::Nv12, 1920, 1080, 1920)?;
let target = Surface::new(dst, PixelFormat::Nv12, 640, 360, 640)?;
let output = Output { channel: Channel::Ch0, crop: Crop::full(1920, 1080), target, flip: false };
vpss.run(&mut Job::new(source, &[output])?)?;
# Ok(())
# }
```

A job can only be built from surfaces checked against the size of their dma-bufs. The driver does
less: it takes kernel pointers from the request, measures a buffer against `stride * height`, and
places the chroma plane by another field. Those fields are always zero here.

Left out: tiled and compressed (FBC) formats, privacy masks, background fill, the encoder ring,
mirroring, quarter turns and the average scaler.

To check on a board:

- Whether CH0 reaches 1:32. The driver's check allows it, while the header's comment gives 1:8
  across and 1:6 down for CH0.
- Whether a stride that is a multiple of four is enough. The driver checks four, the header's
  comment says 16.
- Whether 4096x3072 is the single-pass limit. The header gives it; the driver's constant was not
  in the sources read.
- Which channels the camera path owns on a given image: selecting a channel takes it from there.
