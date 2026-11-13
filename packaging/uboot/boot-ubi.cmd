# Starts the board's slot on raw NAND from the record the slot driver keeps (record = "ubi").
# Build: mkimage -A <arch> -O linux -T script -C none -d boot-ubi.cmd boot.scr
# Install as BOOT/boot.scr: the built script at the root of the boot medium U-Boot loads it from (packaging/files.txt).
# Only on a board that boots from raw NAND; boot.cmd is for the others.
# It takes the MTD partition that holds UBI from ubi_mtd (the board's default environment sets
# it), and needs the ubi and ubifsmount commands, sysboot over UBIFS, env import and export, and
# test. The record is the static volume boot-record; the slots' kernels and extlinux.conf files
# are in the UBIFS volume boot. It never uses the saved U-Boot environment or a boot count.
#
# The record's trial_attempts counts the boots that may still start the trial, as in boot.cmd:
# a trial boot takes one, the record is written, and then the trial starts. At 0 it starts the
# default.

setenv default
setenv trial
setenv trial_attempts
setenv left

ubi part ${ubi_mtd}
ubifsmount ubi0:boot
if ubi read ${scriptaddr} boot-record; then
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
			if ubi write ${scriptaddr} boot-record ${filesize}; then
				sysboot ubi 0:boot any ${pxefile_addr_r} /slots/${trial}/extlinux/extlinux.conf
			fi
		fi
	fi
	if test -n "${default}"; then
		sysboot ubi 0:boot any ${pxefile_addr_r} /slots/${default}/extlinux/extlinux.conf
	fi
fi
sysboot ubi 0:boot any ${pxefile_addr_r} /extlinux/extlinux.conf
