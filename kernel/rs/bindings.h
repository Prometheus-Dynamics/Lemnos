/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The kernel headers the Rust half needs and the kernel's own Rust bindings leave out
 * (rust/bindings/bindings_helper.h), and the module's own uAPI. The Kbuild runs the kernel's bindgen
 * over this file with the kernel's own C flags; rs/bindings.rs includes the result. Only the items
 * the Kbuild allowlists are generated: every other type they name is the kernel crate's.
 */
#include <linux/anon_inodes.h>
#include <linux/shmem_fs.h>
#include <linux/spi/spi.h>
#include <linux/thermal.h>
#include <linux/vmalloc.h>

#include "../lemnos.h"
#include "../lemnos_regjob.h"
#include "../lemnos_gsdma.h"
#include "../lemnos_vip.h"
