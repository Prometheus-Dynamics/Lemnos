/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The blocks in the lemnos module with a uAPI of their own (their DMA reaches
 * any address a register names, so the module writes their jobs). Each keeps
 * the platform driver name, character device and ioctls it had as a module of
 * its own.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef LEMNOS_PERSONALITY_H
#define LEMNOS_PERSONALITY_H

#include <linux/platform_device.h>

extern struct platform_driver lemnos_pisp_be_driver;	/* lemnos_pisp_be.c */
extern struct platform_driver lemnos_rp1_csi2_driver;	/* lemnos_rp1_csi2.c */

#endif
