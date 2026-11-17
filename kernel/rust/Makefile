# SPDX-License-Identifier: GPL-2.0
#
# The Rust half of lemnos.ko (docs/kernel-module.md). Built out of tree against a kernel with
# CONFIG_RUST=y: make -C <kernel build> M=$(pwd) LLVM=1 modules. Stage 0 is a skeleton: a platform
# driver and a miscdevice. It loads beside the C module under its own names until the port is done.

obj-m := lemnos_rs.o
lemnos_rs-y := lemnos_rs_main.o lemnos_helpers.o

# The module's one C file, kernel/lemnos_helpers.c, is shared with lemnos.ko: this builds it here too.
ccflags-y := -Werror
$(obj)/lemnos_helpers.o: $(src)/../lemnos_helpers.c FORCE
	$(call if_changed_rule,cc_o_c)
