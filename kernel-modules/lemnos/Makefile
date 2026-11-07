# SPDX-License-Identifier: GPL-2.0
#
# Out-of-tree build: make KDIR=<the image's built kernel tree> ARCH=arm64 CROSS_COMPILE=...

KDIR ?= /lib/modules/$(shell uname -r)/build
# modules_install strips debug information (the shipped module); a build keeps it.
INSTALL_MOD_STRIP ?= 1
export INSTALL_MOD_STRIP

all: modules

modules modules_install clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) $@

.PHONY: all modules modules_install clean
