/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The kernel headers the Rust half needs and the kernel's own Rust bindings leave out
 * (rust/bindings/bindings_helper.h), and the module's own uAPI. The Kbuild runs the kernel's bindgen
 * over this file with the kernel's own C flags; rs/bindings.rs includes the result. Only the items
 * the Kbuild allowlists are generated: every other type they name is the kernel crate's.
 */
#include <linux/spi/spi.h>
#include <linux/thermal.h>
#include <uapi/linux/media/raspberrypi/pisp_fe_config.h>

#include "../lemnos.h"
#include "../lemnos_regjob.h"
#include "../lemnos_gsdma.h"
#include "../lemnos_vip.h"
#include "../lemnos_pisp_fe.h"
#include "../lemnos_rp1_csi2_priv.h"
#include "../lemnos_stream_priv.h"
