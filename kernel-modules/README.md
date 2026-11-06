# kernel-modules

Kernel modules that Lemnos owns. Everything in this directory is GPL-2.0 and is kept apart from
the rest of the repository, which is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).

- **Out of tree.** Each module builds outside the kernel source tree, against the exact kernel
  tree of the image it runs on: the configured and built tree that produced the image, not just
  the same version. A module only loads when its vermagic and symbol CRCs match that kernel.
- **Camera-stack image variants only.** A module here goes into the images of the camera stack
  and nowhere else. It is never built into any other image variant.
- **Not shipped.** No image built from this repository includes a module from this directory
  today. The packaging under `packaging/` does not copy it.

## Contents

- `lemnos-sensor-bridge/`: the sensor bridge, a V4L2 subdevice that carries a camera sensor driven
  from user space. Its user-space side is the `lemnos-sensor-bridge` crate. See
  `lemnos-sensor-bridge/README.md` and `lemnos-sensor-bridge/PROTOCOL.md`.
- `lemnos-pisp-be/`: the Raspberry Pi 5 PiSP back end as a character device, an opt-in fast path
  beside the stock `pispbe` driver, bound only through `driver_override`. Its user-space side is
  `lemnos-drivers-pisp`'s `be-module` feature. See `lemnos-pisp-be/README.md`.
- `lemnos-rp1-csi2/`: the Raspberry Pi RP1 CSI-2 receiver (D-PHY, CSI-2 host and one DMA
  channel) as a character device, an opt-in fast path beside the stock `rp1-cfe` driver, bound
  only through `driver_override`. The sensor is set up from user space over i2c-dev. Its
  user-space side is `lemnos-drivers-pisp`'s `csi2-module` feature. See
  `lemnos-rp1-csi2/README.md`.
- `kernel-env.sh`: the kernel selection that the modules' build scripts source.

## Licence boundary

The crates in `crates/` do not link these modules and do not take code from them. They talk to a
module only through its uAPI (controls, events and ioctls), which the kernel offers to any
program. The reasoning is in `docs/kernel-module.md`, section 4. A module's own copyright notices
stay as they are.
