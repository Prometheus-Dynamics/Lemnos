/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The kernel headers the test fixtures need and the kernel's own Rust bindings leave out
 * (rust/bindings/bindings_helper.h). The Kbuild runs the kernel's bindgen over this file with the
 * kernel's own C flags; fixture.rs includes the result. Only the items the Kbuild allowlists are
 * generated: every other type they name is the kernel crate's.
 */
#include <linux/clk-provider.h>
#include <linux/clkdev.h>
#include <linux/irq_sim.h>
#include <linux/regulator/driver.h>
#include <linux/regulator/machine.h>
#include <linux/reset-controller.h>
#include <linux/spi/spi.h>
#include <linux/thermal.h>
