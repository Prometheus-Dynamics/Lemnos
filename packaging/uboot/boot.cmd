# Starts the board's slot from the record the slot driver keeps (record = "uboot").
# Build: mkimage -A <arch> -O linux -T script -C none -d boot.cmd boot.scr
# Install as BOOT/boot.scr: the built script at the root of the boot partition (BOOT is that partition's
# root; packaging/files.txt). A board that boots from raw NAND takes boot-ubi.cmd instead.
# It takes devtype, devnum and distro_bootpart from U-Boot's standard boot flow, and needs
# sysboot, env import and export, test, and write support for the boot partition (save).
# It never uses the saved U-Boot environment or a boot count.
#
# The record's trial_attempts counts the boots that may still start the trial. Before a trial
# boot the script takes one, writes the record, and only then starts the trial: a boot that never
# reaches the daemon has used its attempt. At 0 (or none) it starts the default.

setenv slot_part ${devnum}:${distro_bootpart}
setenv default
setenv trial
setenv trial_attempts
setenv left

if load ${devtype} ${slot_part} ${scriptaddr} boot-record.env; then
	env import -t ${scriptaddr} ${filesize} default trial trial_attempts
	if test -n "${trial}"; then
		if test "${trial_attempts}" = "3"; then
			setenv left 2
		elif test "${trial_attempts}" = "2"; then
			setenv left 1
		elif test "${trial_attempts}" = "1"; then
			setenv left 0
		fi
		if test -n "${left}"; then
			setenv trial_attempts ${left}
			env export -t ${scriptaddr} default trial trial_attempts
			if save ${devtype} ${slot_part} ${scriptaddr} boot-record.env ${filesize}; then
				sysboot ${devtype} ${slot_part} any ${pxefile_addr_r} /slots/${trial}/extlinux/extlinux.conf
			fi
		fi
	fi
	if test -n "${default}"; then
		sysboot ${devtype} ${slot_part} any ${pxefile_addr_r} /slots/${default}/extlinux/extlinux.conf
	fi
fi
sysboot ${devtype} ${slot_part} any ${pxefile_addr_r} /extlinux/extlinux.conf
