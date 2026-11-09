/* SPDX-License-Identifier: GPL-2.0 */
/* Finds a block's number on the control node by its device name. Copyright (c) 2026 the Lemnos authors. */
#ifndef LEMNOS_TEST_FIND_BLOCK_H
#define LEMNOS_TEST_FIND_BLOCK_H

#include <string.h>
#include <sys/ioctl.h>

#include "../lemnos.h"

/* The block's number, or 0 if no block has that name. */
static inline __u32 find_block(int fd, const char *name)
{
	struct lemnos_next n = { 0 };

	while (!ioctl(fd, LEMNOS_IOC_NEXT, &n)) {
		if (!strcmp(n.name, name))
			return n.block;
		n.after = n.block;
	}
	return 0;
}

#endif
