################################################################################
# Linux extension: build the Lemnos sensor bridge into the kernel tree
#
# For a Buildroot external tree (a device package's buildroot-external tree, or a
# BCM2712 image's Buildroot tree): copy
# the whole kernel-modules/lemnos-sensor-bridge directory to
# linux/lemnos-sensor-bridge in the external tree, then place this file (and the
# Config.ext.in entries) in linux/ next to it. Buildroot includes
# linux/linux-ext-*.mk automatically.
#
# Kernel-agnostic: the module is staged as drivers/media/platform/lemnos/ of
# whatever kernel the package builds (Raspberry Pi 6.12 for image release 1.0.x,
# rpi-7.2.y for 1.1.0) and built as part of it (=m), so it is built with
# that kernel's own Module.symvers, config and compiler and always matches the
# image. The overlays are compiled by that kernel's dtc.
################################################################################

ifeq ($(BR2_LINUX_KERNEL_EXT_LEMNOS_SENSOR_BRIDGE),y)

LEMNOS_BRIDGE_EXT_DIR := $(dir $(lastword $(MAKEFILE_LIST)))lemnos-sensor-bridge
LEMNOS_BRIDGE_KDIR = $(LINUX_DIR)/drivers/media/platform/lemnos
LEMNOS_BRIDGE_OVERLAYS = lemnos-sensor-bridge-bcm2712 lemnos-cam0-i2c-fast

define LEMNOS_BRIDGE_COPY_FILES
	@echo "[lemnos-bridge] staging lemnos_sensor_bridge into the kernel tree"
	@mkdir -p $(LEMNOS_BRIDGE_KDIR)
	@cp -f $(LEMNOS_BRIDGE_EXT_DIR)/lemnos_sensor_bridge.c \
		$(LEMNOS_BRIDGE_EXT_DIR)/lemnos_sensor_bridge.h \
		$(LEMNOS_BRIDGE_EXT_DIR)/Kconfig $(LEMNOS_BRIDGE_KDIR)/
	@cp -f $(LEMNOS_BRIDGE_EXT_DIR)/Kbuild $(LEMNOS_BRIDGE_KDIR)/Makefile
	@grep -q "^obj-y += lemnos/" $(LINUX_DIR)/drivers/media/platform/Makefile || \
		echo 'obj-y += lemnos/' >> $(LINUX_DIR)/drivers/media/platform/Makefile
	@grep -q "platform/lemnos/Kconfig" $(LINUX_DIR)/drivers/media/platform/Kconfig || \
		sed -i '/^endif # MEDIA_PLATFORM_DRIVERS/i source "drivers/media/platform/lemnos/Kconfig"' \
			$(LINUX_DIR)/drivers/media/platform/Kconfig
	@grep -q "platform/lemnos/Kconfig" $(LINUX_DIR)/drivers/media/platform/Kconfig || \
		{ echo "[lemnos-bridge] could not wire Kconfig"; exit 1; }
endef

LINUX_POST_PATCH_HOOKS += LEMNOS_BRIDGE_COPY_FILES

define LEMNOS_BRIDGE_ENABLE_CONFIG
	$(LINUX_DIR)/scripts/config --file $(LINUX_DIR)/.config --module VIDEO_LEMNOS_SENSOR_BRIDGE
	$(MAKE) -C $(LINUX_DIR) $(LINUX_MAKE_FLAGS) olddefconfig
endef

LINUX_POST_CONFIGURE_HOOKS += LEMNOS_BRIDGE_ENABLE_CONFIG

# Compile the overlays; the image's post-build copies $(BINARIES_DIR)/*.dtbo
# like the other device overlays (adjust to the board's overlay handling).
define LEMNOS_BRIDGE_BUILD_OVERLAYS
	for o in $(LEMNOS_BRIDGE_OVERLAYS); do \
		$(LINUX_DIR)/scripts/dtc/dtc -@ -q -I dts -O dtb \
			-o $(BINARIES_DIR)/$$o.dtbo \
			$(LEMNOS_BRIDGE_EXT_DIR)/dts/$$o-overlay.dts || exit 1; \
	done
endef

LINUX_POST_INSTALL_IMAGES_HOOKS += LEMNOS_BRIDGE_BUILD_OVERLAYS

endif
