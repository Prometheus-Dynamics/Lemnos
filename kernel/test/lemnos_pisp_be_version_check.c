// SPDX-License-Identifier: GPL-2.0
/*
 * The PiSP back end's version read-back (LEMNOS_IOC_PISP_BE_VERSION) on the fake block: the value
 * the probe read and accepted, read through the control node, for the block the name finds.
 *
 * Usage: lemnos_pisp_be_version_check /dev/lemnos <device> <expected hex version>
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "../lemnos.h"
#include "../lemnos_pisp_be.h"
#include "find_block.h"

int main(int argc, char **argv)
{
	if (argc != 4) {
		fprintf(stderr, "usage: %s /dev/lemnos <device> <version>\n", argv[0]);
		return 2;
	}
	int fd = open(argv[1], O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		perror("open");
		return 1;
	}
	__u32 block = find_block(fd, argv[2]);
	if (!block) {
		fprintf(stderr, "no block %s\n", argv[2]);
		return 1;
	}
	struct lemnos_pisp_be_version v = { .block = block };
	if (ioctl(fd, LEMNOS_IOC_PISP_BE_VERSION, &v)) {
		perror("LEMNOS_IOC_PISP_BE_VERSION");
		return 1;
	}
	__u32 want = (__u32)strtoul(argv[3], NULL, 0);
	if (v.hw_version != want) {
		printf("version 0x%08x, want 0x%08x\n", v.hw_version, want);
		return 1;
	}
	printf("version 0x%08x read back for block %u\n", v.hw_version, block);
	close(fd);
	return 0;
}
